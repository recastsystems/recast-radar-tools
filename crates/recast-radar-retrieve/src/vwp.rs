//! Vertical wind profile retrieval from a volume of dealiased radial velocity.
//!
//! This is a classical first-harmonic velocity-azimuth display (VAD) fit
//! (Browning & Wexler 1968), applied independently at regularly spaced beam
//! heights.  For one elevation/range annulus the fitted model is
//!
//! `Vr = bias + u * sin(az) * cos(el) + v * cos(az) * cos(el)`
//!
//! where positive radial velocity is away from the radar, `u` is eastward,
//! and `v` is northward.  The intercept absorbs a constant radial bias and the
//! near-constant vertical-wind projection on one PPI tilt; this retrieval does
//! not claim to measure vertical velocity or divergence.
//!
//! The caller owns velocity dealiasing.  Passing the exact fields used by the
//! display keeps the VWP consistent with the selected dealias engine and avoids
//! a hidden second unfolding pass.  A low residual is not proof that the
//! absolute Nyquist branch is correct, so the result also reports how much of
//! the contributing geometry carried a Nyquist value.

use chrono::{DateTime, Utc};
use recast_radar_core::{Field, Quantity, Sweep, SweepMode, Volume, beam_height_above_radar_m};
use thiserror::Error;

const AZIMUTH_SECTORS: usize = 12;
const MIN_SAMPLES: usize = 60;
const MIN_SECTORS: usize = 8;
const GOOD_MIN_SECTORS: usize = 10;
const MAX_AZIMUTH_GAP_DEG: f32 = 120.0;
const GOOD_MAX_AZIMUTH_GAP_DEG: f32 = 60.0;
const GOOD_MAX_RMS_MPS: f32 = 3.1; // approximately 6 kt
const MAX_RMS_MPS: f32 = 5.2; // approximately 10 kt
const GOOD_MAX_OUTLIER_FRACTION: f32 = 0.30;
const MAX_OUTLIER_FRACTION: f32 = 0.55;
const GOOD_MAX_VECTOR_STD_ERROR_MPS: f32 = 1.5;
const TRIM_SIGMA_MULTIPLIER: f32 = 3.0;
const TRIM_MIN_MPS: f32 = 3.0;
const TRIM_MAX_MPS: f32 = 12.0;
const NORMAL_PIVOT_EPSILON: f64 = 1.0e-10;
const MAX_PROFILE_LEVELS: usize = 256;

/// Geometry controls for [`compute_vwp`].  Science/QC thresholds are fixed so
/// two users cannot silently assign different trust labels to the same data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VwpConfig {
    /// First requested beam-center height above the radar.
    pub min_height_m_agl: f32,
    /// Last requested beam-center height above the radar (inclusive).
    pub max_height_m_agl: f32,
    /// Vertical spacing of requested levels.
    pub height_step_m: f32,
    /// Nearest range allowed to contribute, avoiding the worst near-field
    /// clutter and ill-defined very short rings.
    pub min_slant_range_m: f32,
    /// Farthest range allowed to contribute.
    pub max_slant_range_m: f32,
    /// Half-width of the range annulus.  Values within the annulus are reduced
    /// to one median per radial before fitting, so radials with more valid
    /// gates do not receive more weight.
    pub annulus_half_width_m: f32,
    /// Maximum difference between a requested height and the nearest gate's
    /// 4/3-Earth beam-center height.
    pub max_height_mismatch_m: f32,
}

