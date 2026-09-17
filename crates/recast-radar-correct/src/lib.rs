//! Radar data correction: Doppler velocity dealiasing.
//!
//! Three unfolding engines share the helpers in this crate root:
//! - the region-based engine ([`dealias_velocity`],
//!   [`dealias_velocity_with_reference`]),
//! - the model-anchored volume engine ([`dealias_volume`],
//!   [`dealias_velocity_v4`]),
//! - a literal port of Py-ART's region dealiaser
//!   ([`dealias_velocity_pyart_region`]).
//!
//! Every engine works on the FM301 model of `recast-radar-core`: the velocity
//! [`Field`] of a [`Sweep`] in, a dealiased-velocity field (`VRADDH`) on the
//! same rays and gates out.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod dealias_pyart;
mod dealias_v4;
mod region_core;

pub use dealias_pyart::dealias_velocity_pyart_region;
pub use dealias_v4::{
    ConfidenceGrid, EnvWindLevel, EnvironmentalWindProfile, TemporalPrior, V4Diagnostics,
    V4VolumeSolution, dealias_velocity_v4, dealias_volume, project_environmental_winds_onto,
};

use chrono::{DateTime, Utc};
use recast_radar_core::model::{Coding, RowRef};
use recast_radar_core::{
    Field, FieldData, FieldName, Gate, GateMapping, IntCoding, LinearTransform, Sweep, Volume,
};

/// Per-range-band zeroth-harmonic wind reference (Browning & Wexler 1968):
/// v̂(az) = a·cos(az) + b·sin(az), fitted per band of gates. Supplied to the
/// fold resolver as EXTERNAL evidence via
/// [`dealias_velocity_with_reference`]; the bench eval battery fits it
/// on each engine's own output for the `rms_harmonic` metric (dealias-v4
/// spec §10.2).
pub struct RangeBandReference {
    pub band_gates: usize,
    pub fits: Vec<Option<(f32, f32)>>,
}

impl RangeBandReference {
    #[inline]
    fn eval(&self, sin_az: f32, cos_az: f32, gate: usize) -> Option<f32> {
        let (a, b) = (*self.fits.get(gate / self.band_gates.max(1))?)?;
        Some(a * cos_az + b * sin_az)
    }
}

/// Gates per range band for the reference fit.
pub(crate) const REFERENCE_BAND_GATES: usize = 16;
const FIT_MIN_SAMPLES: u32 = 48;
const FIT_MIN_SECTORS: u32 = 5; // of 12 × 30° azimuth sectors
/// Outlier trim for the second fit pass (m/s).
const FIT_TRIM_MPS: f32 = 12.0;

/// Fit the per-range-band zeroth harmonic v(az) = a·cos(az) + b·sin(az) on a
/// (dealiased) velocity field of `sweep`. Two passes: fit, then refit
/// excluding outliers.
/// (Browning & Wexler 1968; formerly the tilt-cascade engine's reference fit
/// — the cascade and hybrid engines were removed at v0.29.0, superseded by
/// the model-anchored `dealias_v4` engine.)
pub fn range_band_reference(sweep: &Sweep, field: &Field) -> RangeBandReference {
    let (rows, gates) = field.shape();
    let bands = gates.div_ceil(REFERENCE_BAND_GATES).max(1);
    let azimuth = |row: usize| -> Option<f32> { sweep.rays.azimuth_deg.get(row).copied() };

    let mut fits: Vec<Option<(f32, f32)>> = vec![None; bands];
    for pass in 0..2 {
        let mut acc = vec![[0.0f64; 6]; bands]; // cc, cs, ss, cv, sv, n
        let mut sectors = vec![0u16; bands];
        for row in 0..rows {
            let Some(az_deg) = azimuth(row) else {
                continue;
            };
            let az = (az_deg as f64).to_radians();
            let (sin_az, cos_az) = (az.sin(), az.cos());
            let sector_bit = 1u16 << ((az_deg.rem_euclid(360.0) / 30.0) as u32 % 12);
            for gate in 0..gates {
                let Some(v) = field.value(row, gate).filter(|v| v.is_finite()) else {
                    continue;
                };
                let band = gate / REFERENCE_BAND_GATES;
                if pass == 1
                    && let Some((a, b)) = fits[band]
                {
                    let predicted = a * cos_az as f32 + b * sin_az as f32;
                    if (v - predicted).abs() > FIT_TRIM_MPS {
                        continue;
                    }
                }
                let entry = &mut acc[band];
                entry[0] += cos_az * cos_az;
                entry[1] += cos_az * sin_az;
                entry[2] += sin_az * sin_az;
                entry[3] += cos_az * v as f64;
                entry[4] += sin_az * v as f64;
                entry[5] += 1.0;
                sectors[band] |= sector_bit;
            }
        }
        for band in 0..bands {
            let entry = &acc[band];
            if (entry[5] as u32) < FIT_MIN_SAMPLES || sectors[band].count_ones() < FIT_MIN_SECTORS {
                fits[band] = None;
                continue;
            }
            let det = entry[0] * entry[2] - entry[1] * entry[1];
            if det.abs() < 1e-6 {
                fits[band] = None;
                continue;
            }
            let a = (entry[3] * entry[2] - entry[4] * entry[1]) / det;
            let b = (entry[4] * entry[0] - entry[3] * entry[1]) / det;
            fits[band] = Some((a as f32, b as f32));
        }
    }
    RangeBandReference {
        band_gates: REFERENCE_BAND_GATES,
        fits,
    }
}

