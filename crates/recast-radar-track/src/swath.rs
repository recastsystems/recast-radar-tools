//! Max-value swath grids — "where the storm has BEEN".
//!
//! A swath is the per-gate extremum of one base-tilt field, accumulated
//! across the frames of a loaded loop and projected onto a SINGLE synthetic
//! single-sweep [`Volume`]. Because the result is an ordinary polar volume
//! it renders through the normal field raster path (and the normal
//! reflectivity / velocity color tables) with no special draw code: the app
//! just points the existing viewport rasterizer at it.
//!
//! The construction reuses one loop frame as the geometric reference (the
//! frame whose base tilt has the finest azimuth / gate sampling) and maps
//! every other frame's base-tilt gates onto that reference by nearest
//! azimuth and matched range, so the swath inherits real radar geometry and
//! the renderer's own azimuth gap-filling instead of striping a synthetic
//! regular grid. This mirrors how a plan-position "digital storm total" or
//! "maximum estimated size of hail" swath is built from a scan series
//! (Witt et al. 1998, *Wea. Forecasting* 13, on volume-scan accumulation
//! products); here the accumulator is a plain per-gate max rather than a
//! rate integral.

use recast_radar_core::{
    Field, FieldData, FieldName, FloatCoding, GateMapping, RangeCoord, Sweep, SweepMode, Volume,
};

/// 0.1° azimuth slots used to map source radials onto the reference tilt —
/// the same granularity the renderer's own `AzimuthLookup`
/// (`recast_radar_render`) uses.
const AZ_SLOTS: usize = 3600;

/// Upper bounds on the synthetic grid so a malformed or hostile input volume
/// cannot make the swath allocate unboundedly. Real WSR-88D base tilts are
/// ~720 radials × ~1840 gates, well under these caps.
const MAX_ROWS: usize = 2000;
const MAX_GATES: usize = 4000;

/// How to combine the per-gate samples of one field across frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SwathAggregation {
    /// Keep the largest value (peak reflectivity: "max REF").
    Max,
    /// Keep the value of largest absolute magnitude, sign preserved (peak
    /// inbound/outbound velocity: "max |V|").
    MaxMagnitude,
}

impl SwathAggregation {
    fn combine(self, existing: f32, candidate: f32) -> f32 {
        if !existing.is_finite() {
            return candidate;
        }
        match self {
            Self::Max => existing.max(candidate),
            Self::MaxMagnitude => {
                if candidate.abs() > existing.abs() {
                    candidate
                } else {
                    existing
                }
            }
        }
    }
}

/// Rows a field provides: its rows minus the absent ones.
fn provided_rows(field: &Field) -> usize {
    (field.nrays as usize).saturating_sub(field.absent_rows.len())
}

/// Lowest sweep of `volume` ([`Sweep::tilt_elevation_deg`], the first on
/// ties) that carries a field named `name` with decoded rows — the base tilt
/// for that field. Split super-res cuts (reflectivity and velocity in
/// separate sweeps) are handled naturally: this picks the lowest sweep that
/// actually holds the requested field.
pub fn base_tilt_sweep(volume: &Volume, name: &FieldName) -> Option<usize> {
    let source = volume.provenance.source_format;
    volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, sweep)| {
            sweep
                .field(name)
                .is_some_and(|field| provided_rows(field) > 0)
        })
        .map(|(index, sweep)| (index, sweep.tilt_elevation_deg(source)))
        .filter(|(_, elevation)| elevation.is_finite())
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index)
}

/// One frame's base-tilt contribution: its sweep (for ray azimuths), the
/// field sampled onto the swath and the field's native geometry.
struct FrameTilt<'a> {
    sweep: &'a Sweep,
    /// [`Sweep::tilt_elevation_deg`] of `sweep`.
    tilt_elevation_deg: f32,
    field: &'a Field,
    first_gate_m: f64,
    spacing_m: f64,
}

/// Native geometry of a swath target: centre of gate 0 and spacing (m).
#[derive(Clone, Copy)]
struct Target {
    first_gate_m: f64,
    spacing_m: f64,
    gate_count: usize,
}

