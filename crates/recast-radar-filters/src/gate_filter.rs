//! Reflectivity gate filter (the GR2Analyst "GateFilter"): hide gates of a
//! non-reflectivity field wherever the SAME SWEEP's reflectivity is below a
//! threshold — the standard declutter for clear-air noise on velocity and
//! dual-pol products. Applied once per (volume, sweep, product) on the render
//! worker and cached; the per-frame fast path is untouched.

use recast_radar_core::{Field, Quantity, Sweep};

use crate::physical_field_like;

/// Filter `field` (any field of `sweep`) against the sweep's reflectivity
/// ([`Sweep::find`] of [`Quantity::Reflectivity`]): gates whose co-located
/// reflectivity is missing or below `threshold_dbz` become empty. Gate
/// spacings may differ (legacy VCPs mix 250 m Doppler with 1000 m
/// surveillance gates) — reflectivity is sampled by true range. Returns a
/// physical `F32` field with `field`'s name and native geometry.
pub fn apply_reflectivity_gate_filter(sweep: &Sweep, field: &Field, threshold_dbz: f32) -> Field {
    let (rows, gates) = field.shape();
    let mut values = vec![f32::NAN; rows * gates];
    if let Some(reflectivity) = sweep.find(Quantity::Reflectivity)
        && let Some((ref_first, ref_spacing)) = reflectivity.native_geometry(&sweep.range)
        && let Some((own_first, own_spacing)) = field.native_geometry(&sweep.range)
    {
        let ref_spacing = ref_spacing.max(1.0);
        let own_spacing = own_spacing.max(1.0);
        let ref_gates = reflectivity.ngates as usize;
        // Both fields belong to `sweep`, so row `r` of each is ray `r`.
        let ref_rows = reflectivity.nrays as usize;
        for row in 0..rows.min(ref_rows) {
            for gate in 0..gates {
                let Some(value) = field.value(row, gate).filter(|v| v.is_finite()) else {
                    continue;
                };
                let range_m = own_first + gate as f64 * own_spacing;
                let ref_gate = ((range_m - ref_first) / ref_spacing).round();
                let passes = ref_gate >= 0.0
                    && (ref_gate as usize) < ref_gates
                    && reflectivity
                        .value(row, ref_gate as usize)
                        .filter(|v| v.is_finite())
                        .is_some_and(|dbz| dbz >= threshold_dbz);
                if passes {
                    values[row * gates + gate] = value;
                }
            }
        }
    }
    physical_field_like(field, field.nrays, field.ngates, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sweep_with;
    use recast_radar_core::FieldName;

    fn sweep(ref_values: &[f32], vel_values: &[f32], gates: usize) -> Sweep {
        let rows = ref_values.len() / gates;
        let azimuths: Vec<f32> = (0..rows).map(|row| row as f32).collect();
        sweep_with(
            &azimuths,
            1000.0,
            250.0,
            gates,
            vec![
                (FieldName::Dbzh, ref_values.to_vec()),
                (FieldName::Vradh, vel_values.to_vec()),
            ],
            Some(28.0),
        )
    }

    #[test]
    fn keeps_velocity_only_where_reflectivity_clears_the_threshold() {
        // 1 row x 4 gates: REF = [35, 5, NaN, 20]; threshold 10 dBZ.
        let sweep = sweep(&[35.0, 5.0, f32::NAN, 20.0], &[10.0, -12.0, 8.0, -20.0], 4);
        let vel = sweep.field(&FieldName::Vradh).unwrap();
        let filtered = apply_reflectivity_gate_filter(&sweep, vel, 10.0);
        assert_eq!(filtered.name, FieldName::Vradh);
        assert_eq!(filtered.value(0, 0), Some(10.0));
        assert_eq!(filtered.value(0, 1), None);
        assert_eq!(filtered.value(0, 2), None);
        assert_eq!(filtered.value(0, 3), Some(-20.0));
    }

    #[test]
    fn no_reflectivity_field_blanks_everything() {
        let mut sweep = sweep(&[35.0, 35.0], &[10.0, -10.0], 2);
        sweep.fields.retain(|field| field.name != FieldName::Dbzh);
        let vel = sweep.field(&FieldName::Vradh).unwrap().clone();
        let filtered = apply_reflectivity_gate_filter(&sweep, &vel, 0.0);
        assert_eq!(filtered.value(0, 0), None);
    }
}
