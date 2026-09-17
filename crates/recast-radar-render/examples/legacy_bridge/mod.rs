//! Pre-FM301 APIs the render examples still call, behind one seam
//! (docs/design/fm301-model.md section 13.3): Level II decoding
//! (`recast-radar-io-nexrad`) and the `map`, `retrieve`, `filters` and `track`
//! algorithms, which take and return legacy grids. Each example converts here
//! and works on `Volume` / `Field` everywhere else. This module is deleted as
//! those crates migrate.
//!
//! Every example includes it with `#[path = "legacy_bridge/mod.rs"]`, so each
//! uses a subset of it. Developer tooling: a derived grid that does not fit
//! its own sweep's range is a bug in the algorithm, reported by a panic.
#![allow(deprecated, dead_code, unused_imports, clippy::expect_used)]

use std::error::Error;
use std::path::Path;

use recast_radar_core::legacy::{self, LegacyConvention};
use recast_radar_core::{
    Field, FieldData, FieldName, FloatCoding, IntCoding, LinearTransform, MomentGrid,
    MomentStorage, MomentType, Quantity, RadarVolume, RangeCoord, Sweep, Volume,
};

pub use recast_radar_map::ECHO_TOP_THRESHOLD_DBZ;

pub type BoxError = Box<dyn Error>;

/// Decode a Level II file to an FM301 volume. The legacy decode's buffers
/// move into the volume; nothing is copied.
pub fn read_volume(path: &Path) -> Result<Volume, BoxError> {
    let legacy = recast_radar_io_nexrad::decode_volume_from_path(path)?;
    Ok(Volume::try_from(legacy)?)
}

/// [`read_volume`] from bytes already in memory.
pub fn read_volume_bytes(raw: &[u8]) -> Result<Volume, BoxError> {
    let legacy = recast_radar_io_nexrad::decode_volume_from_bytes(raw)?;
    Ok(Volume::try_from(legacy)?)
}

/// The app's preview decode: gzip archives stream and report the first
/// displayable cut, block-bzip2 archives report it from the pipelined decode,
/// anything else decodes whole. `on_first_preview` receives the preview's
/// decoded ray count once.
pub fn read_volume_bytes_with_preview(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_first_preview: impl FnMut(usize),
) -> Result<Volume, BoxError> {
    let mut seen = false;
    let mut on_preview = |preview: RadarVolume| {
        if !seen {
            seen = true;
            on_first_preview(preview.metadata.decoded_radial_count);
        }
    };
    let legacy = if raw.starts_with(&[0x1f, 0x8b]) {
        recast_radar_io_nexrad::decode_gzip_volume_from_bytes_with_preview(
            raw,
            min_displayable_radials,
            on_preview,
        )?
    } else {
        recast_radar_io_nexrad::decode_volume_from_bytes_with_bzip_preview(
            raw,
            min_displayable_radials,
            &mut on_preview,
        )?
    };
    Ok(Volume::try_from(legacy)?)
}

/// The decode split into its stages, for timing: archive normalisation
/// (decompression) and parsing of the normalised bytes.
pub mod stages {
    use super::*;
    pub use recast_radar_io_nexrad::ArchiveCompression;

    pub fn normalize(raw: &[u8]) -> Result<(Vec<u8>, ArchiveCompression), BoxError> {
        Ok(recast_radar_io_nexrad::normalize_archive_bytes(raw)?)
    }

    pub fn parse_normalized(
        normalized: &[u8],
        compression: ArchiveCompression,
    ) -> Result<Volume, BoxError> {
        let legacy =
            recast_radar_io_nexrad::decode_normalized_volume_bytes(normalized, compression)?;
        Ok(Volume::try_from(legacy)?)
    }

    pub fn read_gzip_stream(reader: impl std::io::Read) -> Result<Volume, BoxError> {
        let legacy = recast_radar_io_nexrad::decode_gzip_volume_from_reader(reader)?;
        Ok(Volume::try_from(legacy)?)
    }
}

/// A field derived by a legacy algorithm, with the range coordinate its gate
/// mapping refers to (the sweep's range, refined or lengthened when the
/// product is finer or reaches further than the sweep's own fields).
pub struct Derived {
    pub field: Field,
    pub range: RangeCoord,
}

/// A decoded volume kept beside its legacy form, for the algorithms that
/// still take legacy volumes and grids. `volume` is the FM301 form; the
/// legacy form is a clone made once at decode.
pub struct Decoded {
    pub volume: Volume,
    legacy: RadarVolume,
}