/// Dealias (unfold) the velocity field `source` of `sweep`; returns a
/// `VRADDH` field on the same rays and native gates.
///
/// This is a **region-based** unfolder, not a gate-by-gate radial walk. A
/// radial/gate-sequential continuity scheme (the previous implementation)
/// propagates a single bad fold down an entire ray, producing the radial
/// "spokes" that plague high-shear convection (derechos, mesocyclones). The
/// region-based approach decides whole coherent regions at once and lets
/// genuine discontinuities remain at region boundaries, so an error cannot
/// run down a radial. See Feldmann et al. (2020, *R2D2*, JTECH-D-20-0054.1),
/// Jing & Wiener (1993), and Py-ART's `dealias_region_based`
/// (Helmus & Collis 2016).
///
/// Steps: (1) flood-fill connected regions whose neighbouring gates differ by
/// less than half a Nyquist (so no fold occurs *within* a region); (2) build a
/// region-adjacency graph whose edges carry the integer Nyquist fold between
/// the two regions (the consensus over all shared boundary gate-pairs);
/// (3) resolve folds strongest-boundary-first via a union-find with per-node
/// fold offset; (4) anchor each connected group so its largest region is
/// unfolded (fold 0); (5) apply and despeckle.
pub fn dealias_velocity(sweep: &Sweep, source: &Field) -> Field {
    dealias_velocity_with_reference(sweep, source, None)
}

/// True when [`dealias_velocity`] over this sweep can only pass raw
/// velocity through: no provided row of the velocity field carries a usable
/// (finite, positive) Nyquist velocity, so every per-row Nyquist and the
/// median fallback are unknown and no fold correction can ever apply
/// (`v` is emitted unchanged). JMA is always in this state by design
/// (staggered PRF, Nyquist left `None` — see `recast-radar-io-jma/src/lib.rs`);
/// the occasional ODIM file omits `how/NI` too. The UI queries this so a
/// product labeled "dealiased" can disclose the pass-through instead of
/// silently rendering raw velocity under a dealiased label (data trust:
/// the fold warning can never fire without a Nyquist).
pub fn dealias_skipped_no_nyquist(sweep: &Sweep, source: &Field) -> bool {
    median_nyquist_mps(sweep, source).is_none()
}

/// [`dealias_velocity`] with an optional external wind reference: the
/// resolver uses it for connected-group BRANCH selection and per-region
/// verification (UNRAVEL-style checks, Louf et al. 2020). With `None` the
/// behavior is identical to the plain region engine.
pub fn dealias_velocity_with_reference(
    sweep: &Sweep,
    source: &Field,
    reference: Option<&RangeBandReference>,
) -> Field {
    let (rows, gate_count) = source.shape();
    let total = rows.saturating_mul(gate_count);
    let fallback_nyquist = median_nyquist_mps(sweep, source);

    // Per-row Nyquist (NaN where unknown) and observed velocities (NaN for
    // no-data / range-folded gates, which never join a region).
    let mut nyq = vec![f32::NAN; rows.max(1)];
    for (row, slot) in nyq.iter_mut().enumerate().take(rows) {
        *slot = row_nyquist_mps(sweep, row)
            .or(fallback_nyquist)
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(f32::NAN);
    }

    let mut observed = vec![f32::NAN; total];
    if total > 0 {
        let mut row_buf = vec![f32::NAN; gate_count];
        for row in 0..rows {
            copy_scaled_velocity_row(source, row, &mut row_buf);
            observed[row * gate_count..(row + 1) * gate_count].copy_from_slice(&row_buf);
        }
    }

    let azimuths = radial_azimuths(sweep, source);
    let folds = region_based_dealias_folds(&observed, &nyq, rows, gate_count, &azimuths, reference);

    let mut corrected = vec![DEALIASED_VELOCITY_NODATA; total];
    #[allow(clippy::needless_range_loop)]
    for row in 0..rows {
        let n = nyq[row];
        for gate in 0..gate_count {
            let idx = row * gate_count + gate;
            let v = observed[idx];
            if !v.is_finite() {
                continue;
            }
            let value = if n.is_finite() {
                v + 2.0 * n * folds[idx] as f32
            } else {
                v
            };
            corrected[idx] = encode_dealiased_velocity(value);
        }
    }

    despeckle_dealiased_velocity(&mut corrected, &nyq, rows, gate_count);

    dealiased_velocity_field(source, corrected)
}

