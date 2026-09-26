//! Rain attenuation correction by the Z-PHI method, as Py-ART's
//! `pyart.correct.calculate_attenuation_zphi` computes it (Testud et al.
//! 2000; the base-10 formulation of Gu et al. 2011).
//!
//! Per ray, below the freezing level: the specific attenuation
//!
//! ```text
//! A_h(r) = Z(r)^b (10^(0.1 b a dPhi) - 1) / (I(r0, rm) + (10^(0.1 b a dPhi) - 1) I(r, rm))
//! I(r, rm) = 0.46 b int_r^rm Z(s)^b ds
//! ```
//!
//! with `dPhi` the propagation phase at the end of the path, `Z` linear
//! reflectivity, `a` the phase-attenuation ratio and `b` the exponent of
//! `A_h = a' Z^b`. The path-integrated attenuation is twice the range
//! integral of `A_h`, the specific differential attenuation `c A_h^d`.
//!
//! Details follow Py-ART so its output is the reference:
//! - the phase is the propagation phase with the system phase removed, set
//!   to 0 where missing, negative or above the freezing level, and made
//!   monotonic by a running maximum;
//! - `Z` in the integral is the reflectivity after a first correction
//!   (`Z + a Phi`), averaged over `smooth_window_gates` gates. The average
//!   takes the gates with reflectivity only; Py-ART's rolling window drops
//!   the mask and also averages the underlying data of missing gates (-33
//!   dBZ for NEXRAD), which this port does not reproduce;
//! - `dPhi` is the median phase of the last six gates with reflectivity
//!   below the freezing level; rays with fewer than six such gates, or
//!   whose processing range is not longer than the smoothing window, get
//!   zero attenuation;
//! - integrals are cumulative trapezoids over the gate index times the gate
//!   spacing; `I` at the last processed gate is the trapezoid of the last
//!   two gates, and the path-integrated attenuation at gate `g` integrates
//!   to gate `g + 1`, as `scipy.integrate.cumulative_trapezoid` places them
//!   in Py-ART.

use crate::sweep::RadarBand;

/// Z-PHI settings (Py-ART `calculate_attenuation_zphi` arguments with
/// `temp_ref = "fixed_fzl"`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZPhiAttenuation {
    /// Two-way attenuation per degree of propagation phase, dB/deg (Py-ART
    /// `a_coef`).
    pub alpha_db_per_deg: f64,
    /// Exponent `b` of `A_h = a' Z^b` (Py-ART `beta`).
    pub beta: f64,
    /// Coefficient of `A_dp = c A_h^d` (Py-ART `c`).
    pub differential_coefficient: f64,
    /// Exponent of `A_dp = c A_h^d` (Py-ART `d`).
    pub differential_exponent: f64,
    /// Reflectivity smoothing window in gates (Py-ART `smooth_window_len`);
    /// 0 disables smoothing.
    pub smooth_window_gates: usize,
    /// Freezing level height above the radar antenna, m. Gates whose beam
    /// centre (4/3-Earth model at the sweep's fixed angle) reaches it end the
    /// processing range (Py-ART `fzl` minus the radar altitude).
    pub freezing_level_above_radar_m: f64,
    /// Gates at the end of every ray excluded from processing (Py-ART
    /// `doc`).
    pub excluded_end_gates: usize,
}

impl ZPhiAttenuation {
    /// Py-ART's band coefficients (`_param_attzphi_table`), a freezing level
    /// 4 km above the radar antenna and 15 excluded end gates. `None` for an
    /// unknown band.
    ///
    /// Py-ART's default without temperature information is `fzl = 4000` m
    /// and `doc = 15`, but its `fzl` is an altitude: `fzl_index` adds the
    /// radar altitude to the beam height before comparing. The two agree only
    /// for a radar at sea level; to reproduce Py-ART's default for a radar
    /// at altitude `h`, set `freezing_level_above_radar_m` to `4000 - h`.
    pub fn for_band(band: RadarBand) -> Option<Self> {
        let (alpha, differential_coefficient) = match band {
            RadarBand::S => (0.02, 0.15917),
            RadarBand::C => (0.08, 0.3),
            RadarBand::X => (0.31916, 0.15917),
            RadarBand::Unknown => return None,
        };
        Some(Self {
            alpha_db_per_deg: alpha,
            beta: 0.64884,
            differential_coefficient,
            differential_exponent: 1.0804,
            smooth_window_gates: 5,
            freezing_level_above_radar_m: 4000.0,
            excluded_end_gates: 15,
        })
    }
}

