//! Reflectivity gate filter (the GR2Analyst "GateFilter"): hide gates of a
//! non-reflectivity moment wherever the SAME CUT's reflectivity is below a
//! threshold — the standard declutter for clear-air noise on velocity and
//! dual-pol products. Applied once per (volume, cut, product) on the render
//! worker and cached; the per-frame fast path is untouched.

use recast_radar_core::{ElevationCut, MomentGrid, MomentStorage, MomentType};

/// Filter `grid` (any moment sharing `cut`'s radials) against the cut's
/// reflectivity: gates whose co-located REF is missing or below
/// `threshold_dbz` become empty. Gate spacings may differ (legacy VCPs mix
/// 250 m Doppler with 1000 m surveillance gates) — REF is sampled by true
/// range. Returns an F32 grid with identical geometry to the input.
pub fn apply_reflectivity_gate_filter(
    cut: &ElevationCut,
    grid: &MomentGrid,
    threshold_dbz: f32,
) -> MomentGrid {
    let rows = grid.radial_count();
    let gates = grid.gate_range.gate_count;
    let mut values = vec![f32::NAN; rows * gates];
    if let Some(reflectivity) = cut.moments.get(&MomentType::Reflectivity) {
        let ref_first = reflectivity.gate_range.first_gate_m as f64;
        let ref_spacing = reflectivity.gate_range.gate_spacing_m.max(1) as f64;
        let ref_gates = reflectivity.gate_range.gate_count;
        let own_first = grid.gate_range.first_gate_m as f64;
        let own_spacing = grid.gate_range.gate_spacing_m.max(1) as f64;
        // The two grids share the cut's radials but may index rows through
        // different radial_indices orderings; map by radial index.
        let mut ref_row_by_radial =
            vec![usize::MAX; cut.radials.len().max(reflectivity.radial_indices.len())];
        for (row, &radial) in reflectivity.radial_indices.iter().enumerate() {
            if radial < ref_row_by_radial.len() {
                ref_row_by_radial[radial] = row;
            }
        }
        for (row, &radial) in grid.radial_indices.iter().enumerate().take(rows) {
            let ref_row = ref_row_by_radial.get(radial).copied().unwrap_or(usize::MAX);
            if ref_row == usize::MAX {
                continue;
            }
            for gate in 0..gates {
                let Some(value) = grid.scaled_value(row, gate).filter(|v| v.is_finite()) else {
                    continue;
                };
                let range_m = own_first + gate as f64 * own_spacing;
                let ref_gate = ((range_m - ref_first) / ref_spacing).round();
                let passes = ref_gate >= 0.0
                    && (ref_gate as usize) < ref_gates
                    && reflectivity
                        .scaled_value(ref_row, ref_gate as usize)
                        .filter(|v| v.is_finite())
                        .is_some_and(|dbz| dbz >= threshold_dbz);
                if passes {
                    values[row * gates + gate] = value;
                }
            }
        }
    }
    MomentGrid {
        moment: grid.moment.clone(),
        gate_range: grid.gate_range.clone(),
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices: grid.radial_indices.clone(),
        storage: MomentStorage::F32(values),
    }
}
