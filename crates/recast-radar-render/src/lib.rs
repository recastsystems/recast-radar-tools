//! 2D radar rendering contracts.
//!
//! The long-term renderer will be GPU-backed, but this crate already provides a
//! CPU raster path for smoke tests, screenshots, and early visual validation.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::f32::consts::PI;
use std::ops::Range;
use std::path::Path;

pub mod color;

pub use color::{ColorSampler, ColorTable, ColorTableFamily, ColorTableSet};
use image::{ImageBuffer, ImageError, Rgba};
use rayon::prelude::*;
use recast_radar_core::{
    ElevationCut, GateRange, MomentGrid, MomentStorage, MomentType, ProductId, RadarVolume,
};
use recast_radar_correct::dealias_velocity_grid;
use thiserror::Error;

const AZIMUTH_BINS: usize = 3600;
const AZIMUTH_BIN_WIDTH_DEG: f32 = 0.1;
const MAX_AZIMUTH_HALF_WIDTH_DEG: f32 = 3.0;
const MAX_AZIMUTH_CANDIDATES: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderLayer {
    pub product: ProductId,
    pub moment: Option<MomentType>,
    pub visible: bool,
}

impl RenderLayer {
    pub fn base(moment: MomentType) -> Self {
        Self {
            product: ProductId::from(moment.clone()),
            moment: Some(moment),
            visible: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterOptions {
    pub width: u32,
    pub height: u32,
    pub range_fraction: u8,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 1024,
            range_fraction: 94,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportRasterOptions {
    pub width: u32,
    pub height: u32,
    pub radar_x_px: f32,
    pub radar_y_px: f32,
    pub km_per_px_x: f32,
    pub km_per_px_y: f32,
    /// Clockwise screen rotation of local north at the radar (radians) —
    /// the AEQD meridian-convergence angle. Baked into the per-pixel
    /// azimuth so the raster fills the whole rect with no draw-time
    /// rotation cutoff; range is rotation-invariant so the row-pruning
    /// optimizations are unaffected.
    pub rotation_rad: f32,
}

impl ViewportRasterOptions {
    /// Hard ceiling on either raster dimension. 4096²·4 = 64 MiB per RGBA
    /// frame — a single-allocation safety cap that also stays within the
    /// common wgpu 2D texture limit. The supersample never pushes a raster
    /// past this; the loop-total memory is deliberately NOT capped here (the
    /// caller's budget owns that).
    pub const MAX_SUPERSAMPLED_DIMENSION: u32 = 4096;

    /// Return these options scaled up by an integer supersample `factor`
    /// (1/2/4 for Standard/High/Ultra): more raster pixels over the SAME map
    /// rect, so the polar data is sampled finer (genuine detail, not upscaling).
    ///
    /// The scale is uniform on both axes and reduced from `factor` only so far
    /// as needed to keep each dimension `<= MAX_SUPERSAMPLED_DIMENSION`. Because
    /// `width · km_per_px` (the ground the raster covers) is held invariant —
    /// width scales up by `s`, `km_per_px` down by `s` — the draw-time
    /// re-projection in `anchored_radar_texture_rect` cancels the supersample,
    /// so placement is unchanged and only sharpness improves.
    ///
    /// `factor <= 1` returns `self` untouched (bit-identical to Standard). A
    /// base raster that already exceeds the ceiling is left as-is (never
    /// downscaled), preserving today's behavior on very large windows.
    pub fn supersampled(self, factor: u32) -> Self {
        if factor <= 1 {
            return self;
        }
        let base_max = self.width.max(self.height);
        if base_max == 0 {
            return self;
        }
        let cap_scale = Self::MAX_SUPERSAMPLED_DIMENSION as f32 / base_max as f32;
        // Never below 1.0 (no downscaling), never above the requested factor.
        let scale = (factor as f32).min(cap_scale.max(1.0));
        if scale <= 1.0 {
            return self;
        }
        Self {
            width: ((self.width as f32) * scale).round().max(1.0) as u32,
            height: ((self.height as f32) * scale).round().max(1.0) as u32,
            radar_x_px: self.radar_x_px * scale,
            radar_y_px: self.radar_y_px * scale,
            km_per_px_x: self.km_per_px_x / scale,
            km_per_px_y: self.km_per_px_y / scale,
            rotation_rad: self.rotation_rad,
        }
    }
}

pub fn viewport_rgba_buffer_len(options: ViewportRasterOptions) -> usize {
    let (width, height) = viewport_dimensions(options);
    rgba_len(width, height)
}

pub fn viewport_sample_cache_storage_upper_bound(options: ViewportRasterOptions) -> usize {
    let (width, height) = viewport_dimensions(options);
    (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(std::mem::size_of::<CachedSample>())
        .saturating_add((height as usize).saturating_mul(std::mem::size_of::<CachedRowSpan>()))
}

pub fn viewport_sample_cache_storage_upper_bound_for_grid(
    grid: &MomentGrid,
    options: ViewportRasterOptions,
) -> usize {
    let (_, height) = viewport_dimensions(options);
    let geometry = viewport_geometry(grid, options);
    let sample_slots = (0..height)
        .filter_map(|y| geometry.x_range_for_row(y))
        .map(|range| range.len())
        .sum::<usize>();
    sample_slots
        .saturating_mul(std::mem::size_of::<CachedSample>())
        .saturating_add((height as usize).saturating_mul(std::mem::size_of::<CachedRowSpan>()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StormMotion {
    pub direction_deg: f32,
    pub speed_mps: f32,
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("cut index {index} is out of range for {cut_count} cuts")]
    CutOutOfRange { index: usize, cut_count: usize },
    #[error("moment {moment} is not available in cut {cut_index}")]
    MissingMoment {
        cut_index: usize,
        moment: MomentType,
    },
    #[error("moment {moment} in cut {cut_index} has no decoded rows")]
    EmptyMoment {
        cut_index: usize,
        moment: MomentType,
    },
    #[error("RGBA buffer has {actual} bytes, expected {expected} for {width}x{height}")]
    BufferSizeMismatch {
        actual: usize,
        expected: usize,
        width: u32,
        height: u32,
    },
    #[error("viewport render cache belongs to a different radar volume")]
    CacheVolumeMismatch,
    #[error("viewport render cache is for cut {actual}, expected cut {expected}")]
    CacheCutMismatch { expected: usize, actual: usize },
    #[error("viewport render cache is for {actual}, expected {expected}")]
    CacheMomentMismatch {
        expected: MomentType,
        actual: MomentType,
    },
    #[error("viewport render cache storage no longer matches the moment storage")]
    CacheStorageMismatch,
    #[error("viewport geometry cache does not match this moment grid")]
    GeometryCacheMismatch,
    #[error("image write failed: {0}")]
    Image(#[from] ImageError),
}

pub type Result<T> = std::result::Result<T, RenderError>;

/// Wrap a rendered RGBA pixel buffer as an image, reporting a size mismatch
/// as [`RenderError::BufferSizeMismatch`].
fn rgba_image(width: u32, height: u32, pixels: Vec<u8>) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let actual = pixels.len();
    ImageBuffer::from_raw(width, height, pixels).ok_or(RenderError::BufferSizeMismatch {
        actual,
        expected: (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4),
        width,
        height,
    })
}

/// Render a decoded polar moment to a simple radar PNG.
pub fn render_moment_png(
    volume: &RadarVolume,
    cut_index: usize,
    moment: MomentType,
    out_path: &Path,
    options: RasterOptions,
) -> Result<()> {
    let image = render_moment_image(volume, cut_index, moment, options)?;
    image.save(out_path)?;
    Ok(())
}

pub fn render_moment_image(
    volume: &RadarVolume,
    cut_index: usize,
    moment: MomentType,
    options: RasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let cut = volume
        .cuts
        .get(cut_index)
        .ok_or(RenderError::CutOutOfRange {
            index: cut_index,
            cut_count: volume.cuts.len(),
        })?;
    let grid = cut
        .moments
        .get(&moment)
        .ok_or_else(|| RenderError::MissingMoment {
            cut_index,
            moment: moment.clone(),
        })?;

    if grid.radial_indices.is_empty() {
        return Err(RenderError::EmptyMoment { cut_index, moment });
    }

    let row_lookup = AzimuthLookup::new(cut, grid);
    let width = options.width.max(64);
    let height = options.height.max(64);
    let center_x = (width as f32 - 1.0) / 2.0;
    let center_y = (height as f32 - 1.0) / 2.0;
    let radius_px = center_x.min(center_y) * (f32::from(options.range_fraction) / 100.0);
    let max_range_m = max_range_m(grid).max(1.0);

    let mut pixels = vec![0; width as usize * height as usize * 4];
    let color_tables = ColorTableSet::default();
    let validation_table = validation_color_table_for_moment(&grid.moment);
    let color_table = validation_table
        .as_ref()
        .unwrap_or_else(|| color_tables.for_family(color_family_for_moment(&grid.moment)));

    match &grid.storage {
        MomentStorage::U8(values) => {
            let palette = build_u8_palette(grid, color_table);
            render_compact_storage(
                &mut pixels,
                values,
                &palette,
                grid,
                &row_lookup,
                RasterGeometry {
                    width,
                    center_x,
                    center_y,
                    radius_px,
                    radius_sq_px: radius_px * radius_px,
                    max_range_m,
                },
                false,
            );
        }
        MomentStorage::U16(values) => {
            let palette = build_u16_palette(grid, color_table);
            render_compact_storage(
                &mut pixels,
                values,
                &palette,
                grid,
                &row_lookup,
                RasterGeometry {
                    width,
                    center_x,
                    center_y,
                    radius_px,
                    radius_sq_px: radius_px * radius_px,
                    max_range_m,
                },
                false,
            );
        }
        MomentStorage::F32(values) => render_f32_storage(
            &mut pixels,
            values,
            grid,
            &row_lookup,
            color_table,
            RasterGeometry {
                width,
                center_x,
                center_y,
                radius_px,
                radius_sq_px: radius_px * radius_px,
                max_range_m,
            },
            false,
        ),
    }

    rgba_image(width, height, pixels)
}

pub fn render_moment_viewport_image(
    volume: &RadarVolume,
    cut_index: usize,
    moment: MomentType,
    options: ViewportRasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let (width, height, pixels) = render_moment_viewport_rgba(volume, cut_index, moment, options)?;
    rgba_image(width, height, pixels)
}

pub fn render_moment_viewport_rgba(
    volume: &RadarVolume,
    cut_index: usize,
    moment: MomentType,
    options: ViewportRasterOptions,
) -> Result<(u32, u32, Vec<u8>)> {
    let (width, height) = viewport_dimensions(options);
    let mut pixels = vec![0; rgba_len(width, height)];
    render_moment_viewport_rgba_into(volume, cut_index, moment, options, &mut pixels)?;
    Ok((width, height, pixels))
}

pub fn render_moment_viewport_rgba_into(
    volume: &RadarVolume,
    cut_index: usize,
    moment: MomentType,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
) -> Result<(u32, u32)> {
    let cache = ViewportMomentCache::new(volume, cut_index, moment)?;
    cache.render_moment_rgba_into(volume, options, pixels)
}

pub struct ViewportMomentCache {
    volume_ptr: usize,
    cut_index: usize,
    moment: MomentType,
    row_lookup: AzimuthLookup,
    color_lookup: CachedColorLookup,
    storm_motion_basis: Option<StormMotionBasis>,
    dealiased_grid: Option<MomentGrid>,
}

pub struct ViewportSampleCache {
    volume_ptr: usize,
    cut_index: usize,
    moment: MomentType,
    width: u32,
    height: u32,
    sample_count: usize,
    row_spans: Vec<CachedRowSpan>,
    samples: Vec<CachedSample>,
}

pub struct ViewportGeometryCache {
    width: u32,
    height: u32,
    gate_range: GateRange,
    sample_count: usize,
    row_spans: Vec<CachedRowSpan>,
    samples: Vec<CachedSample>,
}

pub struct StormRelativePaletteCache {
    volume_ptr: usize,
    cut_index: usize,
    row_palettes: Vec<[[u8; 4]; 256]>,
}

impl ViewportSampleCache {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    pub fn storage_bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<CachedSample>()
            + self.row_spans.len() * std::mem::size_of::<CachedRowSpan>()
    }

    fn geometry(&self) -> CachedViewportGeometry<'_> {
        CachedViewportGeometry {
            row_spans: &self.row_spans,
            samples: &self.samples,
        }
    }
}

impl ViewportGeometryCache {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    pub fn storage_bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<CachedSample>()
            + self.row_spans.len() * std::mem::size_of::<CachedRowSpan>()
    }

    fn geometry(&self) -> CachedViewportGeometry<'_> {
        CachedViewportGeometry {
            row_spans: &self.row_spans,
            samples: &self.samples,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CachedRowSpan {
    start: u32,
    end: u32,
    sample_offset: usize,
}

impl CachedRowSpan {
    fn empty() -> Self {
        Self {
            start: 0,
            end: 0,
            sample_offset: 0,
        }
    }

    fn range(self) -> Option<Range<u32>> {
        (self.start < self.end).then_some(self.start..self.end)
    }
}

struct CachedRowBuild {
    start: u32,
    samples: Vec<CachedSample>,
    sample_count: usize,
}

impl CachedRowBuild {
    fn empty() -> Self {
        Self {
            start: 0,
            samples: Vec::new(),
            sample_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CachedSample(u32);

impl CachedSample {
    const GATE_BITS: u32 = 16;
    const GATE_MASK: u32 = (1 << Self::GATE_BITS) - 1;
    const SKIP_FLAG: u32 = 1 << 31;
    const SKIP_MASK: u32 = Self::SKIP_FLAG - 1;
    const ROW_LIMIT: usize = 1 << (u32::BITS - Self::GATE_BITS - 1);

    fn new(sample: ResolvedSample) -> Option<Self> {
        if sample.row >= Self::ROW_LIMIT || sample.gate > Self::GATE_MASK as usize {
            return None;
        }
        Some(Self(
            ((sample.row as u32) << Self::GATE_BITS) | sample.gate as u32,
        ))
    }

    fn skip(pixel_count: u32) -> Option<Self> {
        (pixel_count > 0 && pixel_count <= Self::SKIP_MASK)
            .then_some(Self(Self::SKIP_FLAG | pixel_count))
    }

    #[cfg(test)]
    fn sample(self) -> Option<ResolvedSample> {
        (!self.is_skip()).then_some(ResolvedSample {
            row: (self.0 >> Self::GATE_BITS) as usize,
            gate: (self.0 & Self::GATE_MASK) as usize,
        })
    }

    #[inline]
    fn is_skip(self) -> bool {
        self.0 & Self::SKIP_FLAG != 0
    }

    #[inline]
    fn skip_len(self) -> Option<u32> {
        self.is_skip().then_some(self.0 & Self::SKIP_MASK)
    }

    #[inline]
    fn row(self) -> usize {
        (self.0 >> Self::GATE_BITS) as usize
    }

    #[inline]
    fn gate(self) -> usize {
        (self.0 & Self::GATE_MASK) as usize
    }
}

// Display interpolation (`recast_radar_filters::upsample_moment_grid`) sizes
// its grids to this packing and to the azimuth lookup's fill width; the
// test build fails if the two crates drift apart.
#[cfg(test)]
const _: () = {
    assert!(recast_radar_filters::INTERP_ROW_LIMIT == CachedSample::ROW_LIMIT);
    assert!(recast_radar_filters::INTERP_MAX_GATES == CachedSample::GATE_MASK as usize);
    assert!(recast_radar_filters::INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG == MAX_AZIMUTH_HALF_WIDTH_DEG);
};

struct StormMotionBasis {
    beam_cos: Vec<f32>,
    beam_sin: Vec<f32>,
}

impl StormMotionBasis {
    fn new(cut: &ElevationCut, grid: &MomentGrid) -> Self {
        let mut beam_cos = Vec::with_capacity(grid.radial_indices.len());
        let mut beam_sin = Vec::with_capacity(grid.radial_indices.len());
        for radial_index in &grid.radial_indices {
            let azimuth_rad = cut
                .radials
                .get(*radial_index)
                .map(|radial| radial.azimuth_deg.to_radians())
                .unwrap_or(0.0);
            beam_cos.push(azimuth_rad.cos());
            beam_sin.push(azimuth_rad.sin());
        }
        Self { beam_cos, beam_sin }
    }

    fn row_motion_components(&self, storm_motion: StormMotion) -> Vec<f32> {
        let direction_rad = storm_motion.direction_deg.to_radians();
        let storm_cos = storm_motion.speed_mps * direction_rad.cos();
        let storm_sin = storm_motion.speed_mps * direction_rad.sin();
        self.beam_cos
            .iter()
            .zip(&self.beam_sin)
            .map(|(beam_cos, beam_sin)| storm_cos * *beam_cos + storm_sin * *beam_sin)
            .collect()
    }
}

enum CachedColorLookup {
    U8 {
        palette: Box<[[u8; 4]; 256]>,
        color_table: ColorTable,
    },
    U16 {
        palette: Vec<[u8; 4]>,
        color_table: ColorTable,
    },
    F32 {
        color_table: ColorTable,
    },
}

impl CachedColorLookup {
    fn new(grid: &MomentGrid, color_tables: &ColorTableSet) -> Self {
        Self::new_for_family(grid, color_tables, color_family_for_moment(&grid.moment))
    }

    fn new_for_family(
        grid: &MomentGrid,
        color_tables: &ColorTableSet,
        family: ColorTableFamily,
    ) -> Self {
        let color_table = validation_color_table_for_moment(&grid.moment)
            .unwrap_or_else(|| color_tables.for_family(family).clone());
        match &grid.storage {
            MomentStorage::U8(_) => Self::U8 {
                palette: Box::new(build_u8_palette(grid, &color_table)),
                color_table,
            },
            MomentStorage::U16(_) => Self::U16 {
                palette: build_u16_palette(grid, &color_table),
                color_table,
            },
            MomentStorage::F32(_) => Self::F32 { color_table },
        }
    }

    fn color_table(&self) -> &ColorTable {
        match self {
            Self::U8 { color_table, .. }
            | Self::U16 { color_table, .. }
            | Self::F32 { color_table } => color_table,
        }
    }
}

impl ViewportMomentCache {
    pub fn new(volume: &RadarVolume, cut_index: usize, moment: MomentType) -> Result<Self> {
        Self::new_with_color_tables(volume, cut_index, moment, &ColorTableSet::default())
    }

    pub fn new_with_color_tables(
        volume: &RadarVolume,
        cut_index: usize,
        moment: MomentType,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        Self::new_with_color_tables_for_family(volume, cut_index, moment, color_tables, None)
    }

    pub fn new_with_color_tables_for_family(
        volume: &RadarVolume,
        cut_index: usize,
        moment: MomentType,
        color_tables: &ColorTableSet,
        family: Option<ColorTableFamily>,
    ) -> Result<Self> {
        let cut = volume
            .cuts
            .get(cut_index)
            .ok_or(RenderError::CutOutOfRange {
                index: cut_index,
                cut_count: volume.cuts.len(),
            })?;
        let grid = cut
            .moments
            .get(&moment)
            .ok_or_else(|| RenderError::MissingMoment {
                cut_index,
                moment: moment.clone(),
            })?;

        if grid.radial_indices.is_empty() {
            return Err(RenderError::EmptyMoment { cut_index, moment });
        }

        Ok(Self {
            volume_ptr: volume as *const RadarVolume as usize,
            cut_index,
            storm_motion_basis: (moment == MomentType::Velocity)
                .then(|| StormMotionBasis::new(cut, grid)),
            moment,
            row_lookup: AzimuthLookup::new(cut, grid),
            color_lookup: CachedColorLookup::new_for_family(
                grid,
                color_tables,
                family.unwrap_or_else(|| color_family_for_moment(&grid.moment)),
            ),
            dealiased_grid: None,
        })
    }

    pub fn new_dealiased_velocity(volume: &RadarVolume, cut_index: usize) -> Result<Self> {
        Self::new_dealiased_velocity_with_color_tables(volume, cut_index, &ColorTableSet::default())
    }

    pub fn new_dealiased_velocity_with_color_tables(
        volume: &RadarVolume,
        cut_index: usize,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let cut = volume
            .cuts
            .get(cut_index)
            .ok_or(RenderError::CutOutOfRange {
                index: cut_index,
                cut_count: volume.cuts.len(),
            })?;
        let source_grid =
            cut.moments
                .get(&MomentType::Velocity)
                .ok_or_else(|| RenderError::MissingMoment {
                    cut_index,
                    moment: MomentType::Velocity,
                })?;

        if source_grid.radial_indices.is_empty() {
            return Err(RenderError::EmptyMoment {
                cut_index,
                moment: MomentType::Velocity,
            });
        }

        let dealiased_grid = dealias_velocity_grid(cut, source_grid);
        Self::new_dealiased_velocity_from_grid_with_color_tables(
            volume,
            cut_index,
            dealiased_grid,
            color_tables,
        )
    }

    /// Like [`Self::new_dealiased_velocity_with_color_tables`] but reuses a
    /// velocity grid that was ALREADY dealiased (e.g. served from a per-volume
    /// memo) instead of running the region dealiaser again. Identical result;
    /// it just skips the ~100 ms dealias so loop replay / product toggles do
    /// not recompute it per frame.
    pub fn new_dealiased_velocity_from_grid_with_color_tables(
        volume: &RadarVolume,
        cut_index: usize,
        dealiased_grid: MomentGrid,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let cut = volume
            .cuts
            .get(cut_index)
            .ok_or(RenderError::CutOutOfRange {
                index: cut_index,
                cut_count: volume.cuts.len(),
            })?;
        if dealiased_grid.radial_indices.is_empty() {
            return Err(RenderError::EmptyMoment {
                cut_index,
                moment: MomentType::Velocity,
            });
        }
        Ok(Self {
            volume_ptr: volume as *const RadarVolume as usize,
            cut_index,
            moment: MomentType::Velocity,
            row_lookup: AzimuthLookup::new(cut, &dealiased_grid),
            color_lookup: CachedColorLookup::new(&dealiased_grid, color_tables),
            storm_motion_basis: Some(StormMotionBasis::new(cut, &dealiased_grid)),
            dealiased_grid: Some(dealiased_grid),
        })
    }

    /// Build a cache around a pre-computed derived grid (composite reflectivity,
    /// echo tops, VIL, …) drawn on `cut_index`'s geometry. The grid overrides
    /// the cut's moments via the same mechanism as the dealiased path.
    pub fn new_derived(
        volume: &RadarVolume,
        cut_index: usize,
        grid: MomentGrid,
        family: ColorTableFamily,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let cut = volume
            .cuts
            .get(cut_index)
            .ok_or(RenderError::CutOutOfRange {
                index: cut_index,
                cut_count: volume.cuts.len(),
            })?;
        if grid.radial_indices.is_empty() {
            return Err(RenderError::EmptyMoment {
                cut_index,
                moment: grid.moment.clone(),
            });
        }
        Ok(Self {
            volume_ptr: volume as *const RadarVolume as usize,
            cut_index,
            moment: grid.moment.clone(),
            row_lookup: AzimuthLookup::new(cut, &grid),
            color_lookup: CachedColorLookup::new_for_family(&grid, color_tables, family),
            storm_motion_basis: None,
            dealiased_grid: Some(grid),
        })
    }

    /// Build a cache around a display grid whose ROWS are synthetic — the
    /// interpolated (bilinear-upsampled) grid from `upsample_moment_grid`.
    /// Unlike `new_derived`, the azimuth lookup comes from the grid's own
    /// per-row azimuths instead of the cut's radials (the grid has more
    /// rows than the sweep). Renders through the same fast path.
    pub fn new_resampled(
        volume: &RadarVolume,
        cut_index: usize,
        grid: MomentGrid,
        row_azimuths_deg: &[f32],
        family: ColorTableFamily,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        if cut_index >= volume.cuts.len() {
            return Err(RenderError::CutOutOfRange {
                index: cut_index,
                cut_count: volume.cuts.len(),
            });
        }
        if grid.radial_indices.is_empty() || row_azimuths_deg.len() != grid.radial_count() {
            return Err(RenderError::EmptyMoment {
                cut_index,
                moment: grid.moment.clone(),
            });
        }
        Ok(Self {
            volume_ptr: volume as *const RadarVolume as usize,
            cut_index,
            moment: grid.moment.clone(),
            row_lookup: AzimuthLookup::from_row_azimuths(row_azimuths_deg, &grid),
            color_lookup: CachedColorLookup::new_for_family(&grid, color_tables, family),
            storm_motion_basis: None,
            dealiased_grid: Some(grid),
        })
    }

    pub fn cut_index(&self) -> usize {
        self.cut_index
    }

    pub fn moment(&self) -> &MomentType {
        &self.moment
    }

    pub fn render_moment_rgba_into(
        &self,
        volume: &RadarVolume,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        let (_, grid) = self.cut_and_grid(volume)?;
        let (width, height) = viewport_dimensions(options);
        ensure_rgba_buffer(pixels, width, height)?;
        render_moment_viewport_grid_into(
            grid,
            &self.row_lookup,
            &self.color_lookup,
            options,
            pixels,
            true,
        )?;
        Ok((width, height))
    }

    pub fn build_sample_cache(
        &self,
        volume: &RadarVolume,
        options: ViewportRasterOptions,
    ) -> Result<ViewportSampleCache> {
        let (_, grid) = self.cut_and_grid(volume)?;
        let (width, height) = viewport_dimensions(options);
        let geometry = viewport_geometry(grid, options);
        let lookup_table = ViewportLookupTable::new(grid, geometry);

        let row_builds = match &grid.storage {
            MomentStorage::U8(values) => {
                build_sample_cache_rows(height, &lookup_table, &self.row_lookup, |sample| {
                    resolve_compact_sample(values, grid, &self.row_lookup, sample)
                })
            }
            MomentStorage::U16(values) => {
                build_sample_cache_rows(height, &lookup_table, &self.row_lookup, |sample| {
                    resolve_compact_sample(values, grid, &self.row_lookup, sample)
                })
            }
            MomentStorage::F32(values) => {
                build_sample_cache_rows(height, &lookup_table, &self.row_lookup, |sample| {
                    resolve_f32_sample(values, grid, &self.row_lookup, sample)
                })
            }
        };

        Ok(viewport_sample_cache_from_rows(
            self.volume_ptr,
            self.cut_index,
            self.moment.clone(),
            width,
            height,
            row_builds,
        ))
    }

    pub fn build_geometry_cache(
        &self,
        volume: &RadarVolume,
        options: ViewportRasterOptions,
    ) -> Result<ViewportGeometryCache> {
        let (_, grid) = self.cut_and_grid(volume)?;
        let (width, height) = viewport_dimensions(options);
        let geometry = viewport_geometry(grid, options);
        let lookup_table = ViewportLookupTable::new(grid, geometry);
        let row_builds = build_geometry_cache_rows(height, &lookup_table, &self.row_lookup);
        let (sample_count, row_spans, samples) = flatten_cached_rows(height, row_builds);

        Ok(ViewportGeometryCache {
            width,
            height,
            gate_range: grid.gate_range.clone(),
            sample_count,
            row_spans,
            samples,
        })
    }

    pub fn build_sample_cache_from_geometry_cache(
        &self,
        volume: &RadarVolume,
        geometry_cache: &ViewportGeometryCache,
    ) -> Result<ViewportSampleCache> {
        let (_, grid) = self.cut_and_grid(volume)?;
        if grid.gate_range != geometry_cache.gate_range {
            return Err(RenderError::GeometryCacheMismatch);
        }
        let geometry = geometry_cache.geometry();
        let row_builds = match &grid.storage {
            MomentStorage::U8(values) => {
                build_sample_cache_rows_from_geometry(geometry_cache.height, geometry, |sample| {
                    resolve_compact_sample(values, grid, &self.row_lookup, sample)
                })
            }
            MomentStorage::U16(values) => {
                build_sample_cache_rows_from_geometry(geometry_cache.height, geometry, |sample| {
                    resolve_compact_sample(values, grid, &self.row_lookup, sample)
                })
            }
            MomentStorage::F32(values) => {
                build_sample_cache_rows_from_geometry(geometry_cache.height, geometry, |sample| {
                    resolve_f32_sample(values, grid, &self.row_lookup, sample)
                })
            }
        };

        Ok(viewport_sample_cache_from_rows(
            self.volume_ptr,
            self.cut_index,
            self.moment.clone(),
            geometry_cache.width,
            geometry_cache.height,
            row_builds,
        ))
    }

    pub fn sample_cache_storage_upper_bound(
        &self,
        volume: &RadarVolume,
        options: ViewportRasterOptions,
    ) -> Result<usize> {
        let (_, grid) = self.cut_and_grid(volume)?;
        Ok(viewport_sample_cache_storage_upper_bound_for_grid(
            grid, options,
        ))
    }

    pub fn render_moment_rgba_with_sample_cache(
        &self,
        volume: &RadarVolume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_moment_rgba_with_sample_cache_impl(volume, sample_cache, pixels, true)
    }

    /// Renders over an existing RGBA buffer without clearing transparent pixels first.
    ///
    /// Callers must only use this when `pixels` was last rendered with the same
    /// volume, cut, moment, and viewport sample footprint. The app worker tracks
    /// that provenance before taking this path.
    pub fn render_moment_rgba_with_sample_cache_reusing_transparency(
        &self,
        volume: &RadarVolume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_moment_rgba_with_sample_cache_impl(volume, sample_cache, pixels, false)
    }

    fn render_moment_rgba_with_sample_cache_impl(
        &self,
        volume: &RadarVolume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
        clear_pixels: bool,
    ) -> Result<(u32, u32)> {
        let (_, grid) = self.cut_and_grid(volume)?;
        self.ensure_sample_cache(sample_cache)?;
        ensure_rgba_buffer(pixels, sample_cache.width, sample_cache.height)?;
        render_moment_sample_cache_grid_into(
            grid,
            &self.color_lookup,
            sample_cache,
            pixels,
            clear_pixels,
        )?;
        Ok(sample_cache.dimensions())
    }

    pub fn render_storm_relative_velocity_rgba_into(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_storm_relative_velocity_rgba_into_cached(
            volume,
            storm_motion,
            None,
            options,
            pixels,
        )
    }

    pub fn build_storm_relative_velocity_palette_cache(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
    ) -> Result<Option<StormRelativePaletteCache>> {
        if self.moment != MomentType::Velocity {
            return Err(RenderError::CacheMomentMismatch {
                expected: MomentType::Velocity,
                actual: self.moment.clone(),
            });
        }

        let (cut, grid) = self.cut_and_grid(volume)?;
        let MomentStorage::U8(_) = &grid.storage else {
            return Ok(None);
        };
        let row_motion = self
            .storm_motion_basis
            .as_ref()
            .map(|basis| basis.row_motion_components(storm_motion))
            .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
        Ok(Some(StormRelativePaletteCache {
            volume_ptr: self.volume_ptr,
            cut_index: self.cut_index,
            row_palettes: build_storm_relative_u8_row_palettes(
                grid,
                &row_motion,
                self.color_lookup.color_table(),
            ),
        }))
    }

    pub fn render_storm_relative_velocity_rgba_into_with_palette_cache(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        palette_cache: &StormRelativePaletteCache,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.ensure_storm_relative_palette_cache(palette_cache)?;
        self.render_storm_relative_velocity_rgba_into_cached(
            volume,
            storm_motion,
            Some(palette_cache),
            options,
            pixels,
        )
    }

    fn render_storm_relative_velocity_rgba_into_cached(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        palette_cache: Option<&StormRelativePaletteCache>,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        if self.moment != MomentType::Velocity {
            return Err(RenderError::CacheMomentMismatch {
                expected: MomentType::Velocity,
                actual: self.moment.clone(),
            });
        }

        let (cut, grid) = self.cut_and_grid(volume)?;
        let (width, height) = viewport_dimensions(options);
        ensure_rgba_buffer(pixels, width, height)?;
        render_storm_relative_velocity_viewport_grid_into(
            cut,
            grid,
            StormRelativeRenderCache {
                row_lookup: &self.row_lookup,
                storm_motion_basis: self.storm_motion_basis.as_ref(),
                color_table: self.color_lookup.color_table(),
                palette_cache,
            },
            storm_motion,
            options,
            pixels,
            true,
        );
        Ok((width, height))
    }

    pub fn render_storm_relative_velocity_rgba_with_sample_cache(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_storm_relative_velocity_rgba_with_sample_cache_impl(
            volume,
            storm_motion,
            None,
            sample_cache,
            pixels,
            true,
        )
    }

    /// Renders SRV over an existing RGBA buffer without clearing transparent pixels first.
    ///
    /// This is safe only when the buffer came from the same velocity sample
    /// footprint. The storm motion may differ because every cached velocity
    /// sample is overwritten during this render.
    pub fn render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_storm_relative_velocity_rgba_with_sample_cache_impl(
            volume,
            storm_motion,
            None,
            sample_cache,
            pixels,
            false,
        )
    }

    pub fn render_storm_relative_velocity_rgba_with_sample_cache_and_palette_cache(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        palette_cache: &StormRelativePaletteCache,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.ensure_storm_relative_palette_cache(palette_cache)?;
        self.render_storm_relative_velocity_rgba_with_sample_cache_impl(
            volume,
            storm_motion,
            Some(palette_cache),
            sample_cache,
            pixels,
            true,
        )
    }

    pub fn render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency_and_palette_cache(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        palette_cache: &StormRelativePaletteCache,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.ensure_storm_relative_palette_cache(palette_cache)?;
        self.render_storm_relative_velocity_rgba_with_sample_cache_impl(
            volume,
            storm_motion,
            Some(palette_cache),
            sample_cache,
            pixels,
            false,
        )
    }

    fn render_storm_relative_velocity_rgba_with_sample_cache_impl(
        &self,
        volume: &RadarVolume,
        storm_motion: StormMotion,
        palette_cache: Option<&StormRelativePaletteCache>,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
        clear_pixels: bool,
    ) -> Result<(u32, u32)> {
        if self.moment != MomentType::Velocity {
            return Err(RenderError::CacheMomentMismatch {
                expected: MomentType::Velocity,
                actual: self.moment.clone(),
            });
        }

        let (cut, grid) = self.cut_and_grid(volume)?;
        self.ensure_sample_cache(sample_cache)?;
        ensure_rgba_buffer(pixels, sample_cache.width, sample_cache.height)?;
        render_storm_relative_velocity_sample_cache_grid_into(
            cut,
            grid,
            StormRelativeRenderCache {
                row_lookup: &self.row_lookup,
                storm_motion_basis: self.storm_motion_basis.as_ref(),
                color_table: self.color_lookup.color_table(),
                palette_cache,
            },
            storm_motion,
            sample_cache,
            pixels,
            clear_pixels,
        );
        Ok(sample_cache.dimensions())
    }

    fn ensure_sample_cache(&self, sample_cache: &ViewportSampleCache) -> Result<()> {
        if self.volume_ptr != sample_cache.volume_ptr {
            return Err(RenderError::CacheVolumeMismatch);
        }
        if self.cut_index != sample_cache.cut_index {
            return Err(RenderError::CacheCutMismatch {
                expected: self.cut_index,
                actual: sample_cache.cut_index,
            });
        }
        if self.moment != sample_cache.moment {
            return Err(RenderError::CacheMomentMismatch {
                expected: self.moment.clone(),
                actual: sample_cache.moment.clone(),
            });
        }
        Ok(())
    }

    fn ensure_storm_relative_palette_cache(
        &self,
        palette_cache: &StormRelativePaletteCache,
    ) -> Result<()> {
        if self.volume_ptr != palette_cache.volume_ptr {
            return Err(RenderError::CacheVolumeMismatch);
        }
        if self.cut_index != palette_cache.cut_index {
            return Err(RenderError::CacheCutMismatch {
                expected: self.cut_index,
                actual: palette_cache.cut_index,
            });
        }
        Ok(())
    }

    fn cut_and_grid<'a>(
        &'a self,
        volume: &'a RadarVolume,
    ) -> Result<(&'a ElevationCut, &'a MomentGrid)> {
        if self.volume_ptr != volume as *const RadarVolume as usize {
            return Err(RenderError::CacheVolumeMismatch);
        }

        let cut = volume
            .cuts
            .get(self.cut_index)
            .ok_or(RenderError::CutOutOfRange {
                index: self.cut_index,
                cut_count: volume.cuts.len(),
            })?;
        if let Some(grid) = &self.dealiased_grid {
            return Ok((cut, grid));
        }
        let grid = cut
            .moments
            .get(&self.moment)
            .ok_or_else(|| RenderError::MissingMoment {
                cut_index: self.cut_index,
                moment: self.moment.clone(),
            })?;
        Ok((cut, grid))
    }
}

fn render_moment_viewport_grid_into(
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    color_lookup: &CachedColorLookup,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
    clear_pixels: bool,
) -> Result<()> {
    let geometry = viewport_geometry(grid, options);
    let lookup_table = ViewportLookupTable::new(grid, geometry);

    match (&grid.storage, color_lookup) {
        (MomentStorage::U8(values), CachedColorLookup::U8 { palette, .. }) => {
            render_compact_viewport_storage(
                pixels,
                values,
                palette.as_ref(),
                grid,
                row_lookup,
                &lookup_table,
                clear_pixels,
            );
        }
        (MomentStorage::U16(values), CachedColorLookup::U16 { palette, .. }) => {
            render_compact_viewport_storage(
                pixels,
                values,
                palette,
                grid,
                row_lookup,
                &lookup_table,
                clear_pixels,
            );
        }
        (MomentStorage::F32(values), color_lookup) => {
            render_f32_viewport_storage(
                pixels,
                values,
                grid,
                row_lookup,
                color_lookup.color_table(),
                &lookup_table,
                clear_pixels,
            );
        }
        _ => return Err(RenderError::CacheStorageMismatch),
    }
    Ok(())
}

fn render_moment_sample_cache_grid_into(
    grid: &MomentGrid,
    color_lookup: &CachedColorLookup,
    sample_cache: &ViewportSampleCache,
    pixels: &mut [u8],
    clear_pixels: bool,
) -> Result<()> {
    match (&grid.storage, color_lookup) {
        (MomentStorage::U8(values), CachedColorLookup::U8 { palette, .. }) => {
            render_compact_sample_cache_storage(
                pixels,
                values,
                palette.as_ref(),
                grid,
                sample_cache,
                clear_pixels,
            );
        }
        (MomentStorage::U16(values), CachedColorLookup::U16 { palette, .. }) => {
            render_compact_sample_cache_storage(
                pixels,
                values,
                palette,
                grid,
                sample_cache,
                clear_pixels,
            );
        }
        (MomentStorage::F32(values), color_lookup) => {
            render_f32_sample_cache_storage(
                pixels,
                values,
                grid,
                color_lookup.color_table(),
                sample_cache,
                clear_pixels,
            );
        }
        _ => return Err(RenderError::CacheStorageMismatch),
    }
    Ok(())
}

pub fn render_storm_relative_velocity_image(
    volume: &RadarVolume,
    cut_index: usize,
    storm_motion: StormMotion,
    options: RasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let cut = volume
        .cuts
        .get(cut_index)
        .ok_or(RenderError::CutOutOfRange {
            index: cut_index,
            cut_count: volume.cuts.len(),
        })?;
    let grid =
        cut.moments
            .get(&MomentType::Velocity)
            .ok_or_else(|| RenderError::MissingMoment {
                cut_index,
                moment: MomentType::Velocity,
            })?;

    if grid.radial_indices.is_empty() {
        return Err(RenderError::EmptyMoment {
            cut_index,
            moment: MomentType::Velocity,
        });
    }

    let row_lookup = AzimuthLookup::new(cut, grid);
    let row_motion = row_motion_components(cut, grid, storm_motion);
    let width = options.width.max(64);
    let height = options.height.max(64);
    let center_x = (width as f32 - 1.0) / 2.0;
    let center_y = (height as f32 - 1.0) / 2.0;
    let radius_px = center_x.min(center_y) * (f32::from(options.range_fraction) / 100.0);
    let max_range_m = max_range_m(grid).max(1.0);

    let mut pixels = vec![0; width as usize * height as usize * 4];
    let color_tables = ColorTableSet::default();
    let color_table = color_tables.for_family(ColorTableFamily::Velocity);
    let geometry = RasterGeometry {
        width,
        center_x,
        center_y,
        radius_px,
        radius_sq_px: radius_px * radius_px,
        max_range_m,
    };

    match &grid.storage {
        MomentStorage::U8(values) => {
            let row_palettes = build_storm_relative_u8_row_palettes(grid, &row_motion, color_table);
            render_storm_relative_u8_storage(
                &mut pixels,
                values,
                grid,
                &row_lookup,
                &row_palettes,
                geometry,
                false,
            );
        }
        MomentStorage::U16(values) => {
            render_storm_relative_storage(
                &mut pixels,
                values,
                grid,
                &row_lookup,
                StormRelativeValueLookup {
                    row_motion: &row_motion,
                    color_table,
                },
                geometry,
                false,
            );
        }
        MomentStorage::F32(values) => render_storm_relative_f32_storage(
            &mut pixels,
            values,
            grid,
            &row_lookup,
            StormRelativeValueLookup {
                row_motion: &row_motion,
                color_table,
            },
            geometry,
            false,
        ),
    }

    rgba_image(width, height, pixels)
}

pub fn render_storm_relative_velocity_viewport_image(
    volume: &RadarVolume,
    cut_index: usize,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let (width, height, pixels) =
        render_storm_relative_velocity_viewport_rgba(volume, cut_index, storm_motion, options)?;
    rgba_image(width, height, pixels)
}

pub fn render_storm_relative_velocity_viewport_rgba(
    volume: &RadarVolume,
    cut_index: usize,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
) -> Result<(u32, u32, Vec<u8>)> {
    let (width, height) = viewport_dimensions(options);
    let mut pixels = vec![0; rgba_len(width, height)];
    render_storm_relative_velocity_viewport_rgba_into(
        volume,
        cut_index,
        storm_motion,
        options,
        &mut pixels,
    )?;
    Ok((width, height, pixels))
}

pub fn render_storm_relative_velocity_viewport_rgba_into(
    volume: &RadarVolume,
    cut_index: usize,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
) -> Result<(u32, u32)> {
    let cache = ViewportMomentCache::new(volume, cut_index, MomentType::Velocity)?;
    cache.render_storm_relative_velocity_rgba_into(volume, storm_motion, options, pixels)
}

fn render_storm_relative_velocity_viewport_grid_into(
    cut: &ElevationCut,
    grid: &MomentGrid,
    render_cache: StormRelativeRenderCache<'_>,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
    clear_pixels: bool,
) {
    let geometry = viewport_geometry(grid, options);
    let lookup_table = ViewportLookupTable::new(grid, geometry);

    match &grid.storage {
        MomentStorage::U8(values) => {
            let built_palettes;
            let row_palettes = if let Some(palette_cache) = render_cache.palette_cache {
                &palette_cache.row_palettes
            } else {
                let row_motion = render_cache
                    .storm_motion_basis
                    .map(|basis| basis.row_motion_components(storm_motion))
                    .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
                built_palettes = build_storm_relative_u8_row_palettes(
                    grid,
                    &row_motion,
                    render_cache.color_table,
                );
                &built_palettes
            };
            render_storm_relative_u8_viewport_storage(
                pixels,
                values,
                grid,
                render_cache.row_lookup,
                row_palettes,
                &lookup_table,
                clear_pixels,
            );
        }
        MomentStorage::U16(values) => {
            let row_motion = render_cache
                .storm_motion_basis
                .map(|basis| basis.row_motion_components(storm_motion))
                .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
            render_storm_relative_viewport_storage(
                pixels,
                values,
                grid,
                render_cache.row_lookup,
                StormRelativeValueLookup {
                    row_motion: &row_motion,
                    color_table: render_cache.color_table,
                },
                &lookup_table,
                clear_pixels,
            );
        }
        MomentStorage::F32(values) => {
            let row_motion = render_cache
                .storm_motion_basis
                .map(|basis| basis.row_motion_components(storm_motion))
                .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
            render_storm_relative_f32_viewport_storage(
                pixels,
                values,
                grid,
                render_cache.row_lookup,
                StormRelativeValueLookup {
                    row_motion: &row_motion,
                    color_table: render_cache.color_table,
                },
                &lookup_table,
                clear_pixels,
            );
        }
    }
}

fn render_storm_relative_velocity_sample_cache_grid_into(
    cut: &ElevationCut,
    grid: &MomentGrid,
    render_cache: StormRelativeRenderCache<'_>,
    storm_motion: StormMotion,
    sample_cache: &ViewportSampleCache,
    pixels: &mut [u8],
    clear_pixels: bool,
) {
    match &grid.storage {
        MomentStorage::U8(values) => {
            let built_palettes;
            let row_palettes = if let Some(palette_cache) = render_cache.palette_cache {
                &palette_cache.row_palettes
            } else {
                let row_motion = render_cache
                    .storm_motion_basis
                    .map(|basis| basis.row_motion_components(storm_motion))
                    .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
                built_palettes = build_storm_relative_u8_row_palettes(
                    grid,
                    &row_motion,
                    render_cache.color_table,
                );
                &built_palettes
            };
            render_storm_relative_u8_sample_cache_storage(
                pixels,
                values,
                grid,
                row_palettes,
                sample_cache,
                clear_pixels,
            );
        }
        MomentStorage::U16(values) => {
            let row_motion = render_cache
                .storm_motion_basis
                .map(|basis| basis.row_motion_components(storm_motion))
                .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
            render_storm_relative_sample_cache_storage(
                pixels,
                values,
                grid,
                &row_motion,
                render_cache.color_table,
                sample_cache,
                clear_pixels,
            );
        }
        MomentStorage::F32(values) => {
            let row_motion = render_cache
                .storm_motion_basis
                .map(|basis| basis.row_motion_components(storm_motion))
                .unwrap_or_else(|| row_motion_components(cut, grid, storm_motion));
            render_storm_relative_f32_sample_cache_storage(
                pixels,
                values,
                grid,
                &row_motion,
                render_cache.color_table,
                sample_cache,
                clear_pixels,
            );
        }
    }
}

struct StormRelativeRenderCache<'a> {
    row_lookup: &'a AzimuthLookup,
    storm_motion_basis: Option<&'a StormMotionBasis>,
    color_table: &'a ColorTable,
    palette_cache: Option<&'a StormRelativePaletteCache>,
}

#[derive(Clone, Copy)]
struct StormRelativeValueLookup<'a> {
    row_motion: &'a [f32],
    color_table: &'a ColorTable,
}

#[derive(Clone, Copy, Debug)]
struct RasterGeometry {
    width: u32,
    center_x: f32,
    center_y: f32,
    radius_px: f32,
    radius_sq_px: f32,
    max_range_m: f32,
}

#[derive(Clone, Copy, Debug)]
struct ViewportGeometry {
    width: u32,
    radar_x_px: f32,
    radar_y_px: f32,
    km_per_px_x: f32,
    km_per_px_y: f32,
    max_range_km_sq: f32,
    rot_sin: f32,
    rot_cos: f32,
}

fn viewport_dimensions(options: ViewportRasterOptions) -> (u32, u32) {
    (options.width.max(1), options.height.max(1))
}

fn viewport_geometry(grid: &MomentGrid, options: ViewportRasterOptions) -> ViewportGeometry {
    let (width, _) = viewport_dimensions(options);
    let max_range_km = max_range_m(grid).max(1.0) / 1000.0;
    let (rot_sin, rot_cos) = options.rotation_rad.sin_cos();
    ViewportGeometry {
        width,
        radar_x_px: options.radar_x_px,
        radar_y_px: options.radar_y_px,
        km_per_px_x: options.km_per_px_x.max(f32::EPSILON),
        km_per_px_y: options.km_per_px_y.max(f32::EPSILON),
        max_range_km_sq: max_range_km * max_range_km,
        rot_sin,
        rot_cos,
    }
}

fn rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn ensure_rgba_buffer(pixels: &[u8], width: u32, height: u32) -> Result<()> {
    let expected = rgba_len(width, height);
    if pixels.len() == expected {
        Ok(())
    } else {
        Err(RenderError::BufferSizeMismatch {
            actual: pixels.len(),
            expected,
            width,
            height,
        })
    }
}

trait LookupGeometry: Copy + Sync {
    fn width(self) -> u32;
    fn x_range_for_row(self, y: u32) -> Option<Range<u32>>;
    fn lookup(
        self,
        x: u32,
        y: u32,
        grid: &MomentGrid,
        row_lookup: &AzimuthLookup,
    ) -> Option<SampleLookup>;
}

impl LookupGeometry for RasterGeometry {
    fn width(self) -> u32 {
        self.width
    }

    fn x_range_for_row(self, _y: u32) -> Option<Range<u32>> {
        Some(0..self.width)
    }

    fn lookup(
        self,
        x: u32,
        y: u32,
        grid: &MomentGrid,
        row_lookup: &AzimuthLookup,
    ) -> Option<SampleLookup> {
        raster_lookup(x, y, grid, row_lookup, self)
    }
}

impl LookupGeometry for ViewportGeometry {
    fn width(self) -> u32 {
        self.width
    }

    fn x_range_for_row(self, y: u32) -> Option<Range<u32>> {
        let dy_km = (self.radar_y_px - (y as f32 + 0.5)) * self.km_per_px_y;
        let dy_km_sq = dy_km * dy_km;
        if dy_km_sq > self.max_range_km_sq {
            return None;
        }

        let max_dx_km = (self.max_range_km_sq - dy_km_sq).max(0.0).sqrt();
        let max_dx_px = max_dx_km / self.km_per_px_x;
        let first = (self.radar_x_px - max_dx_px - 0.5).floor() as i64 - 1;
        let last_exclusive = (self.radar_x_px + max_dx_px - 0.5).ceil() as i64 + 2;
        let width = i64::from(self.width);
        let start = first.clamp(0, width) as u32;
        let end = last_exclusive.clamp(0, width) as u32;
        (start < end).then_some(start..end)
    }

    fn lookup(
        self,
        x: u32,
        y: u32,
        grid: &MomentGrid,
        row_lookup: &AzimuthLookup,
    ) -> Option<SampleLookup> {
        viewport_lookup(x, y, grid, row_lookup, self)
    }
}

#[derive(Debug)]
struct ViewportLookupTable {
    geometry: ViewportGeometry,
    first_gate_m: f32,
    gate_spacing_m: f32,
    gate_count: usize,
}

impl ViewportLookupTable {
    fn new(grid: &MomentGrid, geometry: ViewportGeometry) -> Self {
        Self {
            geometry,
            first_gate_m: grid.gate_range.first_gate_m as f32,
            gate_spacing_m: grid.gate_range.gate_spacing_m.max(1) as f32,
            gate_count: grid.gate_range.gate_count,
        }
    }

    fn width(&self) -> u32 {
        self.geometry.width
    }

    fn row(&self, y: u32) -> Option<ViewportLookupRow> {
        let dy_km = (self.geometry.radar_y_px - (y as f32 + 0.5)) * self.geometry.km_per_px_y;
        let dy_km_sq = dy_km * dy_km;
        if dy_km_sq > self.geometry.max_range_km_sq {
            return None;
        }

        let max_dx_km = (self.geometry.max_range_km_sq - dy_km_sq).max(0.0).sqrt();
        let max_dx_px = max_dx_km / self.geometry.km_per_px_x;
        let first = (self.geometry.radar_x_px - max_dx_px - 0.5).floor() as i64 - 1;
        let last_exclusive = (self.geometry.radar_x_px + max_dx_px - 0.5).ceil() as i64 + 2;
        let width = i64::from(self.geometry.width);
        let start = first.clamp(0, width) as u32;
        let end = last_exclusive.clamp(0, width) as u32;
        (start < end).then_some(ViewportLookupRow {
            x_range: start..end,
            dy_km,
            dy_km_sq,
            max_range_km_sq: self.geometry.max_range_km_sq,
            radar_x_px: self.geometry.radar_x_px,
            km_per_px_x: self.geometry.km_per_px_x,
            rot_sin: self.geometry.rot_sin,
            rot_cos: self.geometry.rot_cos,
            first_gate_m: self.first_gate_m,
            gate_spacing_m: self.gate_spacing_m,
            gate_count: self.gate_count,
        })
    }
}

#[derive(Clone, Debug)]
struct ViewportLookupRow {
    x_range: Range<u32>,
    dy_km: f32,
    dy_km_sq: f32,
    max_range_km_sq: f32,
    radar_x_px: f32,
    km_per_px_x: f32,
    rot_sin: f32,
    rot_cos: f32,
    first_gate_m: f32,
    gate_spacing_m: f32,
    gate_count: usize,
}

impl ViewportLookupRow {
    fn lookup(&self, x: u32, row_lookup: &AzimuthLookup) -> Option<SampleLookup> {
        let dx_km = (x as f32 + 0.5 - self.radar_x_px) * self.km_per_px_x;
        let range_km_sq = dx_km.mul_add(dx_km, self.dy_km_sq);
        if range_km_sq > self.max_range_km_sq {
            return None;
        }

        let range_m = range_km_sq.sqrt() * 1000.0;
        let gate = ((range_m - self.first_gate_m) / self.gate_spacing_m).round() as isize;
        if gate < 0 || gate as usize >= self.gate_count {
            return None;
        }

        // Same screen-ENU → radar-ENU rotation as `viewport_lookup`: the
        // viewport raster bakes the AEQD convergence angle and the draw
        // quad applies only the residual, so a table that skipped the
        // rotation skewed every azimuth by the baked amount (field
        // report: slight pan/zoom-dependent offset). Range is
        // rotation-invariant, so the gate above stays raw.
        let east_km = dx_km * self.rot_cos - self.dy_km * self.rot_sin;
        let north_km = dx_km * self.rot_sin + self.dy_km * self.rot_cos;
        let azimuth_deg = azimuth_from_xy(east_km, north_km);
        let azimuth_bin = row_lookup.filled_bin_for_azimuth(azimuth_deg)?;
        Some(SampleLookup {
            azimuth_bin,
            gate: gate as usize,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct CachedViewportGeometry<'a> {
    row_spans: &'a [CachedRowSpan],
    samples: &'a [CachedSample],
}

impl<'a> CachedViewportGeometry<'a> {
    fn row_samples(&self, y: usize) -> Option<(u32, &'a [CachedSample])> {
        let span = self.row_spans.get(y)?;
        let range = span.range()?;
        let start = span.sample_offset;
        let end = start + (range.end - range.start) as usize;
        Some((range.start, &self.samples[start..end]))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SampleLookup {
    azimuth_bin: usize,
    gate: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedSample {
    row: usize,
    gate: usize,
}

trait RawMomentValue: Copy + Sync {
    fn to_usize(self) -> usize;
}

impl RawMomentValue for u8 {
    fn to_usize(self) -> usize {
        usize::from(self)
    }
}

impl RawMomentValue for u16 {
    fn to_usize(self) -> usize {
        usize::from(self)
    }
}

fn render_compact_storage<T: RawMomentValue, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = geometry.width();
    let row_stride = width as usize * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(x_range) = geometry.x_range_for_row(y) else {
                return;
            };
            for x in x_range {
                let Some(sample) = geometry.lookup(x, y, grid, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let color = palette[raw.to_usize()];
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_compact_viewport_storage<T: RawMomentValue>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = lookup_table.width();
    let row_stride = width as usize * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return;
            };
            for x in row_lookup_table.x_range.clone() {
                let Some(sample) = row_lookup_table.lookup(x, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let color = palette[raw.to_usize()];
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_compact_sample_cache_storage<T: RawMomentValue>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    grid: &MomentGrid,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let geometry = sample_cache.geometry();
    let width = sample_cache.width as usize;
    let row_stride = width * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return;
            };
            let mut pixel = row_start_x as usize * 4;
            for cached_sample in row_samples {
                if let Some(skip) = cached_sample.skip_len() {
                    pixel += skip as usize * 4;
                    continue;
                }
                let index = cached_sample.row() * gate_count + cached_sample.gate();
                debug_assert!(index < values.len());
                let color = palette[values[index].to_usize()];
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

fn render_f32_storage<G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    color_table: &ColorTable,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = geometry.width();
    let row_stride = width as usize * 4;
    let sampler = color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(x_range) = geometry.x_range_for_row(y) else {
                return;
            };
            for x in x_range {
                let Some(sample) = geometry.lookup(x, y, grid, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(value) = values.get(index).copied().filter(|value| value.is_finite())
                    else {
                        continue;
                    };
                    let color = sampler.color_for_value(value);
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_f32_viewport_storage(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    color_table: &ColorTable,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = lookup_table.width();
    let row_stride = width as usize * 4;
    let sampler = color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return;
            };
            for x in row_lookup_table.x_range.clone() {
                let Some(sample) = row_lookup_table.lookup(x, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(value) = values.get(index).copied().filter(|value| value.is_finite())
                    else {
                        continue;
                    };
                    let color = sampler.color_for_value(value);
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_f32_sample_cache_storage(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let geometry = sample_cache.geometry();
    let width = sample_cache.width as usize;
    let row_stride = width * 4;
    let sampler = color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return;
            };
            let mut pixel = row_start_x as usize * 4;
            for cached_sample in row_samples {
                if let Some(skip) = cached_sample.skip_len() {
                    pixel += skip as usize * 4;
                    continue;
                }
                let index = cached_sample.row() * gate_count + cached_sample.gate();
                debug_assert!(index < values.len());
                let value = values[index];
                if value.is_finite() {
                    let color = sampler.color_for_value(value);
                    if color[3] != 0 {
                        row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    }
                }
                pixel += 4;
            }
        });
}

fn render_storm_relative_storage<T: RawMomentValue, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = geometry.width();
    let row_stride = width as usize * 4;
    let sampler = value_lookup.color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(x_range) = geometry.x_range_for_row(y) else {
                return;
            };
            for x in x_range {
                let Some(sample) = geometry.lookup(x, y, grid, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied().map(RawMomentValue::to_usize) else {
                        continue;
                    };
                    if grid.nodata == Some(raw as u16) {
                        continue;
                    }
                    let color = if grid.range_folded == Some(raw as u16) {
                        sampler.range_folded_color()
                    } else {
                        let velocity = (raw as f32 - grid.offset) / grid.scale;
                        let relative = velocity
                            - value_lookup
                                .row_motion
                                .get(candidate.row)
                                .copied()
                                .unwrap_or(0.0);
                        sampler.color_for_value(relative)
                    };
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_storm_relative_viewport_storage<T: RawMomentValue>(
    pixels: &mut [u8],
    values: &[T],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = lookup_table.width();
    let row_stride = width as usize * 4;
    let sampler = value_lookup.color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return;
            };
            for x in row_lookup_table.x_range.clone() {
                let Some(sample) = row_lookup_table.lookup(x, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied().map(RawMomentValue::to_usize) else {
                        continue;
                    };
                    if grid.nodata == Some(raw as u16) {
                        continue;
                    }
                    let color = if grid.range_folded == Some(raw as u16) {
                        sampler.range_folded_color()
                    } else {
                        let velocity = (raw as f32 - grid.offset) / grid.scale;
                        let relative = velocity
                            - value_lookup
                                .row_motion
                                .get(candidate.row)
                                .copied()
                                .unwrap_or(0.0);
                        sampler.color_for_value(relative)
                    };
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn build_storm_relative_u8_row_palettes(
    grid: &MomentGrid,
    row_motion: &[f32],
    color_table: &ColorTable,
) -> Vec<[[u8; 4]; 256]> {
    let sampler = color_table.sampler();
    row_motion
        .par_iter()
        .map(|motion| {
            let mut palette = [[0, 0, 0, 0]; 256];
            for raw in 0..=u8::MAX {
                palette[usize::from(raw)] =
                    storm_relative_u8_color_for_raw(grid, &sampler, raw, *motion);
            }
            palette
        })
        .collect()
}

fn storm_relative_u8_color_for_raw(
    grid: &MomentGrid,
    sampler: &ColorSampler,
    raw: u8,
    row_motion: f32,
) -> [u8; 4] {
    let raw = u16::from(raw);
    if grid.nodata == Some(raw) {
        return [0, 0, 0, 0];
    }
    if grid.range_folded == Some(raw) {
        return sampler.range_folded_color();
    }
    let velocity = (raw as f32 - grid.offset) / grid.scale;
    sampler.color_for_value(velocity - row_motion)
}

fn render_storm_relative_u8_storage<G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[u8],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    row_palettes: &[[[u8; 4]; 256]],
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = geometry.width();
    let row_stride = width as usize * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(x_range) = geometry.x_range_for_row(y) else {
                return;
            };
            for x in x_range {
                let Some(sample) = geometry.lookup(x, y, grid, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let Some(palette) = row_palettes.get(candidate.row) else {
                        continue;
                    };
                    let color = palette[usize::from(raw)];
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_storm_relative_u8_viewport_storage(
    pixels: &mut [u8],
    values: &[u8],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    row_palettes: &[[[u8; 4]; 256]],
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = lookup_table.width();
    let row_stride = width as usize * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return;
            };
            for x in row_lookup_table.x_range.clone() {
                let Some(sample) = row_lookup_table.lookup(x, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let Some(palette) = row_palettes.get(candidate.row) else {
                        continue;
                    };
                    let color = palette[usize::from(raw)];
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_storm_relative_u8_sample_cache_storage(
    pixels: &mut [u8],
    values: &[u8],
    grid: &MomentGrid,
    row_palettes: &[[[u8; 4]; 256]],
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let geometry = sample_cache.geometry();
    let width = sample_cache.width as usize;
    let row_stride = width * 4;
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return;
            };
            let mut pixel = row_start_x as usize * 4;
            for cached_sample in row_samples {
                if let Some(skip) = cached_sample.skip_len() {
                    pixel += skip as usize * 4;
                    continue;
                }
                let row = cached_sample.row();
                let index = row * gate_count + cached_sample.gate();
                debug_assert!(index < values.len());
                debug_assert!(row < row_palettes.len());
                let color = row_palettes[row][usize::from(values[index])];
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

fn render_storm_relative_sample_cache_storage<T: RawMomentValue>(
    pixels: &mut [u8],
    values: &[T],
    grid: &MomentGrid,
    row_motion: &[f32],
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let geometry = sample_cache.geometry();
    let width = sample_cache.width as usize;
    let row_stride = width * 4;
    let sampler = color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return;
            };
            let mut pixel = row_start_x as usize * 4;
            for cached_sample in row_samples {
                if let Some(skip) = cached_sample.skip_len() {
                    pixel += skip as usize * 4;
                    continue;
                }
                let row = cached_sample.row();
                let index = row * gate_count + cached_sample.gate();
                debug_assert!(index < values.len());
                debug_assert!(row < row_motion.len());
                let raw = values[index].to_usize();
                if grid.nodata == Some(raw as u16) {
                    pixel += 4;
                    continue;
                }
                let color = if grid.range_folded == Some(raw as u16) {
                    sampler.range_folded_color()
                } else {
                    let velocity = (raw as f32 - grid.offset) / grid.scale;
                    let relative = velocity - row_motion[row];
                    sampler.color_for_value(relative)
                };
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

fn render_storm_relative_f32_storage<G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = geometry.width();
    let row_stride = width as usize * 4;
    let sampler = value_lookup.color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(x_range) = geometry.x_range_for_row(y) else {
                return;
            };
            for x in x_range {
                let Some(sample) = geometry.lookup(x, y, grid, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(velocity) =
                        values.get(index).copied().filter(|value| value.is_finite())
                    else {
                        continue;
                    };
                    let relative = velocity
                        - value_lookup
                            .row_motion
                            .get(candidate.row)
                            .copied()
                            .unwrap_or(0.0);
                    let color = sampler.color_for_value(relative);
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_storm_relative_f32_viewport_storage(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let width = lookup_table.width();
    let row_stride = width as usize * 4;
    let sampler = value_lookup.color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return;
            };
            for x in row_lookup_table.x_range.clone() {
                let Some(sample) = row_lookup_table.lookup(x, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(velocity) =
                        values.get(index).copied().filter(|value| value.is_finite())
                    else {
                        continue;
                    };
                    let relative = velocity
                        - value_lookup
                            .row_motion
                            .get(candidate.row)
                            .copied()
                            .unwrap_or(0.0);
                    let color = sampler.color_for_value(relative);
                    if color[3] == 0 {
                        continue;
                    }
                    let pixel = x as usize * 4;
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    break;
                }
            }
        });
}

fn render_storm_relative_f32_sample_cache_storage(
    pixels: &mut [u8],
    values: &[f32],
    grid: &MomentGrid,
    row_motion: &[f32],
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = grid.gate_range.gate_count;
    let geometry = sample_cache.geometry();
    let width = sample_cache.width as usize;
    let row_stride = width * 4;
    let sampler = color_table.sampler();
    pixels
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_pixels)| {
            if clear_pixels {
                row_pixels.fill(0);
            }
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return;
            };
            let mut pixel = row_start_x as usize * 4;
            for cached_sample in row_samples {
                if let Some(skip) = cached_sample.skip_len() {
                    pixel += skip as usize * 4;
                    continue;
                }
                let row = cached_sample.row();
                let index = row * gate_count + cached_sample.gate();
                debug_assert!(index < values.len());
                debug_assert!(row < row_motion.len());
                let velocity = values[index];
                if velocity.is_finite() {
                    let relative = velocity - row_motion[row];
                    let color = sampler.color_for_value(relative);
                    if color[3] != 0 {
                        row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    }
                }
                pixel += 4;
            }
        });
}

fn build_sample_cache_rows<R>(
    height: u32,
    lookup_table: &ViewportLookupTable,
    row_lookup: &AzimuthLookup,
    resolve: R,
) -> Vec<CachedRowBuild>
where
    R: Fn(SampleLookup) -> Option<ResolvedSample> + Sync,
{
    (0..height as usize)
        .into_par_iter()
        .map(|y| {
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return CachedRowBuild::empty();
            };
            let x_range = row_lookup_table.x_range.clone();
            let x_range_len = x_range.len();
            let mut start = None;
            let mut next_x = 0u32;
            let mut samples = Vec::with_capacity(x_range_len);
            let mut count = 0;
            for x in x_range {
                if let Some(sample) = row_lookup_table.lookup(x, row_lookup).and_then(&resolve)
                    && let Some(cached_sample) = CachedSample::new(sample)
                {
                    let start_x = *start.get_or_insert(x);
                    if samples.is_empty() {
                        next_x = start_x;
                    }
                    if x > next_x {
                        push_cached_sample_skip(&mut samples, x - next_x);
                    }
                    samples.push(cached_sample);
                    count += 1;
                    next_x = x + 1;
                }
            }
            // `start` is set exactly when the first sample is pushed.
            match start {
                Some(start) if !samples.is_empty() => CachedRowBuild {
                    start,
                    samples,
                    sample_count: count,
                },
                _ => CachedRowBuild::empty(),
            }
        })
        .collect()
}

fn build_geometry_cache_rows(
    height: u32,
    lookup_table: &ViewportLookupTable,
    row_lookup: &AzimuthLookup,
) -> Vec<CachedRowBuild> {
    (0..height as usize)
        .into_par_iter()
        .map(|y| {
            let y = y as u32;
            let Some(row_lookup_table) = lookup_table.row(y) else {
                return CachedRowBuild::empty();
            };
            let x_range = row_lookup_table.x_range.clone();
            let mut start = None;
            let mut next_x = 0u32;
            let mut samples = Vec::with_capacity(x_range.len());
            let mut count = 0usize;
            for x in x_range {
                if let Some(sample) = row_lookup_table.lookup(x, row_lookup)
                    && let Some(cached_sample) = CachedSample::new(ResolvedSample {
                        row: sample.azimuth_bin,
                        gate: sample.gate,
                    })
                {
                    let start_x = *start.get_or_insert(x);
                    if samples.is_empty() {
                        next_x = start_x;
                    }
                    if x > next_x {
                        push_cached_sample_skip(&mut samples, x - next_x);
                    }
                    samples.push(cached_sample);
                    count += 1;
                    next_x = x + 1;
                }
            }
            // `start` is set exactly when the first sample is pushed.
            match start {
                Some(start) if !samples.is_empty() => CachedRowBuild {
                    start,
                    samples,
                    sample_count: count,
                },
                _ => CachedRowBuild::empty(),
            }
        })
        .collect()
}

fn build_sample_cache_rows_from_geometry<R>(
    height: u32,
    geometry: CachedViewportGeometry<'_>,
    resolve: R,
) -> Vec<CachedRowBuild>
where
    R: Fn(SampleLookup) -> Option<ResolvedSample> + Sync,
{
    (0..height as usize)
        .into_par_iter()
        .map(|y| {
            let Some((row_start_x, row_samples)) = geometry.row_samples(y) else {
                return CachedRowBuild::empty();
            };
            let mut start = None;
            let mut next_x = 0u32;
            let mut x = row_start_x;
            let mut samples = Vec::with_capacity(row_samples.len());
            let mut count = 0usize;
            for cached_lookup in row_samples {
                if let Some(skip) = cached_lookup.skip_len() {
                    x += skip;
                    continue;
                }
                let sample = SampleLookup {
                    azimuth_bin: cached_lookup.row(),
                    gate: cached_lookup.gate(),
                };
                if let Some(sample) = resolve(sample)
                    && let Some(cached_sample) = CachedSample::new(sample)
                {
                    let start_x = *start.get_or_insert(x);
                    if samples.is_empty() {
                        next_x = start_x;
                    }
                    if x > next_x {
                        push_cached_sample_skip(&mut samples, x - next_x);
                    }
                    samples.push(cached_sample);
                    count += 1;
                    next_x = x + 1;
                }
                x += 1;
            }
            // `start` is set exactly when the first sample is pushed.
            match start {
                Some(start) if !samples.is_empty() => CachedRowBuild {
                    start,
                    samples,
                    sample_count: count,
                },
                _ => CachedRowBuild::empty(),
            }
        })
        .collect()
}

fn viewport_sample_cache_from_rows(
    volume_ptr: usize,
    cut_index: usize,
    moment: MomentType,
    width: u32,
    height: u32,
    row_builds: Vec<CachedRowBuild>,
) -> ViewportSampleCache {
    let (sample_count, row_spans, samples) = flatten_cached_rows(height, row_builds);
    ViewportSampleCache {
        volume_ptr,
        cut_index,
        moment,
        width,
        height,
        sample_count,
        row_spans,
        samples,
    }
}

fn flatten_cached_rows(
    height: u32,
    row_builds: Vec<CachedRowBuild>,
) -> (usize, Vec<CachedRowSpan>, Vec<CachedSample>) {
    let sample_storage_len = row_builds.iter().map(|row| row.samples.len()).sum();
    let mut row_spans = Vec::with_capacity(height as usize);
    let mut samples = Vec::with_capacity(sample_storage_len);
    let mut sample_count = 0;
    for row in row_builds {
        if row.samples.is_empty() {
            row_spans.push(CachedRowSpan::empty());
            continue;
        }
        let sample_offset = samples.len();
        let end = row.start + row.samples.len() as u32;
        sample_count += row.sample_count;
        row_spans.push(CachedRowSpan {
            start: row.start,
            end,
            sample_offset,
        });
        samples.extend(row.samples);
    }
    while row_spans.len() < height as usize {
        row_spans.push(CachedRowSpan::empty());
    }
    (sample_count, row_spans, samples)
}

fn push_cached_sample_skip(samples: &mut Vec<CachedSample>, mut pixel_count: u32) {
    while pixel_count > 0 {
        let chunk = pixel_count.min(CachedSample::SKIP_MASK);
        // `chunk` is in 1..=SKIP_MASK, which `skip` always encodes.
        if let Some(skip) = CachedSample::skip(chunk) {
            samples.push(skip);
        }
        pixel_count -= chunk;
    }
}

fn resolve_compact_sample<T: RawMomentValue>(
    values: &[T],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    sample: SampleLookup,
) -> Option<ResolvedSample> {
    let gate_count = grid.gate_range.gate_count;
    for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
        let index = candidate.row * gate_count + sample.gate;
        if index >= values.len() {
            continue;
        }
        let raw = values[index].to_usize() as u16;
        if grid.nodata == Some(raw) {
            continue;
        }
        return Some(ResolvedSample {
            row: candidate.row,
            gate: sample.gate,
        });
    }
    None
}

fn resolve_f32_sample(
    values: &[f32],
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    sample: SampleLookup,
) -> Option<ResolvedSample> {
    let gate_count = grid.gate_range.gate_count;
    for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
        let index = candidate.row * gate_count + sample.gate;
        if index < values.len() && values[index].is_finite() {
            return Some(ResolvedSample {
                row: candidate.row,
                gate: sample.gate,
            });
        }
    }
    None
}

fn raster_lookup(
    x: u32,
    y: u32,
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    geometry: RasterGeometry,
) -> Option<SampleLookup> {
    let dx = x as f32 - geometry.center_x;
    let dy = geometry.center_y - y as f32;
    let radius_sq = dx.mul_add(dx, dy * dy);
    if radius_sq > geometry.radius_sq_px {
        return None;
    }

    let radius = radius_sq.sqrt();
    let range_m = radius / geometry.radius_px * geometry.max_range_m;
    let gate = ((range_m - grid.gate_range.first_gate_m as f32)
        / grid.gate_range.gate_spacing_m.max(1) as f32)
        .round() as isize;
    if gate < 0 || gate as usize >= grid.gate_range.gate_count {
        return None;
    }

    let azimuth_deg = azimuth_from_xy(dx, dy);
    let azimuth_bin = row_lookup.filled_bin_for_azimuth(azimuth_deg)?;
    Some(SampleLookup {
        azimuth_bin,
        gate: gate as usize,
    })
}

fn viewport_lookup(
    x: u32,
    y: u32,
    grid: &MomentGrid,
    row_lookup: &AzimuthLookup,
    geometry: ViewportGeometry,
) -> Option<SampleLookup> {
    let dx_km = (x as f32 + 0.5 - geometry.radar_x_px) * geometry.km_per_px_x;
    let dy_km = (geometry.radar_y_px - (y as f32 + 0.5)) * geometry.km_per_px_y;
    let range_km_sq = dx_km.mul_add(dx_km, dy_km * dy_km);
    if range_km_sq > geometry.max_range_km_sq {
        return None;
    }

    let range_m = range_km_sq.sqrt() * 1000.0;
    let gate = ((range_m - grid.gate_range.first_gate_m as f32)
        / grid.gate_range.gate_spacing_m.max(1) as f32)
        .round() as isize;
    if gate < 0 || gate as usize >= grid.gate_range.gate_count {
        return None;
    }

    // Rotate screen-frame ENU into the radar's true ENU (verified by the
    // rotated-north unit test below): e = dx·cosγ − dy·sinγ,
    // n = dx·sinγ + dy·cosγ. Range is rotation-invariant.
    let east_km = dx_km * geometry.rot_cos - dy_km * geometry.rot_sin;
    let north_km = dx_km * geometry.rot_sin + dy_km * geometry.rot_cos;
    let azimuth_deg = azimuth_from_xy(east_km, north_km);
    let azimuth_bin = row_lookup.filled_bin_for_azimuth(azimuth_deg)?;
    Some(SampleLookup {
        azimuth_bin,
        gate: gate as usize,
    })
}

#[cfg(test)]
mod rotation_lookup_tests {
    use super::*;

    #[test]
    fn rotated_north_pixel_resolves_to_azimuth_zero() {
        // A gate due NORTH of the radar appears on screen rotated clockwise
        // by the convergence angle. With the same angle baked into the
        // lookup, that pixel must map back to azimuth 0.
        let gamma: f32 = 0.1;
        let geometry = ViewportGeometry {
            width: 512,
            radar_x_px: 256.0,
            radar_y_px: 256.0,
            km_per_px_x: 1.0,
            km_per_px_y: 1.0,
            max_range_km_sq: 1.0e9,
            rot_sin: gamma.sin(),
            rot_cos: gamma.cos(),
        };
        // Screen position of the north gate (visual clockwise rotation,
        // y-down): x' = r·sin γ right, y' = r·cos γ up.
        let r = 100.0f32;
        let dx_km = r * gamma.sin();
        let dy_km = r * gamma.cos();
        let east = dx_km * geometry.rot_cos - dy_km * geometry.rot_sin;
        let north = dx_km * geometry.rot_sin + dy_km * geometry.rot_cos;
        assert!(east.abs() < 1e-3, "east {east}");
        assert!((north - r).abs() < 1e-3, "north {north}");
        let azimuth = azimuth_from_xy(east, north);
        assert!(
            azimuth.abs() < 0.01 || (azimuth - 360.0).abs() < 0.01,
            "{azimuth}"
        );
    }
}

fn build_u8_palette(grid: &MomentGrid, color_table: &ColorTable) -> [[u8; 4]; 256] {
    let sampler = color_table.sampler();
    let mut palette = [[0, 0, 0, 0]; 256];
    for raw in 0..=u8::MAX {
        palette[usize::from(raw)] = color_for_raw(grid, &sampler, u16::from(raw));
    }
    palette
}

fn build_u16_palette(grid: &MomentGrid, color_table: &ColorTable) -> Vec<[u8; 4]> {
    let sampler = color_table.sampler();
    let max_raw = match &grid.storage {
        MomentStorage::U16(values) => values.iter().copied().max().unwrap_or(0),
        _ => u16::MAX,
    };
    let mut palette = vec![[0, 0, 0, 0]; usize::from(max_raw) + 1];
    for raw in 0..=max_raw {
        palette[usize::from(raw)] = color_for_raw(grid, &sampler, raw);
    }
    palette
}

fn color_for_raw(grid: &MomentGrid, sampler: &ColorSampler, raw: u16) -> [u8; 4] {
    if grid.nodata == Some(raw) {
        return [0, 0, 0, 0];
    }
    if grid.range_folded == Some(raw) {
        return sampler.range_folded_color();
    }
    sampler.color_for_value((raw as f32 - grid.offset) / grid.scale)
}

fn max_range_m(grid: &MomentGrid) -> f32 {
    grid.gate_range.first_gate_m as f32
        + grid.gate_range.gate_spacing_m as f32 * grid.gate_range.gate_count as f32
}

fn azimuth_from_xy(dx: f32, dy: f32) -> f32 {
    let mut degrees = dx.atan2(dy) * 180.0 / PI;
    if degrees < 0.0 {
        degrees += 360.0;
    }
    degrees
}

struct AzimuthLookup {
    bins: Vec<AzimuthBin>,
}

impl AzimuthLookup {
    fn new(cut: &ElevationCut, grid: &MomentGrid) -> Self {
        Self::from_row_azimuths_iter(
            grid,
            grid.radial_indices
                .iter()
                .enumerate()
                .filter_map(|(row, radial_index)| {
                    cut.radials
                        .get(*radial_index)
                        .map(|radial| (row, radial.azimuth_deg))
                }),
        )
    }

    /// Lookup for a grid whose rows do NOT correspond to cut radials —
    /// the interpolated display grid carries its own synthetic per-row
    /// azimuths (one entry per grid row).
    fn from_row_azimuths(row_azimuths_deg: &[f32], grid: &MomentGrid) -> Self {
        Self::from_row_azimuths_iter(grid, row_azimuths_deg.iter().copied().enumerate())
    }

    fn from_row_azimuths_iter(
        grid: &MomentGrid,
        row_azimuths: impl Iterator<Item = (usize, f32)>,
    ) -> Self {
        let mut groups = vec![None; AZIMUTH_BINS];
        for (row, azimuth_deg) in row_azimuths {
            let azimuth = azimuth_deg.rem_euclid(360.0);
            let bin = azimuth_bin(azimuth);
            let group = groups[bin].get_or_insert_with(|| AzimuthGroup {
                azimuth: bin as f32 * AZIMUTH_BIN_WIDTH_DEG,
                candidates: Vec::new(),
            });
            group.candidates.push(RowCandidate {
                row,
                valid_extent: row_valid_extent(grid, row),
            });
        }

        let mut groups = groups.into_iter().flatten().collect::<Vec<_>>();
        for group in &mut groups {
            group
                .candidates
                .sort_by_key(|candidate| std::cmp::Reverse(candidate.rank()));
        }
        groups.sort_by(|left, right| left.azimuth.total_cmp(&right.azimuth));

        let mut bins = vec![AzimuthBin::default(); AZIMUTH_BINS];
        if groups.is_empty() {
            return Self { bins };
        }
        if groups.len() == 1 {
            fill_azimuth_bins(&mut bins, 0.0, 360.0, &groups[0].candidates);
            return Self { bins };
        }

        for index in 0..groups.len() {
            let group = &groups[index];
            let prev_azimuth = groups
                .get(index.wrapping_sub(1))
                .or_else(|| groups.last())
                .map(|group| group.azimuth)
                .unwrap_or(group.azimuth);
            let next_azimuth = groups
                .get(index + 1)
                .or_else(|| groups.first())
                .map(|group| group.azimuth)
                .unwrap_or(group.azimuth);
            let left_width = (clockwise_delta_deg(prev_azimuth, group.azimuth) * 0.5)
                .min(MAX_AZIMUTH_HALF_WIDTH_DEG);
            let right_width = (clockwise_delta_deg(group.azimuth, next_azimuth) * 0.5)
                .min(MAX_AZIMUTH_HALF_WIDTH_DEG);
            fill_azimuth_bins(
                &mut bins,
                group.azimuth - left_width,
                group.azimuth + right_width,
                &group.candidates,
            );
        }

        Self { bins }
    }

    #[cfg(test)]
    fn row_for_azimuth(&self, azimuth_deg: f32) -> Option<usize> {
        self.candidates_for_bin(self.filled_bin_for_azimuth(azimuth_deg)?)
            .first()
            .map(|candidate| candidate.row)
    }

    fn filled_bin_for_azimuth(&self, azimuth_deg: f32) -> Option<usize> {
        let bin = azimuth_bin(azimuth_deg);
        (!self.bins[bin].is_empty()).then_some(bin)
    }

    fn candidates_for_bin(&self, bin: usize) -> &[RowCandidate] {
        self.bins[bin].candidates()
    }
}

#[derive(Clone, Copy, Debug)]
struct RowCandidate {
    row: usize,
    valid_extent: usize,
}

impl RowCandidate {
    fn rank(self) -> (usize, usize) {
        (self.valid_extent, self.row)
    }
}

impl Default for RowCandidate {
    fn default() -> Self {
        Self {
            row: usize::MAX,
            valid_extent: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct AzimuthGroup {
    azimuth: f32,
    candidates: Vec<RowCandidate>,
}

#[derive(Clone, Copy, Debug)]
struct AzimuthBin {
    candidates: [RowCandidate; MAX_AZIMUTH_CANDIDATES],
    len: usize,
}

impl Default for AzimuthBin {
    fn default() -> Self {
        Self {
            candidates: [RowCandidate::default(); MAX_AZIMUTH_CANDIDATES],
            len: 0,
        }
    }
}

impl AzimuthBin {
    fn is_empty(self) -> bool {
        self.len == 0
    }

    fn candidates(&self) -> &[RowCandidate] {
        &self.candidates[..self.len]
    }

    fn push_candidate(&mut self, candidate: RowCandidate) {
        if self
            .candidates()
            .iter()
            .any(|existing| existing.row == candidate.row)
        {
            return;
        }

        let insert_at = self
            .candidates()
            .iter()
            .position(|existing| candidate.rank() > existing.rank())
            .unwrap_or(self.len);
        if self.len < MAX_AZIMUTH_CANDIDATES {
            for index in (insert_at..self.len).rev() {
                self.candidates[index + 1] = self.candidates[index];
            }
            self.candidates[insert_at] = candidate;
            self.len += 1;
        } else if insert_at < MAX_AZIMUTH_CANDIDATES {
            for index in (insert_at..MAX_AZIMUTH_CANDIDATES - 1).rev() {
                self.candidates[index + 1] = self.candidates[index];
            }
            self.candidates[insert_at] = candidate;
        }
    }
}

fn azimuth_bin(azimuth_deg: f32) -> usize {
    ((azimuth_deg.rem_euclid(360.0) / AZIMUTH_BIN_WIDTH_DEG).round() as usize) % AZIMUTH_BINS
}

fn row_valid_extent(grid: &MomentGrid, row: usize) -> usize {
    let gate_count = grid.gate_range.gate_count;
    let start = row.saturating_mul(gate_count);
    let Some(end) = start.checked_add(gate_count) else {
        return 0;
    };
    match &grid.storage {
        MomentStorage::U8(values) => values
            .get(start..end)
            .and_then(|row| {
                row.iter().rposition(|raw| {
                    let raw = u16::from(*raw);
                    grid.nodata != Some(raw)
                })
            })
            .map(|gate| gate + 1)
            .unwrap_or(0),
        MomentStorage::U16(values) => values
            .get(start..end)
            .and_then(|row| row.iter().rposition(|raw| grid.nodata != Some(*raw)))
            .map(|gate| gate + 1)
            .unwrap_or(0),
        MomentStorage::F32(values) => values
            .get(start..end)
            .and_then(|row| row.iter().rposition(|value| value.is_finite()))
            .map(|gate| gate + 1)
            .unwrap_or(0),
    }
}

fn fill_azimuth_bins(bins: &mut [AzimuthBin], start_deg: f32, end_deg: f32, rows: &[RowCandidate]) {
    let start_bin = (start_deg / AZIMUTH_BIN_WIDTH_DEG).floor() as i32;
    let end_bin = (end_deg / AZIMUTH_BIN_WIDTH_DEG).ceil() as i32;
    for bin in start_bin..=end_bin {
        let target = &mut bins[bin.rem_euclid(AZIMUTH_BINS as i32) as usize];
        for row in rows {
            target.push_candidate(*row);
        }
    }
}

fn clockwise_delta_deg(from_deg: f32, to_deg: f32) -> f32 {
    (to_deg - from_deg).rem_euclid(360.0)
}

fn row_motion_components(
    cut: &ElevationCut,
    grid: &MomentGrid,
    storm_motion: StormMotion,
) -> Vec<f32> {
    grid.radial_indices
        .iter()
        .map(|radial_index| {
            cut.radials
                .get(*radial_index)
                .map(|radial| motion_component_away_mps(storm_motion, radial.azimuth_deg))
                .unwrap_or(0.0)
        })
        .collect()
}

pub fn storm_relative_velocity_mps(
    radar_velocity_mps: f32,
    beam_azimuth_deg: f32,
    storm_motion: StormMotion,
) -> f32 {
    radar_velocity_mps - motion_component_away_mps(storm_motion, beam_azimuth_deg)
}

fn motion_component_away_mps(storm_motion: StormMotion, beam_azimuth_deg: f32) -> f32 {
    let delta = (storm_motion.direction_deg - beam_azimuth_deg).to_radians();
    storm_motion.speed_mps * delta.cos()
}

pub fn color_family_for_moment(moment: &MomentType) -> ColorTableFamily {
    match moment {
        MomentType::Reflectivity => ColorTableFamily::Reflectivity,
        MomentType::Velocity => ColorTableFamily::Velocity,
        MomentType::SpectrumWidth => ColorTableFamily::SpectrumWidth,
        MomentType::CorrelationCoefficient => ColorTableFamily::CorrelationCoefficient,
        MomentType::DifferentialReflectivity => ColorTableFamily::DifferentialReflectivity,
        MomentType::DifferentialPhase => ColorTableFamily::DifferentialPhase,
        MomentType::SpecificDifferentialPhase => ColorTableFamily::SpecificDifferentialPhase,
        MomentType::Unknown(name) if unknown_reflectivity_like(name) => {
            ColorTableFamily::Reflectivity
        }
        _ => ColorTableFamily::Generic,
    }
}

/// A validation moment carries a physical display scale that is independent
/// of the user's ordinary radar-family palette binding. Keeping this resolver
/// at the render seam means every cache path (native, smoothed, interpolated,
/// and direct PNG) sees the same true 0..1 quality ramp or centered residual
/// ramp even when the caller supplied the Generic family for an Unknown id.
pub fn validation_color_table_for_moment(moment: &MomentType) -> Option<ColorTable> {
    let MomentType::Unknown(name) = moment else {
        return None;
    };
    color::validation_table_for_moment_id(name)
}

fn unknown_reflectivity_like(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "DBUZ" | "UDBZ" | "UDBZH" | "DBZ_U" | "THU" | "TVU"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // ---- real-data fixtures ----
    //
    // Expected values: `testdata/golden/render/*.json`, written by
    // `tools/render_bench_golden.py render` with Py-ART 2.2.5 (raw gate
    // codes, azimuths, gate geometry, `storm_relative_velocity`) and MetPy
    // 1.7.1 (scaled values), never with this workspace's readers.

    /// Parsed golden file `testdata/golden/render/<name>`.
    fn golden(name: &str) -> Value {
        let path = recast_radar_testdata::testdata_dir()
            .join("golden")
            .join("render")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    /// Decode a corpus Level II file with the NEXRAD reader.
    fn level2(path: &Path) -> RadarVolume {
        recast_radar_io_nexrad::decode_volume_from_path(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn as_usize(value: &Value) -> usize {
        value
            .as_u64()
            .unwrap_or_else(|| panic!("expected an unsigned integer, got {value}")) as usize
    }

    fn as_f32(value: &Value) -> f32 {
        value
            .as_f64()
            .unwrap_or_else(|| panic!("expected a number, got {value}")) as f32
    }

    fn array(value: &Value) -> &Vec<Value> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("expected an array, got {value}"))
    }

    fn row_gate(pair: &Value) -> (usize, usize) {
        (as_usize(&pair[0]), as_usize(&pair[1]))
    }

    fn u8_values(grid: &MomentGrid) -> &[u8] {
        let MomentStorage::U8(values) = &grid.storage else {
            panic!("{:?} should be stored as u8 codes", grid.moment);
        };
        values
    }

    fn azimuth_of(cut: &ElevationCut, grid: &MomentGrid, row: usize) -> f32 {
        cut.radials[grid.radial_indices[row]].azimuth_deg
    }

    /// The decoded grid must carry the golden gate geometry and, row by row,
    /// the golden ray azimuths (the f32 angle field of the file).
    fn assert_geometry(cut: &ElevationCut, grid: &MomentGrid, expected: &Value) {
        assert_eq!(grid.radial_count(), as_usize(&expected["rows"]));
        assert_eq!(grid.gate_range.gate_count, as_usize(&expected["gates"]));
        assert_eq!(
            grid.gate_range.first_gate_m,
            expected["first_gate_m"].as_i64().expect("first gate") as i32
        );
        assert_eq!(
            grid.gate_range.gate_spacing_m,
            expected["gate_spacing_m"].as_i64().expect("gate spacing") as i32
        );
        let azimuths = array(&expected["azimuth_deg"]);
        assert_eq!(azimuths.len(), grid.radial_count());
        for (row, azimuth) in azimuths.iter().enumerate() {
            assert!(
                (azimuth_of(cut, grid, row) - as_f32(azimuth)).abs() < 1e-4,
                "row {row}: azimuth {} != {azimuth}",
                azimuth_of(cut, grid, row)
            );
        }
    }

    /// A 65 x 65 viewport at 50 m per pixel whose centre pixel (32, 32) sits
    /// exactly on the centre of `gate` of `row`; returns the options and the
    /// centre pixel's index.
    fn gate_centre_viewport(
        cut: &ElevationCut,
        grid: &MomentGrid,
        row: usize,
        gate: usize,
    ) -> (ViewportRasterOptions, usize) {
        const SIZE: u32 = 65;
        const KM_PER_PX: f32 = 0.05;
        let centre_px = (SIZE / 2) as f32 + 0.5;
        let azimuth = azimuth_of(cut, grid, row).to_radians();
        let range_km = (grid.gate_range.first_gate_m as f32
            + gate as f32 * grid.gate_range.gate_spacing_m as f32)
            / 1000.0;
        let options = ViewportRasterOptions {
            width: SIZE,
            height: SIZE,
            radar_x_px: centre_px - range_km * azimuth.sin() / KM_PER_PX,
            radar_y_px: centre_px + range_km * azimuth.cos() / KM_PER_PX,
            km_per_px_x: KM_PER_PX,
            km_per_px_y: KM_PER_PX,
            rotation_rad: 0.0,
        };
        let centre = (SIZE / 2) as usize * SIZE as usize + (SIZE / 2) as usize;
        (options, centre)
    }

    fn pixel(pixels: &[u8], index: usize) -> [u8; 4] {
        let mut color = [0; 4];
        color.copy_from_slice(&pixels[index * 4..index * 4 + 4]);
        color
    }

    fn assert_color_close(actual: [u8; 4], expected: [u8; 4], what: &str) {
        assert!(
            actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.abs_diff(*b) <= 1),
            "{what}: {actual:?} != {expected:?}"
        );
    }

    fn opaque_pixels(pixels: &[u8]) -> usize {
        pixels.chunks_exact(4).filter(|pixel| pixel[3] != 0).count()
    }

    fn has_visible_pixel(pixels: &[u8]) -> bool {
        pixels.chunks_exact(4).any(|pixel| pixel[3] != 0)
    }

    fn has_transparent_pixel(pixels: &[u8]) -> bool {
        pixels.chunks_exact(4).any(|pixel| pixel[3] == 0)
    }

    fn sample_viewport_options() -> ViewportRasterOptions {
        ViewportRasterOptions {
            width: 800,
            height: 600,
            radar_x_px: 400.0,
            radar_y_px: 300.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.1,
        }
    }

    /// 333 x 217 viewport at 0.5 km per pixel, radar at the centre: the
    /// storm-scale window the cache tests share.
    fn window_viewport_options() -> ViewportRasterOptions {
        ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        }
    }

    // ---- options and tables (no radar data) ----

    #[test]
    fn base_layer_starts_visible() {
        assert!(RenderLayer::base(MomentType::Reflectivity).visible);
    }

    #[test]
    fn supersample_factor_one_is_identity() {
        let base = sample_viewport_options();
        assert_eq!(base.supersampled(1), base);
        assert_eq!(base.supersampled(0), base);
    }

    #[test]
    fn supersample_scales_pixels_and_preserves_ground_coverage() {
        let base = sample_viewport_options();
        let hi = base.supersampled(4);
        assert_eq!(hi.width, 3200);
        assert_eq!(hi.height, 2400);
        assert_eq!(hi.radar_x_px, 1600.0);
        assert_eq!(hi.radar_y_px, 1200.0);
        // rotation is unchanged; ground covered (dimension · km_per_px) invariant.
        assert_eq!(hi.rotation_rad, base.rotation_rad);
        assert!(
            ((hi.width as f32 * hi.km_per_px_x) - (base.width as f32 * base.km_per_px_x)).abs()
                < 1e-3
        );
        assert!(
            ((hi.height as f32 * hi.km_per_px_y) - (base.height as f32 * base.km_per_px_y)).abs()
                < 1e-3
        );
    }

    #[test]
    fn supersample_clamps_to_the_dimension_ceiling() {
        // Ultra (4x) on a 1500-wide base would be 6000px; clamp holds it at 4096.
        let base = ViewportRasterOptions {
            width: 1500,
            height: 1000,
            radar_x_px: 0.0,
            radar_y_px: 0.0,
            km_per_px_x: 1.0,
            km_per_px_y: 1.0,
            rotation_rad: 0.0,
        };
        let hi = base.supersampled(4);
        assert!(hi.width <= ViewportRasterOptions::MAX_SUPERSAMPLED_DIMENSION);
        assert!(hi.height <= ViewportRasterOptions::MAX_SUPERSAMPLED_DIMENSION);
        assert_eq!(hi.width, 4096);
        // Ground coverage stays invariant even when clamped.
        assert!(
            ((hi.width as f32 * hi.km_per_px_x) - (base.width as f32 * base.km_per_px_x)).abs()
                < 1.0
        );
    }

    #[test]
    fn supersample_never_downscales_an_oversized_base() {
        // A base already past the ceiling is left untouched (today's behavior).
        let base = ViewportRasterOptions {
            width: 5000,
            height: 3000,
            radar_x_px: 10.0,
            radar_y_px: 20.0,
            km_per_px_x: 0.25,
            km_per_px_y: 0.25,
            rotation_rad: 0.0,
        };
        assert_eq!(base.supersampled(2), base);
    }

    #[test]
    fn unfiltered_reflectivity_codes_use_reflectivity_coloring() {
        assert_eq!(
            color_family_for_moment(&MomentType::Unknown("dBuZ".to_owned())),
            ColorTableFamily::Reflectivity
        );
        assert_eq!(
            color_family_for_moment(&MomentType::Unknown("mystery".to_owned())),
            ColorTableFamily::Generic
        );
    }

    #[test]
    fn synthetic_validation_moments_resolve_physical_palettes() {
        let quality = validation_color_table_for_moment(&MomentType::Unknown("MCOV".to_owned()))
            .expect("quality palette");
        assert_eq!(quality.stops().first().unwrap().value, 0.0);
        assert_eq!(quality.stops().last().unwrap().value, 1.0);

        for id in [
            "DIF_REF", "DIF_VEL", "DIF_ZDR", "DIF_RHO", "DIF_PHI", "DIF_KDP",
        ] {
            let table = validation_color_table_for_moment(&MomentType::Unknown(id.to_owned()))
                .expect("difference palette");
            assert_eq!(
                table.stops().first().unwrap().value,
                -table.stops().last().unwrap().value,
                "{id}"
            );
        }
        assert!(
            validation_color_table_for_moment(&MomentType::Unknown("OTHER".to_owned())).is_none()
        );
    }

    #[test]
    fn azimuth_places_north_at_zero_degrees() {
        assert_eq!(azimuth_from_xy(0.0, 1.0).round(), 0.0);
        assert_eq!(azimuth_from_xy(1.0, 0.0).round(), 90.0);
        assert_eq!(azimuth_from_xy(0.0, -1.0).round(), 180.0);
        assert_eq!(azimuth_from_xy(-1.0, 0.0).round(), 270.0);
    }

    #[test]
    fn velocity_table_has_a_hard_zero_boundary() {
        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Velocity);
        let inbound = table.color_for_value(-2.0);
        let outbound = table.color_for_value(2.0);
        let neutral = table.color_for_value(0.0);

        assert_ne!(inbound, outbound);
        assert_ne!(neutral, inbound);
        assert_ne!(neutral, outbound);
    }

    #[test]
    fn range_folded_gates_are_visible() {
        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Velocity);

        assert_eq!(table.range_folded_color()[3], 245);
    }

    #[test]
    fn storm_relative_velocity_subtracts_motion_along_beam() {
        let storm_motion = StormMotion {
            direction_deg: 0.0,
            speed_mps: 10.0,
        };

        assert_eq!(
            storm_relative_velocity_mps(10.0, 0.0, storm_motion).round(),
            0.0
        );
        assert_eq!(
            storm_relative_velocity_mps(10.0, 180.0, storm_motion).round(),
            20.0
        );
        assert_eq!(
            storm_relative_velocity_mps(10.0, 90.0, storm_motion).round(),
            10.0
        );
    }

    #[test]
    fn cached_sample_packs_lookup_into_four_bytes() {
        assert_eq!(std::mem::size_of::<CachedSample>(), 4);

        let sample = ResolvedSample {
            row: 3_599,
            gate: 1_832,
        };
        let cached = CachedSample::new(sample).expect("sample fits packed cache entry");

        assert_eq!(cached.sample(), Some(sample));
        let skip = CachedSample::skip(37).expect("skip fits packed cache entry");
        assert_eq!(skip.skip_len(), Some(37));
        assert_eq!(skip.sample(), None);
        assert_eq!(
            CachedSample::new(ResolvedSample {
                row: CachedSample::ROW_LIMIT,
                gate: 0
            }),
            None
        );
    }

    #[test]
    fn sample_cache_storage_upper_bound_scales_with_viewport_pixels() {
        let options = ViewportRasterOptions {
            width: 1_920,
            height: 1_080,
            radar_x_px: 960.0,
            radar_y_px: 540.0,
            km_per_px_x: 1.0,
            km_per_px_y: 1.0,
            rotation_rad: 0.0,
        };

        assert_eq!(
            viewport_sample_cache_storage_upper_bound(options),
            1_920 * 1_080 * std::mem::size_of::<CachedSample>()
                + 1_080 * std::mem::size_of::<CachedRowSpan>()
        );
    }

    // ---- range folding and palettes on the KTLX 2024-03-15 split cut ----

    /// Range-folded velocity gates (raw code 1: 342 of them in the 0.48 deg
    /// Doppler cut, located with Py-ART) take the velocity table's
    /// range-folded colour, in the palette and in rendered pixels.
    #[test]
    fn velocity_range_folded_bins_render_table_rf_color() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&doppler["sweep"]);
        let cut = &volume.cuts[cut_index];
        let grid = cut
            .moments
            .get(&MomentType::Velocity)
            .expect("velocity grid");
        assert_geometry(cut, grid, doppler);
        assert_eq!(grid.nodata, Some(as_usize(&doppler["no_data_code"]) as u16));
        assert_eq!(
            grid.range_folded,
            Some(as_usize(&doppler["range_folded_code"]) as u16)
        );
        let values = u8_values(grid);
        let gates = grid.gate_range.gate_count;
        assert_eq!(
            values.iter().filter(|&&code| code == 1).count(),
            as_usize(&doppler["range_folded_gates"])
        );
        assert_eq!(
            values.iter().filter(|&&code| code == 0).count(),
            as_usize(&doppler["no_data_gates"])
        );
        for pair in array(&doppler["range_folded_first"]) {
            let (row, gate) = row_gate(pair);
            assert_eq!(values[row * gates + gate], 1, "row {row} gate {gate}");
            assert_eq!(grid.scaled_value(row, gate), None);
        }

        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Velocity);
        let range_folded = table.range_folded_color();
        assert_ne!(range_folded[3], 0);
        assert_eq!(color_for_raw(grid, &table.sampler(), 1), range_folded);
        assert_eq!(build_u8_palette(grid, table)[1], range_folded);

        for pair in array(&doppler["range_folded_interior"]) {
            let (row, gate) = row_gate(pair);
            let (options, centre) = gate_centre_viewport(cut, grid, row, gate);
            let (_, _, pixels) =
                render_moment_viewport_rgba(&volume, cut_index, MomentType::Velocity, options)
                    .expect("viewport velocity");
            assert_eq!(
                pixel(&pixels, centre),
                range_folded,
                "pixel on range-folded gate row {row} gate {gate}"
            );
        }
    }

    /// The Doppler cut's reflectivity carries the same 342 range-folded codes
    /// as its velocity; they render with the reflectivity table's range-folded
    /// colour.
    #[test]
    fn reflectivity_range_folded_bins_render_table_rf_color() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler_reflectivity"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&doppler["sweep"]);
        let cut = &volume.cuts[cut_index];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, doppler);
        assert_eq!(grid.range_folded, Some(1));
        let values = u8_values(grid);
        let gates = grid.gate_range.gate_count;
        assert_eq!(
            values.iter().filter(|&&code| code == 1).count(),
            as_usize(&doppler["range_folded_gates"])
        );
        for pair in array(&doppler["range_folded_first"]) {
            let (row, gate) = row_gate(pair);
            assert_eq!(values[row * gates + gate], 1, "row {row} gate {gate}");
        }

        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Reflectivity);
        let range_folded = table.range_folded_color();
        assert_eq!(color_for_raw(grid, &table.sampler(), 1), range_folded);

        for pair in array(&doppler["range_folded_interior"]) {
            let (row, gate) = row_gate(pair);
            let (options, centre) = gate_centre_viewport(cut, grid, row, gate);
            let (_, _, pixels) =
                render_moment_viewport_rgba(&volume, cut_index, MomentType::Reflectivity, options)
                    .expect("viewport reflectivity");
            assert_eq!(
                pixel(&pixels, centre),
                range_folded,
                "pixel on range-folded gate row {row} gate {gate}"
            );
        }
    }

    /// Per-row storm-relative palettes: for real velocity gates the palette
    /// colour is the table colour of Py-ART's `storm_relative_velocity`
    /// (storm from 225 deg at 18 m/s) for the same gate, and equals the direct
    /// colour math; no-data and range-folded codes keep their colours.
    #[test]
    fn storm_relative_u8_row_palette_matches_pyart_storm_relative_velocity() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&doppler["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Velocity)
            .expect("velocity grid");
        let values = u8_values(grid);
        let gates = grid.gate_range.gate_count;
        let storm_motion = StormMotion {
            direction_deg: as_f32(&doppler["storm_motion"]["direction_deg"]),
            speed_mps: as_f32(&doppler["storm_motion"]["speed_mps"]),
        };
        let tables = ColorTableSet::default();
        let color_table = tables.for_family(ColorTableFamily::Velocity);
        let sampler = color_table.sampler();
        let row_motion = StormMotionBasis::new(cut, grid).row_motion_components(storm_motion);
        let palettes = build_storm_relative_u8_row_palettes(grid, &row_motion, color_table);
        assert_eq!(palettes.len(), grid.radial_count());

        let samples = array(&doppler["storm_relative_samples"]);
        assert!(samples.len() >= 32);
        for sample in samples {
            let (row, gate) = (as_usize(&sample["row"]), as_usize(&sample["gate"]));
            let code = as_usize(&sample["code"]);
            let velocity = as_f32(&sample["velocity_mps"]);
            let storm_relative = as_f32(&sample["storm_relative_mps"]);
            assert_eq!(usize::from(values[row * gates + gate]), code);
            assert_eq!(grid.scaled_value(row, gate), Some(velocity));
            let azimuth = azimuth_of(cut, grid, row);
            assert!(
                (storm_relative_velocity_mps(velocity, azimuth, storm_motion) - storm_relative)
                    .abs()
                    < 1e-3,
                "row {row} gate {gate}"
            );
            assert!((velocity - row_motion[row] - storm_relative).abs() < 1e-3);
            let color = palettes[row][code];
            assert_eq!(
                color,
                storm_relative_u8_color_for_raw(grid, &sampler, code as u8, row_motion[row])
            );
            assert_color_close(
                color,
                sampler.color_for_value(storm_relative),
                &format!("row {row} code {code}"),
            );
        }
        for row in [0, grid.radial_count() / 2, grid.radial_count() - 1] {
            assert_eq!(palettes[row][0], [0, 0, 0, 0]);
            assert_eq!(palettes[row][1], color_table.range_folded_color());
        }
    }

    /// A custom velocity ramp sampled through the u8 palette of the real grid
    /// (scale 2, offset 129 from the file's data block header): every code
    /// present in the sweep maps to the ramp colour of its physical velocity,
    /// including the exact stops at 0 m/s (code 129) and 20 m/s (code 169).
    #[test]
    fn custom_color_table_feeds_precomputed_u8_palette() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&doppler["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Velocity)
            .expect("velocity grid");
        assert_eq!(grid.scale, as_f32(&doppler["scale"]));
        assert_eq!(grid.offset, as_f32(&doppler["offset"]));
        let table = ColorTable::parse(
            "unit test velocity",
            "units: m/s\ncolor: -20 1 2 3\ncolor: 0 10 20 30\ncolor: 20 40 50 60",
        )
        .expect("custom color table");

        let palette = build_u8_palette(grid, &table);

        let values = u8_values(grid);
        let samples = array(&doppler["custom_table_samples"]);
        assert!(samples.len() > 50);
        let mut exact_stops = 0;
        for sample in samples {
            let code = as_usize(&sample["code"]);
            let velocity = as_f32(&sample["velocity_mps"]);
            assert!(
                values.contains(&(code as u8)),
                "code {code} is in the sweep"
            );
            assert_eq!(
                palette[code],
                table.color_for_value(velocity),
                "code {code}"
            );
            match velocity {
                0.0 => {
                    assert_eq!(palette[code], [10, 20, 30, 255]);
                    exact_stops += 1;
                }
                10.0 => {
                    assert_eq!(palette[code], [25, 35, 45, 255]);
                    exact_stops += 1;
                }
                20.0 => {
                    assert_eq!(palette[code], [40, 50, 60, 255]);
                    exact_stops += 1;
                }
                _ => {}
            }
        }
        assert_eq!(exact_stops, 3);
        assert_eq!(palette[0], [0, 0, 0, 0]);
        assert_eq!(palette[1], table.range_folded_color());
    }

    /// The per-row storm-motion basis reproduces `speed · cos(direction −
    /// azimuth)` on the file's azimuths (Py-ART's storm-relative correction).
    #[test]
    fn storm_motion_basis_matches_direct_projection() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&doppler["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Velocity)
            .expect("velocity grid");
        assert_geometry(cut, grid, doppler);
        let basis = StormMotionBasis::new(cut, grid);
        let storm_motion = StormMotion {
            direction_deg: 225.0,
            speed_mps: 18.0,
        };
        let row_motion = basis.row_motion_components(storm_motion);
        assert_eq!(row_motion.len(), grid.radial_count());

        for (row, azimuth) in array(&doppler["azimuth_deg"]).iter().enumerate() {
            let azimuth = as_f32(azimuth);
            let reference =
                storm_motion.speed_mps * (storm_motion.direction_deg - azimuth).to_radians().cos();
            assert!(
                (row_motion[row] - reference).abs() < 1e-4,
                "row {row}: {} != {reference}",
                row_motion[row]
            );
            let direct = motion_component_away_mps(storm_motion, azimuth_of(cut, grid, row));
            assert!((row_motion[row] - direct).abs() < 1e-5);
        }
    }

    // ---- viewport geometry on the KTLX 2024-03-15 surveillance cut ----

    /// The sample-cache bound follows the radar's 460 km footprint (1832 gates
    /// of 250 m from 2125 m): between the exact pixel count inside that circle
    /// and that count plus the row-span padding, and below the full viewport.
    #[test]
    fn grid_sample_cache_upper_bound_tracks_actual_radar_footprint() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let max_range_km = as_f32(&surveillance["max_range_m"]) / 1000.0;
        assert_eq!(max_range_m(grid) / 1000.0, max_range_km);
        let options = ViewportRasterOptions {
            width: 1_920,
            height: 1_080,
            radar_x_px: 960.0,
            radar_y_px: 540.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };

        let full_viewport = viewport_sample_cache_storage_upper_bound(options);
        let radar_footprint = viewport_sample_cache_storage_upper_bound_for_grid(grid, options);
        let span_bytes = 1_080 * std::mem::size_of::<CachedRowSpan>();

        assert!(radar_footprint < full_viewport);
        assert!(radar_footprint > span_bytes);
        let slots = (radar_footprint - span_bytes) / std::mem::size_of::<CachedSample>();
        // Pixels whose centre lies within the footprint circle.
        let mut inside = 0usize;
        for y in 0..options.height {
            let dy_km = (options.radar_y_px - (y as f32 + 0.5)) * options.km_per_px_y;
            for x in 0..options.width {
                let dx_km = (x as f32 + 0.5 - options.radar_x_px) * options.km_per_px_x;
                if dx_km.hypot(dy_km) <= max_range_km {
                    inside += 1;
                }
            }
        }
        assert!(inside > 0);
        assert!(
            slots >= inside && slots <= inside + 4 * options.height as usize,
            "{slots} sample slots for {inside} pixels inside the {max_range_km} km circle"
        );
    }

    #[test]
    fn viewport_lookup_matches_reference_hypot_formula() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let row_lookup = AzimuthLookup::new(cut, grid);
        let max_range_km = max_range_m(grid).max(1.0) / 1000.0;
        let geometry = ViewportGeometry {
            width: 333,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            max_range_km_sq: max_range_km * max_range_km,
            rot_sin: 0.0,
            rot_cos: 1.0,
        };

        // The kept radials run clockwise from 167 deg through west to 47 deg:
        // pixels west, south and north of the radar resolve, the centre pixel
        // (inside the first gate) and pixels to the east do not.
        let mut resolved = 0;
        for (x, y) in [
            (0, 0),
            (10, 108),
            (166, 200),
            (166, 10),
            (60, 60),
            (166, 108),
            (180, 110),
            (220, 70),
            (332, 216),
        ] {
            let sample = viewport_lookup(x, y, grid, &row_lookup, geometry);
            assert_eq!(
                sample,
                viewport_lookup_reference(x, y, grid, &row_lookup, geometry)
            );
            resolved += usize::from(sample.is_some());
        }
        assert_eq!(resolved, 5);
    }

    #[test]
    fn viewport_lookup_table_matches_reference_hypot_formula() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let row_lookup = AzimuthLookup::new(cut, grid);
        let geometry = viewport_geometry(grid, window_viewport_options());
        let lookup_table = ViewportLookupTable::new(grid, geometry);

        let mut resolved = 0;
        for y in [0, 10, 70, 108, 140, 216] {
            for x in [0, 20, 120, 166, 180, 260, 332] {
                let table_sample = lookup_table.row(y).and_then(|row| {
                    row.x_range
                        .contains(&x)
                        .then(|| row.lookup(x, &row_lookup))
                        .flatten()
                });
                assert_eq!(
                    table_sample,
                    viewport_lookup_reference(x, y, grid, &row_lookup, geometry),
                    "lookup mismatch at {x},{y}"
                );
                resolved += usize::from(table_sample.is_some());
            }
        }
        assert!(resolved >= 12, "only {resolved} probe pixels resolved");
    }

    /// The fast table path must agree with `viewport_lookup` (whose
    /// rotation convention is pinned by `rotated_north_pixel_resolves_to_
    /// azimuth_zero`) when a convergence angle is baked in — the table
    /// used to drop the rotation entirely (field-reported skew).
    #[test]
    fn viewport_lookup_table_matches_rotated_viewport_lookup() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let row_lookup = AzimuthLookup::new(cut, grid);
        for rotation_rad in [-0.21f32, 0.005, 0.35] {
            let geometry = viewport_geometry(
                grid,
                ViewportRasterOptions {
                    rotation_rad,
                    ..window_viewport_options()
                },
            );
            let lookup_table = ViewportLookupTable::new(grid, geometry);
            let mut resolved = 0usize;
            for y in 0..217 {
                let row = lookup_table.row(y);
                for x in 0..333 {
                    let table_sample = row.as_ref().and_then(|row| {
                        row.x_range
                            .contains(&x)
                            .then(|| row.lookup(x, &row_lookup))
                            .flatten()
                    });
                    assert_eq!(
                        table_sample,
                        viewport_lookup(x, y, grid, &row_lookup, geometry),
                        "rotated lookup mismatch at {x},{y} (gamma {rotation_rad})"
                    );
                    resolved += usize::from(table_sample.is_some());
                }
            }
            // Two thirds of the circle are kept radials: well over a third of
            // the window resolves.
            assert!(resolved > 333 * 217 / 3, "{resolved} pixels resolved");
        }
    }

    /// Rotation must actually FLOW through the table path: a 0.35 rad
    /// baked convergence has to move some pixels into different azimuth
    /// bins than the unrotated table. The bug was the table silently
    /// ignoring the baked angle, which kept the two identical (so the
    /// parity test above passed at rotation 0 while the screen skewed).
    #[test]
    fn baked_rotation_changes_table_azimuth_bins() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let row_lookup = AzimuthLookup::new(cut, grid);
        let options = |rotation_rad| ViewportRasterOptions {
            width: 96,
            height: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad,
        };
        let rotated = ViewportLookupTable::new(grid, viewport_geometry(grid, options(0.35)));
        let straight = ViewportLookupTable::new(grid, viewport_geometry(grid, options(0.0)));
        let sample_at = |table: &ViewportLookupTable, x: u32, y: u32| {
            table.row(y).and_then(|row| {
                row.x_range
                    .contains(&x)
                    .then(|| row.lookup(x, &row_lookup))
                    .flatten()
            })
        };
        let mut resolved = 0usize;
        let mut moved_bins = 0usize;
        for y in 0..96 {
            for x in 0..96 {
                let (a, b) = (sample_at(&rotated, x, y), sample_at(&straight, x, y));
                if let (Some(a), Some(b)) = (a, b) {
                    resolved += 1;
                    // Range is rotation-invariant; only azimuth may move.
                    assert_eq!(a.gate, b.gate, "gate changed under rotation at {x},{y}");
                    if a.azimuth_bin != b.azimuth_bin {
                        moved_bins += 1;
                    }
                }
            }
        }
        assert!(resolved > 100, "sweep barely hit the volume ({resolved})");
        // 0.35 rad ≈ 20°: against 0.5° super-resolution radials nearly every
        // pixel must land in a different radial than the unrotated table.
        assert!(
            moved_bins * 2 > resolved,
            "baked rotation moved only {moved_bins}/{resolved} pixels — rotation is not reaching the table path"
        );
    }

    #[test]
    fn viewport_row_span_covers_reference_samples() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let row_lookup = AzimuthLookup::new(cut, grid);
        let max_range_m = max_range_m(grid).max(1.0);
        let max_range_km = max_range_m / 1000.0;
        // 96 px at 10 km per pixel: the 460 km footprint ends inside the window.
        let geometry = ViewportGeometry {
            width: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 10.0,
            km_per_px_y: 10.0,
            max_range_km_sq: max_range_km * max_range_km,
            rot_sin: 0.0,
            rot_cos: 1.0,
        };

        let mut covered = 0usize;
        for y in 0..96 {
            let span = geometry.x_range_for_row(y);
            for x in 0..96 {
                if viewport_lookup_reference(x, y, grid, &row_lookup, geometry).is_some() {
                    assert!(
                        span.as_ref().is_some_and(|range| range.contains(&x)),
                        "row span missed reference sample at ({x}, {y})"
                    );
                    covered += 1;
                }
            }
        }
        assert!(covered > 1_000, "{covered} reference samples");
        assert!(geometry.x_range_for_row(0).is_none());
        assert!(geometry.x_range_for_row(95).is_none());
    }

    // ---- azimuth lookup ----

    /// Legacy 1 deg radials (KTLX 1999-05-04, Message 1, 367 radials with
    /// 1 km reflectivity gates) fill every 0.1 deg bin of the circle, and a
    /// query azimuth resolves to the nearest radial (numpy, angular distance
    /// on the file's azimuths).
    #[test]
    fn azimuth_lookup_fills_wider_native_radial_sectors() {
        let expected = golden("ktlx1999.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let spacing = as_f32(&surveillance["median_spacing_deg"]);
        assert!(spacing > 0.9 && spacing < 1.0, "{spacing} deg radials");

        let lookup = AzimuthLookup::new(cut, grid);
        for bin in 0..AZIMUTH_BINS {
            let azimuth = bin as f32 * AZIMUTH_BIN_WIDTH_DEG;
            assert!(
                lookup.row_for_azimuth(azimuth).is_some(),
                "no radial serves azimuth {azimuth}"
            );
        }
        let queries = array(&surveillance["nearest_ray_queries"]);
        assert!(queries.len() >= 50);
        for query in queries {
            let azimuth = as_f32(&query["azimuth_deg"]);
            assert_eq!(
                lookup.row_for_azimuth(azimuth),
                Some(as_usize(&query["row"])),
                "azimuth {azimuth}"
            );
        }
    }

    /// Where two neighbouring radials both serve an azimuth bin (the bin at
    /// the midpoint between them), the one whose data reaches farther wins:
    /// row valid extents come from Py-ART's raw codes, and a gate that only the
    /// longer row fills resolves to that row.
    #[test]
    fn azimuth_lookup_prefers_neighbour_row_with_longer_valid_extent() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&surveillance["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Reflectivity)
            .expect("reflectivity grid");
        assert_geometry(cut, grid, surveillance);
        let values = u8_values(grid);
        let gates = grid.gate_range.gate_count;
        for (row, extent) in array(&surveillance["valid_extent"]).iter().enumerate() {
            assert_eq!(row_valid_extent(grid, row), as_usize(extent), "row {row}");
        }

        let lookup = AzimuthLookup::new(cut, grid);
        let pairs = array(&surveillance["longer_extent_neighbours"]);
        assert!(pairs.len() >= 16);
        for pair in pairs {
            let (first, second) = row_gate(&pair["rows"]);
            assert_eq!(second, first + 1);
            let longer = as_usize(&pair["longer_row"]);
            let shorter = if longer == first { second } else { first };
            let gate = as_usize(&pair["gate"]);
            assert_eq!(gate + 1, row_valid_extent(grid, longer));
            assert!(gate >= row_valid_extent(grid, shorter));
            assert_eq!(values[shorter * gates + gate], 0);
            assert_ne!(values[longer * gates + gate], 0);

            // Bins at the midpoint of the two radials' 0.1 deg bin centres.
            let first_bin = azimuth_bin(azimuth_of(cut, grid, first));
            let mut second_bin = azimuth_bin(azimuth_of(cut, grid, second));
            if second_bin < first_bin {
                second_bin += AZIMUTH_BINS;
            }
            let sum = first_bin + second_bin;
            let probes: Vec<usize> = if sum.is_multiple_of(2) {
                vec![(sum / 2) % AZIMUTH_BINS]
            } else {
                vec![(sum / 2) % AZIMUTH_BINS, (sum / 2 + 1) % AZIMUTH_BINS]
            };
            for bin in probes {
                let candidates: Vec<usize> = lookup
                    .candidates_for_bin(bin)
                    .iter()
                    .map(|candidate| candidate.row)
                    .collect();
                assert!(
                    candidates.contains(&first) && candidates.contains(&second),
                    "bin {bin} between rows {first} and {second} has candidates {candidates:?}"
                );
                assert_eq!(candidates[0], longer, "bin {bin}");
                let sample = SampleLookup {
                    azimuth_bin: bin,
                    gate,
                };
                let resolved = resolve_compact_sample(values, grid, &lookup, sample)
                    .expect("gate within the longer row resolves");
                assert_eq!(resolved.row, longer, "bin {bin} gate {gate}");
                assert_eq!(resolved.gate, gate);
            }
        }
    }

    /// Range-folded gates count as valid data: the valid extent of each
    /// velocity row (one past the last non-zero code, from Py-ART) includes
    /// them, and a range-folded gate resolves to its own row.
    #[test]
    fn compact_sample_resolution_keeps_visible_range_folded_candidates() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut = &volume.cuts[as_usize(&doppler["sweep"])];
        let grid = cut
            .moments
            .get(&MomentType::Velocity)
            .expect("velocity grid");
        assert_geometry(cut, grid, doppler);
        let values = u8_values(grid);
        let gates = grid.gate_range.gate_count;
        for (row, extent) in array(&doppler["valid_extent"]).iter().enumerate() {
            assert_eq!(row_valid_extent(grid, row), as_usize(extent), "row {row}");
        }

        let lookup = AzimuthLookup::new(cut, grid);
        for pair in array(&doppler["range_folded_first"]) {
            let (row, gate) = row_gate(pair);
            assert_eq!(values[row * gates + gate], 1);
            assert!(gate < row_valid_extent(grid, row));
            let sample = SampleLookup {
                azimuth_bin: azimuth_bin(azimuth_of(cut, grid, row)),
                gate,
            };
            let resolved = resolve_compact_sample(values, grid, &lookup, sample)
                .expect("range-folded sample should resolve");
            assert_eq!(resolved, ResolvedSample { row, gate });
        }
    }

    // ---- viewport rendering and caches ----

    #[test]
    fn viewport_render_uses_requested_screen_resolution() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();
        let storm_motion = StormMotion {
            direction_deg: 45.0,
            speed_mps: 10.0,
        };

        let reflectivity =
            render_moment_viewport_image(&volume, cut_index, MomentType::Reflectivity, options)
                .expect("viewport reflectivity");
        assert_eq!(reflectivity.dimensions(), (333, 217));
        assert!(has_visible_pixel(reflectivity.as_raw()));

        let mut reusable_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_moment_viewport_rgba_into(
            &volume,
            cut_index,
            MomentType::Reflectivity,
            options,
            &mut reusable_pixels,
        )
        .expect("viewport reflectivity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&reusable_pixels));
        assert!(has_transparent_pixel(&reusable_pixels));
        assert_eq!(reusable_pixels, *reflectivity.as_raw());

        let reflectivity_cache =
            ViewportMomentCache::new(&volume, cut_index, MomentType::Reflectivity)
                .expect("viewport reflectivity cache");
        reusable_pixels.fill(255);
        let dimensions = reflectivity_cache
            .render_moment_rgba_into(&volume, options, &mut reusable_pixels)
            .expect("cached viewport reflectivity");
        assert_eq!(dimensions, (333, 217));
        assert_eq!(reusable_pixels, *reflectivity.as_raw());

        let storm_relative = render_storm_relative_velocity_viewport_image(
            &volume,
            cut_index,
            storm_motion,
            options,
        )
        .expect("viewport storm-relative velocity");
        assert_eq!(storm_relative.dimensions(), (333, 217));
        assert!(has_visible_pixel(storm_relative.as_raw()));

        let mut storm_relative_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_storm_relative_velocity_viewport_rgba_into(
            &volume,
            cut_index,
            storm_motion,
            options,
            &mut storm_relative_pixels,
        )
        .expect("viewport storm-relative velocity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&storm_relative_pixels));
        assert!(has_transparent_pixel(&storm_relative_pixels));
        assert_eq!(storm_relative_pixels, *storm_relative.as_raw());

        let velocity_cache = ViewportMomentCache::new(&volume, cut_index, MomentType::Velocity)
            .expect("viewport velocity cache");
        storm_relative_pixels.fill(255);
        let dimensions = velocity_cache
            .render_storm_relative_velocity_rgba_into(
                &volume,
                storm_motion,
                options,
                &mut storm_relative_pixels,
            )
            .expect("cached viewport storm-relative velocity");
        assert_eq!(dimensions, (333, 217));
        assert_eq!(storm_relative_pixels, *storm_relative.as_raw());
        // The window is mostly clear air (the storms sit 225 km south): a few
        // hundred reflectivity pixels, tens of thousands of velocity pixels.
        assert!(opaque_pixels(reflectivity.as_raw()) > 500);
        assert!(opaque_pixels(storm_relative.as_raw()) > 10_000);
    }

    /// The sample cache reproduces the direct render exactly when every
    /// measured code has a visible colour (an opaque reflectivity ramp).
    /// Under the default reflectivity palette, which hides low dBZ, the two
    /// paths can differ only where the cache's first candidate radial holds a
    /// hidden code: the direct render falls through to the next candidate of
    /// the azimuth bin, the cache leaves the pixel transparent (a real-data
    /// divergence the four-radial synthetic volume could not show).
    #[test]
    fn viewport_sample_cache_matches_direct_moment_render() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();
        let opaque_ramp = ColorTable::parse(
            "opaque reflectivity",
            "units: dBZ
color: -33 0 0 60
color: 20 0 200 0
color: 95 255 0 0",
        )
        .expect("opaque ramp");
        assert_eq!(opaque_ramp.color_for_value(-32.0)[3], 255);
        assert_eq!(opaque_ramp.color_for_value(94.5)[3], 255);
        let mut tables = ColorTableSet::default();
        tables.set_family(ColorTableFamily::Reflectivity, opaque_ramp);
        let cache = ViewportMomentCache::new_with_color_tables(
            &volume,
            cut_index,
            MomentType::Reflectivity,
            &tables,
        )
        .expect("viewport reflectivity cache");
        let sample_cache = cache
            .build_sample_cache(&volume, options)
            .expect("viewport sample cache");
        let mut direct_pixels = vec![0; viewport_rgba_buffer_len(options)];
        let mut sample_cache_pixels = vec![255; viewport_rgba_buffer_len(options)];

        cache
            .render_moment_rgba_into(&volume, options, &mut direct_pixels)
            .expect("direct viewport render");
        let dimensions = cache
            .render_moment_rgba_with_sample_cache(&volume, &sample_cache, &mut sample_cache_pixels)
            .expect("sample-cache viewport render");

        assert_eq!(dimensions, (333, 217));
        assert_eq!(sample_cache.dimensions(), (333, 217));
        assert!(sample_cache.sample_count() > 0);
        assert!(sample_cache.storage_bytes() < viewport_rgba_buffer_len(options));
        assert!(opaque_pixels(&direct_pixels) > 500);
        assert_eq!(sample_cache_pixels, direct_pixels);

        let mut reused_pixels = direct_pixels.clone();
        cache
            .render_moment_rgba_with_sample_cache_reusing_transparency(
                &volume,
                &sample_cache,
                &mut reused_pixels,
            )
            .expect("sample-cache reuse viewport render");
        assert_eq!(reused_pixels, sample_cache_pixels);

        // Default palette: the same sample cache, transparent low dBZ.
        let cache = ViewportMomentCache::new(&volume, cut_index, MomentType::Reflectivity)
            .expect("default reflectivity cache");
        let sample_cache = cache
            .build_sample_cache(&volume, options)
            .expect("default sample cache");
        cache
            .render_moment_rgba_into(&volume, options, &mut direct_pixels)
            .expect("direct default render");
        cache
            .render_moment_rgba_with_sample_cache(&volume, &sample_cache, &mut sample_cache_pixels)
            .expect("sample-cache default render");
        let mut fallthrough = 0usize;
        for (index, (cached, direct)) in sample_cache_pixels
            .chunks_exact(4)
            .zip(direct_pixels.chunks_exact(4))
            .enumerate()
        {
            if cached != direct {
                assert!(
                    cached[3] == 0 && direct[3] != 0,
                    "pixel {} ({}, {}): cache {cached:?}, direct {direct:?}",
                    index,
                    index % 333,
                    index / 333
                );
                fallthrough += 1;
            }
        }
        // 148 of the 809 opaque pixels of this clear-air window today.
        assert!(
            fallthrough < opaque_pixels(&direct_pixels),
            "{fallthrough} fall-through pixels of {} opaque",
            opaque_pixels(&direct_pixels)
        );
    }

    #[test]
    fn viewport_geometry_cache_resolves_across_compatible_products() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        // The Doppler cut carries reflectivity and velocity on the same gates.
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let cut = &volume.cuts[cut_index];
        assert_eq!(
            cut.moments[&MomentType::Reflectivity].gate_range,
            cut.moments[&MomentType::Velocity].gate_range
        );
        let options = window_viewport_options();
        let reflectivity_cache =
            ViewportMomentCache::new(&volume, cut_index, MomentType::Reflectivity)
                .expect("reflectivity cache");
        let velocity_cache = ViewportMomentCache::new(&volume, cut_index, MomentType::Velocity)
            .expect("velocity cache");
        let geometry_cache = reflectivity_cache
            .build_geometry_cache(&volume, options)
            .expect("geometry cache");
        let geometry_sample_cache = velocity_cache
            .build_sample_cache_from_geometry_cache(&volume, &geometry_cache)
            .expect("velocity sample cache from geometry");
        let direct_sample_cache = velocity_cache
            .build_sample_cache(&volume, options)
            .expect("direct velocity sample cache");
        let mut geometry_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let mut direct_pixels = vec![255; viewport_rgba_buffer_len(options)];

        velocity_cache
            .render_moment_rgba_with_sample_cache(
                &volume,
                &geometry_sample_cache,
                &mut geometry_pixels,
            )
            .expect("geometry-derived sample render");
        velocity_cache
            .render_moment_rgba_with_sample_cache(&volume, &direct_sample_cache, &mut direct_pixels)
            .expect("direct sample render");

        assert_eq!(geometry_cache.dimensions(), (333, 217));
        assert!(geometry_cache.sample_count() >= geometry_sample_cache.sample_count());
        assert!(opaque_pixels(&direct_pixels) > 1_000);
        assert_eq!(geometry_pixels, direct_pixels);
    }

    #[test]
    fn viewport_sample_cache_matches_direct_storm_relative_render() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();
        let storm_motion = StormMotion {
            direction_deg: 45.0,
            speed_mps: 10.0,
        };
        let cache = ViewportMomentCache::new(&volume, cut_index, MomentType::Velocity)
            .expect("velocity cache");
        let sample_cache = cache
            .build_sample_cache(&volume, options)
            .expect("velocity sample cache");
        let mut direct_pixels = vec![0; viewport_rgba_buffer_len(options)];
        let mut sample_cache_pixels = vec![255; viewport_rgba_buffer_len(options)];

        cache
            .render_storm_relative_velocity_rgba_into(
                &volume,
                storm_motion,
                options,
                &mut direct_pixels,
            )
            .expect("direct SRV viewport render");
        let dimensions = cache
            .render_storm_relative_velocity_rgba_with_sample_cache(
                &volume,
                storm_motion,
                &sample_cache,
                &mut sample_cache_pixels,
            )
            .expect("sample-cache SRV viewport render");

        assert_eq!(dimensions, (333, 217));
        assert!(opaque_pixels(&direct_pixels) > 1_000);
        assert_eq!(sample_cache_pixels, direct_pixels);

        let next_storm_motion = StormMotion {
            direction_deg: 220.0,
            speed_mps: 18.0,
        };
        let mut cleared_next_pixels = vec![255; viewport_rgba_buffer_len(options)];
        cache
            .render_storm_relative_velocity_rgba_with_sample_cache(
                &volume,
                next_storm_motion,
                &sample_cache,
                &mut cleared_next_pixels,
            )
            .expect("cleared next SRV viewport render");
        // A different storm motion recolours the measured gates.
        assert_ne!(cleared_next_pixels, sample_cache_pixels);
        assert_eq!(
            opaque_pixels(&cleared_next_pixels),
            opaque_pixels(&sample_cache_pixels)
        );

        let mut reused_next_pixels = sample_cache_pixels;
        cache
            .render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency(
                &volume,
                next_storm_motion,
                &sample_cache,
                &mut reused_next_pixels,
            )
            .expect("reused next SRV viewport render");
        assert_eq!(reused_next_pixels, cleared_next_pixels);
    }

    #[test]
    fn viewport_sample_cache_rejects_mismatched_cache() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let options = ViewportRasterOptions {
            width: 64,
            height: 64,
            radar_x_px: 32.0,
            radar_y_px: 32.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let reflectivity_cache =
            ViewportMomentCache::new(&volume, cut_index, MomentType::Reflectivity)
                .expect("reflectivity cache");
        let velocity_cache = ViewportMomentCache::new(&volume, cut_index, MomentType::Velocity)
            .expect("velocity cache");
        let sample_cache = reflectivity_cache
            .build_sample_cache(&volume, options)
            .expect("reflectivity sample cache");
        let mut pixels = vec![0; viewport_rgba_buffer_len(options)];

        let err = velocity_cache
            .render_moment_rgba_with_sample_cache(&volume, &sample_cache, &mut pixels)
            .expect_err("sample cache should be moment-bound");

        assert!(matches!(
            err,
            RenderError::CacheMomentMismatch {
                expected: MomentType::Velocity,
                actual: MomentType::Reflectivity
            }
        ));
    }

    #[test]
    fn viewport_render_rejects_wrong_sized_reusable_buffer() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();

        let mut pixels = vec![0; viewport_rgba_buffer_len(options) - 4];
        let err = render_moment_viewport_rgba_into(
            &volume,
            cut_index,
            MomentType::Reflectivity,
            options,
            &mut pixels,
        )
        .expect_err("wrong buffer size should be rejected");

        assert!(matches!(
            err,
            RenderError::BufferSizeMismatch {
                width: 333,
                height: 217,
                ..
            }
        ));
    }

    /// A cache built on the 2024 KTLX volume refuses to draw the 2013 one.
    #[test]
    fn viewport_cache_rejects_different_volume() {
        let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
        let volume = level2(&path);
        let other_path = recast_radar_testdata::require_file!("l2-ktlx-20130520-201643-trim");
        let other_volume = level2(&other_path);
        assert_ne!(volume.volume_time, other_volume.volume_time);
        let options = ViewportRasterOptions {
            width: 64,
            height: 64,
            radar_x_px: 32.0,
            radar_y_px: 32.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let cache = ViewportMomentCache::new(&volume, 0, MomentType::Reflectivity)
            .expect("viewport reflectivity cache");
        let mut pixels = vec![0; viewport_rgba_buffer_len(options)];

        let err = cache
            .render_moment_rgba_into(&other_volume, options, &mut pixels)
            .expect_err("cache should be bound to its source volume");

        assert!(matches!(err, RenderError::CacheVolumeMismatch));
        cache
            .render_moment_rgba_into(&volume, options, &mut pixels)
            .expect("the source volume still renders");
        assert!(has_visible_pixel(&pixels));
    }

    /// Differential phase in the Build 13.2 KTLX 2013-05-20 volume is a
    /// 16-bit moment (codes to 1022, scale 2.8361, offset 2): its u16 palette
    /// colours every code by the physical value MetPy reports, and the cached
    /// viewport render equals the direct one.
    #[test]
    fn viewport_cache_renders_u16_palette_moments() {
        let expected = golden("ktlx2013.json");
        let phase = &expected["differential_phase"];
        let path = recast_radar_testdata::require_file!("l2-ktlx-20130520-201643-trim");
        let volume = level2(&path);
        let cut_index = as_usize(&phase["sweep"]);
        let cut = &volume.cuts[cut_index];
        let grid = cut
            .moments
            .get(&MomentType::DifferentialPhase)
            .expect("differential phase grid");
        assert_geometry(cut, grid, phase);
        let MomentStorage::U16(values) = &grid.storage else {
            panic!("16-bit differential phase should be stored as u16");
        };
        let gates = grid.gate_range.gate_count;
        let max_code = as_usize(&phase["max_code"]) as u16;
        assert_eq!(values.iter().copied().max(), Some(max_code));
        assert!((grid.scale - as_f32(&phase["scale"])).abs() < 1e-4);
        assert!((grid.offset - as_f32(&phase["offset"])).abs() < 1e-4);
        let finite = (0..grid.radial_count())
            .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
            .filter(|&(row, gate)| grid.scaled_value(row, gate).is_some())
            .count();
        assert_eq!(finite, as_usize(&phase["finite_gates"]));

        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::DifferentialPhase);
        let palette = build_u16_palette(grid, table);
        assert_eq!(palette.len(), usize::from(max_code) + 1);
        assert_eq!(palette[0], [0, 0, 0, 0]);
        for sample in array(&phase["samples"]) {
            let (row, gate) = (as_usize(&sample["row"]), as_usize(&sample["gate"]));
            let code = as_usize(&sample["code"]);
            let value = as_f32(&sample["value_deg"]);
            assert_eq!(usize::from(values[row * gates + gate]), code);
            let scaled = grid.scaled_value(row, gate).expect("finite phase");
            assert!((scaled - value).abs() < 1e-3, "row {row} gate {gate}");
            assert_eq!(
                palette[code],
                color_for_raw(grid, &table.sampler(), code as u16)
            );
            assert_color_close(
                palette[code],
                table.color_for_value(value),
                &format!("row {row} gate {gate} code {code}"),
            );
        }

        let options = ViewportRasterOptions {
            width: 96,
            height: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let cache = ViewportMomentCache::new(&volume, cut_index, MomentType::DifferentialPhase)
            .expect("viewport u16 differential phase cache");
        let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = cache
            .render_moment_rgba_into(&volume, options, &mut pixels)
            .expect("cached u16 viewport differential phase");
        assert_eq!(dimensions, (96, 96));
        assert!(has_visible_pixel(&pixels));
        assert!(has_transparent_pixel(&pixels));

        let mut direct = vec![0; viewport_rgba_buffer_len(options)];
        render_moment_viewport_rgba_into(
            &volume,
            cut_index,
            MomentType::DifferentialPhase,
            options,
            &mut direct,
        )
        .expect("direct u16 viewport differential phase");
        assert_eq!(direct, pixels);
    }

    fn viewport_lookup_reference(
        x: u32,
        y: u32,
        grid: &MomentGrid,
        row_lookup: &AzimuthLookup,
        geometry: ViewportGeometry,
    ) -> Option<SampleLookup> {
        let dx_km = (x as f32 + 0.5 - geometry.radar_x_px) * geometry.km_per_px_x;
        let dy_km = (geometry.radar_y_px - (y as f32 + 0.5)) * geometry.km_per_px_y;
        let range_m = dx_km.hypot(dy_km) * 1000.0;
        let max_range_m = geometry.max_range_km_sq.sqrt() * 1000.0;
        if range_m > max_range_m {
            return None;
        }

        let gate = ((range_m - grid.gate_range.first_gate_m as f32)
            / grid.gate_range.gate_spacing_m.max(1) as f32)
            .round() as isize;
        if gate < 0 || gate as usize >= grid.gate_range.gate_count {
            return None;
        }

        let azimuth_deg = azimuth_from_xy(dx_km, dy_km);
        let azimuth_bin = row_lookup.filled_bin_for_azimuth(azimuth_deg)?;
        Some(SampleLookup {
            azimuth_bin,
            gate: gate as usize,
        })
    }
}

/// Derived grids from `recast-radar-map` drawn through the viewport cache
/// (moved here from the volumetric tests when the algorithms left this crate).
#[cfg(test)]
mod derived_product_tests {
    use recast_radar_core::{MomentStorage, MomentType, RadarVolume};
    use recast_radar_map::{
        ECHO_TOP_THRESHOLD_DBZ, composite_reflectivity_grid, echo_top_grid, vil_grid,
    };
    use serde_json::Value;
    use std::path::Path;

    use crate::color::{ColorTableFamily, ColorTableSet};
    use crate::{ViewportMomentCache, ViewportRasterOptions, viewport_rgba_buffer_len};

    fn level2(path: &Path) -> RadarVolume {
        recast_radar_io_nexrad::decode_volume_from_path(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn as_f32(value: &Value) -> f32 {
        value
            .as_f64()
            .unwrap_or_else(|| panic!("expected a number, got {value}")) as f32
    }

    fn as_usize(value: &Value) -> usize {
        value
            .as_u64()
            .unwrap_or_else(|| panic!("expected an unsigned integer, got {value}")) as usize
    }

    fn angular_distance(a: f32, b: f32) -> f32 {
        let delta = (a - b).rem_euclid(360.0);
        delta.min(360.0 - delta)
    }

    /// End-to-end on the full KEWX 2016-04-13 volume (19 sweeps): compute
    /// each derived grid and render it through the same ViewportMomentCache
    /// path the GUI worker uses, with its dedicated color family. The
    /// composite peaks at the volume's strongest gate as Py-ART reads it
    /// (76.5 dBZ on the 4.0 deg tilt, 55.9 km out at 251.5 deg; the lowest
    /// tilt alone tops at 70.5 dBZ), and the pixel drawn on that column
    /// carries the reflectivity colour of 76.5 dBZ.
    #[test]
    fn derived_products_render_through_viewport_cache() {
        let golden_path = recast_radar_testdata::testdata_dir()
            .join("golden")
            .join("render")
            .join("kewx.json");
        let text = std::fs::read_to_string(&golden_path)
            .unwrap_or_else(|error| panic!("{}: {error}", golden_path.display()));
        let expected: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", golden_path.display()));
        let path = recast_radar_testdata::require_file!("l2-kewx-20160413-022531");
        let volume = level2(&path);
        assert_eq!(volume.cuts.len(), as_usize(&expected["sweeps"]));

        // Py-ART's first sweep: the 0.48 deg surveillance cut.
        let lowest = &expected["lowest_sweep"];
        let first_grid = volume.cuts[0]
            .moments
            .get(&MomentType::Reflectivity)
            .expect("first sweep reflectivity");
        assert_eq!(first_grid.radial_count(), as_usize(&lowest["rows"]));
        assert_eq!(first_grid.gate_range.gate_count, as_usize(&lowest["gates"]));
        assert_eq!(
            first_grid.gate_range.first_gate_m as f32,
            as_f32(&lowest["first_gate_m"])
        );
        assert_eq!(
            first_grid.gate_range.gate_spacing_m as f32,
            as_f32(&lowest["gate_spacing_m"])
        );
        let first_max = (0..first_grid.radial_count())
            .flat_map(|row| (0..first_grid.gate_range.gate_count).map(move |gate| (row, gate)))
            .filter_map(|(row, gate)| first_grid.scaled_value(row, gate))
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(first_max, as_f32(&lowest["max_dbz"]));

        // The derived grids take the geometry of the reflectivity tilt with
        // the lowest decoded elevation (a cut's elevation is its first
        // radial's, so among the four 0.5 deg passes of this SAILS volume the
        // lowest is one of the Doppler halves).
        let (base_index, base) = volume
            .cuts
            .iter()
            .enumerate()
            .filter(|(_, cut)| cut.moments.contains_key(&MomentType::Reflectivity))
            .min_by(|a, b| a.1.elevation_deg.total_cmp(&b.1.elevation_deg))
            .expect("a reflectivity tilt");
        assert!(base.elevation_deg < 1.0, "{} deg", base.elevation_deg);
        let base_grid = &base.moments[&MomentType::Reflectivity];
        let gates = base_grid.gate_range.gate_count;
        let composite = composite_reflectivity_grid(&volume).expect("composite grid");
        assert_eq!(composite.gate_range, base_grid.gate_range);
        assert_eq!(composite.radial_indices, base_grid.radial_indices);
        let MomentStorage::F32(values) = &composite.storage else {
            panic!("derived grids are f32");
        };
        let (index, peak) = values
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, value)| value.is_finite())
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("finite composite");
        let volume_max = &expected["volume_max"];
        assert!((peak - as_f32(&volume_max["dbz"])).abs() < 1e-3, "{peak}");
        assert!(peak > first_max);
        let (row, gate) = (index / gates, index % gates);
        let azimuth = base.radials[composite.radial_indices[row]].azimuth_deg;
        assert!(
            angular_distance(azimuth, as_f32(&volume_max["azimuth_deg"])) < 1.0,
            "composite peak at {azimuth} deg"
        );
        let range_m = (composite.gate_range.first_gate_m
            + gate as i32 * composite.gate_range.gate_spacing_m) as f32;
        assert!(
            (range_m - as_f32(&volume_max["ground_range_m"])).abs() < 400.0,
            "composite peak at {range_m} m"
        );

        let echo_top = echo_top_grid(&volume, ECHO_TOP_THRESHOLD_DBZ).expect("echo top grid");
        let top = echo_top
            .scaled_value(row, gate)
            .expect("echo top over the peak column");
        // The 76.5 dBZ gate itself clears the 18.3 dBZ threshold, so the top
        // is at least its beam height (Py-ART's ray elevation is up to 0.2 deg
        // from the decoded cut elevation: 250 m at this range).
        assert!(
            top >= as_f32(&volume_max["height_above_radar_m"]) - 250.0,
            "{top} m"
        );
        let vil = vil_grid(&volume).expect("VIL grid");
        assert!(vil.scaled_value(row, gate).is_some_and(|vil| vil > 0.0));

        let tables = ColorTableSet::default();
        // 65 x 65 pixels at 50 m per pixel centred on the peak column.
        let size = 65u32;
        let km_per_px = 0.05f32;
        let centre_px = (size / 2) as f32 + 0.5;
        let radians = azimuth.to_radians();
        let opts = ViewportRasterOptions {
            width: size,
            height: size,
            radar_x_px: centre_px - range_m / 1000.0 * radians.sin() / km_per_px,
            radar_y_px: centre_px + range_m / 1000.0 * radians.cos() / km_per_px,
            km_per_px_x: km_per_px,
            km_per_px_y: km_per_px,
            rotation_rad: 0.0,
        };
        let centre = ((size / 2) * size + size / 2) as usize * 4;
        let cases = [
            (composite, ColorTableFamily::Reflectivity, Some(peak)),
            (echo_top, ColorTableFamily::EchoTops, None),
            (vil, ColorTableFamily::Vil, None),
        ];
        for (grid, family, expected_value) in cases {
            let cache =
                ViewportMomentCache::new_derived(&volume, base_index, grid, family, &tables)
                    .expect("derived cache");
            let mut pixels = vec![0u8; viewport_rgba_buffer_len(opts)];
            cache
                .render_moment_rgba_into(&volume, opts, &mut pixels)
                .expect("render");
            assert!(
                pixels.chunks_exact(4).any(|p| p[3] > 0),
                "{family:?} derived product rendered no opaque pixels"
            );
            if let Some(value) = expected_value {
                assert_eq!(
                    &pixels[centre..centre + 4],
                    &tables.for_family(family).color_for_value(value),
                    "{family:?} pixel on the peak column"
                );
            }
        }
    }
}
