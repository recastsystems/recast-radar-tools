//! Polar-domain smoothing for display: a NaN-aware 3×3 binomial kernel
//! ([1 2 1]⊗[1 2 1]) over azimuth × range on the field's physical values.
//! Smoothing the FIELD once (cached per volume/sweep/product by the render
//! worker) and rendering it through the existing nearest-gate fast path
//! keeps pans at full speed — the smoothed look costs one ~5–10 ms pass per
//! product instead of per-pixel work every frame.
//!
//! Range-folded and missing gates contribute nothing (weights renormalize);
//! a gate with no finite neighbors stays empty. Note: RF gates therefore
//! render transparent in smoothed mode — analysts who need the RF purple
//! should use the native (unsmoothed) display.

use rayon::prelude::*;
use recast_radar_core::Field;

use crate::physical_field_like;

/// Smooth a field's physical values into a new `F32` field with identical
/// geometry. Rows are rays in storage order: azimuth wraps from the last row
/// to the first; range is clamped at the ends.
pub fn smooth_field(field: &Field) -> Field {
    let (rows, gates) = field.shape();
    let mut values = vec![f32::NAN; rows * gates];
    if rows > 0 && gates > 0 {
        // Materialize physical values once (NaN for every sentinel).
        let mut source = vec![f32::NAN; rows * gates];
        source
            .par_chunks_mut(gates)
            .enumerate()
            .for_each(|(row, out_row)| {
                for (gate, cell) in out_row.iter_mut().enumerate() {
                    if let Some(v) = field.value(row, gate).filter(|v| v.is_finite()) {
                        *cell = v;
                    }
                }
            });
        const KERNEL: [f32; 3] = [1.0, 2.0, 1.0];
        values
            .par_chunks_mut(gates)
            .enumerate()
            .for_each(|(row, out_row)| {
                for (gate, cell) in out_row.iter_mut().enumerate() {
                    // A gate only renders where the native display would —
                    // smoothing must not grow coverage.
                    if !source[row * gates + gate].is_finite() {
                        continue;
                    }
                    let mut sum = 0.0f32;
                    let mut weight = 0.0f32;
                    for (di, &kr) in KERNEL.iter().enumerate() {
                        let r = ((row as i64 + di as i64 - 1).rem_euclid(rows as i64)) as usize;
                        for (dj, &kg) in KERNEL.iter().enumerate() {
                            let g = gate as i64 + dj as i64 - 1;
                            if g < 0 || g >= gates as i64 {
                                continue;
                            }
                            let v = source[r * gates + g as usize];
                            if v.is_finite() {
                                let k = kr * kg;
                                sum += v * k;
                                weight += k;
                            }
                        }
                    }
                    if weight > 0.0 {
                        *cell = sum / weight;
                    }
                }
            });
    }
    physical_field_like(field, field.nrays, field.ngates, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sweep_with;
    use recast_radar_core::FieldName;

    fn field(rows: usize, gates: usize, data: Vec<f32>) -> Field {
        let azimuths: Vec<f32> = (0..rows).map(|row| row as f32 * 45.0).collect();
        let mut sweep = sweep_with(
            &azimuths,
            250.0,
            250.0,
            gates,
            vec![(FieldName::Dbzh, data)],
            None,
        );
        sweep.fields.remove(0)
    }

    #[test]
    fn uniform_field_is_unchanged() {
        let s = smooth_field(&field(8, 8, vec![35.0; 64]));
        for row in 0..8 {
            for gate in 0..8 {
                let v = s.value(row, gate).unwrap();
                assert!((v - 35.0).abs() < 1e-4, "{v}");
            }
        }
    }

    #[test]
    fn steps_soften_and_coverage_does_not_grow() {
        // Left half 20 dBZ, right half NaN.
        let mut data = vec![f32::NAN; 64];
        for row in 0..8 {
            for gate in 0..4 {
                data[row * 8 + gate] = 20.0;
            }
        }
        let s = smooth_field(&field(8, 8, data));
        // Edge gate keeps its value (NaN neighbors renormalize)…
        assert!((s.value(0, 3).unwrap() - 20.0).abs() < 1e-4);
        // …and empty gates STAY empty (no coverage bleed).
        assert_eq!(s.value(0, 4), None);
    }

    #[test]
    fn interior_step_blends() {
        // Gate column 4 jumps 0 -> 40: smoothed neighbors blend toward each
        // other across the step.
        let mut data = vec![0.0f32; 64];
        for row in 0..8 {
            for gate in 4..8 {
                data[row * 8 + gate] = 40.0;
            }
        }
        let s = smooth_field(&field(8, 8, data));
        let low_side = s.value(3, 3).unwrap();
        let high_side = s.value(3, 4).unwrap();
        assert!(low_side > 0.0 && low_side < 20.0, "{low_side}");
        assert!(high_side > 20.0 && high_side < 40.0, "{high_side}");
    }
}