/// Z-PHI output on the reflectivity grid, row-major; NaN where the
/// reflectivity is missing.
pub(crate) struct ZPhiResult {
    pub(crate) specific_attenuation: Vec<f64>,
    pub(crate) path_integrated_attenuation: Vec<f64>,
    pub(crate) specific_differential_attenuation: Vec<f64>,
    pub(crate) path_integrated_differential_attenuation: Vec<f64>,
}

/// Index of the last gate whose beam centre is below the freezing level
/// (Py-ART `fzl_index`: 6 when every gate is above it), capped at
/// `gates - excluded_end_gates` (`det_process_range`).
pub(crate) fn processing_end_gate(
    first_m: f64,
    spacing_m: f64,
    gates: usize,
    elevation_deg: f64,
    config: &ZPhiAttenuation,
) -> usize {
    let effective_radius = 4.0 * 6371.0 * 1000.0 / 3.0;
    let sin_elevation = (elevation_deg * std::f64::consts::PI / 180.0).sin();
    let below = (0..gates).rev().find(|&gate| {
        let range = first_m + gate as f64 * spacing_m;
        let height = (range * range
            + effective_radius * effective_radius
            + 2.0 * range * effective_radius * sin_elevation)
            .sqrt()
            - effective_radius;
        height < config.freezing_level_above_radar_m
    });
    let end = below.unwrap_or(6);
    end.min(gates.saturating_sub(config.excluded_end_gates))
}

/// Z-PHI on one sweep. `reflectivity` and `phase` are row-major on the
/// same `rows x gates` grid (NaN = missing); `phase` is the propagation
/// phase with the system phase removed; `end_gate` from
/// [`processing_end_gate`]; `dr_km` the gate spacing.
pub(crate) fn zphi(
    reflectivity: &[f64],
    phase: &[f64],
    rows: usize,
    gates: usize,
    end_gate: usize,
    dr_km: f64,
    config: &ZPhiAttenuation,
) -> ZPhiResult {
    let total = rows * gates;
    let mut ah = vec![0.0f64; total];
    let mut pia = vec![0.0f64; total];
    let mut adiff = vec![0.0f64; total];
    let mut pida = vec![0.0f64; total];
    let beta = config.beta;
    let alpha = config.alpha_db_per_deg;

    for row in 0..rows {
        let z = &reflectivity[row * gates..(row + 1) * gates];
        // Prepared phase: masked (missing, above the freezing level or
        // negative) -> 0, then a running maximum along the ray.
        let mut corrected_phase = vec![0.0f64; gates];
        let mut running = f64::NEG_INFINITY;
        for gate in 0..gates {
            let value = phase[row * gates + gate];
            let usable = value.is_finite() && gate <= end_gate && value >= 0.0;
            let filled = if usable { value } else { 0.0 };
            running = running.max(filled);
            corrected_phase[gate] = running;
        }
        // First correction, then the smoothed linear reflectivity power.
        let initial: Vec<f64> = (0..gates)
            .map(|gate| {
                if z[gate].is_finite() {
                    z[gate] + corrected_phase[gate] * alpha
                } else {
                    f64::NAN
                }
            })
            .collect();
        let smoothed = smooth_masked_mean(&initial, config.smooth_window_gates);
        let linear: Vec<f64> = smoothed
            .iter()
            .map(|value| {
                if value.is_finite() {
                    10.0f64.powf(0.1 * beta * value)
                } else {
                    0.0
                }
            })
            .collect();

        if end_gate > config.smooth_window_gates {
            let valid: Vec<usize> = (0..end_gate).filter(|&gate| z[gate].is_finite()).collect();
            if valid.len() >= 6 {
                let last_six: Vec<f64> = valid[valid.len() - 6..]
                    .iter()
                    .map(|&gate| corrected_phase[gate])
                    .collect();
                let phidp_max = median(&last_six);
                let self_consistency = 10.0f64.powf(0.1 * beta * alpha * phidp_max) - 1.0;
                // I_indef: cumulative trapezoid of the reversed profile, its
                // last value repeated, reversed back.
                let profile: Vec<f64> = linear[..end_gate]
                    .iter()
                    .rev()
                    .map(|value| 0.46 * beta * dr_km * value)
                    .collect();
                let mut integral = cumulative_trapezoid(&profile);
                if let Some(&last) = integral.last() {
                    integral.push(last);
                }
                integral.reverse();
                if integral.len() == end_gate {
                    let total_integral = integral[0];
                    for gate in 0..end_gate {
                        ah[row * gates + gate] = linear[gate] * self_consistency
                            / (total_integral + self_consistency * integral[gate]);
                    }
                }
                let row_ah = &ah[row * gates..(row + 1) * gates];
                let row_adiff: Vec<f64> = row_ah
                    .iter()
                    .enumerate()
                    .map(|(gate, value)| {
                        if gate < end_gate {
                            config.differential_coefficient
                                * value.powf(config.differential_exponent)
                        } else {
                            0.0
                        }
                    })
                    .collect();
                integrate_path(row_ah, dr_km, &mut pia[row * gates..(row + 1) * gates]);
                integrate_path(&row_adiff, dr_km, &mut pida[row * gates..(row + 1) * gates]);
                adiff[row * gates..(row + 1) * gates].copy_from_slice(&row_adiff);
            }
        }
    }

    // Output masked where the reflectivity is missing.
    for index in 0..total {
        if !reflectivity[index].is_finite() {
            ah[index] = f64::NAN;
            pia[index] = f64::NAN;
            adiff[index] = f64::NAN;
            pida[index] = f64::NAN;
        }
    }
    ZPhiResult {
        specific_attenuation: ah,
        path_integrated_attenuation: pia,
        specific_differential_attenuation: adiff,
        path_integrated_differential_attenuation: pida,
    }
}

