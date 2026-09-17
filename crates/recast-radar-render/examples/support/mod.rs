//! Shared helpers of the render examples: Level II decoding
//! (`recast-radar-io-nexrad`) and the `map`, `retrieve`, `filters` and
//! `track` products the examples draw, each returned with the range
//! coordinate its gate mapping refers to. Every example includes this module
//! with `#[path = "support/mod.rs"]`, so each uses a subset of it. Developer
//! tooling: a derived field that does not fit its own sweep's range is a bug
//! in the algorithm, reported by a panic.
#![allow(dead_code, unused_imports, clippy::expect_used)]

use std::error::Error;
use std::path::Path;

use recast_radar_core::{Field, FieldName, Quantity, RangeCoord, Sweep, Volume};

pub use recast_radar_map::ECHO_TOP_THRESHOLD_DBZ;

pub type BoxError = Box<dyn Error>;

/// Decode a Level II file.
pub fn read_volume(path: &Path) -> Result<Volume, BoxError> {
    Ok(recast_radar_io_nexrad::read_volume_from_path(path)?)
}

/// [`read_volume`] from bytes already in memory.
pub fn read_volume_bytes(raw: &[u8]) -> Result<Volume, BoxError> {
    Ok(recast_radar_io_nexrad::read_volume_from_bytes(raw)?)
}

/// The app's preview decode: gzip archives stream and report the first
/// displayable sweep, block-bzip2 archives report it from the pipelined
/// decode, anything else decodes whole. `on_first_preview` receives the
/// preview's decoded ray count once.
pub fn read_volume_bytes_with_preview(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_first_preview: impl FnMut(usize),
) -> Result<Volume, BoxError> {
    let mut seen = false;
    let mut on_preview = |preview: Volume| {
        if !seen {
            seen = true;
            on_first_preview(preview.provenance.decode.decoded_ray_count);
        }
    };
    let volume = if raw.starts_with(&[0x1f, 0x8b]) {
        recast_radar_io_nexrad::read_gzip_volume_from_bytes_with_preview(
            raw,
            min_displayable_radials,
            on_preview,
        )?
    } else {
        recast_radar_io_nexrad::read_volume_from_bytes_with_bzip_preview(
            raw,
            min_displayable_radials,
            &mut on_preview,
        )?
    };
    Ok(volume)
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
        Ok(recast_radar_io_nexrad::read_normalized_volume_bytes(
            normalized,
            compression,
        )?)
    }

    pub fn read_gzip_stream(reader: impl std::io::Read) -> Result<Volume, BoxError> {
        Ok(recast_radar_io_nexrad::read_gzip_volume_from_reader(
            reader,
        )?)
    }
}

/// A derived field with the range coordinate its gate mapping refers to (the
/// range of the sweep the product was computed on).
pub struct Derived {
    pub field: Field,
    pub range: RangeCoord,
}

/// A decoded volume with the derived products the examples draw.
pub struct Decoded {
    pub volume: Volume,
}

impl Decoded {
    pub fn from_path(path: &Path) -> Result<Self, BoxError> {
        Ok(Self {
            volume: read_volume(path)?,
        })
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, BoxError> {
        Ok(Self {
            volume: read_volume_bytes(raw)?,
        })
    }

    /// Lowest sweep (fixed angle, then index) with a field of `quantity`.
    pub fn lowest_sweep_with(&self, quantity: Quantity) -> Option<usize> {
        lowest_sweep_with(&self.volume, quantity)
    }

    /// Composite (column-maximum) reflectivity on the base reflectivity
    /// sweep's rays and gates (`CREF`). `sweep` must be that base sweep.
    pub fn composite_reflectivity(&self, sweep: usize) -> Option<Derived> {
        let field = recast_radar_map::composite_reflectivity(&self.volume)?;
        Some(self.derived(sweep, field))
    }

    /// Echo tops (metres above the radar) of `threshold_dbz` (`ET`).
    pub fn echo_tops(&self, sweep: usize, threshold_dbz: f32) -> Option<Derived> {
        let field = recast_radar_map::echo_top(&self.volume, threshold_dbz)?;
        Some(self.derived(sweep, field))
    }

    /// Vertically integrated liquid (kg m-2) (`VIL`).
    pub fn vil(&self, sweep: usize) -> Option<Derived> {
        let field = recast_radar_map::vil(&self.volume)?;
        Some(self.derived(sweep, field))
    }

    /// VIL density (`VILD`).
    pub fn vil_density(&self, sweep: usize) -> Option<Derived> {
        let field = recast_radar_map::vil_density(&self.volume)?;
        Some(self.derived(sweep, field))
    }

    /// Maximum expected hail size (`MEHS`).
    pub fn mehs(
        &self,
        sweep: usize,
        freezing_level_m: f32,
        minus20c_level_m: f32,
    ) -> Option<Derived> {
        let field = recast_radar_map::mehs(&self.volume, freezing_level_m, minus20c_level_m)?;
        Some(self.derived(sweep, field))
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
        recast_radar_map::reflectivity_section(&self.volume, start_km, end_km, width, height, top_m)
    }

    /// LLSD azimuthal shear of the sweep's radial velocity.
    pub fn azimuthal_shear(&self, sweep: usize) -> Option<Derived> {
        let (model, velocity) = self.velocity(sweep)?;
        let shear = recast_radar_retrieve::azimuthal_shear(model, velocity);
        Some(self.derived(sweep, shear))
    }

    /// Radial divergence of the sweep's radial velocity.
    pub fn radial_divergence(&self, sweep: usize) -> Option<Derived> {
        let (model, velocity) = self.velocity(sweep)?;
        let divergence = recast_radar_retrieve::radial_divergence(model, velocity);
        Some(self.derived(sweep, divergence))
    }

    /// The display-smoothed copy of a sweep field.
    pub fn smoothed(&self, sweep: usize, name: &FieldName) -> Option<Derived> {
        let field = self.volume.sweeps.get(sweep)?.field(name)?;
        let smoothed = recast_radar_filters::smooth_field(field);
        Some(self.derived(sweep, smoothed))
    }

    /// The display-upsampled copy of a sweep field: a sweep with more rays
    /// than the source, on its own azimuths, holding the upsampled field.
    pub fn upsampled(
        &self,
        sweep: usize,
        name: &FieldName,
    ) -> Option<recast_radar_filters::UpsampledSweep> {
        let model = self.volume.sweeps.get(sweep)?;
        let field = model.field(name)?;
        recast_radar_filters::upsample_field(model, field)
    }

    fn velocity(&self, sweep: usize) -> Option<(&Sweep, &Field)> {
        let model = self.volume.sweeps.get(sweep)?;
        let field = model.find(Quantity::RadialVelocity)?;
        Some((model, field))
    }

    /// A derived field on `sweep`'s rays.
    fn derived(&self, sweep: usize, field: Field) -> Derived {
        let model = &self.volume.sweeps[sweep];
        assert_eq!(
            field.nrays as usize,
            model.nrays(),
            "{}: derived field rows must be the sweep's rays",
            field.name.as_str()
        );
        Derived {
            field,
            range: model.range.clone(),
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
        let first = frames.first()?;
        let sweep = first.lowest_sweep_with(quantity)?;
        let name = first.volume.sweeps[sweep].find(quantity)?.name.clone();
        let volumes: Vec<&Volume> = frames.iter().map(|frame| &frame.volume).collect();
        recast_radar_track::value_swath(&volumes, &name, aggregation)
    }
}
