//! Azimuthal (rotational) shear via the Linear Least-Squares Derivative (LLSD)
//! method — the operational basis for the MRMS azimuthal-shear / rotation-track
//! products and a primary mesocyclone/tornado detection field.
//!
//! Smith & Elmore (2004), *The use of radial velocity derivatives to diagnose
//! rotation and divergence*, 11th Conf. Aviation, Range & Aerospace Meteorology;
//! Mahalik et al. (2019), *Estimterm… Azimuthal Shear* (MWR/WAF) for the modern
//! LLSD formulation. Azimuthal shear ≈ ∂Vr/∂x across the radial, estimated by a
//! least-squares fit of dealiased radial velocity over a small azimuth×range
//! window. Computed on the DEALIASED velocity field so range folds never
//! manufacture spurious shear.

use recast_radar_core::{ElevationCut, MomentGrid, MomentStorage, MomentType};

/// Half-width of the LLSD window in azimuth (radials) and range (gates).
const AZ_HALF: isize = 1; // ±1 radial (3 beams)
const RG_HALF: isize = 1; // ±1 gate
/// Output is stored in 10^-3 s^-1 (the conventional shear display unit). The
/// fit is along +azimuth (daz = azr - az0, clockwise), so positive azimuthal
/// shear = Vr increasing toward increasing azimuth (clockwise / right of the
/// down-range beam) — the cyclonic sense in the Northern Hemisphere.
const SHEAR_DISPLAY_SCALE: f32 = 1000.0;

/// Azimuthal shear (×10^-3 s^-1): ∂Vr across the radial. Mesocyclone/TVS
/// rotation detector. NaN = no data.
pub fn azimuthal_shear_grid(cut: &ElevationCut, velocity: &MomentGrid) -> MomentGrid {
    let dealiased = recast_radar_correct::dealias_velocity_grid(cut, velocity);
    azimuthal_shear_grid_from_dealiased(cut, &dealiased)
}

/// Azimuthal shear from a velocity grid the caller has already dealiased.
/// This function performs no dealiasing: engine and model/temporal-anchor
/// ownership stay with the caller. Application pipelines should prefer this
/// entry point after resolving their selected engine.
pub fn azimuthal_shear_grid_from_dealiased(
    cut: &ElevationCut,
    dealiased_velocity: &MomentGrid,
) -> MomentGrid {
    llsd_velocity_derivative(cut, dealiased_velocity, Axis::Azimuthal)
}

/// Radial divergence (×10^-3 s^-1): ∂Vr along the radial. Positive = divergence
/// (e.g. downburst outflow / DCZ), negative = convergence (gust front / boundary)
/// — the defining derecho signature. Smith & Elmore (2004). NaN = no data.
pub fn radial_divergence_grid(cut: &ElevationCut, velocity: &MomentGrid) -> MomentGrid {
    let dealiased = recast_radar_correct::dealias_velocity_grid(cut, velocity);
    radial_divergence_grid_from_dealiased(cut, &dealiased)
}

/// Radial divergence from a velocity grid the caller has already dealiased.
/// This function performs no dealiasing.
pub fn radial_divergence_grid_from_dealiased(
    cut: &ElevationCut,
    dealiased_velocity: &MomentGrid,
) -> MomentGrid {
    llsd_velocity_derivative(cut, dealiased_velocity, Axis::Radial)
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    /// Cross-radial (azimuthal) derivative → rotational shear.
    Azimuthal,
    /// Along-radial (range) derivative → divergence/convergence.
    Radial,
}

/// Shared LLSD core: fit v = a + b·x over a small azimuth×range window, where x
/// is cross-radial arc distance (Azimuthal) or along-radial distance (Radial).
/// b is the velocity derivative (s^-1), output scaled to ×10^-3 s^-1.
fn llsd_velocity_derivative(
    cut: &ElevationCut,
    dealiased_velocity: &MomentGrid,
    axis: Axis,
) -> MomentGrid {
    let rows = dealiased_velocity.radial_count();
    let gates = dealiased_velocity.gate_range.gate_count;
    let gr = &dealiased_velocity.gate_range;
    let spacing = gr.gate_spacing_m as f32;

    let az_deg: Vec<f32> = (0..rows)
        .map(|r| {
            dealiased_velocity
                .radial_indices
                .get(r)
                .and_then(|ri| cut.radials.get(*ri))
                .map(|radial| radial.azimuth_deg.rem_euclid(360.0))
                .unwrap_or(f32::NAN)
        })
        .collect();

    let mut out = vec![f32::NAN; rows.saturating_mul(gates)];
    for row in 0..rows {
        let az0 = az_deg[row];
        if !az0.is_finite() {
            continue;
        }
        for gate in 0..gates {
            let r_m = gr.first_gate_m as f32 + gate as f32 * spacing;
            if r_m <= 0.0 {
                continue;
            }
            let (mut sx, mut sv, mut sxx, mut sxv, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0u32);
            for dr in -AZ_HALF..=AZ_HALF {
                let Some(rr) = row.checked_add_signed(dr) else {
                    continue;
                };
                if rr >= rows {
                    continue;
                }
                let azr = az_deg[rr];
                if !azr.is_finite() {
                    continue;
                }
                // signed azimuth delta (radians, wrapped to [-pi, pi])
                let mut daz = (azr - az0).to_radians();
                while daz > std::f32::consts::PI {
                    daz -= std::f32::consts::TAU;
                }
                while daz < -std::f32::consts::PI {
                    daz += std::f32::consts::TAU;
                }
                for dg in -RG_HALF..=RG_HALF {
                    let Some(gg) = gate.checked_add_signed(dg) else {
                        continue;
                    };
                    if gg >= gates {
                        continue;
                    }
                    let Some(v) = dealiased_velocity.scaled_value(rr, gg) else {
                        continue;
                    };
                    if !v.is_finite() {
                        continue;
                    }
                    let x = match axis {
                        Axis::Azimuthal => r_m * daz, // cross-radial arc (m)
                        Axis::Radial => (gg as f32 - gate as f32) * spacing, // along-radial (m)
                    } as f64;
                    let v = v as f64;
                    sx += x;
                    sv += v;
                    sxx += x * x;
                    sxv += x * v;
                    n += 1;
                }
            }
            if n < 4 {
                continue;
            }
            let nf = n as f64;
            let denom = nf * sxx - sx * sx;
            if denom.abs() < 1e-6 {
                continue;
            }
            let slope = (nf * sxv - sx * sv) / denom; // s^-1
            out[row * gates + gate] = (slope as f32) * SHEAR_DISPLAY_SCALE;
        }
    }

    MomentGrid {
        moment: MomentType::Velocity,
        gate_range: gr.clone(),
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices: dealiased_velocity.radial_indices.clone(),
        storage: MomentStorage::F32(out),
    }
}