/// `pia[:-1] = cumulative_trapezoid(a) * dr * 2; pia[-1] = pia[-2]`.
fn integrate_path(specific: &[f64], dr_km: f64, out: &mut [f64]) {
    let integral = cumulative_trapezoid(specific);
    for (slot, value) in out.iter_mut().zip(&integral) {
        *slot = value * dr_km * 2.0;
    }
    let n = out.len();
    if n >= 2 {
        out[n - 1] = out[n - 2];
    }
}

/// `scipy.integrate.cumulative_trapezoid(y)` with unit spacing: `n - 1`
/// running sums of `(y[i] + y[i + 1]) / 2`.
fn cumulative_trapezoid(values: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(values.len().saturating_sub(1));
    let mut sum = 0.0;
    for pair in values.windows(2) {
        sum += (pair[0] + pair[1]) / 2.0;
        out.push(sum);
    }
    out
}

/// Py-ART `smooth_masked(wind_type="mean", min_valid=1)`: the mean of the
/// finite values in a centred window (odd length; an even length grows by
/// one), at gates that are finite and at least half a window from both
/// ends; NaN elsewhere. A zero window returns the input.
fn smooth_masked_mean(values: &[f64], window: usize) -> Vec<f64> {
    if window == 0 {
        return values.to_vec();
    }
    let window = if window.is_multiple_of(2) {
        window + 1
    } else {
        window
    };
    let half = (window - 1) / 2;
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if n < window {
        return out;
    }
    for gate in half..n - half {
        if !values[gate].is_finite() {
            continue;
        }
        let samples = &values[gate - half..=gate + half];
        let (sum, count) = samples
            .iter()
            .filter(|value| value.is_finite())
            .fold((0.0, 0usize), |(sum, count), value| {
                (sum + value, count + 1)
            });
        if count >= 1 {
            out[gate] = sum / count as f64;
        }
    }
    out
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        0.5 * (sorted[middle - 1] + sorted[middle])
    } else {
        sorted[middle]
    }
}
