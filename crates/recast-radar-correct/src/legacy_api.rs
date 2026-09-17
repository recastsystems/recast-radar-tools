//! Legacy-model signatures kept while other crates migrate to the FM301
//! model (`docs/design/fm301-model.md` section 13.3). Only this module names
//! legacy items; it is deleted with the shim.
//!
//! Every public function that took legacy types keeps its old name and
//! signature here. Three of them (`radial_azimuths`,
//! `copy_scaled_velocity_row`, `dealias_skipped_no_nyquist`) kept their names
//! for the FM301 signatures, which shadow the wrappers at the crate root; the
//! legacy forms are reachable as `legacy_api::<name>` only (no un-migrated
//! crate calls them). `TemporalPrior::Volume` now holds a `Volume`; nothing
//! outside this crate constructed it. Each wrapper converts its legacy input
//! to the FM301 model with [`convert`], calls the FM301 function and converts
//! the result back, so both paths share one implementation and produce
//! identical values. [`convert`] is also used by the legacy wrappers of the
//! map, retrieve and track crates.

#![allow(deprecated)]

use recast_radar_core::{ElevationCut, MomentGrid, MomentType, RadarVolume};

use crate::{EnvironmentalWindProfile, RangeBandReference, TemporalPrior, V4VolumeSolution};

/// Legacy-model copies of a [`V4VolumeSolution`]'s tilts, indexed like
/// `RadarVolume::cuts`. Empty unless the solution came from
/// [`dealias_volume_v4`].
#[derive(Default)]
pub struct LegacyTilts(Vec<Option<MomentGrid>>);

/// Dealias a base velocity moment with the region-based engine. See
/// [`crate::dealias_velocity`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::dealias_velocity")
)]
pub fn dealias_velocity_grid(cut: &ElevationCut, source: &MomentGrid) -> MomentGrid {
    let sweep = convert::sweep_for_grid(cut, source);
    let field = crate::dealias_velocity(&sweep, &sweep.fields[0]);
    convert::grid_like(field, MomentType::Velocity, source, None)
}

/// [`dealias_velocity_grid`] with an optional external wind reference. See
/// [`crate::dealias_velocity_with_reference`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_correct::dealias_velocity_with_reference"
    )
)]
pub fn dealias_velocity_grid_with_reference(
    cut: &ElevationCut,
    source: &MomentGrid,
    reference: Option<&RangeBandReference>,
) -> MomentGrid {
    let sweep = convert::sweep_for_grid(cut, source);
    let field = crate::dealias_velocity_with_reference(&sweep, &sweep.fields[0], reference);
    convert::grid_like(field, MomentType::Velocity, source, None)
}

/// Dealias a base velocity moment with the Py-ART region port. See
/// [`crate::dealias_velocity_pyart_region`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::dealias_velocity_pyart_region")
)]
pub fn dealias_velocity_grid_pyart_region(cut: &ElevationCut, source: &MomentGrid) -> MomentGrid {
    let sweep = convert::sweep_for_grid(cut, source);
    let field = crate::dealias_velocity_pyart_region(&sweep, &sweep.fields[0]);
    convert::grid_like(field, MomentType::Velocity, source, None)
}

/// True when [`dealias_velocity_grid`] over this cut can only pass raw
/// velocity through. See [`crate::dealias_skipped_no_nyquist`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::dealias_skipped_no_nyquist")
)]
pub fn dealias_skipped_no_nyquist(cut: &ElevationCut, source: &MomentGrid) -> bool {
    let sweep = convert::sweep_for_grid(cut, source);
    crate::dealias_skipped_no_nyquist(&sweep, &sweep.fields[0])
}

/// Per-row beam azimuth of `grid` in degrees, normalized to `[0, 360)`; NaN
/// for a row whose radial index is missing from `cut`. See
/// [`crate::radial_azimuths`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::radial_azimuths")
)]
pub fn radial_azimuths(cut: &ElevationCut, grid: &MomentGrid) -> Vec<f32> {
    let sweep = convert::sweep_for_grid(cut, grid);
    crate::radial_azimuths(&sweep, &sweep.fields[0])
}

