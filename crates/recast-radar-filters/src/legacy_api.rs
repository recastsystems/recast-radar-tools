//! Legacy-model signatures kept while other crates migrate to the FM301
//! model (`docs/design/fm301-model.md` section 13.3). Only this module names
//! legacy items; it is deleted with the shim.
//!
//! Every public function that took legacy types keeps its old name and
//! signature here. `apply_reflectivity_gate_filter` kept its name for the
//! FM301 signature, which shadows the wrapper at the crate root; the legacy
//! form is reachable as `legacy_api::apply_reflectivity_gate_filter` only (no
//! other crate calls it). Each wrapper converts its legacy input to the FM301
//! model, calls the FM301 function and converts the result back, so both
//! paths share one implementation and produce identical values.
//!
//! Geometry is converted with the legacy consumer's own reading of
//! `GateRange`: `first_gate_m` is the range of gate 0 and `gate_spacing_m`
//! the spacing, so every range a filter computes is exactly the value it
//! computed from the legacy grid. Values keep their legacy semantics:
//! integer codes are `(raw - offset) / scale` with `nodata` and
//! `range_folded` as no value, `F32` grids are physical.

#![allow(deprecated)]

use recast_radar_core::{
    ElevationCut, Field, FieldData, FieldName, FloatCoding, GateMapping, GateRange, IntCoding,
    LinearTransform, MomentGrid, MomentStorage, MomentType, RangeCoord, Sweep, SweepMode,
};

/// A moment grid upsampled for display plus the per-row azimuths the
/// synthetic rows render at (native rows keep their exact beam azimuth).
/// See [`crate::UpsampledSweep`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_filters::UpsampledSweep")
)]
pub struct InterpolatedGrid {
    pub grid: MomentGrid,
    pub row_azimuths_deg: Vec<f32>,
}

/// Smooth a moment grid's values into a new F32 grid with identical
/// geometry. See [`crate::smooth_field`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_filters::smooth_field")
)]
pub fn smooth_moment_grid(grid: &MomentGrid) -> MomentGrid {
    let smoothed = crate::smooth_field(&field_from_grid(grid));
    physical_grid_like(grid, physical_values(smoothed))
}

/// Filter `grid` (any moment sharing `cut`'s radials) against the cut's
/// reflectivity. See [`crate::apply_reflectivity_gate_filter`].
///
/// The reflectivity grid's rows are matched to `grid`'s rows by radial
/// index. A reflectivity geometry that cannot share one range coordinate
/// with `grid`'s (non-multiple spacings, or edges off the finer lattice)
/// has no FM301 form; the grid is then blanked, as when the cut has no
/// reflectivity. No decoded file produces such a cut.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_filters::apply_reflectivity_gate_filter")
)]
pub fn apply_reflectivity_gate_filter(
    cut: &ElevationCut,
    grid: &MomentGrid,
    threshold_dbz: f32,
) -> MomentGrid {
    let mut sweep = sweep_for_grid(cut, grid);
    if let Some(reflectivity) = cut.moments.get(&MomentType::Reflectivity)
        && reflectivity.moment != grid.moment
    {
        add_grid_by_radial(&mut sweep, &grid.radial_indices, reflectivity);
    }
    let filtered = crate::apply_reflectivity_gate_filter(&sweep, &sweep.fields[0], threshold_dbz);
    physical_grid_like(grid, physical_values(filtered))
}

