//! Assembly of one scan split across several files (ODIM product-per-file
//! feeds, JMA member tars) into one [`Volume`].

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::sweep::{PrtSequence, RayVariables, Sweep};
use super::volume::{SourceFormat, TimeCoverage, Volume};

/// Tolerance used to treat two fixed angles or two ray azimuths as the same.
pub const ANGLE_MATCH_TOLERANCE_DEG: f32 = 0.05;

/// Counters describing what [`merge_volumes`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MergeReport {
    /// Fields moved from a later part into an angle-matched sweep.
    pub merged_fields: usize,
    /// Later-part sweeps that matched a fixed angle but not the ray geometry,
    /// plus fields whose gates do not align with the matched sweep's range;
    /// all dropped.
    pub skipped_geometry: usize,
    /// Fields dropped because the matched sweep already had that name (first
    /// part wins).
    pub field_collisions: usize,
}

/// Why [`merge_volumes`] refused its parts.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MergeError {
    /// No parts were given.
    #[error("no radar volumes to merge")]
    NoParts,
    /// The parts come from different radars (`attrs.instrument_name`).
    #[error("cannot merge radar volumes from different sites: '{first}' vs '{other}'")]
    SiteMismatch {
        /// The first part's instrument name.
        first: String,
        /// The differing part's instrument name.
        other: String,
    },
    /// The parts were decoded from different formats.
    #[error("cannot merge radar volumes from different source formats: {first:?} vs {other:?}")]
    SourceMismatch {
        /// The first part's format.
        first: SourceFormat,
        /// The differing part's format.
        other: SourceFormat,
    },
}

/// Merge per-product / per-sweep partial volumes of one scan into one volume.
///
/// Semantics:
/// - All parts share `attrs.instrument_name` and `provenance.source_format`;
///   the first part supplies every volume-level item, its provenance
///   included. Parts of different source formats are rejected rather than
///   merged: the merged volume carries one `source_format`, and the readings
///   that depend on it — [`Sweep::tilt_elevation_deg`] above all — would then
///   be taken under the first part's format for every sweep.
/// - `time_reference` becomes the earliest part's; ray times of the other parts
///   are rebased onto it. `time_coverage` is the union.
/// - Sweeps match by `fixed_angle_deg` within [`ANGLE_MATCH_TOLERANCE_DEG`] and
///   identical ray geometry (ray count, azimuths within the tolerance,
///   wrap-aware). An incoming sweep merges into the first matched sweep that has
///   none of its field names, else the first matched sweep. Fields keep their
///   native geometry: each is re-attached to the matched sweep's range, and a
///   field that does not align is dropped and counted in `skipped_geometry`.
/// - Missing per-ray instrument values of the matched sweep (NaN, `-9999`
///   samples, or an absent variable) are filled from the incoming sweep; values
///   the matched sweep has are kept. A matched-sweep variable whose length is
///   not the ray count is replaced by the incoming one, and an incoming
///   variable whose length is not the ray count is ignored.
/// - Unmatched sweeps are appended; the result is sorted by fixed angle
///   (stable) and renumbered: `sweep_number = i`, `elevation_number = i + 1`.
pub fn merge_volumes(parts: Vec<Volume>) -> Result<(Volume, MergeReport), MergeError> {
    let mut parts = parts.into_iter();
    let Some(mut base) = parts.next() else {
        return Err(MergeError::NoParts);
    };
    let mut report = MergeReport::default();

    for part in parts {
        if part.attrs.instrument_name != base.attrs.instrument_name {
            return Err(MergeError::SiteMismatch {
                first: base.attrs.instrument_name.clone(),
                other: part.attrs.instrument_name.clone(),
            });
        }
        if part.provenance.source_format != base.provenance.source_format {
            return Err(MergeError::SourceMismatch {
                first: base.provenance.source_format,
                other: part.provenance.source_format,
            });
        }
        if part.time_reference < base.time_reference {
            let shift = (base.time_reference - part.time_reference).num_seconds() as f64;
            for sweep in &mut base.sweeps {
                sweep.rays.time_s.iter_mut().for_each(|time| *time += shift);
            }
            base.time_reference = part.time_reference;
        }
        let part_shift = (part.time_reference - base.time_reference).num_seconds() as f64;
        base.time_coverage = match (base.time_coverage, part.time_coverage) {
            (Some(a), Some(b)) => Some(TimeCoverage {
                start: a.start.min(b.start),
                end: a.end.max(b.end),
            }),
            (a, b) => a.or(b),
        };

        for mut sweep in part.sweeps {
            if part_shift != 0.0 {
                sweep
                    .rays
                    .time_s
                    .iter_mut()
                    .for_each(|time| *time += part_shift);
            }
            let mut angle_matched = false;
            let mut target = None;
            for (index, existing) in base.sweeps.iter().enumerate() {
                if (existing.fixed_angle_deg - sweep.fixed_angle_deg).abs()
                    > ANGLE_MATCH_TOLERANCE_DEG
                {
                    continue;
                }
                angle_matched = true;
                if !rays_match(existing, &sweep) {
                    continue;
                }
                if target.is_none() {
                    target = Some(index);
                }
                if sweep
                    .fields
                    .iter()
                    .all(|field| existing.field(&field.name).is_none())
                {
                    target = Some(index);
                    break;
                }
            }
            let Some(index) = target else {
                if angle_matched {
                    report.skipped_geometry += 1;
                } else {
                    base.sweeps.push(sweep);
                }
                continue;
            };
            let existing = &mut base.sweeps[index];
            let nrays = existing.nrays();
            fill_ray_variables(&mut existing.ray_vars, &sweep.ray_vars, nrays);
            let incoming_range = sweep.range.clone();
            for mut field in sweep.fields {
                if existing.field(&field.name).is_some() {
                    report.field_collisions += 1;
                    continue;
                }
                let attached =
                    field
                        .native_geometry(&incoming_range)
                        .and_then(|(center, spacing)| {
                            existing.attach_geometry(center, spacing, field.ngates).ok()
                        });
                match attached {
                    Some(mapping) => {
                        field.gates = mapping;
                        existing.fields.push(field);
                        report.merged_fields += 1;
                    }
                    None => report.skipped_geometry += 1,
                }
            }
        }
    }

    base.sweeps
        .sort_by(|a, b| a.fixed_angle_deg.total_cmp(&b.fixed_angle_deg));
    for (index, sweep) in base.sweeps.iter_mut().enumerate() {
        sweep.sweep_number = u32::try_from(index).unwrap_or(u32::MAX);
        sweep.elevation_number = u16::try_from(index + 1).ok();
    }
    Ok((base, report))
}