/// Write row `row` of `source` into `row_values` as physical values (m/s).
/// See [`crate::copy_scaled_velocity_row`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::copy_scaled_velocity_row")
)]
pub fn copy_scaled_velocity_row(source: &MomentGrid, row: usize, row_values: &mut [f32]) {
    crate::copy_scaled_velocity_row(&convert::field_for_grid(source), row, row_values)
}

/// Fit the per-range-band zeroth harmonic on a velocity grid. See
/// [`crate::range_band_reference`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::range_band_reference")
)]
pub fn fit_range_band_reference(cut: &ElevationCut, grid: &MomentGrid) -> RangeBandReference {
    let sweep = convert::sweep_for_grid(cut, grid);
    crate::range_band_reference(&sweep, &sweep.fields[0])
}

/// Predicted radial velocity for one cut on its velocity grid's lattice. See
/// [`crate::project_environmental_winds_onto`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_correct::project_environmental_winds_onto"
    )
)]
pub fn project_environmental_winds(
    profile: &EnvironmentalWindProfile,
    cut: &ElevationCut,
    grid: &MomentGrid,
) -> Vec<f32> {
    let sweep = convert::sweep_for_grid(cut, grid);
    crate::project_environmental_winds_onto(profile, &sweep, &sweep.fields[0])
}

/// Solve the whole volume once with the v4 engine. See
/// [`crate::dealias_volume`]; the result also serves the legacy
/// [`V4VolumeSolution::tilt_grid`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::dealias_volume")
)]
pub fn dealias_volume_v4(
    volume: &RadarVolume,
    previous: Option<TemporalPrior<'_>>,
    environment: Option<&EnvironmentalWindProfile>,
) -> V4VolumeSolution {
    let converted = convert::volume_with(volume, |moment| *moment == MomentType::Velocity);
    let mut solution = crate::dealias_volume(&converted, previous, environment);
    let grids = volume
        .cuts
        .iter()
        .enumerate()
        .map(|(cut_index, cut)| {
            let source = cut.moments.get(&MomentType::Velocity)?;
            let field = solution.tilt_field(cut_index)?.clone();
            Some(convert::grid_like(
                field,
                MomentType::Velocity,
                source,
                Some(&converted.sweeps[cut_index]),
            ))
        })
        .collect();
    solution.legacy_tilts = LegacyTilts(grids);
    solution
}

/// Per-cut convenience over [`dealias_volume_v4`]. See
/// [`crate::dealias_velocity_v4`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_correct::dealias_velocity_v4")
)]
pub fn dealias_velocity_grid_v4(
    volume: &RadarVolume,
    cut_index: usize,
    previous: Option<&RadarVolume>,
    environment: Option<&EnvironmentalWindProfile>,
) -> Option<MomentGrid> {
    let source = volume
        .cuts
        .get(cut_index)?
        .moments
        .get(&MomentType::Velocity)?;
    let converted = convert::volume_with(volume, |moment| *moment == MomentType::Velocity);
    let previous = previous
        .map(|previous| convert::volume_with(previous, |moment| *moment == MomentType::Velocity));
    let solution = crate::dealias_volume(
        &converted,
        previous.as_ref().map(TemporalPrior::Volume),
        environment,
    );
    let field = solution.into_tilt_field(cut_index)?;
    Some(convert::grid_like(
        field,
        MomentType::Velocity,
        source,
        Some(&converted.sweeps[cut_index]),
    ))
}

