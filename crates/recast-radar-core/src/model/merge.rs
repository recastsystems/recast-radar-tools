//! Assembly of one scan split across several files (ODIM product-per-file
//! feeds, JMA member tars) into one [`Volume`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::sweep::{RayVariables, Sweep};
use super::volume::{TimeCoverage, Volume};

/// Tolerance used to treat two fixed angles or two ray azimuths as the same.
pub const ANGLE_MATCH_TOLERANCE_DEG: f32 = 0.05;

/// Counters describing what [`merge_volumes`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MergeError {
    #[error("no radar volumes to merge")]
    NoParts,
    #[error("cannot merge radar volumes from different sites: '{first}' vs '{other}'")]
    SiteMismatch { first: String, other: String },
}

/// Merge per-product / per-sweep partial volumes of one scan into one volume.
///
/// The FM301 counterpart of the legacy `merge_radar_volumes`, with the same
/// semantics:
/// - All parts share `attrs.instrument_name`; the first part supplies every
///   volume-level item.
/// - `time_reference` becomes the earliest part's; ray times of the other parts
///   are rebased onto it. `time_coverage` is the union.
/// - Sweeps match by `fixed_angle_deg` within [`ANGLE_MATCH_TOLERANCE_DEG`] and
///   identical ray geometry (ray count, azimuths within the tolerance,
///   wrap-aware). An incoming sweep merges into the first matched sweep that has
///   none of its field names, else the first matched sweep. Fields keep their
///   native geometry: each is re-attached to the matched sweep's range, and a
///   field that does not align is dropped and counted in `skipped_geometry`.
/// - Missing per-ray instrument values of the matched sweep are filled from the
///   incoming sweep.
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
            fill_ray_variables(&mut existing.ray_vars, &sweep.ray_vars);
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

fn fill_f32(base: &mut Option<Vec<f32>>, incoming: &Option<Vec<f32>>) {
    match (base.as_mut(), incoming) {
        (None, Some(values)) => *base = Some(values.clone()),
        (Some(values), Some(other)) if values.len() == other.len() => {
            for (value, other) in values.iter_mut().zip(other) {
                if value.is_nan() {
                    *value = *other;
                }
            }
        }
        _ => {}
    }
}

fn fill_ray_variables(base: &mut RayVariables, incoming: &RayVariables) {
    fill_f32(
        &mut base.nyquist_velocity_mps,
        &incoming.nyquist_velocity_mps,
    );
    fill_f32(&mut base.unambiguous_range_m, &incoming.unambiguous_range_m);
    fill_f32(&mut base.prt_s, &incoming.prt_s);
    fill_f32(&mut base.prt_ratio, &incoming.prt_ratio);
    fill_f32(&mut base.pulse_width_s, &incoming.pulse_width_s);
    fill_f32(&mut base.scan_rate_deg_per_s, &incoming.scan_rate_deg_per_s);
    fill_f32(
        &mut base.rx_range_resolution_m,
        &incoming.rx_range_resolution_m,
    );
    fill_f32(&mut base.independent_samples, &incoming.independent_samples);
    match (base.n_samples.as_mut(), &incoming.n_samples) {
        (None, Some(values)) => base.n_samples = Some(values.clone()),
        (Some(values), Some(other)) if values.len() == other.len() => {
            for (value, other) in values.iter_mut().zip(other) {
                if *value == -9999 {
                    *value = *other;
                }
            }
        }
        _ => {}
    }
    if base.prt_sequence_s.is_none() {
        base.prt_sequence_s = incoming.prt_sequence_s.clone();
    }
    if base.antenna_transition.is_none() {
        base.antenna_transition = incoming.antenna_transition.clone();
    }
    if base.calib_index.is_none() {
        base.calib_index = incoming.calib_index.clone();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::model::{Field, FieldData, FieldName, GateMapping, IntCoding, SweepMode};

    fn part(site: &str, seconds: i64, angle: f32, name: FieldName, spacing: f64) -> Volume {
        let mut volume = Volume::new(
            site,
            DateTime::<Utc>::from_timestamp(seconds, 0).unwrap_or_default(),
        );
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, angle);
        for ray in 0..3 {
            sweep.push_ray(f64::from(ray), ray as f32 * 120.0, angle);
        }
        let gates = sweep.attach_geometry(spacing / 2.0, spacing, 4).unwrap();
        let mut field = Field::new(
            name,
            gates,
            4,
            FieldData::U8 {
                values: Vec::new(),
                coding: IntCoding::nexrad(2.0, 66.0),
            },
        );
        for ray in 0..3 {
            field.push_row_u8(ray, &[2, 3, 4, 5]).unwrap();
        }
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();
        volume.sweeps.push(sweep);
        volume
    }

    #[test]
    fn merges_fields_of_matching_sweeps_and_rebases_time() {
        let dbzh = part("SKJAV", 1_010, 0.5, FieldName::Dbzh, 250.0);
        let vradh = part("SKJAV", 1_000, 0.52, FieldName::Vradh, 1000.0);
        let (merged, report) = merge_volumes(vec![dbzh, vradh]).unwrap();
        assert_eq!(merged.sweeps.len(), 1);
        assert_eq!(report.merged_fields, 1);
        let sweep = &merged.sweeps[0];
        assert_eq!(sweep.fields[1].name, FieldName::Vradh);
        assert_eq!(
            sweep.fields[1].gates,
            GateMapping {
                start: 0,
                stride: 4
            }
        );
        assert_eq!(merged.time_reference.timestamp(), 1_000);
        assert_eq!(sweep.rays.time_s[0], 10.0);
    }

    #[test]
    fn rejects_other_sites_and_counts_collisions() {
        let a = part("SKJAV", 0, 0.5, FieldName::Dbzh, 250.0);
        let b = part("SKKOJ", 0, 0.5, FieldName::Dbzh, 250.0);
        assert!(matches!(
            merge_volumes(vec![a.clone(), b]),
            Err(MergeError::SiteMismatch { .. })
        ));
        let (_, report) = merge_volumes(vec![a.clone(), a]).unwrap();
        assert_eq!(report.field_collisions, 1);
        assert!(matches!(
            merge_volumes(Vec::new()),
            Err(MergeError::NoParts)
        ));
    }
}
