//! Inter-gate display interpolation: bilinear upsampling of a field on the
//! polar lattice (azimuth × range). The pass runs ONCE per
//! volume/sweep/product on the render worker (cached exactly like the
//! binomial Soften pass in `smooth.rs`) and the finer sweep is drawn through
//! the unchanged nearest-gate fast path — pans stay full speed; only the
//! cached field is bigger.
//!
//! Technique: standard separable bilinear resampling, applied on the polar
//! grid rather than the screen raster — the same "smoothed display" approach
//! popularized by GR2Analyst-class viewers (literally "adds grid cells in
//! between the scan lines"). No novel science. The per-moment-family guards
//! mirror `volumetric.rs` (`InterpPolicy`): correlation coefficient never
//! blends through a rho_hv minimum (Giangrande, Krause & Ryzhkov 2008 — the
//! minimum IS the melting-layer signature), and velocity never blends across
//! a spread larger than the volumetric module's 30 m/s guard (interpolating
//! across an aliasing fold or a couplet fabricates intermediate values).
//!
//! ## Upsample policy (input geometry → factors)
//!
//! Targets ≤ 0.25° azimuth and ≤ 250 m gates, capped at 4× per axis and a
//! 64 MB F32 buffer budget. Coarse fields are upsampled aggressively, fine
//! fields mildly or not at all:
//!
//! | native sweep                     | factors (az × rng) | result          |
//! |----------------------------------|--------------------|-----------------|
//! | 1.0° × 1000 m (legacy/intl)      | 4 × 4              | 0.25° × 250 m   |
//! | 1.0° × 500 m (European C-band)   | 4 × 2              | 0.25° × 250 m   |
//! | 1.0° × 250 m                     | 4 × 1              | 0.25° × 250 m   |
//! | 0.5° × 250 m (NEXRAD super-res)  | 2 × 1              | 0.25° × 250 m   |
//! | ≤ 0.25° × ≤ 250 m                | 1 × 1              | native (no pass)|
//!
//! Range factors are additionally constrained to keep whole-metre gate
//! geometry exact (sub-spacing must divide evenly and the half-cell shift
//! must be a whole meter); failing factors step down. The rule is applied to
//! the native spacing rounded to the metre, and the output geometry is
//! computed in f64 from the exact spacing. Row/gate counts are clamped to the
//! packed sample encoding's limits.
//!
//! ## Coverage discipline
//!
//! Interpolation must NOT grow echo coverage: a sub-cell renders only where
//! the native display would. Each sub-cell lies inside exactly one native
//! cell (its nearest parent); if that parent is missing/RF the sub-cell
//! stays empty, and if any of the four bilinear parents is missing the
//! sub-cell takes the nearest parent's value instead of a partial blend.
//! Sub-rows exactly on a native beam boundary (t = 0.5) render only where
//! BOTH bracketing beams carry echo — otherwise one side's echo would
//! reach half a sub-beam past the midpoint the native display stops at.
//! Azimuth wraps; range clamps. Like the Soften pass, RF gates render
//! transparent (the native display is the place to read the RF purple).
//! Sub-rows are synthesized only between beams whose azimuth gap is small
//! (sector-scan edges keep their native hole).

use crate::{InterpPolicy, physical_field_like};
use rayon::prelude::*;
use recast_radar_core::{Field, GateMapping, Quantity, RangeCoord, Sweep};

/// Display target: no coarser than 0.25° between rendered radials.
pub const INTERP_TARGET_AZIMUTH_DEG: f32 = 0.25;
/// Display target: no coarser than 250 m between rendered gates.
pub const INTERP_TARGET_GATE_SPACING_M: i32 = 250;
/// Hard cap per axis — beyond 4× the cost outruns the visual return.
pub const INTERP_MAX_FACTOR: usize = 4;
/// Budget for the cached F32 field (the moment-cache entry that holds it).
pub const INTERP_MAX_GRID_BYTES: usize = 64 << 20;
/// Upsampled row count stays below this: the renderer's fast path packs
/// (row, gate) into 31 bits with 16 gate bits (`recast_radar_render`'s
/// packed sample cache).
pub const INTERP_ROW_LIMIT: usize = 1 << 15;
/// Upsampled gate count stays at or below this (the 16-bit gate field of
/// the same packed sample encoding).
pub const INTERP_MAX_GATES: usize = (1 << 16) - 1;
/// Widest azimuth half-width (degrees) the renderer's azimuth lookup fills
/// to each side of a native beam; sub-rows are never synthesized across a
/// gap wider than twice this.
pub const INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG: f32 = 3.0;