/// Upsample a moment grid for display. See [`crate::upsample_field`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_filters::upsample_field")
)]
pub fn upsample_moment_grid(cut: &ElevationCut, grid: &MomentGrid) -> Option<InterpolatedGrid> {
    if grid
        .radial_indices
        .iter()
        .any(|radial| cut.radials.get(*radial).is_none())
    {
        // The legacy function refused a broken radial linkage.
        return None;
    }
    let sweep = sweep_for_grid(cut, grid);
    let up = crate::upsample_field(&sweep, &sweep.fields[0])?;
    let (first_gate_m, gate_spacing_m) = match &up.sweep.range {
        RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ..
        } => (*first_center_m, *spacing_m),
        RangeCoord::Explicit { .. } => return None,
    };
    let row_azimuths_deg = up.sweep.rays.azimuth_deg.clone();
    // Each output row links back to its nearest parent's radial so
    // cut-radial lookups (Nyquist, beam azimuth basis) stay valid.
    let radial_indices = up
        .parent_rays
        .iter()
        .map(|parent| grid.radial_indices[*parent as usize])
        .collect();
    let field = up.sweep.fields.into_iter().next()?;
    let gate_count = field.ngates as usize;
    Some(InterpolatedGrid {
        grid: MomentGrid {
            moment: grid.moment.clone(),
            gate_range: GateRange {
                first_gate_m: first_gate_m.round() as i32,
                gate_spacing_m: gate_spacing_m.round() as i32,
                gate_count,
            },
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices,
            storage: MomentStorage::F32(physical_values(field)),
        },
        row_azimuths_deg,
    })
}

/// The legacy `F32` output grid of a filter: `template`'s moment, geometry
/// and radial indices over physical `values`.
fn physical_grid_like(template: &MomentGrid, values: Vec<f32>) -> MomentGrid {
    MomentGrid {
        moment: template.moment.clone(),
        gate_range: template.gate_range.clone(),
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices: template.radial_indices.clone(),
        storage: MomentStorage::F32(values),
    }
}

fn range_of(gate_range: &GateRange) -> RangeCoord {
    RangeCoord::Uniform {
        first_center_m: f64::from(gate_range.first_gate_m),
        spacing_m: f64::from(gate_range.gate_spacing_m),
        ngates: u32::try_from(gate_range.gate_count).unwrap_or(u32::MAX),
    }
}

fn coding_data(grid: &MomentGrid, storage: MomentStorage) -> FieldData {
    let transform = LinearTransform::IcdScaleOffset {
        scale: grid.scale,
        offset: grid.offset,
    };
    match storage {
        MomentStorage::U8(values) => FieldData::U8 {
            values,
            coding: IntCoding {
                fill_value: grid.nodata.and_then(|code| u8::try_from(code).ok()),
                range_folded: grid.range_folded.and_then(|code| u8::try_from(code).ok()),
                ..IntCoding::new(transform)
            },
        },
        MomentStorage::U16(values) => FieldData::U16 {
            values,
            coding: IntCoding {
                fill_value: grid.nodata,
                range_folded: grid.range_folded,
                ..IntCoding::new(transform)
            },
        },
        MomentStorage::F32(values) => FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
    }
}

/// A field over `storage` rows (already in ray order) with `grid`'s coding
/// and name, identity gate mapping.
fn field_with(grid: &MomentGrid, storage: MomentStorage, nrays: usize) -> Field {
    let ngates = u32::try_from(grid.gate_range.gate_count).unwrap_or(u32::MAX);
    let mut field = Field::new(
        grid.moment
            .to_field_name(recast_radar_core::legacy::LegacyConvention::Generic),
        GateMapping::IDENTITY,
        ngates,
        coding_data(grid, storage),
    );
    field.nrays = u32::try_from(nrays).unwrap_or(u32::MAX);
    field
}

/// A grid's rows as a field (row `r` of the grid is row `r` of the field),
/// with the exact legacy value semantics.
fn field_from_grid(grid: &MomentGrid) -> Field {
    field_with(grid, grid.storage.clone(), grid.radial_count())
}