/// Per-row beam azimuth of `field` (a field of `sweep`) in degrees,
/// normalized to `[0, 360)`; NaN for a row past the sweep's rays.
pub fn radial_azimuths(sweep: &Sweep, field: &Field) -> Vec<f32> {
    (0..field.nrays as usize)
        .map(|row| {
            sweep
                .rays
                .azimuth_deg
                .get(row)
                .map(|azimuth| azimuth.rem_euclid(360.0))
                .unwrap_or(f32::NAN)
        })
        .collect()
}

/// Whether row order closes a full 360° sweep (so the last radial is azimuthally
/// adjacent to the first). True for any normal NEXRAD PPI.
fn sweep_wraps(azimuths: &[f32]) -> bool {
    let rows = azimuths.len();
    if rows < 8 {
        return false;
    }
    let (Some(first), Some(last)) = (azimuths.first(), azimuths.last()) else {
        return false;
    };
    if !first.is_finite() || !last.is_finite() {
        return false;
    }
    let gap = (first - last)
        .rem_euclid(360.0)
        .min((last - first).rem_euclid(360.0));
    let typical = 360.0 / rows as f32;
    gap <= 3.0 * typical
}

/// Core region-based fold solver. Returns the integer Nyquist fold for every
/// gate (0 where unknown / no data). Steps 1–4 (segmentation, vote graph,
/// strongest-first resolution, anchoring) live in [`region_core`]; this
/// wrapper applies the optional external-reference pass (5b).
fn region_based_dealias_folds(
    observed: &[f32],
    nyq: &[f32],
    rows: usize,
    gates: usize,
    azimuths: &[f32],
    reference: Option<&RangeBandReference>,
) -> Vec<i32> {
    let total = rows.saturating_mul(gates);
    let mut folds = vec![0i32; total];
    if total == 0 || observed.len() != total {
        return folds;
    }

    let region_core::RegionSolve {
        region_of,
        region_size,
        mut region_fold,
        region_offset,
        region_group,
        group_count,
        ..
    } = region_core::solve_region_folds(observed, nyq, rows, gates, azimuths);
    let region_count = region_size.len();
    if region_count == 0 {
        return folds;
    }

    // ---- 5b. external-reference checks (optional caller reference) ----
    // Boundary votes lock RELATIVE folds, but each connected group's absolute
    // branch — and any vote-graph misbranch — needs independent evidence.
    // A clean external harmonic reference supplies it: choose each group's
    // branch against the reference, then re-test each region individually and
    // override only when decisive.
    if let Some(reference) = reference {
        let mut row_trig = vec![(0.0f32, 0.0f32); rows];
        for row in 0..rows {
            let az = azimuths[row].to_radians();
            row_trig[row] = (az.sin(), az.cos());
        }
        // Group branch: cost per (group, g) for g ∈ −2..=+2.
        let mut group_cost = vec![([0.0f64; 5], 0u64, 0u64); group_count];
        for (row, n) in nyq.iter().copied().enumerate().take(rows) {
            if !n.is_finite() || n <= 0.0 {
                continue;
            }
            let (sin_az, cos_az) = row_trig[row];
            for gate in 0..gates {
                let idx = row * gates + gate;
                let rid = region_of[idx];
                if rid == u32::MAX {
                    continue;
                }
                let off = region_offset[rid as usize];
                let entry = &mut group_cost[region_group[rid as usize] as usize];
                entry.2 += 1;
                let Some(predicted) = reference.eval(sin_az, cos_az, gate) else {
                    continue;
                };
                entry.1 += 1;
                let v = observed[idx];
                for (slot, g) in (-2i32..=2).enumerate() {
                    let unfolded = v + (off + g) as f32 * 2.0 * n;
                    entry.0[slot] += (unfolded - predicted).abs() as f64;
                }
            }
        }
        for (group, (costs, covered, total_gates)) in group_cost.iter().enumerate() {
            if *total_gates == 0 || (*covered as f64) < 0.5 * *total_gates as f64 {
                continue;
            }
            let best_slot = first_minimum_index(costs);
            let branch = best_slot as i32 - 2;
            for rid in 0..region_count {
                if region_group[rid] as usize == group {
                    region_fold[rid] = (region_offset[rid] + branch)
                        .clamp(-region_core::REGION_MAX_FOLD, region_core::REGION_MAX_FOLD);
                }
            }
        }
        // Per-region override: repairs vote-graph misbranches that survive
        // group selection (a subgraph can be internally consistent yet wrong).
        let mut cost = vec![[0.0f64; 3]; region_count];
        let mut covered = vec![0u32; region_count];
        for (row, n) in nyq.iter().copied().enumerate().take(rows) {
            if !n.is_finite() || n <= 0.0 {
                continue;
            }
            let (sin_az, cos_az) = row_trig[row];
            for gate in 0..gates {
                let idx = row * gates + gate;
                let rid = region_of[idx];
                if rid == u32::MAX {
                    continue;
                }
                let Some(predicted) = reference.eval(sin_az, cos_az, gate) else {
                    continue;
                };
                let v = observed[idx];
                let fold = region_fold[rid as usize];
                covered[rid as usize] += 1;
                for (slot, dg) in (-1i32..=1).enumerate() {
                    let unfolded = v + (fold + dg) as f32 * 2.0 * n;
                    cost[rid as usize][slot] += (unfolded - predicted).abs() as f64;
                }
            }
        }
        for rid in 0..region_count {
            if (covered[rid] as f64) < 0.6 * region_size[rid] as f64 {
                continue;
            }
            let current = cost[rid][1];
            let best_slot = first_minimum_index(&cost[rid]);
            let best_cost = &cost[rid][best_slot];
            if best_slot != 1 && *best_cost < 0.6 * current {
                let dg = best_slot as i32 - 1;
                region_fold[rid] = (region_fold[rid] + dg)
                    .clamp(-region_core::REGION_MAX_FOLD, region_core::REGION_MAX_FOLD);
            }
        }
    }

    for idx in 0..total {
        let rid = region_of[idx];
        if rid != u32::MAX {
            folds[idx] = region_fold[rid as usize];
        }
    }
    folds
}

