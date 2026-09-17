//! Damaging-wind products (verified spec: docs/hail-wind-algo-spec.md).
//!
//! MARC — Mid-Altitude Radial Convergence (Schmocker, Przybylinski & Lin
//! 1996, 15th Conf. Wea. Analysis & Forecasting, 306–311; Przybylinski 1995,
//! Wea. Forecasting 10, 203–218): the velocity difference between the maximum
//! inbound and maximum outbound within ~6 km along a single radial, in the
//! 3–7 km layer. ΔV ≥ 25 m/s (50 kt), persistent and deep, precedes damaging
//! surface winds in bow echoes/QLCS by 15–20 min (NWS operational guidance).
//! Caveat from the literature: the signature is masked where mid-level flow
//! runs normal to the beam — the display is a *precursor aid*, not truth.
//!
//! Low-level gust proxy — Smith, Elmore & Dulin (2004, Wea. Forecasting 19,
//! 240–250): radar radial wind observed with the beam centerline below ~1 km
//! maps ≈1:1 to a surface gust in NWS research practice (≥25 m/s ≈ severe).
//! The product is |dealiased Vr| on the lowest velocity tilt, masked to
//! beam heights < 1 km above the radar.

use rayon::prelude::*;
use recast_radar_core::{
    Field, FieldName, Quantity, Sweep, Volume, beam_ground_range_m, beam_height_above_radar_m,
};
use recast_radar_correct::dealias_velocity;

use crate::sweep::physical_field;

/// MARC layer bounds (m above radar) — Schmocker et al. 1996 / NWS LMK.
const MARC_LAYER_BOTTOM_M: f64 = 3000.0;
const MARC_LAYER_TOP_M: f64 = 7000.0;
/// Along-radial search half-window, meters. [ENG] 3 km each side of the
/// gate keeps the max inbound–outbound pair separation ≤ 6 km, matching the
/// published "within 6 km along a single radial" definition.
const MARC_HALF_WINDOW_M: f64 = 3000.0;

/// Name of the MARC composite field.
pub const MARC_NAME: &str = "MARC";
/// Name of the low-level gust proxy field.
pub const GUST_NAME: &str = "GUST";

/// Sliding window max-inbound-vs-max-outbound convergence per gate.
/// Convergent orientation: outbound (positive Vr) NEARER the radar than
/// inbound (negative Vr) — i.e. ΔV = max(V near) − min(V far) > 0.
/// 3-gate median along the radial — kills single-gate dealias spikes that
/// would otherwise fabricate enormous ΔV (observed: 138 m/s on the KEAX
/// derecho from one bad gate pair). NaN-tolerant: needs 2 finite of 3.
fn median3(values: &[f32]) -> Vec<f32> {
    let n = values.len();
    let mut out = vec![f32::NAN; n];
    for (g, cell) in out.iter_mut().enumerate() {
        let mut window: Vec<f32> = (g.saturating_sub(1)..=(g + 1).min(n - 1))
            .map(|i| values[i])
            .filter(|v| v.is_finite())
            .collect();
        if window.len() >= 2 {
            window.sort_by(f32::total_cmp);
            *cell = window[window.len() / 2];
        }
    }
    out
}

fn radial_convergence_row(values: &[f32], half_window_gates: usize) -> Vec<f32> {
    let values = median3(values);
    let n = values.len();
    let mut out = vec![f32::NAN; n];
    if n == 0 {
        return out;
    }
    for g in 0..n {
        let near_start = g.saturating_sub(half_window_gates);
        let far_end = (g + half_window_gates).min(n - 1);
        let mut near_max = f32::NEG_INFINITY;
        for &v in &values[near_start..=g] {
            if v.is_finite() && v > near_max {
                near_max = v;
            }
        }
        let mut far_min = f32::INFINITY;
        for &v in &values[g..=far_end] {
            if v.is_finite() && v < far_min {
                far_min = v;
            }
        }
        if near_max.is_finite() && far_min.is_finite() {
            let delta = near_max - far_min;
            // [ENG] ΔV > 70 m/s exceeds anything in the MARC literature
            // (Funk et al. case max ≈ 38) — at that magnitude it is a
            // residual-fold artifact, not meteorology. Reject, don't cap.
            if delta > 0.0 && delta <= 70.0 {
                out[g] = delta;
            }
        }
    }
    out
}