/// Same threshold as `volumetric.rs`'s `InterpPolicy::VelocityGuard`.
const VELOCITY_GUARD_SPREAD_MPS: f32 = 30.0;
/// Same floor as `volumetric.rs`'s `InterpPolicy::CcGuard`.
const CC_GUARD_FLOOR: f32 = 0.97;
/// Beams closer than this are duplicates — nothing to synthesize between.
const MIN_SYNTH_DELTA_DEG: f32 = 0.01;

/// A field upsampled for display, as a sweep of its own.
///
/// `sweep` holds exactly one field (the upsampled one, a physical `F32`
/// field with the source's name). Its rays are the output rows: native rows
/// keep their exact beam azimuth (normalized to `[0, 360)`), synthetic rows
/// sit between them; each ray's time, elevation and Nyquist velocity are its
/// nearest parent's. Its `range` is the sub-gate range coordinate.
pub struct UpsampledSweep {
    /// The upsampled sweep, holding the one upsampled field.
    pub sweep: Sweep,
    /// For each output ray, the source ray whose cell contains it (the
    /// nearest parent), so per-ray lookups on the source sweep stay valid.
    pub parent_rays: Vec<u32>,
}

/// How many output rows and gates [`upsample_field`] makes per native row and gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpsampleFactors {
    /// Output rows per native row (1 to 4).
    pub azimuth: usize,
    /// Output gates per native gate (1 to 4).
    pub range: usize,
}

impl UpsampleFactors {
    /// Whether both factors are 1 (no upsampling).
    pub fn is_identity(self) -> bool {
        self.azimuth <= 1 && self.range <= 1
    }
}

/// Sub-spacing must stay whole meters and the cell-centered half-shift
/// `(spacing - spacing/factor) / 2` must be a whole meter too, so the
/// upsampled annulus is EXACTLY the native one.
fn range_factor_is_exact(spacing_m: i32, factor: usize) -> bool {
    let factor = factor as i32;
    factor > 0 && spacing_m % factor == 0 && (spacing_m - spacing_m / factor) % 2 == 0
}

/// Pick adaptive upsample factors for a sweep's nominal geometry (see the
/// module-level policy table).
pub fn upsample_factors(
    nominal_azimuth_deg: f32,
    gate_spacing_m: i32,
    rows: usize,
    gates: usize,
) -> UpsampleFactors {
    let mut azimuth = 1usize;
    if nominal_azimuth_deg.is_finite() && nominal_azimuth_deg > 0.0 {
        while azimuth < INTERP_MAX_FACTOR
            && nominal_azimuth_deg / azimuth as f32 > INTERP_TARGET_AZIMUTH_DEG + 1e-3
        {
            azimuth += 1;
        }
    }
    let mut range = 1usize;
    if gate_spacing_m > 0 {
        while range < INTERP_MAX_FACTOR
            && gate_spacing_m as f32 / range as f32 > INTERP_TARGET_GATE_SPACING_M as f32
        {
            range += 1;
        }
        while range > 1 && !range_factor_is_exact(gate_spacing_m, range) {
            range -= 1;
        }
    }
    // The fast path packs (row, gate) into 31 bits — stay inside it.
    while azimuth > 1 && rows.saturating_mul(azimuth) >= INTERP_ROW_LIMIT {
        azimuth -= 1;
    }
    while range > 1 && gates.saturating_mul(range) > INTERP_MAX_GATES {
        range -= 1;
    }
    // Memory budget for the cached F32 buffer. Range steps down first (gates
    // are usually already the finer axis; the azimuth seams are what the
    // interpolated mode exists to fill).
    while rows
        .saturating_mul(azimuth)
        .saturating_mul(gates)
        .saturating_mul(range)
        .saturating_mul(std::mem::size_of::<f32>())
        > INTERP_MAX_GRID_BYTES
    {
        if range > 1 {
            range -= 1;
        } else if azimuth > 1 {
            azimuth -= 1;
        } else {
            break;
        }
    }
    UpsampleFactors { azimuth, range }
}