impl V4VolumeSolution {
    /// Dealiased grid for a cut of the legacy volume passed to
    /// [`dealias_volume_v4`]. `None` for solutions from
    /// [`crate::dealias_volume`]; use [`V4VolumeSolution::tilt_field`].
    #[cfg_attr(
        recast_legacy_deprecation,
        deprecated(note = "FM301 migration: use V4VolumeSolution::tilt_field")
    )]
    pub fn tilt_grid(&self, cut_index: usize) -> Option<&MomentGrid> {
        self.legacy_tilts.0.get(cut_index)?.as_ref()
    }

    /// Consume the solution, extracting one cut's legacy grid without a
    /// clone. `None` for solutions from [`crate::dealias_volume`]; use
    /// [`V4VolumeSolution::into_tilt_field`].
    #[cfg_attr(
        recast_legacy_deprecation,
        deprecated(note = "FM301 migration: use V4VolumeSolution::into_tilt_field")
    )]
    pub fn into_tilt_grid(mut self, cut_index: usize) -> Option<MomentGrid> {
        self.legacy_tilts.0.get_mut(cut_index)?.take()
    }
}

/// Conversions between the legacy model and the FM301 model for the legacy
/// wrappers of the algorithm crates.
///
/// Geometry is converted with the legacy consumer's own reading of
/// `GateRange`: `first_gate_m` is the range of gate 0 and `gate_spacing_m`
/// the spacing, so every range an algorithm computes is exactly the value it
/// computed from the legacy grid (unlike
/// `recast_radar_core::legacy::volume_from_legacy`, which moves ODIM and
/// CfRadial gates to their true centres). Values keep their legacy
/// semantics: integer codes are `(raw - offset) / scale` with `nodata` and
/// `range_folded` as no value, `F32` grids are physical.
///
/// Names: a [`Naming`] maps each legacy moment to its field name. The
/// default is the design note's 5.4 table ([`field_name`]); the retrieve
/// crate adds its derived-product ids on top.
#[doc(hidden)]
pub mod convert {
    use chrono::{DateTime, Utc};
    use recast_radar_core::legacy::LegacyConvention;
    use recast_radar_core::model::{Location, TimeCoverage};
    use recast_radar_core::{
        ElevationCut, Field, FieldData, FieldName, FloatCoding, GateMapping, GateRange, IntCoding,
        LinearTransform, MomentGrid, MomentStorage, MomentType, RadarVolume, RangeCoord, ScanMode,
        Sweep, SweepMode, Volume,
    };

    /// A legacy moment to field name mapping.
    pub type Naming<'a> = &'a dyn Fn(&MomentType) -> FieldName;

    /// The FM301 field name the wrappers give a legacy moment (design note
    /// 5.4).
    pub fn field_name(moment: &MomentType) -> FieldName {
        moment.to_field_name(LegacyConvention::Generic)
    }

    /// The legacy moment of a converted field's name (the inverse of
    /// [`field_name`]).
    pub fn moment_of(name: &FieldName) -> MomentType {
        name.to_legacy_moment(LegacyConvention::Generic)
    }

    fn range_of(gate_range: &GateRange) -> RangeCoord {
        RangeCoord::Uniform {
            first_center_m: f64::from(gate_range.first_gate_m),
            spacing_m: f64::from(gate_range.gate_spacing_m),
            ngates: u32::try_from(gate_range.gate_count).unwrap_or(u32::MAX),
        }
    }