/// Build the per-gate max-value swath over `frames` for the field named
/// `name`.
///
/// Returns a single-sweep [`Volume`] whose one field is F32 (NaN = no data)
/// so the renderer's transparency handling drops empty gates, or `None`
/// when no frame carries the field. `frames` should all be the same radar
/// (the caller's loop history is single-site); the newest frame's station
/// and time label the result.
pub fn value_swath(
    frames: &[&Volume],
    name: &FieldName,
    aggregation: SwathAggregation,
) -> Option<Volume> {
    let tilts: Vec<FrameTilt<'_>> = frames
        .iter()
        .filter_map(|volume| {
            let sweep_index = base_tilt_sweep(volume, name)?;
            let sweep = volume.sweeps.get(sweep_index)?;
            let field = sweep.field(name)?;
            let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range)?;
            Some(FrameTilt {
                sweep,
                tilt_elevation_deg: volume.tilt_elevation_deg(sweep_index)?,
                field,
                first_gate_m,
                spacing_m,
            })
        })
        .collect();
    if tilts.is_empty() {
        return None;
    }

    // Reference geometry: the base tilt with the finest sampling, so the
    // swath keeps the best azimuth/range resolution present in the loop.
    let reference = tilts.iter().max_by(|a, b| {
        provided_rows(a.field)
            .cmp(&provided_rows(b.field))
            .then(a.field.ngates.cmp(&b.field.ngates))
            .then(max_range_m(a).total_cmp(&max_range_m(b)))
    })?;

    let nrows = (reference.field.nrays as usize).min(MAX_ROWS);
    if nrows == 0 {
        return None;
    }
    let target = Target {
        first_gate_m: reference.first_gate_m,
        spacing_m: reference.spacing_m.max(1.0),
        gate_count: (reference.field.ngates as usize).min(MAX_GATES),
    };
    let gate_count = target.gate_count;
    if gate_count == 0 {
        return None;
    }

    // Reference ray azimuth per swath row (row i ↔ field row i).
    let target_azimuths: Vec<f32> = (0..nrows)
        .map(|row| reference_row_azimuth(reference, row))
        .collect();
    let slot_to_row = build_slot_to_row(&target_azimuths);

    let mut values = vec![f32::NAN; nrows * gate_count];
    for tilt in &tilts {
        accumulate_tilt(&mut values, tilt, &target, &slot_to_row, nrows, aggregation);
    }

    // Everything in the swath is finite-or-NaN; NaN gates render transparent.
    let reference_elevation = reference.tilt_elevation_deg;
    let newest = frames
        .iter()
        .max_by_key(|volume| swath_time(volume))
        .copied()?;

    let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, reference_elevation);
    sweep.elevation_number = Some(1);
    sweep.reserve_rays(nrows);
    for &azimuth_deg in &target_azimuths {
        sweep.push_ray(0.0, azimuth_deg, reference_elevation);
    }
    sweep.range = RangeCoord::Uniform {
        first_center_m: target.first_gate_m,
        spacing_m: target.spacing_m,
        ngates: u32::try_from(gate_count).unwrap_or(u32::MAX),
    };
    let mut field = Field::new(
        name.clone(),
        GateMapping::IDENTITY,
        u32::try_from(gate_count).unwrap_or(u32::MAX),
        FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
    );
    field.quantity = reference.field.quantity;
    field.polarization = reference.field.polarization;
    field.attrs.units = reference.field.attrs.units.clone();
    sweep.fields.push(field);

    let mut volume = Volume::new(newest.attrs.instrument_name.clone(), swath_time(newest));
    volume.attrs.site_name = newest.attrs.site_name.clone();
    volume.location = newest.location;
    volume.time_coverage = newest.time_coverage;
    volume.sweeps.push(sweep);
    Some(volume)
}

/// The time that labels a frame: its first ray, else its time reference.
fn swath_time(volume: &Volume) -> chrono::DateTime<chrono::Utc> {
    volume
        .time_coverage
        .map_or(volume.time_reference, |coverage| coverage.start)
}

/// Azimuth of one reference field row (the sweep's ray of that row).
fn reference_row_azimuth(reference: &FrameTilt<'_>, row: usize) -> f32 {
    reference
        .sweep
        .rays
        .azimuth_deg
        .get(row)
        .map(|azimuth| azimuth.rem_euclid(360.0))
        .unwrap_or(0.0)
}

/// Map every 0.1° azimuth slot to the nearest swath row, so a source radial
/// at any azimuth lands on the closest reference row (no gaps, no striping).
fn build_slot_to_row(target_azimuths: &[f32]) -> Vec<usize> {
    (0..AZ_SLOTS)
        .map(|slot| {
            let slot_az = slot as f32 * (360.0 / AZ_SLOTS as f32);
            target_azimuths
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    azimuth_delta_deg(slot_az, **a).total_cmp(&azimuth_delta_deg(slot_az, **b))
                })
                .map(|(row, _)| row)
                .unwrap_or(0)
        })
        .collect()
}

/// Fold one frame's base-tilt gates into the swath accumulator.
fn accumulate_tilt(
    values: &mut [f32],
    tilt: &FrameTilt<'_>,
    target: &Target,
    slot_to_row: &[usize],
    nrows: usize,
    aggregation: SwathAggregation,
) {
    let src_first = tilt.first_gate_m as f32;
    let src_spacing = tilt.spacing_m.max(1.0) as f32;
    let src_gate_count = tilt.field.ngates as usize;
    let gate_count = target.gate_count;
    // Fast path: identical range layout means gate g maps to source gate g.
    let aligned = tilt.first_gate_m == target.first_gate_m
        && tilt.spacing_m.max(1.0) == target.spacing_m.max(1.0);

    for row in 0..tilt.field.nrays as usize {
        if tilt.field.is_absent(row) {
            continue;
        }
        let Some(azimuth) = tilt.sweep.rays.azimuth_deg.get(row) else {
            continue;
        };
        let slot =
            ((azimuth.rem_euclid(360.0) / (360.0 / AZ_SLOTS as f32)).round() as usize) % AZ_SLOTS;
        let target_row = slot_to_row.get(slot).copied().unwrap_or(0);
        if target_row >= nrows {
            continue;
        }
        let base = target_row * gate_count;
        for g in 0..gate_count {
            let src_gate = if aligned {
                g
            } else {
                let range_m = target.first_gate_m as f32 + g as f32 * target.spacing_m as f32;
                let src_gate = ((range_m - src_first) / src_spacing).round();
                if src_gate < 0.0 {
                    continue;
                }
                src_gate as usize
            };
            if src_gate >= src_gate_count {
                if aligned {
                    break;
                }
                continue;
            }
            let Some(value) = tilt.field.value(row, src_gate) else {
                continue;
            };
            let slot = &mut values[base + g];
            *slot = aggregation.combine(*slot, value);
        }
    }
}

/// Wrap-aware absolute azimuth difference in degrees.
fn azimuth_delta_deg(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs() % 360.0;
    diff.min(360.0 - diff)
}

fn max_range_m(tilt: &FrameTilt<'_>) -> f32 {
    tilt.first_gate_m as f32 + tilt.spacing_m as f32 * tilt.field.ngates as f32
}