/// One velocity sweep prepared for the MARC composite.
struct VelSweep {
    elevation_deg: f32,
    az_rows: Vec<(f32, usize)>,
    conv: Vec<f32>, // rows x gates ΔV field
    gates: usize,
    first_gate_m: f64,
    gate_spacing_m: f64,
}

impl VelSweep {
    fn nearest_row(&self, az: f32) -> Option<usize> {
        if self.az_rows.is_empty() {
            return None;
        }
        let idx = self
            .az_rows
            .partition_point(|(a, _)| *a < az)
            .min(self.az_rows.len() - 1);
        let after = self.az_rows[idx];
        let before = self.az_rows[idx.saturating_sub(1)];
        let pick = |c: (f32, usize)| {
            let mut d = (c.0 - az).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            (d, c.1)
        };
        let (da, ra) = pick(after);
        let (db, rb) = pick(before);
        let (d, row) = if da <= db { (da, ra) } else { (db, rb) };
        // ~1.5 beamwidths max — beyond that the radial doesn't cover az.
        (d <= 1.5).then_some(row)
    }
}

/// Per-row azimuths of a field's rows, normalized to `[0, 360)`.
fn row_azimuths(sweep: &Sweep, field: &Field) -> Vec<f32> {
    (0..field.nrays as usize)
        .map(|r| {
            sweep
                .rays
                .azimuth_deg
                .get(r)
                .map(|azimuth| azimuth.rem_euclid(360.0))
                .unwrap_or(f32::NAN)
        })
        .collect()
}

fn velocity_sweeps_from_dealiased(
    volume: &Volume,
    dealiased_velocity: &[Option<&Field>],
) -> Vec<VelSweep> {
    let mut sweeps: Vec<VelSweep> = volume
        .sweeps
        .iter()
        .enumerate()
        .filter_map(|(sweep_index, sweep)| {
            let dealiased = dealiased_velocity.get(sweep_index).copied().flatten()?;
            let (rows, gates) = dealiased.shape();
            if gates == 0 || rows == 0 {
                return None;
            }
            let (first_gate_m, gate_spacing_m) = dealiased.native_geometry(&sweep.range)?;
            let half_gates = ((MARC_HALF_WINDOW_M / gate_spacing_m).round() as usize).max(2);
            let mut az_rows: Vec<(f32, usize)> = row_azimuths(sweep, dealiased)
                .into_iter()
                .enumerate()
                .filter(|(_, az)| az.is_finite())
                .map(|(row, az)| (az, row))
                .collect();
            az_rows.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Per-row convergence (parallel over rows).
            let mut row_values = vec![f32::NAN; rows * gates];
            for row in 0..rows {
                for gate in 0..gates {
                    if let Some(v) = dealiased.value(row, gate) {
                        row_values[row * gates + gate] = v;
                    }
                }
            }
            let conv: Vec<f32> = row_values
                .par_chunks(gates)
                .flat_map_iter(|row| radial_convergence_row(row, half_gates))
                .collect();
            Some(VelSweep {
                elevation_deg: sweep.tilt_elevation_deg(volume.provenance.source_format),
                az_rows,
                conv,
                gates,
                first_gate_m,
                gate_spacing_m,
            })
        })
        .collect();
    sweeps.sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    // SAILS de-dupe: keep the first sweep at each elevation (within 0.1°).
    sweeps.dedup_by(|b, a| (a.elevation_deg - b.elevation_deg).abs() < 0.1);
    sweeps
}