    /// The sweep mode of a legacy scan mode (`None` and PPI both map to
    /// azimuth surveillance).
    pub fn sweep_mode(scan_mode: Option<ScanMode>) -> SweepMode {
        match scan_mode {
            None | Some(ScanMode::Ppi) => SweepMode::AzimuthSurveillance,
            Some(ScanMode::Rhi) => SweepMode::Rhi,
            Some(ScanMode::VerticalPointing) => SweepMode::VerticalPointing,
            Some(ScanMode::Other) => SweepMode::Other("other".into()),
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

    /// A field over `storage` rows (already in ray order) with `grid`'s
    /// coding, named by `naming`.
    fn field_with(
        grid: &MomentGrid,
        storage: MomentStorage,
        nrays: usize,
        naming: Naming<'_>,
    ) -> Field {
        let ngates = u32::try_from(grid.gate_range.gate_count).unwrap_or(u32::MAX);
        let mut field = Field::new(
            naming(&grid.moment),
            GateMapping::IDENTITY,
            ngates,
            coding_data(grid, storage),
        );
        field.nrays = u32::try_from(nrays).unwrap_or(u32::MAX);
        field
    }

    /// A grid's rows as a field (row `r` of the grid is row `r` of the
    /// field), with the exact legacy value semantics and an identity gate
    /// mapping.
    pub fn field_for_grid(grid: &MomentGrid) -> Field {
        field_with(grid, grid.storage.clone(), grid.radial_count(), &field_name)
    }

    /// A sweep whose rays are `grid`'s rows (row `r` is the cut's radial
    /// `grid.radial_indices[r]`), with the grid as its only field and the
    /// grid's gate geometry as its range. Nyquist velocities come from the
    /// radials; a missing radial gives NaN coordinates.
    pub fn sweep_for_grid(cut: &ElevationCut, grid: &MomentGrid) -> Sweep {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, cut.elevation_deg);
        sweep.elevation_number = cut.elevation_number.map(u16::from);
        push_rays(&mut sweep, cut, &grid.radial_indices);
        sweep.range = range_of(&grid.gate_range);
        sweep.fields.push(field_for_grid(grid));
        sweep
    }

    fn push_rays(sweep: &mut Sweep, cut: &ElevationCut, radials: &[usize]) {
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
    }

    /// The field a grid of a cut becomes on a sweep whose rays are the cut's
    /// radials `ray_radials` (`ray_radials[r]` is ray `r`'s radial): rows are
    /// placed by radial, rays the grid lacks become absent rows, and the
    /// grid's geometry is attached to the sweep's range (which may grow or
    /// refine). `None` when the geometry does not align with the range. The
    /// field is not added to the sweep.
    pub fn field_for_sweep(
        sweep: &mut Sweep,
        ray_radials: &[usize],
        grid: &MomentGrid,
        naming: Naming<'_>,
    ) -> Option<Field> {
        let spacing = f64::from(grid.gate_range.gate_spacing_m);
        let ngates = u32::try_from(grid.gate_range.gate_count).unwrap_or(u32::MAX);
        let unset =
            matches!(sweep.range, RangeCoord::Uniform { spacing_m, .. } if spacing_m <= 0.0);
        let gates = if unset || spacing <= 0.0 {
            if !unset && sweep.range != range_of(&grid.gate_range) {
                return None;
            }
            sweep.range = range_of(&grid.gate_range);
            GateMapping::IDENTITY
        } else {
            sweep
                .attach_geometry(f64::from(grid.gate_range.first_gate_m), spacing, ngates)
                .ok()?
        };
        let nrays = ray_radials.len();
        let mut field = if grid.radial_indices == ray_radials {
            field_with(grid, grid.storage.clone(), nrays, naming)
        } else {
            scattered_field(grid, ray_radials, naming)
        };
        field.gates = gates;
        Some(field)
    }

    /// Add another grid of the same cut to a sweep whose rays are the cut's
    /// radials `ray_radials` ([`field_for_sweep`]). A name the sweep already
    /// has becomes `Other("<Variant>")` with the quantity kept. Returns
    /// `false` (and leaves the sweep unchanged) when the grid's geometry does
    /// not align with the sweep's range.
    pub fn add_grid(
        sweep: &mut Sweep,
        ray_radials: &[usize],
        grid: &MomentGrid,
        naming: Naming<'_>,
    ) -> bool {
        let Some(mut field) = field_for_sweep(sweep, ray_radials, grid, naming) else {
            return false;
        };
        if sweep.field(&field.name).is_some() {
            let (quantity, polarization) = (field.quantity, field.polarization);
            field.name = FieldName::Other(format!("{:?}", grid.moment).into());
            field.quantity = quantity;
            field.polarization = polarization;
        }
        sweep.fields.push(field);
        true
    }

    fn scattered_field(grid: &MomentGrid, ray_radials: &[usize], naming: Naming<'_>) -> Field {
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
            MomentStorage::F32(values) => {
                MomentStorage::F32(gather(values, &rows, gates, f32::NAN))
            }
        };
        let mut field = field_with(grid, storage, nrays, naming);
        field.absent_rows = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_none())
            .map(|(ray, _)| ray as u32)
            .collect();
        field
    }

    /// A sweep with the cut's radials as rays and the moments `wanted`
    /// selects as fields, named by `naming` (grids that do not align with
    /// the sweep's first grid are left out).
    pub fn sweep_for_cut(
        cut: &ElevationCut,
        number: u32,
        mode: SweepMode,
        wanted: &dyn Fn(&MomentType) -> bool,
        naming: Naming<'_>,
    ) -> Sweep {
        let mut sweep = Sweep::new(number, mode, cut.elevation_deg);
        sweep.elevation_number = cut.elevation_number.map(u16::from);
        let radials: Vec<usize> = (0..cut.radials.len()).collect();
        push_rays(&mut sweep, cut, &radials);
        for grid in cut.moments.values().filter(|grid| wanted(&grid.moment)) {
            add_grid(&mut sweep, &radials, grid, naming);
        }
        sweep
    }

    /// A volume holding, for every cut, a sweep with the cut's radials as
    /// rays and the moments `wanted` selects as fields ([`sweep_for_cut`]).
    /// Sweep indices equal cut indices. The volume time is exact:
    /// `time_coverage` starts at the legacy `volume_time`.
    pub fn volume_with(volume: &RadarVolume, wanted: impl Fn(&MomentType) -> bool) -> Volume {
        volume_with_naming(volume, &wanted, &field_name)
    }

    /// [`volume_with`] with a custom naming.
    pub fn volume_with_naming(
        volume: &RadarVolume,
        wanted: &dyn Fn(&MomentType) -> bool,
        naming: Naming<'_>,
    ) -> Volume {
        let mut converted = Volume::new(volume.site.id.clone(), volume.volume_time);
        converted.attrs.site_name = volume.site.name.clone();
        converted.location = Location {
            latitude_deg: volume.site.latitude_deg.map(f64::from),
            longitude_deg: volume.site.longitude_deg.map(f64::from),
            altitude_m: volume.site.elevation_m.map(f64::from),
            altitude_agl_m: None,
        };
        converted.time_coverage = Some(TimeCoverage {
            start: volume.volume_time,
            end: volume.volume_time,
        });
        let mode = sweep_mode(volume.metadata.scan_mode);
        for (index, cut) in volume.cuts.iter().enumerate() {
            converted.sweeps.push(sweep_for_cut(
                cut,
                u32::try_from(index).unwrap_or(u32::MAX),
                mode.clone(),
                wanted,
                naming,
            ));
        }
        converted
    }

    /// The legacy grid of an output field: `template`'s gate range and
    /// radial indices, `moment` as the key. `sweep` is the converted sweep
    /// whose rays are the cut's radials ([`volume_with`]); with `None` the
    /// field's rows are already in `template`'s row order
    /// ([`sweep_for_grid`]).
    pub fn grid_like(
        field: Field,
        moment: MomentType,
        template: &MomentGrid,
        sweep: Option<&Sweep>,
    ) -> MomentGrid {
        let ngates = field.ngates as usize;
        let (scale, offset, nodata, range_folded, storage) = storage_of(field.data);
        let storage = match sweep {
            Some(_)
                if template
                    .radial_indices
                    .iter()
                    .enumerate()
                    .all(|(i, r)| i == *r) =>
            {
                truncate(storage, template.radial_indices.len() * ngates)
            }
            Some(_) => reorder(storage, &template.radial_indices, ngates),
            None => storage,
        };
        MomentGrid {
            moment,
            gate_range: GateRange {
                gate_count: ngates,
                ..template.gate_range.clone()
            },
            scale,
            offset,
            nodata,
            range_folded,
            radial_indices: template.radial_indices.clone(),
            storage,
        }
    }

    /// The legacy grid of an output field of `sweep` (a converted sweep whose
    /// rays are the cut's radials) without a template: the rows the field
    /// provides, in ray order, keyed by their radial index; the gate range
    /// from the field's native geometry, rounded to the metre.
    pub fn grid_from_field(field: Field, moment: MomentType, sweep: &Sweep) -> MomentGrid {
        let ngates = field.ngates as usize;
        let (first_m, spacing_m) = field.native_geometry(&sweep.range).unwrap_or((0.0, 0.0));
        let radial_indices: Vec<usize> = (0..field.nrays as usize)
            .filter(|&row| !field.is_absent(row))
            .collect();
        let compact = radial_indices.len() != field.nrays as usize;
        let (scale, offset, nodata, range_folded, storage) = storage_of(field.data);
        let storage = if compact {
            reorder(storage, &radial_indices, ngates)
        } else {
            storage
        };
        MomentGrid {
            moment,
            gate_range: GateRange {
                first_gate_m: first_m.round() as i32,
                gate_spacing_m: spacing_m.round() as i32,
                gate_count: ngates,
            },
            scale,
            offset,
            nodata,
            range_folded,
            radial_indices,
            storage,
        }
    }

    /// The legacy coding and storage of field data.
    fn storage_of(data: FieldData) -> (f32, f32, Option<u16>, Option<u16>, MomentStorage) {
        match data {
            FieldData::U8 { values, coding } => {
                let (scale, offset) = icd(coding.transform);
                (
                    scale,
                    offset,
                    coding.fill_value.map(u16::from),
                    coding.range_folded.map(u16::from),
                    MomentStorage::U8(values),
                )
            }
            FieldData::U16 { values, coding } => {
                let (scale, offset) = icd(coding.transform);
                (
                    scale,
                    offset,
                    coding.fill_value,
                    coding.range_folded,
                    MomentStorage::U16(values),
                )
            }
            FieldData::F32 { values, .. } => (1.0, 0.0, None, None, MomentStorage::F32(values)),
            other => {
                let physical = Field {
                    data: other,
                    ..Field::new(FieldName::Dbzh, GateMapping::IDENTITY, 0, empty_f32())
                };
                (
                    1.0,
                    0.0,
                    None,
                    None,
                    MomentStorage::F32(physical.to_physical()),
                )
            }
        }
    }

    fn empty_f32() -> FieldData {
        FieldData::F32 {
            values: Vec::new(),
            coding: FloatCoding::default(),
        }
    }

    fn icd(transform: LinearTransform) -> (f32, f32) {
        match transform {
            LinearTransform::IcdScaleOffset { scale, offset } => (scale, offset),
            LinearTransform::CfScaleOffset {
                scale_factor,
                add_offset,
                ..
            } => (
                (1.0 / scale_factor) as f32,
                (-add_offset / scale_factor) as f32,
            ),
        }
    }

    fn truncate(storage: MomentStorage, len: usize) -> MomentStorage {
        match storage {
            MomentStorage::U8(mut values) => {
                values.truncate(len);
                MomentStorage::U8(values)
            }
            MomentStorage::U16(mut values) => {
                values.truncate(len);
                MomentStorage::U16(values)
            }
            MomentStorage::F32(mut values) => {
                values.truncate(len);
                MomentStorage::F32(values)
            }
        }
    }

    fn reorder(storage: MomentStorage, rays: &[usize], gates: usize) -> MomentStorage {
        fn gather<T: Copy + Default>(values: &[T], rays: &[usize], gates: usize) -> Vec<T> {
            let mut out = Vec::with_capacity(rays.len() * gates);
            for &ray in rays {
                match values.get(ray * gates..(ray + 1) * gates) {
                    Some(slice) => out.extend_from_slice(slice),
                    None => out.resize(out.len() + gates, T::default()),
                }
            }
            out
        }
        match storage {
            MomentStorage::U8(values) => MomentStorage::U8(gather(&values, rays, gates)),
            MomentStorage::U16(values) => MomentStorage::U16(gather(&values, rays, gates)),
            MomentStorage::F32(values) => MomentStorage::F32(gather(&values, rays, gates)),
        }
    }

    /// Exact legacy volume time of a converted volume.
    pub fn volume_time(volume: &Volume) -> DateTime<Utc> {
        volume
            .time_coverage
            .map_or(volume.time_reference, |coverage| coverage.start)
    }
}
