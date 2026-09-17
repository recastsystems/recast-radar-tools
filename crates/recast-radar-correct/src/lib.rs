//! Radar data correction: Doppler velocity dealiasing.
//!
//! Three unfolding engines share the helpers in this crate root:
//! - the region-based engine ([`dealias_velocity_grid`],
//!   [`dealias_velocity_grid_with_reference`]),
//! - the model-anchored volume engine ([`dealias_volume_v4`],
//!   [`dealias_velocity_grid_v4`]),
//! - a literal port of Py-ART's region dealiaser
//!   ([`dealias_velocity_grid_pyart_region`]).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod dealias_pyart;
mod dealias_v4;
mod region_core;

pub use dealias_pyart::dealias_velocity_grid_pyart_region;
pub use dealias_v4::{
    ConfidenceGrid, EnvWindLevel, EnvironmentalWindProfile, TemporalPrior, V4Diagnostics,
    V4VolumeSolution, dealias_velocity_grid_v4, dealias_volume_v4, project_environmental_winds,
};
use recast_radar_core::{ElevationCut, MomentGrid, MomentStorage, MomentType};

/// Dealias (unfold) a base velocity moment.
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
/// Per-range-band zeroth-harmonic wind reference (Browning & Wexler 1968):
/// v̂(az) = a·cos(az) + b·sin(az), fitted per band of gates. Supplied to the
/// fold resolver as EXTERNAL evidence via
/// [`dealias_velocity_grid_with_reference`]; the bench eval battery fits it
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
/// (dealiased) velocity grid. Two passes: fit, then refit excluding outliers.
/// (Browning & Wexler 1968; formerly the tilt-cascade engine's reference fit
/// — the cascade and hybrid engines were removed at v0.29.0, superseded by
/// the model-anchored `dealias_v4` engine.)
pub fn fit_range_band_reference(cut: &ElevationCut, grid: &MomentGrid) -> RangeBandReference {
    let rows = grid.radial_count();
    let gates = grid.gate_range.gate_count;
    let bands = gates.div_ceil(REFERENCE_BAND_GATES).max(1);
    let azimuth = |row: usize| -> Option<f32> {
        grid.radial_indices
            .get(row)
            .and_then(|&i| cut.radials.get(i))
            .map(|r| r.azimuth_deg)
    };

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
                let Some(v) = grid.scaled_value(row, gate).filter(|v| v.is_finite()) else {
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

pub fn dealias_velocity_grid(cut: &ElevationCut, source: &MomentGrid) -> MomentGrid {
    dealias_velocity_grid_with_reference(cut, source, None)
}

/// True when [`dealias_velocity_grid`] over this cut can only pass raw
/// velocity through: no radial of the velocity grid carries a usable
/// (finite, positive) Nyquist velocity, so every per-row Nyquist and the
/// median fallback are unknown and no fold correction can ever apply
/// (`v` is emitted unchanged). JMA is always in this state by design
/// (staggered PRF, Nyquist left `None` — see `recast-radar-io-jma/src/lib.rs`);
/// the occasional ODIM file omits `how/NI` too. The UI queries this so a
/// product labeled "dealiased" can disclose the pass-through instead of
/// silently rendering raw velocity under a dealiased label (data trust:
/// the fold warning can never fire without a Nyquist).
pub fn dealias_skipped_no_nyquist(cut: &ElevationCut, source: &MomentGrid) -> bool {
    median_nyquist_mps(cut, source).is_none()
}

/// `dealias_velocity_grid` with an optional external wind reference: the
/// resolver uses it for connected-group BRANCH selection and per-region
/// verification (UNRAVEL-style checks, Louf et al. 2020). With `None` the
/// behavior is identical to the plain region engine.
pub fn dealias_velocity_grid_with_reference(
    cut: &ElevationCut,
    source: &MomentGrid,
    reference: Option<&RangeBandReference>,
) -> MomentGrid {
    let rows = source.radial_count();
    let gate_count = source.gate_range.gate_count;
    let total = rows.saturating_mul(gate_count);
    let fallback_nyquist = median_nyquist_mps(cut, source);

    // Per-row Nyquist (NaN where unknown) and observed velocities (NaN for
    // no-data / range-folded gates, which never join a region).
    let mut nyq = vec![f32::NAN; rows.max(1)];
    for (row, slot) in nyq.iter_mut().enumerate().take(rows) {
        *slot = row_nyquist_mps(cut, source, row)
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

    let azimuths = radial_azimuths(cut, source);
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

    MomentGrid {
        moment: MomentType::Velocity,
        gate_range: source.gate_range.clone(),
        scale: DEALIASED_VELOCITY_SCALE,
        offset: DEALIASED_VELOCITY_OFFSET,
        nodata: Some(DEALIASED_VELOCITY_NODATA),
        range_folded: None,
        radial_indices: source.radial_indices.clone(),
        storage: MomentStorage::U16(corrected),
    }
}

/// Per-row beam azimuth of `grid` in degrees, normalized to `[0, 360)`; NaN
/// for a row whose radial index is missing from `cut`.
pub fn radial_azimuths(cut: &ElevationCut, grid: &MomentGrid) -> Vec<f32> {
    grid.radial_indices
        .iter()
        .map(|ri| {
            cut.radials
                .get(*ri)
                .map(|r| r.azimuth_deg.rem_euclid(360.0))
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

/// Write row `row` of `source` into `row_values` as physical values (m/s).
///
/// No-data and range-folded gates become NaN. `row_values` must be exactly
/// `gate_count` long; otherwise (or when the row is out of range) it is left
/// all NaN.
pub fn copy_scaled_velocity_row(source: &MomentGrid, row: usize, row_values: &mut [f32]) {
    row_values.fill(f32::NAN);
    let gate_count = source.gate_range.gate_count;
    if gate_count == 0 || row_values.len() != gate_count {
        return;
    }
    let Some(row_start) = row.checked_mul(gate_count) else {
        return;
    };
    let row_end = row_start + gate_count;
    match &source.storage {
        MomentStorage::U8(values) => {
            let Some(raw_row) = values.get(row_start..row_end) else {
                return;
            };
            for (raw, value) in raw_row.iter().zip(row_values.iter_mut()) {
                let raw = u16::from(*raw);
                if source.nodata == Some(raw) || source.range_folded == Some(raw) {
                    continue;
                }
                *value = (raw as f32 - source.offset) / source.scale;
            }
        }
        MomentStorage::U16(values) => {
            let Some(raw_row) = values.get(row_start..row_end) else {
                return;
            };
            for (raw, value) in raw_row.iter().zip(row_values.iter_mut()) {
                if source.nodata == Some(*raw) || source.range_folded == Some(*raw) {
                    continue;
                }
                *value = (*raw as f32 - source.offset) / source.scale;
            }
        }
        MomentStorage::F32(values) => {
            let Some(source_row) = values.get(row_start..row_end) else {
                return;
            };
            row_values.copy_from_slice(source_row);
        }
    }
}

fn median_nyquist_mps(cut: &ElevationCut, grid: &MomentGrid) -> Option<f32> {
    let mut values = grid
        .radial_indices
        .iter()
        .filter_map(|radial_index| cut.radials.get(*radial_index)?.nyquist_velocity_mps)
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}

fn row_nyquist_mps(cut: &ElevationCut, grid: &MomentGrid, row: usize) -> Option<f32> {
    let radial_index = *grid.radial_indices.get(row)?;
    cut.radials.get(radial_index)?.nyquist_velocity_mps
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
    use super::*;
    use recast_radar_core::{GateRange, Radial};

    #[test]
    fn lightweight_velocity_dealias_unfolds_radial_continuity() {
        let gate_range = GateRange {
            first_gate_m: 0,
            gate_spacing_m: 1_000,
            gate_count: 5,
        };
        let mut cut = ElevationCut::new(0.5, Some(1));
        cut.radials.push(Radial {
            azimuth_deg: 0.0,
            elevation_deg: 0.5,
            time_offset_ms: 0,
            gate_range: gate_range.clone(),
            nyquist_velocity_mps: Some(10.0),
            radial_status: None,
        });
        let grid = MomentGrid {
            moment: MomentType::Velocity,
            gate_range,
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices: vec![0],
            storage: MomentStorage::F32(vec![0.0, 5.0, 9.0, -9.0, -7.0]),
        };

        let corrected = dealias_velocity_grid(&cut, &grid);
        assert!(matches!(corrected.storage, MomentStorage::U16(_)));

        let values = (0..corrected.gate_range.gate_count)
            .map(|gate| corrected.scaled_value(0, gate).expect("corrected gate"))
            .collect::<Vec<_>>();
        assert_eq!(values, vec![0.0, 5.0, 9.0, 11.0, 13.0]);
    }

    #[test]
    fn dealias_skip_detection_reports_nyquist_less_feeds() {
        // JMA-style feed: staggered PRF leaves Nyquist unset on every
        // radial, so the "dealiased" grid is a pure pass-through and the
        // UI must be able to disclose that.
        let observed = vec![2.0f32, 4.0, 6.0, 8.0];
        let rows_data: Vec<Vec<f32>> = (0..4).map(|_| observed.clone()).collect();
        let (mut cut, grid) = test_velocity_grid_rows(rows_data);
        for radial in &mut cut.radials {
            radial.nyquist_velocity_mps = None;
        }

        assert!(
            dealias_skipped_no_nyquist(&cut, &grid),
            "no usable Nyquist on any radial must report the skip"
        );
        // The skip really is a pass-through: values come back as recorded.
        let corrected = dealias_velocity_grid(&cut, &grid);
        for (gate, value) in observed.iter().enumerate() {
            assert_eq!(
                corrected.scaled_value(1, gate),
                Some(*value),
                "gate {gate} must pass through unchanged"
            );
        }

        // Any radial with a usable Nyquist flips the answer (the median
        // fallback then covers Nyquist-less rows).
        cut.radials[0].nyquist_velocity_mps = Some(20.0);
        assert!(!dealias_skipped_no_nyquist(&cut, &grid));

        // Non-finite / non-positive declarations are not usable Nyquists.
        cut.radials[0].nyquist_velocity_mps = Some(0.0);
        assert!(dealias_skipped_no_nyquist(&cut, &grid));
        cut.radials[0].nyquist_velocity_mps = Some(f32::NAN);
        assert!(dealias_skipped_no_nyquist(&cut, &grid));
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
        let (mut cut, grid) = test_velocity_grid_rows(rows_data);
        for radial in &mut cut.radials {
            radial.nyquist_velocity_mps = Some(nyq);
        }

        let corrected = dealias_velocity_grid(&cut, &grid);
        let recovered: Vec<f32> = (0..gates)
            .map(|g| corrected.scaled_value(rows / 2, g).expect("gate"))
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
        let (mut cut, grid) = test_velocity_grid_rows(rows_data);
        for radial in &mut cut.radials {
            radial.nyquist_velocity_mps = Some(nyq);
        }

        let corrected = dealias_velocity_grid(&cut, &grid);
        for (g, value) in coherent.iter().enumerate() {
            assert_eq!(
                corrected.scaled_value(5, g),
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
        let (mut cut, grid) = test_velocity_grid_rows(rows_data);
        for radial in &mut cut.radials {
            radial.nyquist_velocity_mps = Some(nyq);
        }

        let reference = dealias_velocity_grid(&cut, &grid);
        for run in 0..16 {
            let corrected = dealias_velocity_grid(&cut, &grid);
            assert_eq!(
                corrected.storage, reference.storage,
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
        let (cut, grid) = test_velocity_grid_rows(vec![
            quiet.clone(),
            quiet.clone(),
            folded,
            quiet.clone(),
            quiet,
        ]);

        let corrected = dealias_velocity_grid(&cut, &grid);

        assert_eq!(corrected.scaled_value(2, 3), Some(11.0));
        assert_eq!(corrected.scaled_value(2, 4), Some(13.0));
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
    ) -> ElevationCut {
        let gate_range = GateRange {
            first_gate_m: 1000,
            gate_spacing_m: 250,
            gate_count: gates,
        };
        let mut cut = ElevationCut::new(elevation, None);
        let mut data = vec![f32::NAN; rows * gates];
        for row in 0..rows {
            let az = row as f32 * (360.0 / rows as f32);
            cut.radials.push(Radial {
                azimuth_deg: az,
                elevation_deg: elevation,
                time_offset_ms: 0,
                gate_range: gate_range.clone(),
                nyquist_velocity_mps: Some(nyquist),
                radial_status: None,
            });
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
        cut.moments.insert(
            MomentType::Velocity,
            MomentGrid {
                moment: MomentType::Velocity,
                gate_range,
                scale: 1.0,
                offset: 0.0,
                nodata: None,
                range_folded: None,
                radial_indices: (0..rows).collect(),
                storage: MomentStorage::F32(data),
            },
        );
        cut
    }

    /// Pins the two v0.29.0 survivors of the retired cascade/hybrid engines:
    /// [`fit_range_band_reference`] (also the bench battery's `rms_harmonic`
    /// metric input) and the region engine's external-reference branch
    /// selection ([`dealias_velocity_grid_with_reference`]).
    #[test]
    fn external_harmonic_reference_selects_the_absolute_branch() {
        // 35 m/s wind. Clean tilt: Nyquist 40 — no aliasing, honest fit.
        // Target tilt: Nyquist 20 — large sectors wrap (|v| up to 35), the
        // regime where a same-sweep reference is circular.
        let clean = tilt_with_uniform_wind(2.4, 35.0, 180.0, 40.0, 360, 200);
        let clean_grid = clean.moments.get(&MomentType::Velocity).unwrap();
        let reference = fit_range_band_reference(&clean, clean_grid);
        assert!(
            reference.fits.iter().filter(|fit| fit.is_some()).count() > 0,
            "clean uniform wind must produce usable band fits"
        );

        let target = tilt_with_uniform_wind(0.5, 35.0, 180.0, 20.0, 360, 200);
        let target_grid = target.moments.get(&MomentType::Velocity).unwrap();
        let dealiased =
            dealias_velocity_grid_with_reference(&target, target_grid, Some(&reference));
        let mut worst = 0.0f32;
        for row in 0..360 {
            let az = row as f32;
            let truth = 35.0 * ((az - 180.0).to_radians()).cos();
            for gate in (0..200).step_by(7) {
                if let Some(v) = dealiased.scaled_value(row, gate).filter(|v| v.is_finite()) {
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
        let (cut, grid) = test_velocity_grid_rows(vec![
            quiet.clone(),
            folded.clone(),
            folded.clone(),
            folded,
            quiet,
        ]);

        let corrected = dealias_velocity_grid(&cut, &grid);

        assert_eq!(corrected.scaled_value(2, 3), Some(11.0));
        assert_eq!(corrected.scaled_value(2, 4), Some(13.0));
    }

    fn test_velocity_grid_rows(rows: Vec<Vec<f32>>) -> (ElevationCut, MomentGrid) {
        let gate_range = GateRange {
            first_gate_m: 0,
            gate_spacing_m: 1_000,
            gate_count: rows.first().map(Vec::len).unwrap_or(0),
        };
        let mut cut = ElevationCut::new(0.5, Some(1));
        for index in 0..rows.len() {
            cut.radials.push(Radial {
                azimuth_deg: index as f32,
                elevation_deg: 0.5,
                time_offset_ms: 0,
                gate_range: gate_range.clone(),
                nyquist_velocity_mps: Some(10.0),
                radial_status: None,
            });
        }
        let grid = MomentGrid {
            moment: MomentType::Velocity,
            gate_range,
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices: (0..cut.radials.len()).collect(),
            storage: MomentStorage::F32(rows.into_iter().flatten().collect()),
        };
        (cut, grid)
    }
}