fn rays_match(a: &Sweep, b: &Sweep) -> bool {
    a.rays.azimuth_deg.len() == b.rays.azimuth_deg.len()
        && a.rays
            .azimuth_deg
            .iter()
            .zip(&b.rays.azimuth_deg)
            .all(|(x, y)| azimuth_difference_deg(*x, *y) <= ANGLE_MATCH_TOLERANCE_DEG)
}

/// Smallest absolute angular difference, wrap-aware.
fn azimuth_difference_deg(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs() % 360.0;
    diff.min(360.0 - diff)
}

/// Fill one per-ray variable of a matched sweep with `nrays` rays from the
/// incoming sweep: gates `missing` in `base` take the incoming value. An
/// absent or mis-sized `base` takes the incoming vector whole; a mis-sized
/// incoming vector is ignored.
fn fill_values<T: Copy>(
    base: &mut Option<Vec<T>>,
    incoming: &Option<Vec<T>>,
    nrays: usize,
    missing: impl Fn(&T) -> bool,
) {
    let Some(other) = incoming.as_ref().filter(|other| other.len() == nrays) else {
        return;
    };
    match base.as_mut() {
        Some(values) if values.len() == nrays => {
            for (value, other) in values.iter_mut().zip(other) {
                if missing(value) {
                    *value = *other;
                }
            }
        }
        _ => *base = Some(other.clone()),
    }
}

fn fill_f32(base: &mut Option<Vec<f32>>, incoming: &Option<Vec<f32>>, nrays: usize) {
    fill_values(base, incoming, nrays, |value| value.is_nan());
}

/// Take the incoming vector when `base` has none or a mis-sized one.
fn fill_whole<T: Clone>(base: &mut Option<Vec<T>>, incoming: &Option<Vec<T>>, nrays: usize) {
    if base.as_ref().is_some_and(|values| values.len() == nrays) {
        return;
    }
    if let Some(other) = incoming.as_ref().filter(|other| other.len() == nrays) {
        *base = Some(other.clone());
    }
}

fn fill_ray_variables(base: &mut RayVariables, incoming: &RayVariables, nrays: usize) {
    fill_f32(
        &mut base.nyquist_velocity_mps,
        &incoming.nyquist_velocity_mps,
        nrays,
    );
    fill_f32(
        &mut base.unambiguous_range_m,
        &incoming.unambiguous_range_m,
        nrays,
    );
    fill_f32(&mut base.prt_s, &incoming.prt_s, nrays);
    fill_f32(&mut base.prt_ratio, &incoming.prt_ratio, nrays);
    fill_f32(&mut base.pulse_width_s, &incoming.pulse_width_s, nrays);
    fill_f32(
        &mut base.scan_rate_deg_per_s,
        &incoming.scan_rate_deg_per_s,
        nrays,
    );
    fill_f32(
        &mut base.rx_range_resolution_m,
        &incoming.rx_range_resolution_m,
        nrays,
    );
    fill_f32(
        &mut base.independent_samples,
        &incoming.independent_samples,
        nrays,
    );
    fill_values(&mut base.n_samples, &incoming.n_samples, nrays, |value| {
        *value == -9999
    });
    let sequence_fits = |sequence: &PrtSequence| {
        sequence.values_s.len() == nrays.saturating_mul(sequence.nprt as usize)
    };
    if !base.prt_sequence_s.as_ref().is_some_and(sequence_fits)
        && let Some(sequence) = incoming
            .prt_sequence_s
            .as_ref()
            .filter(|s| sequence_fits(s))
    {
        base.prt_sequence_s = Some(sequence.clone());
    }
    fill_whole(
        &mut base.antenna_transition,
        &incoming.antenna_transition,
        nrays,
    );
    fill_whole(&mut base.calib_index, &incoming.calib_index, nrays);
}
