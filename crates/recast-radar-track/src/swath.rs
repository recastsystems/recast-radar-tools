//! Max-value swath grids — "where the storm has BEEN".
//!
//! A swath is the per-gate extremum of one base-tilt moment, accumulated
//! across the frames of a loaded loop and projected onto a SINGLE synthetic
//! single-tilt [`RadarVolume`]. Because the result is an ordinary polar
//! volume it renders through the normal moment raster path (and the normal
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
    ElevationCut, GateRange, MomentGrid, MomentStorage, MomentType, RadarVolume, Radial,
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

/// How to combine the per-gate samples of one moment across frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// Lowest-elevation cut of `volume` that carries `moment` with decoded rows —
/// the base tilt for that moment. Split super-res cuts (reflectivity and
/// velocity in separate sweeps) are handled naturally: this picks the lowest
/// cut that actually holds the requested moment.
pub fn base_tilt_cut(volume: &RadarVolume, moment: &MomentType) -> Option<usize> {
    volume
        .cuts
        .iter()
        .enumerate()
        .filter(|(_, cut)| {
            cut.moments
                .get(moment)
                .is_some_and(|grid| !grid.radial_indices.is_empty())
        })
        .filter(|(_, cut)| cut.elevation_deg.is_finite())
        .min_by(|(_, a), (_, b)| a.elevation_deg.total_cmp(&b.elevation_deg))
        .map(|(index, _)| index)
}

/// One frame's base-tilt contribution: its cut (for radial azimuths) and the
/// moment grid sampled onto the swath.
struct FrameTilt<'a> {
    cut: &'a ElevationCut,
    grid: &'a MomentGrid,
}

/// Build the per-gate max-value swath over `frames` for `moment`.
///
/// Returns a single-tilt [`RadarVolume`] whose one moment grid is F32 (NaN =
/// no data) so the renderer's transparency handling drops empty gates, or
/// `None` when no frame carries the moment. `frames` should all be the same
/// radar (the caller's loop history is single-site); the newest frame's site
/// and time label the result.
pub fn max_value_swath(
    frames: &[&RadarVolume],
    moment: MomentType,
    aggregation: SwathAggregation,
) -> Option<RadarVolume> {
    let tilts: Vec<FrameTilt<'_>> = frames
        .iter()
        .filter_map(|volume| {
            let cut_index = base_tilt_cut(volume, &moment)?;
            let cut = volume.cuts.get(cut_index)?;
            let grid = cut.moments.get(&moment)?;
            Some(FrameTilt { cut, grid })
        })
        .collect();
    if tilts.is_empty() {
        return None;
    }

    // Reference geometry: the base tilt with the finest sampling, so the
    // swath keeps the best azimuth/range resolution present in the loop.
    let reference = tilts.iter().max_by(|a, b| {
        a.grid
            .radial_count()
            .cmp(&b.grid.radial_count())
            .then(
                a.grid
                    .gate_range
                    .gate_count
                    .cmp(&b.grid.gate_range.gate_count),
            )
            .then(max_range_m(a.grid).total_cmp(&max_range_m(b.grid)))
    })?;

    let nrows = reference.grid.radial_count().min(MAX_ROWS);
    if nrows == 0 {
        return None;
    }
    let target = GateRange {
        first_gate_m: reference.grid.gate_range.first_gate_m,
        gate_spacing_m: reference.grid.gate_range.gate_spacing_m.max(1),
        gate_count: reference.grid.gate_range.gate_count.min(MAX_GATES),
    };
    let gate_count = target.gate_count;
    if gate_count == 0 {
        return None;
    }

    // Reference radial azimuth per swath row (row i ↔ grid row i).
    let target_azimuths: Vec<f32> = (0..nrows)
        .map(|row| reference_row_azimuth(reference, row))
        .collect();
    let slot_to_row = build_slot_to_row(&target_azimuths);

    let mut values = vec![f32::NAN; nrows * gate_count];
    for tilt in &tilts {
        accumulate_tilt(&mut values, tilt, &target, &slot_to_row, nrows, aggregation);
    }

    // Everything in the swath is finite-or-NaN; NaN gates render transparent.
    let reference_elevation = reference.cut.elevation_deg;
    let newest = frames
        .iter()
        .max_by_key(|volume| volume.volume_time)
        .copied()?;

    let radials: Vec<Radial> = target_azimuths
        .iter()
        .map(|&azimuth_deg| Radial {
            azimuth_deg,
            elevation_deg: reference_elevation,
            time_offset_ms: 0,
            gate_range: target.clone(),
            nyquist_velocity_mps: None,
            radial_status: None,
        })
        .collect();

    let grid = MomentGrid {
        moment: moment.clone(),
        gate_range: target,
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices: (0..nrows).collect(),
        storage: MomentStorage::F32(values),
    };
    let mut cut = ElevationCut::new(reference_elevation, Some(1));
    cut.radials = radials;
    cut.moments.insert(moment, grid);

    let mut volume = RadarVolume::new(newest.site.clone(), newest.volume_time);
    volume.cuts.push(cut);
    Some(volume)
}

/// Azimuth of one reference grid row, via the grid's radial-index back-link.
fn reference_row_azimuth(reference: &FrameTilt<'_>, row: usize) -> f32 {
    reference
        .grid
        .radial_indices
        .get(row)
        .and_then(|&radial_index| reference.cut.radials.get(radial_index))
        .map(|radial| radial.azimuth_deg.rem_euclid(360.0))
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
    target: &GateRange,
    slot_to_row: &[usize],
    nrows: usize,
    aggregation: SwathAggregation,
) {
    let src = &tilt.grid.gate_range;
    let src_first = src.first_gate_m as f32;
    let src_spacing = src.gate_spacing_m.max(1) as f32;
    let src_gate_count = src.gate_count;
    let gate_count = target.gate_count;
    // Fast path: identical range layout means gate g maps to source gate g.
    let aligned = src.first_gate_m == target.first_gate_m
        && src.gate_spacing_m.max(1) == target.gate_spacing_m.max(1);

    for row in 0..tilt.grid.radial_count() {
        let Some(&radial_index) = tilt.grid.radial_indices.get(row) else {
            continue;
        };
        let Some(radial) = tilt.cut.radials.get(radial_index) else {
            continue;
        };
        let slot = ((radial.azimuth_deg.rem_euclid(360.0) / (360.0 / AZ_SLOTS as f32)).round()
            as usize)
            % AZ_SLOTS;
        let target_row = slot_to_row.get(slot).copied().unwrap_or(0);
        if target_row >= nrows {
            continue;
        }
        let base = target_row * gate_count;
        for g in 0..gate_count {
            let src_gate = if aligned {
                g
            } else {
                let range_m = target.first_gate_m as f32 + g as f32 * target.gate_spacing_m as f32;
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
            let Some(value) = tilt.grid.scaled_value(row, src_gate) else {
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

fn max_range_m(grid: &MomentGrid) -> f32 {
    grid.gate_range.first_gate_m as f32
        + grid.gate_range.gate_spacing_m as f32 * grid.gate_range.gate_count as f32
}