impl Default for VwpConfig {
    fn default() -> Self {
        Self {
            min_height_m_agl: 250.0,
            max_height_m_agl: 12_000.0,
            height_step_m: 250.0,
            min_slant_range_m: 5_000.0,
            max_slant_range_m: 150_000.0,
            annulus_half_width_m: 2_000.0,
            max_height_mismatch_m: 250.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VwpQuality {
    Good,
    Marginal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VwpRejectionReason {
    /// No supplied velocity tilt has a gate near this requested height inside
    /// the configured range limits.
    NoBeamCoverage,
    InsufficientSamples,
    InsufficientAzimuthCoverage,
    IllConditionedFit,
    ExcessiveOutliers,
    ResidualTooLarge,
}

/// Fit/coverage diagnostics shared by accepted and rejected candidates.
#[derive(Clone, Debug, PartialEq)]
pub struct VwpCandidateDiagnostics {
    pub sweep_index: usize,
    pub height_m_agl: f32,
    pub slant_range_m: f32,
    pub elevation_deg: f32,
    pub samples_total: usize,
    pub samples_used: usize,
    pub azimuth_sectors: usize,
    pub max_azimuth_gap_deg: f32,
    pub outlier_fraction: f32,
    pub rms_mps: Option<f32>,
    /// Fraction of retained azimuth samples whose source radial declared a
    /// finite, positive Nyquist velocity.  Missing Nyquist is disclosed rather
    /// than rejected because several international feeds provide already-
    /// unfolded velocity without it.
    pub nyquist_sample_fraction: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VwpWindLevel {
    pub height_m_agl: f32,
    pub height_m_msl: Option<f32>,
    pub u_mps: f32,
    pub v_mps: f32,
    /// Meteorological direction the wind is coming from, clockwise from north.
    pub direction_deg: f32,
    pub speed_mps: f32,
    /// Zeroth-harmonic/intercept term.  It is diagnostic only, not a vertical
    /// velocity retrieval.
    pub radial_bias_mps: f32,
    pub vector_std_error_mps: f32,
    pub quality: VwpQuality,
    pub diagnostics: VwpCandidateDiagnostics,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VwpRejectedLevel {
    pub reason: VwpRejectionReason,
    /// The candidate that progressed furthest through QC.  `None` means no
    /// tilt/range geometry reached this height at all.
    pub best_candidate: Option<VwpCandidateDiagnostics>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VwpLevelOutcome {
    Retrieved(VwpWindLevel),
    Rejected(VwpRejectedLevel),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VwpLevel {
    pub target_height_m_agl: f32,
    pub outcome: VwpLevelOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VwpProfile {
    pub site_id: String,
    pub valid_time: DateTime<Utc>,
    pub radar_elevation_m: Option<f32>,
    pub velocity_sweep_count: usize,
    /// One entry for every requested height, including explicit rejections so
    /// gaps in the plotted profile never look like an application failure.
    pub levels: Vec<VwpLevel>,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum VwpError {
    #[error(
        "VWP needs one optional dealiased field per volume sweep (got {actual}, expected {expected})"
    )]
    GridCountMismatch { expected: usize, actual: usize },
    #[error("VWP is defined for PPI volume scans, not {0:?}")]
    UnsupportedScanMode(SweepMode),
    #[error("VWP configuration is invalid: {0}")]
    InvalidConfig(&'static str),
    #[error("volume has no caller-supplied dealiased velocity fields")]
    NoVelocityGrids,
}

/// `true` for a raw or dealiased radial-velocity field.
fn is_velocity(field: &Field) -> bool {
    matches!(
        field.quantity,
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity
    )
}

/// Compute a wind profile from caller-dealiased velocity fields.
///
/// `dealiased_velocity` is aligned one-for-one with `volume.sweeps`; use
/// `None` for sweeps without velocity.  Fields are not required to be stored
/// in `volume`, but each must be a field of the corresponding sweep (on its
/// rays and range). Every sweep must be a PPI: an azimuth surveillance or a
/// sector sweep (a sector's partial azimuth coverage is rejected level by
/// level, [`VwpRejectionReason::InsufficientAzimuthCoverage`]).
pub fn compute_vwp(
    volume: &Volume,
    dealiased_velocity: &[Option<&Field>],
    config: VwpConfig,
) -> Result<VwpProfile, VwpError> {
    validate_config(config)?;
    if dealiased_velocity.len() != volume.sweeps.len() {
        return Err(VwpError::GridCountMismatch {
            expected: volume.sweeps.len(),
            actual: dealiased_velocity.len(),
        });
    }
    if let Some(mode) = volume
        .sweeps
        .iter()
        .map(|sweep| &sweep.sweep_mode)
        .find(|mode| !matches!(mode, SweepMode::AzimuthSurveillance | SweepMode::Sector))
    {
        return Err(VwpError::UnsupportedScanMode(mode.clone()));
    }

    let velocity_sweep_count = dealiased_velocity
        .iter()
        .filter(|field| {
            field.is_some_and(|field| is_velocity(field) && field.nrays > 0 && field.ngates > 0)
        })
        .count();
    if velocity_sweep_count == 0 {
        return Err(VwpError::NoVelocityGrids);
    }
    let radar_elevation_m = volume.location.altitude_m.map(|altitude| altitude as f32);

    let mut levels = Vec::new();
    let level_count = ((config.max_height_m_agl - config.min_height_m_agl) / config.height_step_m)
        .floor() as usize
        + 1;
    levels.reserve(level_count);
    for level_index in 0..level_count {
        let target_height = config.min_height_m_agl + level_index as f32 * config.height_step_m;
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for (sweep_index, (sweep, field)) in volume
            .sweeps
            .iter()
            .zip(dealiased_velocity.iter())
            .enumerate()
        {
            let Some(field) = *field else {
                continue;
            };
            if !is_velocity(field) {
                continue;
            }
            let Some(candidate) = candidate_for_height(
                sweep_index,
                sweep,
                field,
                target_height,
                radar_elevation_m,
                config,
            ) else {
                continue;
            };
            match candidate {
                CandidateOutcome::Accepted(wind) => accepted.push(wind),
                CandidateOutcome::Rejected(rejection) => rejected.push(rejection),
            }
        }

        let outcome = if let Some(best) = accepted
            .into_iter()
            .min_by(|left, right| compare_wind_candidates(left, right, target_height))
        {
            VwpLevelOutcome::Retrieved(best)
        } else if let Some(best) = rejected.into_iter().max_by(compare_rejected_candidates) {
            VwpLevelOutcome::Rejected(VwpRejectedLevel {
                reason: best.reason,
                best_candidate: Some(best.diagnostics),
            })
        } else {
            VwpLevelOutcome::Rejected(VwpRejectedLevel {
                reason: VwpRejectionReason::NoBeamCoverage,
                best_candidate: None,
            })
        };
        levels.push(VwpLevel {
            target_height_m_agl: target_height,
            outcome,
        });
    }

    Ok(VwpProfile {
        site_id: volume.attrs.instrument_name.clone(),
        valid_time: volume
            .time_coverage
            .map_or(volume.time_reference, |coverage| coverage.start),
        radar_elevation_m: radar_elevation_m.filter(|value| value.is_finite()),
        velocity_sweep_count,
        levels,
    })
}

fn validate_config(config: VwpConfig) -> Result<(), VwpError> {
    let finite = [
        config.min_height_m_agl,
        config.max_height_m_agl,
        config.height_step_m,
        config.min_slant_range_m,
        config.max_slant_range_m,
        config.annulus_half_width_m,
        config.max_height_mismatch_m,
    ]
    .into_iter()
    .all(f32::is_finite);
    if !finite {
        return Err(VwpError::InvalidConfig("all values must be finite"));
    }
    if config.min_height_m_agl < 0.0 || config.max_height_m_agl < config.min_height_m_agl {
        return Err(VwpError::InvalidConfig(
            "height range is reversed or below zero",
        ));
    }
    if config.height_step_m <= 0.0 {
        return Err(VwpError::InvalidConfig("height step must be positive"));
    }
    let requested_levels =
        ((config.max_height_m_agl - config.min_height_m_agl) / config.height_step_m).floor() + 1.0;
    if !requested_levels.is_finite() || requested_levels > MAX_PROFILE_LEVELS as f32 {
        return Err(VwpError::InvalidConfig("too many requested height levels"));
    }
    if config.min_slant_range_m < 0.0 || config.max_slant_range_m <= config.min_slant_range_m {
        return Err(VwpError::InvalidConfig("slant-range limits are invalid"));
    }
    if config.annulus_half_width_m <= 0.0 || config.max_height_mismatch_m <= 0.0 {
        return Err(VwpError::InvalidConfig(
            "annulus width and height mismatch must be positive",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct VadSample {
    azimuth_deg: f32,
    elevation_deg: f32,
    velocity_mps: f32,
    has_nyquist: bool,
}

struct RejectedCandidate {
    reason: VwpRejectionReason,
    diagnostics: VwpCandidateDiagnostics,
    /// Later stages sort above earlier ones when every candidate failed.
    stage: u8,
}

enum CandidateOutcome {
    Accepted(VwpWindLevel),
    Rejected(RejectedCandidate),
}

fn candidate_for_height(
    sweep_index: usize,
    sweep: &Sweep,
    field: &Field,
    target_height_m_agl: f32,
    radar_elevation_m: Option<f32>,
    config: VwpConfig,
) -> Option<CandidateOutcome> {
    let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range)?;
    if field.nrays == 0 || field.ngates == 0 || spacing_m <= 0.0 {
        return None;
    }
    let geometry = FieldGeometry {
        first_gate_m: first_gate_m.max(0.0) as f32,
        spacing_m: spacing_m as f32,
        gates: field.ngates as usize,
    };
    let elevation_deg = representative_elevation(sweep, field)?;
    if !(-1.0..89.0).contains(&elevation_deg) {
        return None;
    }

    let mut center_gate = None;
    let mut best_height_error = f32::INFINITY;
    let mut center_height = f32::NAN;
    let mut center_range = f32::NAN;
    for gate in 0..geometry.gates {
        let range_m = geometry.range_m(gate);
        if !(config.min_slant_range_m..=config.max_slant_range_m).contains(&range_m) {
            continue;
        }
        let height_m = beam_height_above_radar_m(range_m as f64, elevation_deg as f64) as f32;
        let error = (height_m - target_height_m_agl).abs();
        if error < best_height_error {
            best_height_error = error;
            center_gate = Some(gate);
            center_height = height_m;
            center_range = range_m;
        }
    }
    let center_gate = center_gate?;
    if best_height_error > config.max_height_mismatch_m {
        return None;
    }

    let spacing_m = geometry.spacing_m;
    let gate_radius = (config.annulus_half_width_m / spacing_m).round() as usize;
    let gate_start = center_gate.saturating_sub(gate_radius);
    let gate_end = center_gate
        .saturating_add(gate_radius)
        .min(geometry.gates - 1);
    let samples = annulus_samples(sweep, field, gate_start, gate_end);
    let raw_coverage = coverage(&samples);
    let base_diagnostics = VwpCandidateDiagnostics {
        sweep_index,
        height_m_agl: center_height,
        slant_range_m: center_range,
        elevation_deg,
        samples_total: samples.len(),
        samples_used: samples.len(),
        azimuth_sectors: raw_coverage.sectors,
        max_azimuth_gap_deg: raw_coverage.max_gap_deg,
        outlier_fraction: 0.0,
        rms_mps: None,
        nyquist_sample_fraction: nyquist_fraction(&samples),
    };
    if samples.len() < MIN_SAMPLES {
        return Some(reject(
            VwpRejectionReason::InsufficientSamples,
            base_diagnostics,
            1,
        ));
    }
    if raw_coverage.sectors < MIN_SECTORS || raw_coverage.max_gap_deg > MAX_AZIMUTH_GAP_DEG {
        return Some(reject(
            VwpRejectionReason::InsufficientAzimuthCoverage,
            base_diagnostics,
            2,
        ));
    }

    let Some(initial) = fit_samples(&samples) else {
        return Some(reject(
            VwpRejectionReason::IllConditionedFit,
            base_diagnostics,
            3,
        ));
    };
    let residuals: Vec<f32> = samples
        .iter()
        .map(|sample| sample.velocity_mps - initial.predict(*sample))
        .collect();
    let residual_center = median(residuals.clone()).unwrap_or(0.0);
    let deviations: Vec<f32> = residuals
        .iter()
        .map(|residual| (residual - residual_center).abs())
        .collect();
    let robust_sigma = median(deviations).unwrap_or(0.0) * 1.482_6;
    let trim_limit = (TRIM_SIGMA_MULTIPLIER * robust_sigma).clamp(TRIM_MIN_MPS, TRIM_MAX_MPS);
    let retained: Vec<VadSample> = samples
        .iter()
        .zip(residuals.iter())
        .filter_map(|(sample, residual)| {
            ((residual - residual_center).abs() <= trim_limit).then_some(*sample)
        })
        .collect();
    let outlier_fraction = 1.0 - retained.len() as f32 / samples.len() as f32;
    let retained_coverage = coverage(&retained);
    let mut diagnostics = VwpCandidateDiagnostics {
        samples_used: retained.len(),
        azimuth_sectors: retained_coverage.sectors,
        max_azimuth_gap_deg: retained_coverage.max_gap_deg,
        outlier_fraction,
        nyquist_sample_fraction: nyquist_fraction(&retained),
        ..base_diagnostics
    };
    if retained.len() < MIN_SAMPLES {
        return Some(reject(
            VwpRejectionReason::InsufficientSamples,
            diagnostics,
            4,
        ));
    }
    if retained_coverage.sectors < MIN_SECTORS
        || retained_coverage.max_gap_deg > MAX_AZIMUTH_GAP_DEG
    {
        return Some(reject(
            VwpRejectionReason::InsufficientAzimuthCoverage,
            diagnostics,
            5,
        ));
    }
    if outlier_fraction > MAX_OUTLIER_FRACTION {
        return Some(reject(
            VwpRejectionReason::ExcessiveOutliers,
            diagnostics,
            6,
        ));
    }

    let Some(fit) = fit_samples(&retained) else {
        return Some(reject(
            VwpRejectionReason::IllConditionedFit,
            diagnostics,
            7,
        ));
    };
    diagnostics.rms_mps = Some(fit.rms_mps);
    if fit.rms_mps > MAX_RMS_MPS {
        return Some(reject(VwpRejectionReason::ResidualTooLarge, diagnostics, 8));
    }

    let speed_mps = fit.u_mps.hypot(fit.v_mps);
    let direction_deg = (-fit.u_mps)
        .atan2(-fit.v_mps)
        .to_degrees()
        .rem_euclid(360.0);
    let quality = if retained_coverage.sectors >= GOOD_MIN_SECTORS
        && retained_coverage.max_gap_deg <= GOOD_MAX_AZIMUTH_GAP_DEG
        && fit.rms_mps <= GOOD_MAX_RMS_MPS
        && outlier_fraction <= GOOD_MAX_OUTLIER_FRACTION
        && fit.vector_std_error_mps <= GOOD_MAX_VECTOR_STD_ERROR_MPS
    {
        VwpQuality::Good
    } else {
        VwpQuality::Marginal
    };
    let height_m_msl = radar_elevation_m
        .filter(|height| height.is_finite())
        .map(|radar_height| radar_height + center_height);
    Some(CandidateOutcome::Accepted(VwpWindLevel {
        height_m_agl: center_height,
        height_m_msl,
        u_mps: fit.u_mps,
        v_mps: fit.v_mps,
        direction_deg,
        speed_mps,
        radial_bias_mps: fit.bias_mps,
        vector_std_error_mps: fit.vector_std_error_mps,
        quality,
        diagnostics,
    }))
}

fn reject(
    reason: VwpRejectionReason,
    diagnostics: VwpCandidateDiagnostics,
    stage: u8,
) -> CandidateOutcome {
    CandidateOutcome::Rejected(RejectedCandidate {
        reason,
        diagnostics,
        stage,
    })
}

/// A field's native gate geometry in the units the fit uses.
struct FieldGeometry {
    /// Centre of gate 0, clamped to the radar (a negative first gate has no
    /// beam height).
    first_gate_m: f32,
    spacing_m: f32,
    gates: usize,
}

impl FieldGeometry {
    fn range_m(&self, gate: usize) -> f32 {
        self.first_gate_m + gate as f32 * self.spacing_m
    }
}

fn representative_elevation(sweep: &Sweep, field: &Field) -> Option<f32> {
    let elevations: Vec<f32> = sweep
        .rays
        .elevation_deg
        .iter()
        .take(field.nrays as usize)
        .copied()
        .filter(|elevation| elevation.is_finite())
        .collect();
    median(elevations).or_else(|| {
        sweep
            .fixed_angle_deg
            .is_finite()
            .then_some(sweep.fixed_angle_deg)
    })
}

/// Reduce an annulus to at most one observation per integer azimuth degree.
/// This prevents 0.5-degree NEXRAD and irregular sector scans from silently
/// giving dense azimuths more leverage than sparse ones.
fn annulus_samples(
    sweep: &Sweep,
    field: &Field,
    gate_start: usize,
    gate_end: usize,
) -> Vec<VadSample> {
    let mut bins: [Vec<VadSample>; 360] = std::array::from_fn(|_| Vec::new());
    let nyquist = sweep.ray_vars.nyquist_velocity_mps.as_deref();
    for row in 0..field.nrays as usize {
        let (Some(&azimuth), Some(&elevation)) = (
            sweep.rays.azimuth_deg.get(row),
            sweep.rays.elevation_deg.get(row),
        ) else {
            continue;
        };
        if !azimuth.is_finite() || !elevation.is_finite() {
            continue;
        }
        let values: Vec<f32> = (gate_start..=gate_end)
            .filter_map(|gate| field.value(row, gate))
            .filter(|value| value.is_finite())
            .collect();
        let Some(velocity_mps) = median(values) else {
            continue;
        };
        let azimuth_deg = azimuth.rem_euclid(360.0);
        let bin = (azimuth_deg.floor() as usize).min(359);
        bins[bin].push(VadSample {
            azimuth_deg,
            elevation_deg: elevation,
            velocity_mps,
            has_nyquist: nyquist
                .and_then(|nyquist| nyquist.get(row))
                .is_some_and(|value| value.is_finite() && *value > 0.0),
        });
    }

    bins.into_iter()
        .filter_map(|bin| {
            if bin.is_empty() {
                return None;
            }
            let azimuth_deg = circular_mean_degrees(bin.iter().map(|sample| sample.azimuth_deg))?;
            let elevation_deg = median(bin.iter().map(|sample| sample.elevation_deg).collect())?;
            let velocity_mps = median(bin.iter().map(|sample| sample.velocity_mps).collect())?;
            let nyquist_count = bin.iter().filter(|sample| sample.has_nyquist).count();
            Some(VadSample {
                azimuth_deg,
                elevation_deg,
                velocity_mps,
                has_nyquist: nyquist_count * 2 >= bin.len(),
            })
        })
        .collect()
}

fn circular_mean_degrees(values: impl Iterator<Item = f32>) -> Option<f32> {
    let mut sin_sum = 0.0f64;
    let mut cos_sum = 0.0f64;
    let mut count = 0usize;
    for value in values {
        if !value.is_finite() {
            continue;
        }
        let radians = (value as f64).to_radians();
        sin_sum += radians.sin();
        cos_sum += radians.cos();
        count += 1;
    }
    (count > 0 && (sin_sum != 0.0 || cos_sum != 0.0))
        .then(|| sin_sum.atan2(cos_sum).to_degrees().rem_euclid(360.0) as f32)
}

#[derive(Clone, Copy)]
struct Coverage {
    sectors: usize,
    max_gap_deg: f32,
}

fn coverage(samples: &[VadSample]) -> Coverage {
    if samples.is_empty() {
        return Coverage {
            sectors: 0,
            max_gap_deg: 360.0,
        };
    }
    let mut sector_mask = 0u16;
    let mut azimuths: Vec<f32> = samples
        .iter()
        .map(|sample| sample.azimuth_deg.rem_euclid(360.0))
        .collect();
    for &azimuth in &azimuths {
        let sector = ((azimuth / (360.0 / AZIMUTH_SECTORS as f32)).floor() as usize)
            .min(AZIMUTH_SECTORS - 1);
        sector_mask |= 1u16 << sector;
    }
    azimuths.sort_by(f32::total_cmp);
    let mut max_gap = 0.0f32;
    for pair in azimuths.windows(2) {
        max_gap = max_gap.max(pair[1] - pair[0]);
    }
    if let (Some(first), Some(last)) = (azimuths.first(), azimuths.last()) {
        max_gap = max_gap.max(first + 360.0 - last);
    }
    Coverage {
        sectors: sector_mask.count_ones() as usize,
        max_gap_deg: max_gap,
    }
}

fn nyquist_fraction(samples: &[VadSample]) -> f32 {
    if samples.is_empty() {
        0.0
    } else {
        samples.iter().filter(|sample| sample.has_nyquist).count() as f32 / samples.len() as f32
    }
}

#[derive(Clone, Copy)]
struct VadFit {
    bias_mps: f32,
    u_mps: f32,
    v_mps: f32,
    rms_mps: f32,
    vector_std_error_mps: f32,
}

impl VadFit {
    fn predict(self, sample: VadSample) -> f32 {
        let azimuth = sample.azimuth_deg.to_radians();
        let cos_elevation = sample.elevation_deg.to_radians().cos();
        self.bias_mps
            + self.u_mps * azimuth.sin() * cos_elevation
            + self.v_mps * azimuth.cos() * cos_elevation
    }
}

fn fit_samples(samples: &[VadSample]) -> Option<VadFit> {
    if samples.len() < 3 {
        return None;
    }
    let mut normal = [[0.0f64; 3]; 3];
    let mut rhs = [0.0f64; 3];
    for sample in samples {
        let azimuth = (sample.azimuth_deg as f64).to_radians();
        let cos_elevation = (sample.elevation_deg as f64).to_radians().cos();
        let x = [
            1.0,
            azimuth.sin() * cos_elevation,
            azimuth.cos() * cos_elevation,
        ];
        let y = sample.velocity_mps as f64;
        for row in 0..3 {
            rhs[row] += x[row] * y;
            for column in 0..3 {
                normal[row][column] += x[row] * x[column];
            }
        }
    }
    let inverse = invert_3x3(normal)?;
    let coefficients = multiply_matrix_vector(inverse, rhs);
    if !coefficients.into_iter().all(f64::is_finite) {
        return None;
    }
    let [bias, u, v] = coefficients;
    let mut sse = 0.0f64;
    for sample in samples {
        let azimuth = (sample.azimuth_deg as f64).to_radians();
        let cos_elevation = (sample.elevation_deg as f64).to_radians().cos();
        let prediction =
            bias + u * azimuth.sin() * cos_elevation + v * azimuth.cos() * cos_elevation;
        let residual = sample.velocity_mps as f64 - prediction;
        sse += residual * residual;
    }
    let rms = (sse / samples.len() as f64).sqrt();
    let degrees_of_freedom = samples.len().saturating_sub(3).max(1) as f64;
    let residual_variance = sse / degrees_of_freedom;
    let u_variance = (inverse[1][1] * residual_variance).max(0.0);
    let v_variance = (inverse[2][2] * residual_variance).max(0.0);
    let vector_std_error = (u_variance + v_variance).sqrt();
    if !rms.is_finite() || !vector_std_error.is_finite() {
        return None;
    }
    Some(VadFit {
        bias_mps: bias as f32,
        u_mps: u as f32,
        v_mps: v as f32,
        rms_mps: rms as f32,
        vector_std_error_mps: vector_std_error as f32,
    })
}

fn invert_3x3(matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let mut augmented = [[0.0f64; 6]; 3];
    for row in 0..3 {
        augmented[row][..3].copy_from_slice(&matrix[row]);
        augmented[row][row + 3] = 1.0;
    }
    for pivot_column in 0..3 {
        let pivot_row = (pivot_column..3).max_by(|&left, &right| {
            augmented[left][pivot_column]
                .abs()
                .total_cmp(&augmented[right][pivot_column].abs())
        })?;
        let pivot = augmented[pivot_row][pivot_column];
        if !pivot.is_finite() || pivot.abs() <= NORMAL_PIVOT_EPSILON {
            return None;
        }
        if pivot_row != pivot_column {
            augmented.swap(pivot_row, pivot_column);
        }
        let divisor = augmented[pivot_column][pivot_column];
        for value in &mut augmented[pivot_column] {
            *value /= divisor;
        }
        let pivot_values = augmented[pivot_column];
        for (row, values) in augmented.iter_mut().enumerate() {
            if row == pivot_column {
                continue;
            }
            let factor = values[pivot_column];
            for (value, pivot_value) in values.iter_mut().zip(pivot_values) {
                *value -= factor * pivot_value;
            }
        }
    }
    let mut inverse = [[0.0f64; 3]; 3];
    for row in 0..3 {
        inverse[row].copy_from_slice(&augmented[row][3..]);
    }
    Some(inverse)
}

fn multiply_matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|row| {
        matrix[row]
            .iter()
            .zip(vector.iter())
            .map(|(coefficient, value)| coefficient * value)
            .sum()
    })
}

fn median(mut values: Vec<f32>) -> Option<f32> {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) * 0.5)
    } else {
        Some(values[middle])
    }
}

fn compare_wind_candidates(
    left: &VwpWindLevel,
    right: &VwpWindLevel,
    target_height_m_agl: f32,
) -> std::cmp::Ordering {
    quality_rank(left.quality)
        .cmp(&quality_rank(right.quality))
        .then_with(|| {
            left.diagnostics
                .rms_mps
                .unwrap_or(f32::INFINITY)
                .total_cmp(&right.diagnostics.rms_mps.unwrap_or(f32::INFINITY))
        })
        .then_with(|| {
            left.vector_std_error_mps
                .total_cmp(&right.vector_std_error_mps)
        })
        .then_with(|| {
            right
                .diagnostics
                .azimuth_sectors
                .cmp(&left.diagnostics.azimuth_sectors)
        })
        .then_with(|| {
            (left.height_m_agl - target_height_m_agl)
                .abs()
                .total_cmp(&(right.height_m_agl - target_height_m_agl).abs())
        })
        .then_with(|| {
            left.diagnostics
                .slant_range_m
                .total_cmp(&right.diagnostics.slant_range_m)
        })
}

fn quality_rank(quality: VwpQuality) -> u8 {
    match quality {
        VwpQuality::Good => 0,
        VwpQuality::Marginal => 1,
    }
}

fn compare_rejected_candidates(
    left: &RejectedCandidate,
    right: &RejectedCandidate,
) -> std::cmp::Ordering {
    left.stage
        .cmp(&right.stage)
        .then_with(|| {
            left.diagnostics
                .samples_used
                .cmp(&right.diagnostics.samples_used)
        })
        .then_with(|| {
            left.diagnostics
                .azimuth_sectors
                .cmp(&right.diagnostics.azimuth_sectors)
        })
        .then_with(|| {
            right
                .diagnostics
                .rms_mps
                .unwrap_or(f32::INFINITY)
                .total_cmp(&left.diagnostics.rms_mps.unwrap_or(f32::INFINITY))
        })
}