impl Decoded {
    pub fn from_path(path: &Path) -> Result<Self, BoxError> {
        Self::from_legacy(recast_radar_io_nexrad::decode_volume_from_path(path)?)
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, BoxError> {
        Self::from_legacy(recast_radar_io_nexrad::decode_volume_from_bytes(raw)?)
    }

    fn from_legacy(legacy: RadarVolume) -> Result<Self, BoxError> {
        Ok(Self {
            volume: Volume::try_from(legacy.clone())?,
            legacy,
        })
    }

    /// Lowest sweep (fixed angle, then index) with a field of `quantity`.
    pub fn lowest_sweep_with(&self, quantity: Quantity) -> Option<usize> {
        lowest_sweep_with(&self.volume, quantity)
    }

    /// Composite (column-maximum) reflectivity on the base reflectivity
    /// sweep's rays, named `CREF`.
    pub fn composite_reflectivity(&self, sweep: usize) -> Option<Derived> {
        recast_radar_map::composite_reflectivity_grid(&self.legacy)
            .map(|grid| self.derived(sweep, &grid, "CREF"))
    }

    /// Echo tops (metres above the radar) of `threshold_dbz`, named `ET`.
    pub fn echo_tops(&self, sweep: usize, threshold_dbz: f32) -> Option<Derived> {
        recast_radar_map::echo_top_grid(&self.legacy, threshold_dbz)
            .map(|grid| self.derived(sweep, &grid, "ET"))
    }

    /// Vertically integrated liquid (kg m-2), named `VIL`.
    pub fn vil(&self, sweep: usize) -> Option<Derived> {
        recast_radar_map::vil_grid(&self.legacy).map(|grid| self.derived(sweep, &grid, "VIL"))
    }

    /// VIL density, named `VILD`.
    pub fn vil_density(&self, sweep: usize) -> Option<Derived> {
        recast_radar_map::vil_density_grid(&self.legacy)
            .map(|grid| self.derived(sweep, &grid, "VILD"))
    }

    /// Maximum expected hail size, named `MEHS`.
    pub fn mehs(
        &self,
        sweep: usize,
        freezing_level_m: f32,
        minus20c_level_m: f32,
    ) -> Option<Derived> {
        recast_radar_map::mehs_grid(&self.legacy, freezing_level_m, minus20c_level_m)
            .map(|grid| self.derived(sweep, &grid, "MEHS"))
    }

    /// A vertical reflectivity cross-section between two ground points.
    pub fn reflectivity_cross_section(
        &self,
        start_km: (f32, f32),
        end_km: (f32, f32),
        width: usize,
        height: usize,
        top_m: f32,
    ) -> Option<recast_radar_map::CrossSection> {
        recast_radar_map::reflectivity_cross_section(
            &self.legacy,
            start_km,
            end_km,
            width,
            height,
            top_m,
        )
    }

    /// LLSD azimuthal shear of the sweep's radial velocity, named `AZSHEAR`.
    pub fn azimuthal_shear(&self, sweep: usize) -> Option<Derived> {
        let (cut, grid) = self.velocity_cut(sweep)?;
        let shear = recast_radar_retrieve::azimuthal_shear_grid(cut, grid);
        Some(self.derived(sweep, &shear, "AZSHEAR"))
    }

    /// Radial divergence of the sweep's radial velocity, named `RDIV`.
    pub fn radial_divergence(&self, sweep: usize) -> Option<Derived> {
        let (cut, grid) = self.velocity_cut(sweep)?;
        let divergence = recast_radar_retrieve::radial_divergence_grid(cut, grid);
        Some(self.derived(sweep, &divergence, "RDIV"))
    }

    /// The display-smoothed copy of a sweep field, named `<NAME>_SMOOTH`.
    pub fn smoothed(&self, sweep: usize, name: &FieldName) -> Option<Derived> {
        let grid = self.grid(sweep, name)?;
        let smoothed = recast_radar_filters::smooth_moment_grid(grid);
        Some(self.derived(sweep, &smoothed, &format!("{}_SMOOTH", name.as_str())))
    }

    /// The display-upsampled copy of a sweep field (more rows than the sweep,
    /// on its own azimuths), named `<NAME>_DISPLAY`.
    pub fn upsampled(&self, sweep: usize, name: &FieldName) -> Option<(Derived, Vec<f32>)> {
        let grid = self.grid(sweep, name)?;
        let up = recast_radar_filters::upsample_moment_grid(&self.legacy.cuts[sweep], grid)?;
        let derived = self.derived_rows(sweep, &up.grid, &format!("{}_DISPLAY", name.as_str()));
        Some((derived, up.row_azimuths_deg))
    }

    fn velocity_cut(&self, sweep: usize) -> Option<(&legacy::ElevationCut, &MomentGrid)> {
        let cut = self.legacy.cuts.get(sweep)?;
        let grid = cut.moments.get(&MomentType::Velocity)?;
        Some((cut, grid))
    }

    fn grid(&self, sweep: usize, name: &FieldName) -> Option<&MomentGrid> {
        let moment = name.to_legacy_moment(LegacyConvention::Nexrad);
        self.legacy.cuts.get(sweep)?.moments.get(&moment)
    }

    /// A derived grid on `sweep`'s rays as a field named `name`.
    fn derived(&self, sweep: usize, grid: &MomentGrid, name: &str) -> Derived {
        let nrays = self.volume.sweeps[sweep].nrays();
        assert_eq!(
            grid.radial_indices.len(),
            nrays,
            "{name}: derived grid rows must be the sweep's rays"
        );
        assert!(
            grid.radial_indices.iter().enumerate().all(|(i, r)| i == *r),
            "{name}: derived grid rows must be in ray order"
        );
        self.derived_rows(sweep, grid, name)
    }

    /// A derived grid as a field of `grid.radial_indices.len()` rows, whether
    /// or not they are the sweep's rays.
    fn derived_rows(&self, sweep: usize, grid: &MomentGrid, name: &str) -> Derived {
        let mut scratch = self.volume.sweeps[sweep].clone();
        scratch.fields.clear();
        let field = field_from_grid(grid, FieldName::parse(name), &mut scratch);
        Derived {
            field,
            range: scratch.range,
        }
    }
}

impl Derived {
    /// Add the derived field to `sweep` (whose rays it lies on) and seal the
    /// sweep. The field's geometry is re-attached to the sweep's range, which
    /// refines or lengthens it when the product is finer or reaches further
    /// than the sweep's own fields; those keep their gates through their
    /// mappings. Returns the field's name.
    pub fn add_to(self, sweep: &mut Sweep) -> Result<FieldName, BoxError> {
        let Derived { mut field, range } = self;
        let (first_center_m, spacing_m) = field
            .native_geometry(&range)
            .ok_or("derived field has no gate geometry")?;
        field.gates = sweep.attach_geometry(first_center_m, spacing_m, field.ngates)?;
        let name = field.name.clone();
        sweep.add_field(field)?;
        sweep.seal()?;
        Ok(name)
    }
}

/// Lowest sweep (fixed angle, then index) with rows of a `quantity` field.
pub fn lowest_sweep_with(volume: &Volume, quantity: Quantity) -> Option<usize> {
    volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, sweep)| {
            sweep
                .find(quantity)
                .is_some_and(|field| field.nrays as usize > field.absent_rows.len())
        })
        .min_by(|(li, ls), (ri, rs)| {
            ls.fixed_angle_deg
                .total_cmp(&rs.fixed_angle_deg)
                .then_with(|| li.cmp(ri))
        })
        .map(|(index, _)| index)
}