/// Remove isolated single-gate outliers from the unfolded field: a gate whose
/// decoded velocity differs from the median of its finite 4-neighbours by more
/// than a Nyquist is snapped toward that median (dual-PRF/processor speckle;
/// Holleman & Beekhuis 2003, Altube et al. 2017).
fn despeckle_dealiased_velocity(corrected: &mut [u16], nyq: &[f32], rows: usize, gates: usize) {
    if rows < 3 || gates < 3 || corrected.len() != rows.saturating_mul(gates) {
        return;
    }
    let snapshot = corrected.to_vec();
    let decode = |raw: u16| decode_dealiased_velocity(raw);
    #[allow(clippy::needless_range_loop)]
    for row in 0..rows {
        let n = nyq[row];
        if !n.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let idx = row * gates + gate;
            let Some(v) = decode(snapshot[idx]) else {
                continue;
            };
            let mut neigh = [0.0f32; 4];
            let mut count = 0;
            for (nr, ng) in [
                (row.wrapping_sub(1), gate),
                (row + 1, gate),
                (row, gate.wrapping_sub(1)),
                (row, gate + 1),
            ] {
                if nr >= rows || ng >= gates {
                    continue;
                }
                if let Some(nv) = decode(snapshot[nr * gates + ng]) {
                    neigh[count] = nv;
                    count += 1;
                }
            }
            if count < 3 {
                continue;
            }
            let median = median_small_f32(&mut neigh, count);
            if (v - median).abs() > n {
                // collapse the outlier onto the nearest Nyquist multiple of the
                // local consensus.
                let fold = ((median - v) / (2.0 * n)).round();
                corrected[idx] = encode_dealiased_velocity(v + 2.0 * n * fold);
            }
        }
    }
}

const DEALIASED_VELOCITY_SCALE: f32 = 10.0;
const DEALIASED_VELOCITY_OFFSET: f32 = 32_768.0;
const DEALIASED_VELOCITY_NODATA: u16 = 0;

fn encode_dealiased_velocity(value: f32) -> u16 {
    if !value.is_finite() {
        return DEALIASED_VELOCITY_NODATA;
    }
    (value * DEALIASED_VELOCITY_SCALE + DEALIASED_VELOCITY_OFFSET)
        .round()
        .clamp(1.0, u16::MAX as f32) as u16
}

fn decode_dealiased_velocity(raw: u16) -> Option<f32> {
    if raw == DEALIASED_VELOCITY_NODATA {
        return None;
    }
    Some((raw as f32 - DEALIASED_VELOCITY_OFFSET) / DEALIASED_VELOCITY_SCALE)
}

/// The shared dealiased-velocity field format: `VRADDH`, `u16` with
/// `physical = (raw - 32768) / 10` m/s and raw 0 as `_FillValue`, on the
/// source's rays and native gates. Rows the source did not provide stay
/// absent.
pub(crate) fn dealiased_velocity_field(source: &Field, corrected: Vec<u16>) -> Field {
    dealiased_velocity_field_parts(
        source.gates,
        source.nrays as usize,
        source.ngates as usize,
        source.absent_rows.clone(),
        corrected,
    )
}