/// Every sweep's velocity field ([`Quantity::RadialVelocity`]), dealiased
/// with the region engine; indexed like `volume.sweeps`.
fn dealias_all(volume: &Volume) -> Vec<Option<Field>> {
    volume
        .sweeps
        .iter()
        .map(|sweep| {
            sweep
                .find(Quantity::RadialVelocity)
                .map(|velocity| dealias_velocity(sweep, velocity))
        })
        .collect()
}

/// MARC ΔV composite (m/s): the max windowed radial convergence across all
/// velocity tilts whose beam centers the 3–7 km layer at that ground range.
/// Display guidance: ≥ 25 m/s is the published damaging-wind precursor.
pub fn marc(volume: &Volume) -> Option<Field> {
    let owned = dealias_all(volume);
    let borrowed: Vec<Option<&Field>> = owned.iter().map(Option::as_ref).collect();
    marc_from_dealiased(volume, &borrowed)
}

/// MARC composite from caller-provided, already-dealiased velocity fields.
/// `dealiased_velocity` is indexed exactly like `volume.sweeps`, each entry a
/// field on that sweep's rays and range; missing entries are skipped. This
/// function never chooses or runs a dealias engine. The result (`MARC`) is on
/// the lowest velocity sweep's rays and native gates.
pub fn marc_from_dealiased(
    volume: &Volume,
    dealiased_velocity: &[Option<&Field>],
) -> Option<Field> {
    let sweeps = velocity_sweeps_from_dealiased(volume, dealiased_velocity);
    if sweeps.is_empty() {
        return None;
    }
    // Output geometry: the lowest velocity sweep's field.
    let base_idx = volume
        .sweeps
        .iter()
        .enumerate()
        .find_map(|(i, s)| s.find(Quantity::RadialVelocity).is_some().then_some(i))?;
    let base_field = dealiased_velocity.get(base_idx).copied().flatten()?;
    let base_sweep = volume.sweeps.get(base_idx)?;
    let (rows, gates) = base_field.shape();
    let (base_first_m, base_spacing_m) = base_field.native_geometry(&base_sweep.range)?;
    let base_elev = f64::from(base_sweep.tilt_elevation_deg(volume.provenance.source_format));
    let row_az = row_azimuths(base_sweep, base_field);
    let mut out = vec![f32::NAN; rows * gates];
    out.par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            let az = row_az[row];
            if !az.is_finite() {
                return;
            }
            for (gate, cell) in out_row.iter_mut().enumerate() {
                let slant = base_first_m + gate as f64 * base_spacing_m;
                let ground = beam_ground_range_m(slant, base_elev);
                let mut best = f32::NAN;
                for sweep in &sweeps {
                    // Gate at this ground range on this tilt (slant ≈ ground
                    // at these elevations; refine via the inverse map).
                    let sweep_gate =
                        ((ground - sweep.first_gate_m) / sweep.gate_spacing_m).round() as isize;
                    if sweep_gate < 0 || sweep_gate as usize >= sweep.gates {
                        continue;
                    }
                    let sweep_gate = sweep_gate as usize;
                    let sweep_slant = sweep.first_gate_m + sweep_gate as f64 * sweep.gate_spacing_m;
                    let height = beam_height_above_radar_m(sweep_slant, sweep.elevation_deg as f64);
                    if !(MARC_LAYER_BOTTOM_M..=MARC_LAYER_TOP_M).contains(&height) {
                        continue;
                    }
                    let Some(sweep_row) = sweep.nearest_row(az) else {
                        continue;
                    };
                    let delta = sweep.conv[sweep_row * sweep.gates + sweep_gate];
                    if delta.is_finite() && (!best.is_finite() || delta > best) {
                        best = delta;
                    }
                }
                if best.is_finite() {
                    *cell = best;
                }
            }
        });
    Some(physical_field(
        base_field,
        FieldName::parse(MARC_NAME),
        Quantity::Other,
        Some("m/s"),
        Some("Mid-altitude radial convergence"),
        out,
    ))
}

