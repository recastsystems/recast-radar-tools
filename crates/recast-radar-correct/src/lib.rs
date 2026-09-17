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
#[cfg(test)]
mod real_data;
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
        (RowRef::I32(raw), Coding::I32(coding)) => fill(raw, row_values, |r| coding.resolve(r)),
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
mod tests {
    //! Region engine tests on real Level II sweeps. Expected folds come from
    //! Py-ART 2.2.5 `dealias_region_based` on the same files
    //! (`tools/correct_golden.py`, goldens in `tests/golden/`); a golden's
    //! rays are checked against the decoded rows gate for gate before use.

    use super::*;
    use crate::real_data::{
        self, VelocitySweep, enclosed_patches, fold_agreement, golden_volume, grid_folds,
        radial_jumps_removed,
    };

    /// Along each ray the raw velocity jumps by more than the Nyquist velocity
    /// wherever the wind folds, and Py-ART's unfolding makes those gate pairs
    /// continuous: 3,735 in the KDVN derecho sector (Nyquist 21.0 m/s), 6,713
    /// in the KBOX blizzard sector (28.4 m/s). The region engine must restore
    /// radial continuity at nearly all of them (measured: 3,402 and 6,672).
    #[test]
    fn lightweight_velocity_dealias_unfolds_radial_continuity() {
        for (case, min_share) in [
            ("kdvn_20200810_trim_s1", 0.89),
            ("kbox_20220129_trim_s1", 0.99),
        ] {
            let Some((volume, golden)) = golden_volume(case) else {
                return;
            };
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let corrected = dealias_velocity(cut, real_data::velocity(cut));
            assert!(matches!(corrected.data, FieldData::U16 { .. }));
            assert_eq!(corrected.name, FieldName::Vraddh);
            let engine = grid_folds(&sweep, &corrected);
            let (by_pyart, by_engine) = radial_jumps_removed(&sweep, &pyart, &engine);
            eprintln!("{case}: raw jumps Py-ART removes {by_pyart}, engine also {by_engine}");
            assert!(
                by_pyart > 300,
                "{case}: the sweep must actually fold along rays"
            );
            assert!(
                by_engine as f64 >= min_share * by_pyart as f64,
                "{case}: engine restored continuity at {by_engine} of {by_pyart} folded gate pairs"
            );
        }
    }