/// A legacy grid as a field named `name`, its geometry attached to `sweep`
/// (which refines or lengthens the sweep's range as needed). The grid's
/// rows are taken as they are.
pub fn field_from_grid(grid: &MomentGrid, name: FieldName, sweep: &mut Sweep) -> Field {
    let gates = sweep
        .attach_geometry(
            f64::from(grid.gate_range.first_gate_m),
            f64::from(grid.gate_range.gate_spacing_m),
            grid.gate_range.gate_count as u32,
        )
        .expect("derived grid gates align with the sweep range");
    let transform = LinearTransform::IcdScaleOffset {
        scale: grid.scale,
        offset: grid.offset,
    };
    let data = match &grid.storage {
        MomentStorage::U8(values) => FieldData::U8 {
            values: values.clone(),
            coding: IntCoding {
                transform,
                fill_value: grid.nodata.and_then(|code| u8::try_from(code).ok()),
                undetect: None,
                range_folded: grid.range_folded.and_then(|code| u8::try_from(code).ok()),
                valid_range: None,
            },
        },
        MomentStorage::U16(values) => FieldData::U16 {
            values: values.clone(),
            coding: IntCoding {
                transform,
                fill_value: grid.nodata,
                undetect: None,
                range_folded: grid.range_folded,
                valid_range: None,
            },
        },
        MomentStorage::F32(values) => FieldData::F32 {
            values: values.clone(),
            coding: FloatCoding::default(),
        },
    };
    Field::new(name, gates, grid.gate_range.gate_count as u32, data)
}

/// Max-value swaths over a loop of volumes (`recast-radar-track`).
pub mod swath {
    use super::*;
    pub use recast_radar_track::SwathAggregation;

    /// The base tilt of `quantity`: the lowest sweep carrying it.
    pub fn base_tilt_sweep(volume: &Volume, quantity: Quantity) -> Option<usize> {
        lowest_sweep_with(volume, quantity)
    }

    /// The per-gate maximum of `quantity` across every frame's base tilt, as
    /// a one-sweep volume whose field keeps the quantity's name.
    pub fn max_value_swath(
        frames: &[&Decoded],
        quantity: Quantity,
        aggregation: SwathAggregation,
    ) -> Option<Volume> {
        let moment = match quantity {
            Quantity::Reflectivity => MomentType::Reflectivity,
            Quantity::RadialVelocity => MomentType::Velocity,
            _ => return None,
        };
        let legacy: Vec<&RadarVolume> = frames.iter().map(|frame| &frame.legacy).collect();
        let swath = recast_radar_track::max_value_swath(&legacy, moment, aggregation)?;
        Volume::try_from(swath).ok()
    }
}