/// Per-quantity interpolation policy, mirroring the volumetric module's
/// cross-section policies (`volumetric.rs`): velocity (raw or dealiased)
/// guards against blending across folds/couplets, CC against blending
/// through the melting layer; everything else (REF/ZDR/SW/PHI/KDP) blends
/// linearly.
fn interp_policy_for_quantity(quantity: Quantity) -> InterpPolicy {
    match quantity {
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity => InterpPolicy::VelocityGuard,
        Quantity::CorrelationCoefficient => InterpPolicy::CcGuard,
        _ => InterpPolicy::LinearAngle,
    }
}

/// Shortest signed angular step from `from` to `to`, in (-180, 180].
fn signed_delta_deg(from_deg: f32, to_deg: f32) -> f32 {
    let delta = (to_deg - from_deg).rem_euclid(360.0);
    if delta > 180.0 { delta - 360.0 } else { delta }
}

/// One output row's parents: bilinear weight `t` between scan-order rows
/// `lo` and `hi` (t = 0 reproduces the native row `lo` exactly).
struct RowPlan {
    lo: usize,
    hi: usize,
    t: f32,
    azimuth_deg: f32,
}

/// One output gate's parents: weight `u` between native gate centers `lo`
/// and `hi`; `nearest` is the native gate whose cell contains the sub-gate
/// (the coverage authority).
struct GatePlan {
    lo: usize,
    hi: usize,
    u: f32,
    nearest: usize,
}