    /// Feeds without a usable Nyquist velocity pass through unchanged and
    /// report the skip: the TDWR Doppler cut (every radial carries Nyquist 0;
    /// Py-ART reads the same) and all 13 sweeps of a JMA radial-velocity
    /// station member (staggered PRT; the JMA decoder leaves Nyquist unset).
    #[test]
    fn dealias_skip_detection_reports_nyquist_less_feeds() {
        let Some((volume, golden)) = golden_volume("tstl_20230331_trim_s1") else {
            return;
        };
        assert_eq!(
            golden.nyquist_mps, 0.0,
            "Py-ART reads Nyquist 0 from the TDWR file"
        );
        let mut cut = volume.sweeps[golden.sweep].clone();
        let sweep = VelocitySweep::of_sweep(&cut);
        golden.aligned_folds(&cut, &sweep);
        assert!(dealias_skipped_no_nyquist(&cut, real_data::velocity(&cut)));
        let corrected = dealias_velocity(&cut, real_data::velocity(&cut));
        assert_pass_through(&sweep, &corrected, golden.valid_gates);

        // JMA: 547,108 of 2,201,600 velocity gates are non-missing (manifest
        // description of the member, from the JMA GRIB2 run-length walker).
        let bytes = match recast_radar_testdata::bytes("jma-n6-20191012-090000-rs47773") {
            Ok(bytes) => bytes,
            Err(error) if error.is_offline() => return,
            Err(error) => panic!("{error}"),
        };
        let jma = recast_radar_io_jma::read_jma_tar_first_station(&bytes).expect("decode JMA");
        assert_eq!(jma.sweeps.len(), 13);
        let mut valid = 0;
        for jma_cut in &jma.sweeps {
            let grid = real_data::velocity(jma_cut);
            assert!(dealias_skipped_no_nyquist(jma_cut, grid));
            let jma_sweep = VelocitySweep::of_sweep(jma_cut);
            let finite = jma_sweep.finite_gates();
            assert_pass_through(&jma_sweep, &dealias_velocity(jma_cut, grid), finite);
            valid += finite;
        }
        assert_eq!(valid, 547_108);

        // Positive control: the KDVN Doppler cut carries Nyquist 21.03 m/s.
        let Some((control, control_golden)) = golden_volume("kdvn_20200810_trim_s1") else {
            return;
        };
        let control_cut = &control.sweeps[control_golden.sweep];
        assert!(!dealias_skipped_no_nyquist(
            control_cut,
            real_data::velocity(control_cut)
        ));

        // Edits of the decoded TDWR cut: one radial with the control's
        // Nyquist makes the whole cut dealiasable (median fallback); zero or
        // NaN declarations are not usable.
        real_data::set_nyquist(&mut cut, 0, control_golden.nyquist_mps);
        assert!(!dealias_skipped_no_nyquist(&cut, real_data::velocity(&cut)));
        real_data::set_nyquist(&mut cut, 0, 0.0);
        assert!(dealias_skipped_no_nyquist(&cut, real_data::velocity(&cut)));
        real_data::set_nyquist(&mut cut, 0, f32::NAN);
        assert!(dealias_skipped_no_nyquist(&cut, real_data::velocity(&cut)));
    }

    fn assert_pass_through(sweep: &VelocitySweep, corrected: &Field, valid_gates: usize) {
        let mut valid = 0;
        for (idx, observed) in sweep.observed.iter().enumerate() {
            let value = corrected.value(idx / sweep.gates, idx % sweep.gates);
            if observed.is_finite() {
                valid += 1;
                let value = value.expect("valid gate kept");
                assert!(
                    (value - observed).abs() <= 0.05,
                    "gate {idx}: {value} != observed {observed}"
                );
            } else {
                assert_eq!(value, None, "gate {idx} has no observation");
            }
        }
        assert_eq!(valid, valid_gates);
    }

    /// Smoothly varying winds folded across whole regions: the KBOX blizzard
    /// sector (Py-ART moves 13,895 gates) and Ida's 0.48 deg cut at Nyquist
    /// 23.2 m/s (152,673 gates). The engine's folds must match Py-ART's up to
    /// one global 2N offset, and the unfolded field must not break where
    /// Py-ART's is continuous.
    #[test]
    fn region_dealias_recovers_smooth_folded_ramp() {
        for (case, min_agreement, max_break_share) in [
            ("kbox_20220129_trim_s1", 0.997, 0.001),
            ("klix_20210829_s2", 0.995, 0.0005),
        ] {
            let Some((volume, golden)) = golden_volume(case) else {
                return;
            };
            assert!(
                golden.unfolded_gates > 10_000,
                "{case}: Py-ART unfolds the sweep"
            );
            let cut = &volume.sweeps[golden.sweep];
            let sweep = VelocitySweep::of_sweep(cut);
            let pyart = golden.aligned_folds(cut, &sweep);
            let engine = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
            let agreement = fold_agreement(&sweep, &engine, &pyart, golden.rays_wrap_around);
            eprintln!("{case}: {:.5} {agreement:?}", agreement.fraction());
            assert_eq!(agreement.compared, golden.valid_gates);
            assert!(
                agreement.fraction() >= min_agreement,
                "{case}: fold agreement with Py-ART {:.5}",
                agreement.fraction()
            );
            assert!(
                (agreement.engine_breaks as f64)
                    <= max_break_share * agreement.pyart_continuous_pairs as f64,
                "{case}: {} breaks where Py-ART is continuous",
                agreement.engine_breaks
            );
        }
    }