/// A sweep whose rays are `grid`'s rows (row `r` is the cut's radial
/// `grid.radial_indices[r]`), with the grid as its only field and the grid's
/// gate geometry as its range. Nyquist velocities come from the radials; a
/// missing radial gives NaN coordinates.
fn sweep_for_grid(cut: &ElevationCut, grid: &MomentGrid) -> Sweep {
    let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, cut.elevation_deg);
    sweep.elevation_number = cut.elevation_number.map(u16::from);
    let radials = &grid.radial_indices;
    sweep.reserve_rays(radials.len());
    let mut nyquist = Vec::with_capacity(radials.len());
    for &radial_index in radials {
        match cut.radials.get(radial_index) {
            Some(radial) => {
                sweep.push_ray(
                    f64::from(radial.time_offset_ms) / 1000.0,
                    radial.azimuth_deg,
                    radial.elevation_deg,
                );
                nyquist.push(radial.nyquist_velocity_mps);
            }
            None => {
                sweep.push_ray(f64::NAN, f32::NAN, f32::NAN);
                nyquist.push(None);
            }
        }
    }
    sweep.ray_vars.nyquist_velocity_mps = nyquist
        .iter()
        .any(Option::is_some)
        .then(|| nyquist.iter().map(|v| v.unwrap_or(f32::NAN)).collect());
    sweep.range = range_of(&grid.gate_range);
    sweep
        .fields
        .push(field_with(grid, grid.storage.clone(), grid.radial_count()));
    sweep
}

/// Add another grid of the same cut to a sweep whose rays are the cut's
/// radials `ray_radials` (`ray_radials[r]` is ray `r`'s radial). Rows are
/// placed by radial; rays the grid lacks become absent rows. Returns `false`
/// (and leaves the sweep unchanged) when the grid's geometry does not align
/// with the sweep's range.
fn add_grid_by_radial(sweep: &mut Sweep, ray_radials: &[usize], grid: &MomentGrid) -> bool {
    let ngates = u32::try_from(grid.gate_range.gate_count).unwrap_or(u32::MAX);
    let Ok(gates) = sweep.attach_geometry(
        f64::from(grid.gate_range.first_gate_m),
        f64::from(grid.gate_range.gate_spacing_m),
        ngates,
    ) else {
        return false;
    };
    let nrays = ray_radials.len();
    let mut field = if grid.radial_indices == ray_radials {
        field_with(grid, grid.storage.clone(), nrays)
    } else {
        scattered_field(grid, ray_radials)
    };
    field.gates = gates;
    if sweep.field(&field.name).is_some() {
        let (quantity, polarization) = (field.quantity, field.polarization);
        field.name = FieldName::Other(format!("{:?}", grid.moment).into());
        field.quantity = quantity;
        field.polarization = polarization;
    }
    sweep.fields.push(field);
    true
}

fn scattered_field(grid: &MomentGrid, ray_radials: &[usize]) -> Field {
    let gates = grid.gate_range.gate_count;
    let nrays = ray_radials.len();
    let mut row_of_radial = std::collections::HashMap::new();
    for (row, &radial) in grid.radial_indices.iter().enumerate() {
        row_of_radial.entry(radial).or_insert(row);
    }
    let rows: Vec<Option<usize>> = ray_radials
        .iter()
        .map(|radial| row_of_radial.get(radial).copied())
        .collect();
    fn gather<T: Copy>(values: &[T], rows: &[Option<usize>], gates: usize, fill: T) -> Vec<T> {
        let mut out = Vec::with_capacity(rows.len() * gates);
        for row in rows {
            match row.and_then(|row| values.get(row * gates..(row + 1) * gates)) {
                Some(slice) => out.extend_from_slice(slice),
                None => out.resize(out.len() + gates, fill),
            }
        }
        out
    }
    let storage = match &grid.storage {
        MomentStorage::U8(values) => MomentStorage::U8(gather(
            values,
            &rows,
            gates,
            grid.nodata
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(0),
        )),
        MomentStorage::U16(values) => {
            MomentStorage::U16(gather(values, &rows, gates, grid.nodata.unwrap_or(0)))
        }
        MomentStorage::F32(values) => MomentStorage::F32(gather(values, &rows, gates, f32::NAN)),
    };
    let mut field = field_with(grid, storage, nrays);
    field.absent_rows = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.is_none())
        .map(|(ray, _)| ray as u32)
        .collect();
    field
}

fn physical_values(field: Field) -> Vec<f32> {
    match field.data {
        FieldData::F32 { values, .. } => values,
        other => {
            let field = Field {
                data: other,
                ..field
            };
            field.to_physical()
        }
    }
}