/// Low-level gust proxy (m/s): |dealiased Vr| on the lowest velocity tilt,
/// masked to beam-center heights < 1 km above the radar (Smith, Elmore &
/// Dulin 2004: low-beam radial wind ≈ surface gust; ≥ 25 m/s ≈ severe).
pub fn gust_proxy(volume: &Volume) -> Option<Field> {
    let (sweep_index, sweep, velocity) =
        volume.sweeps.iter().enumerate().find_map(|(index, s)| {
            s.find(Quantity::RadialVelocity)
                .map(|field| (index, s, field))
        })?;
    let dealiased = dealias_velocity(sweep, velocity);
    gust_proxy_from_dealiased(volume, sweep_index, &dealiased)
}

/// Low-level gust proxy from one caller-provided, already-dealiased velocity
/// field on the rays and range of sweep `sweep_index`, which also supplies
/// the reflectivity-support mask. This function performs no dealiasing. The
/// result (`GUST`) is on the dealiased field's rays and native gates.
pub fn gust_proxy_from_dealiased(
    volume: &Volume,
    sweep_index: usize,
    dealiased: &Field,
) -> Option<Field> {
    let sweep = volume.sweeps.get(sweep_index)?;
    sweep.find(Quantity::RadialVelocity)?;
    // Reflectivity-support mask: a gust claim needs an echo. Bird/insect
    // and clutter returns in clear air otherwise fabricate severe gusts
    // (observed: 89 m/s "gusts" on an echo-free volume). Both fields are on
    // the sweep's rays, so row `r` of each is ray `r`.
    let reflectivity = sweep
        .find(Quantity::Reflectivity)
        .and_then(|field| Some((field, field.native_geometry(&sweep.range)?)));
    let (rows, gates) = dealiased.shape();
    let (first_m, spacing_m) = dealiased.native_geometry(&sweep.range)?;
    let elev = f64::from(sweep.tilt_elevation_deg(volume.provenance.source_format));
    let mut out = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        let raw: Vec<f32> = (0..gates)
            .map(|gate| dealiased.value(row, gate).unwrap_or(f32::NAN))
            .collect();
        let filtered = median3(&raw);
        for (gate, &v) in filtered.iter().enumerate() {
            let slant = first_m + gate as f64 * spacing_m;
            if beam_height_above_radar_m(slant, elev) >= 1000.0 {
                // Past this range the lowest beam overshoots the surface
                // layer — an honest product stops rather than extrapolates.
                break;
            }
            if let Some((ref_field, (ref_first_m, ref_spacing_m))) = reflectivity {
                // REF gates are coarser (1 km vs 0.25 km) — map by range.
                let ref_gate = ((slant - ref_first_m) / ref_spacing_m).round() as isize;
                let supported = row < ref_field.nrays as usize
                    && ref_gate >= 0
                    && (ref_gate as usize) < ref_field.ngates as usize
                    && ref_field
                        .value(row, ref_gate as usize)
                        .map(|z| z >= 10.0)
                        .unwrap_or(false);
                if !supported {
                    continue;
                }
            }
            if v.is_finite() {
                out[row * gates + gate] = v.abs();
            }
        }
    }
    Some(physical_field(
        dealiased,
        FieldName::parse(GUST_NAME),
        Quantity::Other,
        Some("m/s"),
        Some("Low-level gust proxy"),
        out,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convergence_window_finds_couplet() {
        // Outbound +20 near, inbound -15 far — 3 gates wide each (the
        // median QC by design suppresses single-gate spikes).
        let mut v = vec![f32::NAN; 40];
        v[8..=10].fill(20.0);
        v[14..=16].fill(-15.0);
        let conv = radial_convergence_row(&v, 12);
        // Between the pair the windowed ΔV sees both: 35 m/s.
        assert!((conv[12] - 35.0).abs() < 1e-3, "{}", conv[12]);
        // Divergent orientation (inbound near, outbound far) must NOT fire.
        let mut d = vec![f32::NAN; 40];
        d[8..=10].fill(-15.0);
        d[14..=16].fill(20.0);
        let div = radial_convergence_row(&d, 12);
        assert!(div[12].is_nan() || div[12] <= 0.0);
    }

    #[test]
    fn median_qc_suppresses_single_gate_spike() {
        // A lone +60 gate in a ±10 field must not fabricate ΔV.
        let mut v = vec![10.0f32; 40];
        v[20] = 60.0;
        let conv = radial_convergence_row(&v, 12);
        for value in conv.iter().filter(|value| value.is_finite()) {
            assert!(*value < 5.0, "{value}");
        }
    }

    /// The gust proxy on the Moore Doppler tilt (KTLX 2013-05-20 sweep 1,
    /// 0.5 deg): on each ray, |dealiased velocity| (3-gate median) where the
    /// same ray's reflectivity at that range reaches 10 dBZ and the beam is
    /// below 1 km above the radar, and nothing elsewhere.
    #[test]
    fn gust_proxy_masks_to_echo_and_the_lowest_kilometre() {
        let path = recast_radar_testdata::require_file!("l2-ktlx-20130520-201643-trim");
        let volume = recast_radar_io_nexrad::read_volume_from_path(&path).expect("decode");
        let sweep_index = 1;
        let sweep = &volume.sweeps[sweep_index];
        assert!(volume.sweeps[0].find(Quantity::RadialVelocity).is_none());
        let velocity = sweep.field(&FieldName::Vradh).expect("VRADH");
        let reflectivity = sweep.field(&FieldName::Dbzh).expect("DBZH");
        let dealiased = dealias_velocity(sweep, velocity);
        let gust = gust_proxy_from_dealiased(&volume, sweep_index, &dealiased).expect("gust");
        assert_eq!(gust.name, FieldName::parse(GUST_NAME));
        assert_eq!(gust.shape(), dealiased.shape());
        // `gust_proxy` dealiases the first velocity sweep itself: the same
        // product.
        let own = gust_proxy(&volume).expect("gust");
        assert_eq!(own.to_physical().len(), gust.to_physical().len());
        assert!(
            own.to_physical()
                .iter()
                .zip(gust.to_physical())
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );

        let (first_m, spacing_m) = dealiased.native_geometry(&sweep.range).expect("geometry");
        let (ref_first_m, ref_spacing_m) = reflectivity
            .native_geometry(&sweep.range)
            .expect("reflectivity geometry");
        let elevation = f64::from(volume.tilt_elevation_deg(sweep_index).expect("sweep"));
        let (rows, gates) = gust.shape();
        let (mut values, mut without_echo, mut too_high) = (0usize, 0usize, 0usize);
        for row in 0..rows {
            for gate in 0..gates {
                let value = gust.value(row, gate);
                let slant = first_m + gate as f64 * spacing_m;
                if beam_height_above_radar_m(slant, elevation) >= 1000.0 {
                    assert_eq!(value, None, "row {row} gate {gate} above 1 km");
                    too_high += 1;
                    continue;
                }
                let ref_gate = ((slant - ref_first_m) / ref_spacing_m).round() as isize;
                let echo = ref_gate >= 0
                    && (ref_gate as usize) < reflectivity.ngates as usize
                    && reflectivity
                        .value(row, ref_gate as usize)
                        .is_some_and(|dbz| dbz >= 10.0);
                if !echo {
                    assert_eq!(value, None, "row {row} gate {gate} without echo");
                    without_echo += 1;
                    continue;
                }
                let Some(value) = value else { continue };
                values += 1;
                // The 3-gate median is one of the three neighbouring values.
                let neighbours: Vec<f32> = (gate.saturating_sub(1)..(gate + 2).min(gates))
                    .filter_map(|g| dealiased.value(row, g))
                    .map(f32::abs)
                    .collect();
                assert!(
                    neighbours.iter().any(|v| (v - value).abs() < 1e-3),
                    "row {row} gate {gate}: {value} not in {neighbours:?}"
                );
            }
        }
        assert!(values > 1_000, "{values} gust gates");
        assert!(without_echo > 1_000, "{without_echo} gates masked for echo");
        assert!(too_high > 1_000, "{too_high} gates above 1 km");
    }
}