    /// Ida's outer rain bands are nearly alias-free: Py-ART moves 13 of
    /// 63,544 gates. Gates Py-ART leaves in place must stay in place, and no
    /// error may run down a radial.
    #[test]
    fn region_dealias_does_not_propagate_errors_down_a_radial() {
        let Some((volume, golden)) = golden_volume("klix_20210829_trim_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let engine = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
        let mut kept_by_pyart = 0;
        let mut moved = 0;
        let mut longest_run = 0;
        for row in 0..sweep.rows {
            let mut run = 0;
            for gate in 0..sweep.gates {
                let idx = row * sweep.gates + gate;
                if pyart[idx] != Some(0) {
                    continue;
                }
                kept_by_pyart += 1;
                if engine[idx] == Some(0) {
                    run = 0;
                } else {
                    moved += 1;
                    run += 1;
                    longest_run = longest_run.max(run);
                }
            }
        }
        eprintln!("kept by Py-ART {kept_by_pyart}, moved {moved}, longest run {longest_run}");
        assert_eq!(kept_by_pyart, golden.valid_gates - golden.unfolded_gates);
        assert!(moved <= 12, "{moved} gates moved that Py-ART keeps");
        assert!(
            longest_run <= 4,
            "a run of {longest_run} moved gates along a ray"
        );
    }

    /// Edge resolution order and tied fold votes must not depend on hash
    /// iteration order: 16 runs over the derecho sector are byte-identical.
    #[test]
    fn region_dealias_is_deterministic_across_runs() {
        let Some((volume, golden)) = golden_volume("kdvn_20200810_trim_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let grid = real_data::velocity(cut);
        let reference = dealias_velocity(cut, grid);
        let sweep = VelocitySweep::of_sweep(cut);
        let moved = grid_folds(&sweep, &reference)
            .iter()
            .filter(|fold| fold.is_some_and(|fold| fold != 0))
            .count();
        eprintln!("moved {moved}");
        assert!(
            moved > 4_000,
            "the sector must exercise fold resolution ({moved} moved)"
        );
        for run in 0..16 {
            assert_eq!(
                dealias_velocity(cut, grid).data,
                reference.data,
                "dealias output changed between identical runs (run {run})"
            );
        }
    }

    /// Patches Py-ART unfolds that are enclosed on every side by gates it
    /// leaves on the sweep's dominant branch (18 patches of 6 gates or more
    /// in the derecho sector): the geometry supports the fold, so the engine
    /// must unfold them too, and leave their rings alone.
    #[test]
    fn region_dealias_unfolds_geometrically_supported_fold() {
        let Some((volume, golden)) = golden_volume("kdvn_20200810_trim_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let engine = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
        let offset = fold_agreement(&sweep, &engine, &pyart, golden.rays_wrap_around).offset;
        let patches = enclosed_patches(&sweep, &pyart, 6);
        let (patch_share, ring_share) = patch_agreement(&patches, &engine, &pyart, offset);
        eprintln!(
            "patches {} patch {patch_share:.4} ring {ring_share:.4}",
            patches.len()
        );
        assert!(patches.len() >= 15);
        assert!(
            patch_share >= 0.97,
            "patch gates matching Py-ART: {patch_share:.4}"
        );
        assert!(
            ring_share >= 0.975,
            "ring gates matching Py-ART: {ring_share:.4}"
        );
    }

    fn patch_agreement(
        patches: &[real_data::FoldedPatch],
        engine: &[Option<i32>],
        pyart: &[Option<i32>],
        offset: i32,
    ) -> (f64, f64) {
        let share = |gates: &mut dyn Iterator<Item = usize>| {
            let (mut total, mut agreeing) = (0usize, 0usize);
            for idx in gates {
                total += 1;
                agreeing += usize::from(engine[idx] == pyart[idx].map(|fold| fold + offset));
            }
            agreeing as f64 / total.max(1) as f64
        };
        (
            share(&mut patches.iter().flat_map(|patch| patch.gates.iter().copied())),
            share(&mut patches.iter().flat_map(|patch| patch.ring.iter().copied())),
        )
    }

    /// [`range_band_reference`] on the Nyquist 32.1 m/s cut of Ida's
    /// split pair, used as the external reference for the 23.2 m/s cut at the
    /// same angle, picks each group's absolute branch. Truth is Py-ART's
    /// output on the branch closest to the HRRR analysis at the site
    /// (`env_offset` in the golden).
    #[test]
    fn external_harmonic_reference_selects_the_absolute_branch() {
        let Some((volume, golden)) = golden_volume("klix_20210829_s2") else {
            return;
        };
        let clean_golden = real_data::PyartGolden::load("klix_20210829_s1");
        let clean = &volume.sweeps[clean_golden.sweep];
        let target = &volume.sweeps[golden.sweep];
        let reference =
            range_band_reference(clean, &dealias_velocity(clean, real_data::velocity(clean)));
        let usable = reference.fits.iter().flatten().count();
        assert!(
            usable >= 70,
            "usable band fits: {usable} of {}",
            reference.fits.len()
        );

        let sweep = VelocitySweep::of_sweep(target);
        let pyart = golden.aligned_folds(target, &sweep);
        let env_offset = golden.env.as_ref().expect("HRRR offset").offset;
        let absolute = |grid: &Field| {
            grid_folds(&sweep, grid)
                .iter()
                .zip(&pyart)
                .filter(|(engine, pyart)| {
                    matches!((engine, pyart), (Some(e), Some(p)) if *e == *p + env_offset)
                })
                .count()
        };
        let grid = real_data::velocity(target);
        let plain = absolute(&dealias_velocity(target, grid));
        let referenced = absolute(&dealias_velocity_with_reference(
            target,
            grid,
            Some(&reference),
        ));
        eprintln!(
            "plain {plain} referenced {referenced} of {}",
            golden.valid_gates
        );
        assert!(
            referenced as f64 >= 0.998 * golden.valid_gates as f64,
            "absolute branch agreement {referenced} of {}",
            golden.valid_gates
        );
        assert!(
            referenced >= plain + 500,
            "the reference must fix branches: {plain} -> {referenced}"
        );
    }

    /// Folded patches spanning three or more adjacent radials, enclosed by
    /// dominant-branch gates in the KBOX blizzard sector: adjacent folded
    /// rows support each other and must all be unfolded.
    #[test]
    fn velocity_dealias_preserves_supported_adjacent_folds() {
        let Some((volume, golden)) = golden_volume("kbox_20220129_trim_s1") else {
            return;
        };
        let cut = &volume.sweeps[golden.sweep];
        let sweep = VelocitySweep::of_sweep(cut);
        let pyart = golden.aligned_folds(cut, &sweep);
        let engine = grid_folds(&sweep, &dealias_velocity(cut, real_data::velocity(cut)));
        let offset = fold_agreement(&sweep, &engine, &pyart, golden.rays_wrap_around).offset;
        let patches: Vec<_> = enclosed_patches(&sweep, &pyart, 6)
            .into_iter()
            .filter(|patch| patch.rows >= 3)
            .collect();
        let (patch_share, ring_share) = patch_agreement(&patches, &engine, &pyart, offset);
        eprintln!(
            "patches {} patch {patch_share:.4} ring {ring_share:.4}",
            patches.len()
        );
        assert!(patches.len() >= 30);
        assert!(
            patch_share >= 0.99,
            "patch gates matching Py-ART: {patch_share:.4}"
        );
        assert!(
            ring_share >= 0.99,
            "ring gates matching Py-ART: {ring_share:.4}"
        );
    }
}