/// [`dealiased_velocity_field`] from the source's mapping, shape and absent
/// rows.
pub(crate) fn dealiased_velocity_field_parts(
    gates: GateMapping,
    nrays: usize,
    ngates: usize,
    absent_rows: Vec<u32>,
    corrected: Vec<u16>,
) -> Field {
    let mut field = Field::new(
        FieldName::Vraddh,
        gates,
        u32::try_from(ngates).unwrap_or(u32::MAX),
        FieldData::U16 {
            values: corrected,
            coding: IntCoding {
                fill_value: Some(DEALIASED_VELOCITY_NODATA),
                ..IntCoding::new(LinearTransform::IcdScaleOffset {
                    scale: DEALIASED_VELOCITY_SCALE,
                    offset: DEALIASED_VELOCITY_OFFSET,
                })
            },
        },
    );
    field.nrays = u32::try_from(nrays).unwrap_or(u32::MAX);
    field.absent_rows = absent_rows;
    field
}

/// Rows `field` provides: its rows minus the absent ones.
pub(crate) fn provided_rows(field: &Field) -> usize {
    (field.nrays as usize).saturating_sub(field.absent_rows.len())
}

/// Write row `row` of `source` into `row_values` as physical values (m/s).
///
/// Every sentinel (no data, below threshold, range folded, outside
/// `valid_range`) becomes NaN, as does a row the source did not provide.
/// `row_values` must be exactly `ngates` long; otherwise (or when the row is
/// out of range) it is left all NaN.
pub fn copy_scaled_velocity_row(source: &Field, row: usize, row_values: &mut [f32]) {
    row_values.fill(f32::NAN);
    let gate_count = source.ngates as usize;
    if gate_count == 0 || row_values.len() != gate_count || source.is_absent(row) {
        return;
    }
    let Some(raw_row) = source.row(row) else {
        return;
    };
    fn fill<T: Copy>(raw: &[T], out: &mut [f32], resolve: impl Fn(T) -> Gate) {
        for (raw, value) in raw.iter().zip(out.iter_mut()) {
            if let Gate::Value(physical) = resolve(*raw) {
                *value = physical;
            }
        }
    }
    match (raw_row, source.data.coding()) {
        (RowRef::U8(raw), Coding::U8(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        (RowRef::U16(raw), Coding::U16(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        (RowRef::I8(raw), Coding::I8(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        (RowRef::I16(raw), Coding::I16(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        (RowRef::F32(raw), Coding::F32(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        (RowRef::F64(raw), Coding::F64(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
        _ => {}
    }
}

/// Median usable (finite, positive) Nyquist velocity over the rows `field`
/// provides.
fn median_nyquist_mps(sweep: &Sweep, field: &Field) -> Option<f32> {
    let mut values = (0..field.nrays as usize)
        .filter(|&row| !field.is_absent(row))
        .filter_map(|row| row_nyquist_mps(sweep, row))
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}

/// Nyquist velocity of ray `row`; `None` when the sweep has no Nyquist
/// variable or the ray's value is missing (NaN).
fn row_nyquist_mps(sweep: &Sweep, row: usize) -> Option<f32> {
    sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()?
        .get(row)
        .copied()
        .filter(|value| !value.is_nan())
}

/// The volume's nominal time: the first ray's time (`time_coverage.start`),
/// else the whole-second time reference.
pub(crate) fn volume_start_time(volume: &Volume) -> DateTime<Utc> {
    volume
        .time_coverage
        .map_or(volume.time_reference, |coverage| coverage.start)
}

fn median_small_f32(values: &mut [f32], count: usize) -> f32 {
    debug_assert!(count > 0 && count <= values.len());
    values[..count].sort_by(f32::total_cmp);
    values[count / 2]
}

/// Index of the first minimum under [`f64::total_cmp`] (0 for an empty
/// slice) — the element `Iterator::min_by` returns, which is the first of
/// equal minima.
fn first_minimum_index(values: &[f64]) -> usize {
    let mut best = 0;
    for (index, value) in values.iter().enumerate().skip(1) {
        if value.total_cmp(&values[best]).is_lt() {
            best = index;
        }
    }
    best
}

#[cfg(test)]
pub(crate) mod test_support {
    use recast_radar_core::{Field, FieldData, FieldName, FloatCoding, Sweep, SweepMode};

    /// A sealed PPI sweep at `elevation_deg` with one physical velocity field
    /// (`VRADH`) of `azimuths.len()` rows on uniform gates, and per-ray Nyquist
    /// velocities when given.
    pub(crate) fn velocity_sweep(
        azimuths: &[f32],
        elevation_deg: f32,
        first_center_m: f64,
        spacing_m: f64,
        gates: usize,
        values: Vec<f32>,
        nyquist_mps: Option<Vec<f32>>,
    ) -> Sweep {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, elevation_deg);
        for (ray, azimuth) in azimuths.iter().enumerate() {
            sweep.push_ray(ray as f64, *azimuth, elevation_deg);
        }
        sweep.ray_vars.nyquist_velocity_mps = nyquist_mps;
        let mapping = sweep
            .attach_geometry(first_center_m, spacing_m, gates as u32)
            .unwrap();
        let mut field = Field::new(
            FieldName::Vradh,
            mapping,
            gates as u32,
            FieldData::F32 {
                values,
                coding: FloatCoding::default(),
            },
        );
        field.nrays = azimuths.len() as u32;
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();
        sweep
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::velocity_sweep;

    fn velocity(sweep: &Sweep) -> &Field {
        &sweep.fields[0]
    }

    #[test]
    fn lightweight_velocity_dealias_unfolds_radial_continuity() {
        let sweep = velocity_sweep(
            &[0.0],
            0.5,
            0.0,
            1000.0,
            5,
            vec![0.0, 5.0, 9.0, -9.0, -7.0],
            Some(vec![10.0]),
        );

        let corrected = dealias_velocity(&sweep, velocity(&sweep));
        assert!(matches!(corrected.data, FieldData::U16 { .. }));
        assert_eq!(corrected.name, FieldName::Vraddh);
        assert_eq!(corrected.gates, velocity(&sweep).gates);

        let values = (0..corrected.ngates as usize)
            .map(|gate| corrected.value(0, gate).expect("corrected gate"))
            .collect::<Vec<_>>();
        assert_eq!(values, vec![0.0, 5.0, 9.0, 11.0, 13.0]);
    }

    #[test]
    fn dealias_skip_detection_reports_nyquist_less_feeds() {
        // JMA-style feed: staggered PRF leaves Nyquist unset on every
        // ray, so the "dealiased" field is a pure pass-through and the UI
        // must be able to disclose that.
        let observed = vec![2.0f32, 4.0, 6.0, 8.0];
        let rows_data: Vec<Vec<f32>> = (0..4).map(|_| observed.clone()).collect();
        let mut sweep = test_velocity_sweep_rows(rows_data);
        sweep.ray_vars.nyquist_velocity_mps = None;

        assert!(
            dealias_skipped_no_nyquist(&sweep, velocity(&sweep)),
            "no usable Nyquist on any ray must report the skip"
        );
        // The skip really is a pass-through: values come back as recorded.
        let corrected = dealias_velocity(&sweep, velocity(&sweep));
        for (gate, value) in observed.iter().enumerate() {
            assert_eq!(
                corrected.value(1, gate),
                Some(*value),
                "gate {gate} must pass through unchanged"
            );
        }

        // Any ray with a usable Nyquist flips the answer (the median
        // fallback then covers Nyquist-less rows).
        let with_first = |first: f32| {
            let mut nyquist = vec![f32::NAN; 4];
            nyquist[0] = first;
            Some(nyquist)
        };
        sweep.ray_vars.nyquist_velocity_mps = with_first(20.0);
        assert!(!dealias_skipped_no_nyquist(&sweep, velocity(&sweep)));

        // Non-finite / non-positive declarations are not usable Nyquists.
        sweep.ray_vars.nyquist_velocity_mps = with_first(0.0);
        assert!(dealias_skipped_no_nyquist(&sweep, velocity(&sweep)));
        sweep.ray_vars.nyquist_velocity_mps = with_first(f32::NAN);
        assert!(dealias_skipped_no_nyquist(&sweep, velocity(&sweep)));
    }

    #[test]
    fn region_dealias_recovers_smooth_folded_ramp() {
        // A smooth radial velocity ramp from -34 to +34 m/s with Nyquist 20 is
        // aliased into [-20, 20]; the region-based unfolder must recover the
        // smooth field (up to a global 2·Nyquist constant from anchoring).
        let nyq = 20.0f32;
        let gates = 24usize;
        let rows = 12usize;
        let truth: Vec<f32> = (0..gates)
            .map(|g| -34.0 + 68.0 * g as f32 / (gates as f32 - 1.0))
            .collect();
        let alias = |v: f32| -> f32 {
            let mut a = v;
            while a > nyq {
                a -= 2.0 * nyq;
            }
            while a < -nyq {
                a += 2.0 * nyq;
            }
            a
        };
        let observed_row: Vec<f32> = truth.iter().map(|v| alias(*v)).collect();
        // The raw row genuinely folds (large gate-to-gate jumps present).
        let raw_jumps = observed_row
            .windows(2)
            .filter(|w| (w[0] - w[1]).abs() > nyq)
            .count();
        assert!(raw_jumps >= 1, "test fixture must actually alias");

        let rows_data: Vec<Vec<f32>> = (0..rows).map(|_| observed_row.clone()).collect();
        let mut sweep = test_velocity_sweep_rows(rows_data);
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyq; rows]);

        let corrected = dealias_velocity(&sweep, velocity(&sweep));
        let recovered: Vec<f32> = (0..gates)
            .map(|g| corrected.value(rows / 2, g).expect("gate"))
            .collect();

        // 1) the unfolded field is smooth: no gate-to-gate jump exceeds Nyquist.
        for w in recovered.windows(2) {
            assert!(
                (w[0] - w[1]).abs() <= nyq,
                "residual fold in dealiased ramp: {w:?}"
            );
        }
        // 2) it matches the truth up to a single constant multiple of 2·Nyquist.
        let offset = recovered[0] - truth[0];
        let folds = (offset / (2.0 * nyq)).round();
        assert!(
            (offset - folds * 2.0 * nyq).abs() < 1.0,
            "offset not a fold multiple: {offset}"
        );
        for (r, t) in recovered.iter().zip(truth.iter()) {
            assert!(
                (r - (t + folds * 2.0 * nyq)).abs() < 1.0,
                "recovered {r} != truth {t} (+{folds} folds)"
            );
        }
    }

    #[test]
    fn region_dealias_does_not_propagate_errors_down_a_radial() {
        // The classic spoke failure: one ambiguous gate near the radar must not
        // flip the entire downrange radial. Two radials of identical, coherent,
        // sub-Nyquist data should come back essentially unchanged (no fold).
        let nyq = 20.0f32;
        let coherent = vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0];
        let rows_data: Vec<Vec<f32>> = (0..16).map(|_| coherent.clone()).collect();
        let mut sweep = test_velocity_sweep_rows(rows_data);
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyq; 16]);

        let corrected = dealias_velocity(&sweep, velocity(&sweep));
        for (g, value) in coherent.iter().enumerate() {
            assert_eq!(
                corrected.value(5, g),
                Some(*value),
                "coherent gate {g} should be untouched"
            );
        }
    }

    #[test]
    fn region_dealias_is_deterministic_across_runs() {
        // Same input must always produce the same unfolded field: edge
        // resolution order and tied fold votes must not depend on HashMap
        // iteration order (which differs per HashMap instance).
        let nyq = 20.0f32;
        let rows = 24usize;
        let gates = 40usize;
        let mut seed = 0x2468_ace1_u32;
        let mut lcg = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as f32 / 65_536.0
        };
        let rows_data: Vec<Vec<f32>> = (0..rows)
            .map(|row| {
                (0..gates)
                    .map(|gate| {
                        let patch = (row / 3) * 7 + gate / 5;
                        let base = match patch % 4 {
                            0 => -38.0,
                            1 => -2.0,
                            2 => 18.5,
                            _ => 39.0,
                        };
                        let v = base + (lcg() - 0.5) * 6.0;
                        let mut aliased = v;
                        while aliased > nyq {
                            aliased -= 2.0 * nyq;
                        }
                        while aliased < -nyq {
                            aliased += 2.0 * nyq;
                        }
                        aliased
                    })
                    .collect()
            })
            .collect();
        let mut sweep = test_velocity_sweep_rows(rows_data);
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyq; rows]);

        let reference = dealias_velocity(&sweep, velocity(&sweep));
        for run in 0..16 {
            let corrected = dealias_velocity(&sweep, velocity(&sweep));
            assert_eq!(
                corrected.data, reference.data,
                "dealias output changed between identical runs (run {run})"
            );
        }
    }

    #[test]
    fn region_dealias_unfolds_geometrically_supported_fold() {
        // A folded 2-gate segment surrounded on three sides by data that
        // consistently implies one fold is unfolded (the old radial-walk code
        // wrongly "suppressed" this as a spike and left the alias in place).
        let quiet = vec![0.0, 3.0, 5.0, 7.0, 8.0];
        let folded = vec![0.0, 5.0, 9.0, -9.0, -7.0];
        let sweep = test_velocity_sweep_rows(vec![
            quiet.clone(),
            quiet.clone(),
            folded,
            quiet.clone(),
            quiet,
        ]);

        let corrected = dealias_velocity(&sweep, velocity(&sweep));

        assert_eq!(corrected.value(2, 3), Some(11.0));
        assert_eq!(corrected.value(2, 4), Some(13.0));
    }

    /// Build a full 360° velocity tilt whose TRUE field is a uniform wind
    /// (radial component = speed·cos(az − dir)), wrapped into ±nyquist.
    /// (Ported from the retired cascade engine's test fixture.)
    fn tilt_with_uniform_wind(
        elevation: f32,
        speed: f32,
        toward_deg: f32,
        nyquist: f32,
        rows: usize,
        gates: usize,
    ) -> Sweep {
        let mut data = vec![f32::NAN; rows * gates];
        let mut azimuths = Vec::with_capacity(rows);
        for row in 0..rows {
            let az = row as f32 * (360.0 / rows as f32);
            azimuths.push(az);
            let true_v = speed * ((az - toward_deg).to_radians()).cos();
            let mut wrapped = true_v;
            while wrapped > nyquist {
                wrapped -= 2.0 * nyquist;
            }
            while wrapped < -nyquist {
                wrapped += 2.0 * nyquist;
            }
            for gate in 0..gates {
                data[row * gates + gate] = wrapped;
            }
        }
        velocity_sweep(
            &azimuths,
            elevation,
            1000.0,
            250.0,
            gates,
            data,
            Some(vec![nyquist; rows]),
        )
    }

    /// Pins the two v0.29.0 survivors of the retired cascade/hybrid engines:
    /// [`range_band_reference`] (also the bench battery's `rms_harmonic`
    /// metric input) and the region engine's external-reference branch
    /// selection ([`dealias_velocity_with_reference`]).
    #[test]
    fn external_harmonic_reference_selects_the_absolute_branch() {
        // 35 m/s wind. Clean tilt: Nyquist 40 — no aliasing, honest fit.
        // Target tilt: Nyquist 20 — large sectors wrap (|v| up to 35), the
        // regime where a same-sweep reference is circular.
        let clean = tilt_with_uniform_wind(2.4, 35.0, 180.0, 40.0, 360, 200);
        let reference = range_band_reference(&clean, velocity(&clean));
        assert!(
            reference.fits.iter().filter(|fit| fit.is_some()).count() > 0,
            "clean uniform wind must produce usable band fits"
        );

        let target = tilt_with_uniform_wind(0.5, 35.0, 180.0, 20.0, 360, 200);
        let dealiased =
            dealias_velocity_with_reference(&target, velocity(&target), Some(&reference));
        let mut worst = 0.0f32;
        for row in 0..360 {
            let az = row as f32;
            let truth = 35.0 * ((az - 180.0).to_radians()).cos();
            for gate in (0..200).step_by(7) {
                if let Some(v) = dealiased.value(row, gate).filter(|v| v.is_finite()) {
                    worst = worst.max((v - truth).abs());
                }
            }
        }
        assert!(
            worst < 2.0,
            "external reference should recover the true field everywhere; worst error {worst} m/s"
        );
    }

    #[test]
    fn velocity_dealias_preserves_supported_adjacent_folds() {
        let quiet = vec![0.0, 3.0, 5.0, 7.0, 8.0];
        let folded = vec![0.0, 5.0, 9.0, -9.0, -7.0];
        let sweep = test_velocity_sweep_rows(vec![
            quiet.clone(),
            folded.clone(),
            folded.clone(),
            folded,
            quiet,
        ]);

        let corrected = dealias_velocity(&sweep, velocity(&sweep));

        assert_eq!(corrected.value(2, 3), Some(11.0));
        assert_eq!(corrected.value(2, 4), Some(13.0));
    }

    #[test]
    fn copy_row_resolves_every_sentinel_and_absent_rows() {
        let mut sweep = velocity_sweep(
            &[0.0, 1.0],
            0.5,
            2125.0,
            250.0,
            3,
            vec![1.0, f32::NAN, 3.0, 4.0, 5.0, 6.0],
            Some(vec![26.0, 26.0]),
        );
        let mut row = vec![0.0f32; 3];
        copy_scaled_velocity_row(velocity(&sweep), 0, &mut row);
        assert_eq!(row[0], 1.0);
        assert!(row[1].is_nan());
        assert_eq!(row[2], 3.0);
        sweep.fields[0].absent_rows = vec![1];
        copy_scaled_velocity_row(velocity(&sweep), 1, &mut row);
        assert!(row.iter().all(|value| value.is_nan()));
    }

    /// Rows of physical velocity, azimuth = row index, 1 km gates from 0 m,
    /// Nyquist 10 m/s on every ray.
    fn test_velocity_sweep_rows(rows: Vec<Vec<f32>>) -> Sweep {
        let gates = rows.first().map(Vec::len).unwrap_or(0);
        let count = rows.len();
        let azimuths: Vec<f32> = (0..count).map(|index| index as f32).collect();
        velocity_sweep(
            &azimuths,
            0.5,
            0.0,
            1000.0,
            gates,
            rows.into_iter().flatten().collect(),
            Some(vec![10.0; count]),
        )
    }
}