/// Upsample `field` (a field of `sweep`) for display. Returns `None` when
/// the field is already at/finer than the display targets (callers fall back
/// to the native path), when the sweep has too few rays, or when the field
/// has more rows than the sweep has rays.
pub fn upsample_field(sweep: &Sweep, field: &Field) -> Option<UpsampledSweep> {
    let (rows, gates) = field.shape();
    if rows < 2 || gates == 0 || rows > sweep.nrays() {
        return None;
    }
    let (first_center_m, spacing_m) = field.native_geometry(&sweep.range)?;
    let azimuths: Vec<f32> = sweep.rays.azimuth_deg[..rows]
        .iter()
        .map(|azimuth| azimuth.rem_euclid(360.0))
        .collect();
    // Scan-order azimuth steps (signed shortest; handles CW and CCW sweeps
    // and the wrap pair alike). The median is the nominal beam spacing —
    // robust to a sector scan's single large wrap gap.
    let deltas: Vec<f32> = (0..rows)
        .map(|row| signed_delta_deg(azimuths[row], azimuths[(row + 1) % rows]))
        .collect();
    let mut magnitudes: Vec<f32> = deltas
        .iter()
        .map(|delta| delta.abs())
        .filter(|delta| delta.is_finite())
        .collect();
    if magnitudes.is_empty() {
        return None;
    }
    magnitudes.sort_by(f32::total_cmp);
    let nominal_deg = magnitudes[magnitudes.len() / 2];
    let spacing_whole_m = if spacing_m.is_finite() && spacing_m.abs() < f64::from(i32::MAX) {
        spacing_m.round() as i32
    } else {
        0
    };
    let factors = upsample_factors(nominal_deg, spacing_whole_m, rows, gates);
    if factors.is_identity() {
        return None;
    }

    // Sub-rows only between beams whose gap is believably adjacent: wider
    // gaps (sector-scan edges, dropped radials) keep their native hole so
    // azimuth coverage cannot grow. Native bins fill at most
    // INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG to each side, hence the absolute cap.
    let gap_limit_deg = (nominal_deg * 2.0).min(INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG * 2.0);
    let mut row_plan = Vec::with_capacity(rows * factors.azimuth);
    for row in 0..rows {
        row_plan.push(RowPlan {
            lo: row,
            hi: row,
            t: 0.0,
            azimuth_deg: azimuths[row],
        });
        let delta = deltas[row];
        if factors.azimuth > 1 && delta.abs() >= MIN_SYNTH_DELTA_DEG && delta.abs() <= gap_limit_deg
        {
            let hi = (row + 1) % rows;
            for step in 1..factors.azimuth {
                let t = step as f32 / factors.azimuth as f32;
                row_plan.push(RowPlan {
                    lo: row,
                    hi,
                    t,
                    azimuth_deg: (azimuths[row] + t * delta).rem_euclid(360.0),
                });
            }
        }
    }

    // Cell-centered range subdivision: native gate g's cell splits into R
    // sub-cells whose centers interpolate between the surrounding native
    // gate CENTERS; ends clamp. The first sub-gate centre is
    // first + (sub - spacing)/2, which keeps the rendered annulus
    // [first - spacing/2, first + (count - 0.5)·spacing) EXACTLY.
    let range_factor = factors.range;
    let new_gates = gates * range_factor;
    let mut gate_plan = Vec::with_capacity(new_gates);
    for sub_gate in 0..new_gates {
        let x = (sub_gate as f32 + 0.5) / range_factor as f32 - 0.5;
        let nearest = sub_gate / range_factor;
        let (lo, hi, u) = if x <= 0.0 {
            (0, 0, 0.0)
        } else if x >= (gates - 1) as f32 {
            (gates - 1, gates - 1, 0.0)
        } else {
            let lo = x.floor() as usize;
            (lo, lo + 1, x - lo as f32)
        };
        gate_plan.push(GatePlan { lo, hi, u, nearest });
    }

    // Materialize physical values once (NaN for every sentinel), as in
    // smooth.rs.
    let mut source = vec![f32::NAN; rows * gates];
    source
        .par_chunks_mut(gates)
        .enumerate()
        .for_each(|(row, out_row)| {
            for (gate, cell) in out_row.iter_mut().enumerate() {
                if let Some(value) = field.value(row, gate).filter(|value| value.is_finite()) {
                    *cell = value;
                }
            }
        });

    let policy = interp_policy_for_quantity(field.quantity);
    let mut values = vec![f32::NAN; row_plan.len() * new_gates];
    values
        .par_chunks_mut(new_gates)
        .zip(row_plan.par_iter())
        .for_each(|(out_row, plan)| {
            let row_lo = &source[plan.lo * gates..(plan.lo + 1) * gates];
            let row_hi = &source[plan.hi * gates..(plan.hi + 1) * gates];
            let nearest_row = if plan.t <= 0.5 { row_lo } else { row_hi };
            // A sub-row exactly on the native beam boundary (t = 0.5)
            // belongs to NEITHER beam: painting it from one side would
            // push that side's echo half a sub-beam past the midpoint the
            // native display stops at. It renders only where both
            // bracketing beams have echo (strict no-growth; at worst the
            // boundary row goes empty at an echo's azimuth edge).
            let on_beam_boundary = plan.t == 0.5;
            for (cell, gate) in out_row.iter_mut().zip(&gate_plan) {
                let nearest = nearest_row[gate.nearest];
                if !nearest.is_finite() {
                    // The native cell containing this sub-cell is empty —
                    // it stays empty (coverage never grows).
                    continue;
                }
                if on_beam_boundary && !row_hi[gate.nearest].is_finite() {
                    continue;
                }
                let v00 = row_lo[gate.lo];
                let v01 = row_lo[gate.hi];
                let v10 = row_hi[gate.lo];
                let v11 = row_hi[gate.hi];
                if !(v00.is_finite() && v01.is_finite() && v10.is_finite() && v11.is_finite()) {
                    // An echo edge: no partial blends, the nearest parent's
                    // value carries through unchanged.
                    *cell = nearest;
                    continue;
                }
                let min = v00.min(v01).min(v10).min(v11);
                let max = v00.max(v01).max(v10).max(v11);
                let guarded = match policy {
                    InterpPolicy::CcGuard => min < CC_GUARD_FLOOR,
                    InterpPolicy::VelocityGuard => max - min > VELOCITY_GUARD_SPREAD_MPS,
                    InterpPolicy::LinearAngle => false,
                };
                *cell = if guarded {
                    nearest
                } else {
                    let lo = v00 + (v01 - v00) * gate.u;
                    let hi = v10 + (v11 - v10) * gate.u;
                    lo + (hi - lo) * plan.t
                };
            }
        });

    let sub_spacing_m = spacing_m / range_factor as f64;
    let new_nrays = u32::try_from(row_plan.len()).ok()?;
    let new_ngates = u32::try_from(new_gates).ok()?;

    // Each output ray links back to its nearest parent's ray so per-ray
    // lookups (Nyquist, beam azimuth basis) stay valid.
    let parent_rays: Vec<u32> = row_plan
        .iter()
        .map(|plan| (if plan.t <= 0.5 { plan.lo } else { plan.hi }) as u32)
        .collect();
    let mut out = Sweep::new(
        sweep.sweep_number,
        sweep.sweep_mode.clone(),
        sweep.fixed_angle_deg,
    );
    out.follow_mode = sweep.follow_mode.clone();
    out.prt_mode = sweep.prt_mode.clone();
    out.polarization_mode = sweep.polarization_mode.clone();
    out.target_scan_rate_deg_per_s = sweep.target_scan_rate_deg_per_s;
    out.elevation_number = sweep.elevation_number;
    out.complete = sweep.complete;
    out.reserve_rays(row_plan.len());
    for (plan, parent) in row_plan.iter().zip(&parent_rays) {
        let parent = *parent as usize;
        out.push_ray(
            sweep.rays.time_s.get(parent).copied().unwrap_or(f64::NAN),
            plan.azimuth_deg,
            sweep
                .rays
                .elevation_deg
                .get(parent)
                .copied()
                .unwrap_or(f32::NAN),
        );
    }
    out.ray_vars.nyquist_velocity_mps =
        sweep.ray_vars.nyquist_velocity_mps.as_ref().map(|nyquist| {
            parent_rays
                .iter()
                .map(|parent| nyquist.get(*parent as usize).copied().unwrap_or(f32::NAN))
                .collect()
        });
    out.range = RangeCoord::Uniform {
        first_center_m: first_center_m + (sub_spacing_m - spacing_m) / 2.0,
        spacing_m: sub_spacing_m,
        ngates: new_ngates,
    };
    let mut upsampled = physical_field_like(field, new_nrays, new_ngates, values);
    upsampled.gates = GateMapping::IDENTITY;
    out.fields.push(upsampled);
    Some(UpsampledSweep {
        sweep: out,
        parent_rays,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_policy_table() {
        // (nominal az, spacing) → (az factor, range factor)
        let cases = [
            ((1.0, 1000), (4, 4)),
            ((1.0, 500), (4, 2)),
            ((1.0, 250), (4, 1)),
            ((0.5, 250), (2, 1)),
            ((0.25, 250), (1, 1)),
            ((0.5, 1000), (2, 4)),
        ];
        for ((az, sp), (fa, fr)) in cases {
            let factors = upsample_factors(az, sp, 360, 1000);
            assert_eq!(
                (factors.azimuth, factors.range),
                (fa, fr),
                "policy for {az}° × {sp} m"
            );
        }
    }

    #[test]
    fn factor_policy_respects_exact_geometry_and_budget() {
        // 900 m gates: 4× would need a fractional half-shift (225 m sub,
        // 337.5 m shift) — steps down to 3× (300 m, exact).
        let factors = upsample_factors(1.0, 900, 360, 1000);
        assert_eq!(factors.range, 3);
        // Budget: 720 × 8000 cells at 2×2 would be 92 MB — range steps
        // down first.
        let factors = upsample_factors(0.5, 500, 720, 8000);
        assert_eq!((factors.azimuth, factors.range), (2, 1));
    }
}
