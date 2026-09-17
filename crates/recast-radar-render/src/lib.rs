//! 2D radar rendering contracts.
//!
//! The long-term renderer will be GPU-backed, but this crate already provides a
//! CPU raster path for smoke tests, screenshots, and early visual validation.
//!
//! Rendering reads the FM301 model ([`Volume`], [`Sweep`], [`Field`]). A field
//! is addressed by sweep index and [`FieldName`] (`DBZH`, `VRADH`, a derived
//! id), drawn straight from its packed storage through per-code palettes, and
//! placed with its native gate geometry on the sweep's range coordinate
//! ([`Field::native_geometry`]). Rows are the sweep's rays; rows the source did
//! not provide ([`Field::absent_rows`]) are never drawn.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::f32::consts::PI;
use std::ops::Range;
use std::path::Path;

pub mod color;

pub use color::{ColorSampler, ColorTable, ColorTableFamily, ColorTableSet};
use image::{ImageBuffer, ImageError, Rgba};
use rayon::prelude::*;
use recast_radar_core::model::PackedInt;
use recast_radar_core::{
    Field, FieldData, FieldName, FloatCoding, IntCoding, Quantity, RangeCoord, Sweep, Volume,
};
use thiserror::Error;

const AZIMUTH_BINS: usize = 3600;
const AZIMUTH_BIN_WIDTH_DEG: f32 = 0.1;
const MAX_AZIMUTH_HALF_WIDTH_DEG: f32 = 3.0;
const MAX_AZIMUTH_CANDIDATES: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderLayer {
    /// The field drawn: a sweep variable name or a derived field id.
    pub field: FieldName,
    pub visible: bool,
}

impl RenderLayer {
    pub fn base(field: FieldName) -> Self {
        Self {
            field,
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

/// Upper bound of a sample cache for `field` (whose gate mapping refers to
/// `range`, normally its sweep's range): only viewport rows and columns inside
/// the field's maximum range can hold samples.
pub fn viewport_sample_cache_storage_upper_bound_for_field(
    field: &Field,
    range: &RangeCoord,
    options: ViewportRasterOptions,
) -> usize {
    FieldGeometry::of(field, range).map_or_else(
        || {
            let (_, height) = viewport_dimensions(options);
            (height as usize).saturating_mul(std::mem::size_of::<CachedRowSpan>())
        },
        |gates| sample_cache_storage_upper_bound(gates, options),
    )
}

/// Sample-cache bytes for a field of geometry `gates`: one slot per viewport
/// pixel inside the field's maximum range, plus one span per row.
fn sample_cache_storage_upper_bound(gates: FieldGeometry, options: ViewportRasterOptions) -> usize {
    let (_, height) = viewport_dimensions(options);
    let geometry = viewport_geometry(gates, options);
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
    #[error("sweep index {index} is out of range for {sweep_count} sweeps")]
    SweepOutOfRange { index: usize, sweep_count: usize },
    #[error("field {field} is not available in sweep {sweep_index}")]
    MissingField {
        sweep_index: usize,
        field: FieldName,
    },
    #[error("field {field} in sweep {sweep_index} has no decoded rows")]
    EmptyField {
        sweep_index: usize,
        field: FieldName,
    },
    #[error("field {field} in sweep {sweep_index} has no gate geometry on the sweep range")]
    NoGateGeometry {
        sweep_index: usize,
        field: FieldName,
    },
    #[error("field {field} is not a radial velocity")]
    NotRadialVelocity { field: FieldName },
    #[error("RGBA buffer has {actual} bytes, expected {expected} for {width}x{height}")]
    BufferSizeMismatch {
        actual: usize,
        expected: usize,
        width: u32,
        height: u32,
    },
    #[error("viewport render cache belongs to a different radar volume")]
    CacheVolumeMismatch,
    #[error("viewport render cache is for sweep {actual}, expected sweep {expected}")]
    CacheSweepMismatch { expected: usize, actual: usize },
    #[error("viewport render cache is for {actual}, expected {expected}")]
    CacheFieldMismatch {
        expected: FieldName,
        actual: FieldName,
    },
    #[error("viewport render cache storage no longer matches the field storage")]
    CacheStorageMismatch,
    #[error("viewport geometry cache does not match this field's gate geometry")]
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

/// Render one field of one sweep to a simple radar PNG.
pub fn render_field_png(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    out_path: &Path,
    options: RasterOptions,
) -> Result<()> {
    let image = render_field_image(volume, sweep_index, field, options)?;
    image.save(out_path)?;
    Ok(())
}

pub fn render_field_image(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    options: RasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let sweep = sweep_at(volume, sweep_index)?;
    let view = drawable_field(sweep, sweep_index, field)?;

    let row_lookup = AzimuthLookup::new(sweep, view);
    let width = options.width.max(64);
    let height = options.height.max(64);
    let center_x = (width as f32 - 1.0) / 2.0;
    let center_y = (height as f32 - 1.0) / 2.0;
    let radius_px = center_x.min(center_y) * (f32::from(options.range_fraction) / 100.0);
    let max_range_m = view.geometry.max_range_m().max(1.0);

    let mut pixels = vec![0; width as usize * height as usize * 4];
    let color_tables = ColorTableSet::default();
    let validation_table = validation_color_table_for_field(&view.field.name);
    let color_table = validation_table
        .as_ref()
        .unwrap_or_else(|| color_tables.for_family(color_family_for_field(view.field)));
    let geometry = RasterGeometry {
        width,
        center_x,
        center_y,
        radius_px,
        radius_sq_px: radius_px * radius_px,
        max_range_m,
    };

    macro_rules! byte_codes {
        ($values:expr, $coding:expr) => {{
            let palette = build_byte_palette(&$coding, color_table);
            render_compact_storage(
                &mut pixels,
                $values,
                &palette,
                view,
                &row_lookup,
                geometry,
                false,
            );
        }};
    }
    macro_rules! wide_codes {
        ($values:expr, $coding:expr) => {{
            let palette = build_wide_palette($values, &$coding, color_table);
            render_compact_storage(
                &mut pixels,
                $values,
                &palette,
                view,
                &row_lookup,
                geometry,
                false,
            );
        }};
    }
    match field_values(view.field) {
        FieldValues::U8(values, coding) => byte_codes!(values, coding),
        FieldValues::I8(values, coding) => byte_codes!(values, coding),
        FieldValues::U16(values, coding) => wide_codes!(values, coding),
        FieldValues::I16(values, coding) => wide_codes!(values, coding),
        FieldValues::F32(values, coding) => render_float_storage(
            &mut pixels,
            values,
            coding,
            view,
            &row_lookup,
            color_table,
            geometry,
            false,
        ),
        FieldValues::I32(values, coding) => render_float_storage(
            &mut pixels,
            values,
            coding,
            view,
            &row_lookup,
            color_table,
            geometry,
            false,
        ),
        FieldValues::F64(values, coding) => render_float_storage(
            &mut pixels,
            values,
            coding,
            view,
            &row_lookup,
            color_table,
            geometry,
            false,
        ),
    }

    rgba_image(width, height, pixels)
}

/// Region-based dealiasing of the radial velocity field `source` of sweep
/// `sweep_index`: `VRADDH` on the same rays, gates and gate mapping, with no
/// rows the source lacks. This is the field a caller memoizes per volume and
/// hands to [`ViewportFieldCache::new_dealiased_velocity_from_field_with_color_tables`],
/// so a loop replay or product toggle does not dealias again.
///
/// The dealiaser is `recast_radar_correct::dealias_velocity`, the region-based
/// engine.
pub fn dealiased_velocity_field(
    volume: &Volume,
    sweep_index: usize,
    source: &FieldName,
) -> Result<Field> {
    let sweep = sweep_at(volume, sweep_index)?;
    let view = drawable_field(sweep, sweep_index, source)?;
    if !is_radial_velocity(view.field.quantity) {
        return Err(RenderError::NotRadialVelocity {
            field: source.clone(),
        });
    }
    Ok(recast_radar_correct::dealias_velocity(sweep, view.field))
}

pub fn render_field_viewport_image(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    options: ViewportRasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let (width, height, pixels) = render_field_viewport_rgba(volume, sweep_index, field, options)?;
    rgba_image(width, height, pixels)
}

pub fn render_field_viewport_rgba(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    options: ViewportRasterOptions,
) -> Result<(u32, u32, Vec<u8>)> {
    let (width, height) = viewport_dimensions(options);
    let mut pixels = vec![0; rgba_len(width, height)];
    render_field_viewport_rgba_into(volume, sweep_index, field, options, &mut pixels)?;
    Ok((width, height, pixels))
}

pub fn render_field_viewport_rgba_into(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
) -> Result<(u32, u32)> {
    let cache = ViewportFieldCache::new(volume, sweep_index, field)?;
    cache.render_field_rgba_into(volume, options, pixels)
}

pub struct ViewportFieldCache {
    volume_ptr: usize,
    sweep_index: usize,
    /// Name of the drawn field: a sweep field, or the owned field below.
    field: FieldName,
    /// The drawn field is a radial velocity (storm-relative rendering allowed).
    velocity: bool,
    row_lookup: AzimuthLookup,
    color_lookup: CachedColorLookup,
    storm_motion_basis: Option<StormMotionBasis>,
    /// A field drawn in place of a sweep field: dealiased, derived or
    /// display-resampled, with its gate geometry.
    owned: Option<OwnedField>,
}

struct OwnedField {
    field: Field,
    geometry: FieldGeometry,
}

pub struct ViewportSampleCache {
    volume_ptr: usize,
    sweep_index: usize,
    field: FieldName,
    width: u32,
    height: u32,
    sample_count: usize,
    row_spans: Vec<CachedRowSpan>,
    samples: Vec<CachedSample>,
}

pub struct ViewportGeometryCache {
    width: u32,
    height: u32,
    geometry: FieldGeometry,
    sample_count: usize,
    row_spans: Vec<CachedRowSpan>,
    samples: Vec<CachedSample>,
}

pub struct StormRelativePaletteCache {
    volume_ptr: usize,
    sweep_index: usize,
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

// ---------------------------------------------------------------------------
// Fields as the raster loops read them
// ---------------------------------------------------------------------------

/// A field's gate geometry: centre of native gate 0, native spacing and gate
/// count, in metres ([`Field::native_geometry`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct FieldGeometry {
    first_center_m: f64,
    spacing_m: f64,
    gate_count: usize,
}

impl FieldGeometry {
    fn of(field: &Field, range: &RangeCoord) -> Option<Self> {
        let (first_center_m, spacing_m) = field.native_geometry(range)?;
        Some(Self {
            first_center_m,
            spacing_m,
            gate_count: field.ngates as usize,
        })
    }

    /// Centre of gate 0, where gate `g` is found at `first + g * spacing`.
    #[inline]
    fn first_gate_m(self) -> f32 {
        self.first_center_m as f32
    }

    /// Spacing for range-to-gate lookups (never below 1 m).
    #[inline]
    fn lookup_spacing_m(self) -> f32 {
        (self.spacing_m as f32).max(1.0)
    }

    fn max_range_m(self) -> f32 {
        self.first_center_m as f32 + self.spacing_m as f32 * self.gate_count as f32
    }
}

/// A field with its gate geometry.
#[derive(Clone, Copy)]
struct FieldView<'a> {
    field: &'a Field,
    geometry: FieldGeometry,
}

impl FieldView<'_> {
    /// Stored gates per row (row-major index stride).
    #[inline]
    fn gate_count(self) -> usize {
        self.field.ngates as usize
    }
}

/// A field's storage borrowed with its coding.
#[derive(Clone, Copy)]
enum FieldValues<'a> {
    U8(&'a [u8], IntCoding<u8>),
    I8(&'a [i8], IntCoding<i8>),
    U16(&'a [u16], IntCoding<u16>),
    I16(&'a [i16], IntCoding<i16>),
    /// 32-bit codes have no palette; they render through the float path.
    I32(&'a [i32], IntCoding<i32>),
    F32(&'a [f32], FloatCoding<f32>),
    F64(&'a [f64], FloatCoding<f64>),
}

fn field_values(field: &Field) -> FieldValues<'_> {
    match &field.data {
        FieldData::U8 { values, coding } => FieldValues::U8(values, *coding),
        FieldData::I8 { values, coding } => FieldValues::I8(values, *coding),
        FieldData::U16 { values, coding } => FieldValues::U16(values, *coding),
        FieldData::I16 { values, coding } => FieldValues::I16(values, *coding),
        FieldData::I32 { values, coding } => FieldValues::I32(values, *coding),
        FieldData::F32 { values, coding } => FieldValues::F32(values, *coding),
        FieldData::F64 { values, coding } => FieldValues::F64(values, *coding),
    }
}

/// Integer codes the raster loops index palettes with.
trait RawCode: PackedInt + Sync + Send {
    /// Palette slot of this code (the code's bit pattern read as unsigned).
    fn palette_index(self) -> usize;
    /// The code at a palette slot.
    fn from_palette_index(index: usize) -> Self;
}

impl RawCode for u8 {
    #[inline]
    fn palette_index(self) -> usize {
        usize::from(self)
    }

    #[inline]
    fn from_palette_index(index: usize) -> Self {
        index as u8
    }
}

impl RawCode for i8 {
    #[inline]
    fn palette_index(self) -> usize {
        usize::from(self as u8)
    }

    #[inline]
    fn from_palette_index(index: usize) -> Self {
        index as u8 as i8
    }
}

impl RawCode for u16 {
    #[inline]
    fn palette_index(self) -> usize {
        usize::from(self)
    }

    #[inline]
    fn from_palette_index(index: usize) -> Self {
        index as u16
    }
}

impl RawCode for i16 {
    #[inline]
    fn palette_index(self) -> usize {
        usize::from(self as u16)
    }

    #[inline]
    fn from_palette_index(index: usize) -> Self {
        index as u16 as i16
    }
}

/// How the renderer treats a packed code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeClass {
    /// Undetect, fill, or outside `valid_range`: transparent, and a sample
    /// that falls through to the next candidate row.
    Blank,
    /// The range-folded flag: drawn in the table's range-folded color.
    RangeFolded,
    /// A physical value (`coding.transform`).
    Value,
}

/// Classify a code in the order of `IntCoding::resolve` (undetect, fill,
/// range folded, `valid_range`), without computing the physical value.
#[inline]
fn code_class<T: PackedInt>(coding: &IntCoding<T>, raw: T) -> CodeClass {
    if coding.undetect == Some(raw) || coding.fill_value == Some(raw) {
        CodeClass::Blank
    } else if coding.range_folded == Some(raw) {
        CodeClass::RangeFolded
    } else if matches!(coding.valid_range, Some([lo, hi]) if raw < lo || raw > hi) {
        CodeClass::Blank
    } else {
        CodeClass::Value
    }
}

#[inline]
fn code_value<T: PackedInt>(coding: &IntCoding<T>, raw: T) -> f32 {
    coding.transform.apply(raw.as_f64())
}

/// Storage the float path renders: a stored value's finite physical value,
/// `None` for NaN, the coding's fill and undetect values, and non-finite
/// results.
trait FloatCode: Copy + Sync + Send {
    type Coding: Copy + Sync + Send;
    fn physical(self, coding: &Self::Coding) -> Option<f32>;
}

impl FloatCode for f32 {
    type Coding = FloatCoding<f32>;

    #[inline]
    fn physical(self, coding: &FloatCoding<f32>) -> Option<f32> {
        if !self.is_finite()
            || coding
                .fill_value
                .is_some_and(|fill| fill.to_bits() == self.to_bits())
            || coding
                .undetect
                .is_some_and(|undetect| undetect.to_bits() == self.to_bits())
        {
            return None;
        }
        let value = match coding.transform {
            Some(transform) => transform.apply(f64::from(self)),
            None => self,
        };
        value.is_finite().then_some(value)
    }
}

impl FloatCode for f64 {
    type Coding = FloatCoding<f64>;

    #[inline]
    fn physical(self, coding: &FloatCoding<f64>) -> Option<f32> {
        if !self.is_finite()
            || coding
                .fill_value
                .is_some_and(|fill| fill.to_bits() == self.to_bits())
            || coding
                .undetect
                .is_some_and(|undetect| undetect.to_bits() == self.to_bits())
        {
            return None;
        }
        let value = match coding.transform {
            Some(transform) => transform.apply(self),
            None => self as f32,
        };
        value.is_finite().then_some(value)
    }
}

/// 32-bit integer codes (CfRadial `int` fields): every sentinel, including the
/// range-folded flag, is blank.
impl FloatCode for i32 {
    type Coding = IntCoding<i32>;

    #[inline]
    fn physical(self, coding: &IntCoding<i32>) -> Option<f32> {
        coding
            .resolve(self)
            .value()
            .filter(|value| value.is_finite())
    }
}

/// `true` for the radial-velocity quantities storm-relative rendering accepts.
fn is_radial_velocity(quantity: Quantity) -> bool {
    matches!(
        quantity,
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity
    )
}

/// `true` when the field has at least one row the source provided.
fn has_rows(field: &Field) -> bool {
    field.nrays as usize > field.absent_rows.len()
}

fn sweep_at(volume: &Volume, sweep_index: usize) -> Result<&Sweep> {
    volume
        .sweeps
        .get(sweep_index)
        .ok_or(RenderError::SweepOutOfRange {
            index: sweep_index,
            sweep_count: volume.sweeps.len(),
        })
}

fn field_in<'a>(sweep: &'a Sweep, sweep_index: usize, name: &FieldName) -> Result<&'a Field> {
    sweep.field(name).ok_or_else(|| RenderError::MissingField {
        sweep_index,
        field: name.clone(),
    })
}

fn view_on<'a>(field: &'a Field, range: &RangeCoord, sweep_index: usize) -> Result<FieldView<'a>> {
    let geometry = FieldGeometry::of(field, range).ok_or_else(|| RenderError::NoGateGeometry {
        sweep_index,
        field: field.name.clone(),
    })?;
    Ok(FieldView { field, geometry })
}

/// A sweep field with rows to draw, placed on the sweep's range.
fn drawable_field<'a>(
    sweep: &'a Sweep,
    sweep_index: usize,
    name: &FieldName,
) -> Result<FieldView<'a>> {
    let field = field_in(sweep, sweep_index, name)?;
    if !has_rows(field) {
        return Err(RenderError::EmptyField {
            sweep_index,
            field: name.clone(),
        });
    }
    view_on(field, &sweep.range, sweep_index)
}

struct StormMotionBasis {
    beam_cos: Vec<f32>,
    beam_sin: Vec<f32>,
}

impl StormMotionBasis {
    fn new(sweep: &Sweep, field: &Field) -> Self {
        let rows = field.nrays as usize;
        let mut beam_cos = Vec::with_capacity(rows);
        let mut beam_sin = Vec::with_capacity(rows);
        for row in 0..rows {
            let azimuth_rad = sweep
                .rays
                .azimuth_deg
                .get(row)
                .map(|azimuth| azimuth.to_radians())
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
    /// `u8` / `i8` storage: one color per byte code.
    Byte {
        palette: Box<[[u8; 4]; 256]>,
        color_table: ColorTable,
        dtype: &'static str,
    },
    /// `u16` / `i16` storage: one color per code up to the largest code
    /// present.
    Wide {
        palette: Vec<[u8; 4]>,
        color_table: ColorTable,
        dtype: &'static str,
    },
    /// Float storage: colors are sampled per value.
    Float {
        color_table: ColorTable,
        dtype: &'static str,
    },
}

impl CachedColorLookup {
    fn new(field: &Field, color_tables: &ColorTableSet) -> Self {
        Self::new_for_family(field, color_tables, color_family_for_field(field))
    }

    fn new_for_family(
        field: &Field,
        color_tables: &ColorTableSet,
        family: ColorTableFamily,
    ) -> Self {
        let color_table = validation_color_table_for_field(&field.name)
            .unwrap_or_else(|| color_tables.for_family(family).clone());
        let dtype = field.data.dtype();
        match field_values(field) {
            FieldValues::U8(_, coding) => Self::Byte {
                palette: Box::new(build_byte_palette(&coding, &color_table)),
                color_table,
                dtype,
            },
            FieldValues::I8(_, coding) => Self::Byte {
                palette: Box::new(build_byte_palette(&coding, &color_table)),
                color_table,
                dtype,
            },
            FieldValues::U16(values, coding) => Self::Wide {
                palette: build_wide_palette(values, &coding, &color_table),
                color_table,
                dtype,
            },
            FieldValues::I16(values, coding) => Self::Wide {
                palette: build_wide_palette(values, &coding, &color_table),
                color_table,
                dtype,
            },
            FieldValues::I32(..) | FieldValues::F32(..) | FieldValues::F64(..) => {
                Self::Float { color_table, dtype }
            }
        }
    }

    fn color_table(&self) -> &ColorTable {
        match self {
            Self::Byte { color_table, .. }
            | Self::Wide { color_table, .. }
            | Self::Float { color_table, .. } => color_table,
        }
    }

    fn dtype(&self) -> &'static str {
        match self {
            Self::Byte { dtype, .. } | Self::Wide { dtype, .. } | Self::Float { dtype, .. } => {
                dtype
            }
        }
    }

    /// The code palette (empty for float storage).
    fn palette(&self) -> &[[u8; 4]] {
        match self {
            Self::Byte { palette, .. } => palette.as_ref(),
            Self::Wide { palette, .. } => palette,
            Self::Float { .. } => &[],
        }
    }
}

impl ViewportFieldCache {
    pub fn new(volume: &Volume, sweep_index: usize, field: &FieldName) -> Result<Self> {
        Self::new_with_color_tables(volume, sweep_index, field, &ColorTableSet::default())
    }

    pub fn new_with_color_tables(
        volume: &Volume,
        sweep_index: usize,
        field: &FieldName,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        Self::new_with_color_tables_for_family(volume, sweep_index, field, color_tables, None)
    }

    pub fn new_with_color_tables_for_family(
        volume: &Volume,
        sweep_index: usize,
        field: &FieldName,
        color_tables: &ColorTableSet,
        family: Option<ColorTableFamily>,
    ) -> Result<Self> {
        let sweep = sweep_at(volume, sweep_index)?;
        let view = drawable_field(sweep, sweep_index, field)?;
        let velocity = is_radial_velocity(view.field.quantity);

        Ok(Self {
            volume_ptr: volume as *const Volume as usize,
            sweep_index,
            field: view.field.name.clone(),
            velocity,
            storm_motion_basis: velocity.then(|| StormMotionBasis::new(sweep, view.field)),
            row_lookup: AzimuthLookup::new(sweep, view),
            color_lookup: CachedColorLookup::new_for_family(
                view.field,
                color_tables,
                family.unwrap_or_else(|| color_family_for_field(view.field)),
            ),
            owned: None,
        })
    }

    /// Dealias the radial velocity field `source` of the sweep and draw the
    /// result (`VRADDH`).
    pub fn new_dealiased_velocity(
        volume: &Volume,
        sweep_index: usize,
        source: &FieldName,
    ) -> Result<Self> {
        Self::new_dealiased_velocity_with_color_tables(
            volume,
            sweep_index,
            source,
            &ColorTableSet::default(),
        )
    }

    pub fn new_dealiased_velocity_with_color_tables(
        volume: &Volume,
        sweep_index: usize,
        source: &FieldName,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let dealiased = dealiased_velocity_field(volume, sweep_index, source)?;
        Self::new_dealiased_velocity_from_field_with_color_tables(
            volume,
            sweep_index,
            dealiased,
            color_tables,
        )
    }

    /// Like [`Self::new_dealiased_velocity_with_color_tables`] but reuses a
    /// velocity field that was ALREADY dealiased (e.g. served from a per-volume
    /// memo) instead of running the region dealiaser again. Identical result;
    /// it just skips the ~100 ms dealias so loop replay / product toggles do
    /// not recompute it per frame. `dealiased` must lie on the sweep's rays and
    /// range.
    pub fn new_dealiased_velocity_from_field_with_color_tables(
        volume: &Volume,
        sweep_index: usize,
        dealiased: Field,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let sweep = sweep_at(volume, sweep_index)?;
        if !has_rows(&dealiased) {
            return Err(RenderError::EmptyField {
                sweep_index,
                field: dealiased.name,
            });
        }
        let geometry = view_on(&dealiased, &sweep.range, sweep_index)?.geometry;
        let view = FieldView {
            field: &dealiased,
            geometry,
        };
        Ok(Self {
            volume_ptr: volume as *const Volume as usize,
            sweep_index,
            field: dealiased.name.clone(),
            velocity: true,
            row_lookup: AzimuthLookup::new(sweep, view),
            color_lookup: CachedColorLookup::new(&dealiased, color_tables),
            storm_motion_basis: Some(StormMotionBasis::new(sweep, &dealiased)),
            owned: Some(OwnedField {
                field: dealiased,
                geometry,
            }),
        })
    }

    /// Build a cache around a pre-computed derived field (composite
    /// reflectivity, echo tops, VIL, …) drawn on `sweep_index`'s rays. Its gate
    /// mapping refers to `range`: the sweep's own range, or the range a
    /// volume product was computed on (a composite over 250 m Doppler sweeps
    /// drawn on a 1 km surveillance sweep). The field is drawn in place of the
    /// sweep's fields via the same mechanism as the dealiased path.
    pub fn new_derived(
        volume: &Volume,
        sweep_index: usize,
        field: Field,
        range: &RangeCoord,
        family: ColorTableFamily,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        let sweep = sweep_at(volume, sweep_index)?;
        if !has_rows(&field) {
            return Err(RenderError::EmptyField {
                sweep_index,
                field: field.name,
            });
        }
        let geometry = view_on(&field, range, sweep_index)?.geometry;
        let view = FieldView {
            field: &field,
            geometry,
        };
        Ok(Self {
            volume_ptr: volume as *const Volume as usize,
            sweep_index,
            field: field.name.clone(),
            velocity: is_radial_velocity(field.quantity),
            row_lookup: AzimuthLookup::new(sweep, view),
            color_lookup: CachedColorLookup::new_for_family(&field, color_tables, family),
            storm_motion_basis: None,
            owned: Some(OwnedField { field, geometry }),
        })
    }

    /// Build a cache around a display field whose ROWS are synthetic — the
    /// interpolated (bilinear-upsampled) field from display interpolation.
    /// Unlike `new_derived`, the azimuth lookup comes from the field's own
    /// per-row azimuths instead of the sweep's rays (the field has more rows
    /// than the sweep), and its gate mapping refers to its own `range`.
    /// Renders through the same fast path.
    pub fn new_resampled(
        volume: &Volume,
        sweep_index: usize,
        field: Field,
        range: &RangeCoord,
        row_azimuths_deg: &[f32],
        family: ColorTableFamily,
        color_tables: &ColorTableSet,
    ) -> Result<Self> {
        if sweep_index >= volume.sweeps.len() {
            return Err(RenderError::SweepOutOfRange {
                index: sweep_index,
                sweep_count: volume.sweeps.len(),
            });
        }
        if !has_rows(&field) || row_azimuths_deg.len() != field.nrays as usize {
            return Err(RenderError::EmptyField {
                sweep_index,
                field: field.name,
            });
        }
        let geometry = view_on(&field, range, sweep_index)?.geometry;
        let view = FieldView {
            field: &field,
            geometry,
        };
        Ok(Self {
            volume_ptr: volume as *const Volume as usize,
            sweep_index,
            field: field.name.clone(),
            velocity: is_radial_velocity(field.quantity),
            row_lookup: AzimuthLookup::from_row_azimuths(row_azimuths_deg, view),
            color_lookup: CachedColorLookup::new_for_family(&field, color_tables, family),
            storm_motion_basis: None,
            owned: Some(OwnedField { field, geometry }),
        })
    }

    pub fn sweep_index(&self) -> usize {
        self.sweep_index
    }

    /// Name of the drawn field.
    pub fn field_name(&self) -> &FieldName {
        &self.field
    }

    pub fn render_field_rgba_into(
        &self,
        volume: &Volume,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        let (_, view) = self.sweep_and_field(volume)?;
        let (width, height) = viewport_dimensions(options);
        ensure_rgba_buffer(pixels, width, height)?;
        render_field_viewport_into(
            view,
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
        volume: &Volume,
        options: ViewportRasterOptions,
    ) -> Result<ViewportSampleCache> {
        let (_, view) = self.sweep_and_field(volume)?;
        let (width, height) = viewport_dimensions(options);
        let geometry = viewport_geometry(view.geometry, options);
        let lookup_table = ViewportLookupTable::new(view.geometry, geometry);
        let gate_count = view.gate_count();
        let row_lookup = &self.row_lookup;

        macro_rules! int_rows {
            ($values:expr, $coding:expr) => {
                build_sample_cache_rows(height, &lookup_table, row_lookup, |sample| {
                    resolve_int_sample($values, &$coding, gate_count, row_lookup, sample)
                })
            };
        }
        macro_rules! float_rows {
            ($values:expr, $coding:expr) => {
                build_sample_cache_rows(height, &lookup_table, row_lookup, |sample| {
                    resolve_float_sample($values, &$coding, gate_count, row_lookup, sample)
                })
            };
        }
        let row_builds = match field_values(view.field) {
            FieldValues::U8(values, coding) => int_rows!(values, coding),
            FieldValues::I8(values, coding) => int_rows!(values, coding),
            FieldValues::U16(values, coding) => int_rows!(values, coding),
            FieldValues::I16(values, coding) => int_rows!(values, coding),
            FieldValues::F32(values, coding) => float_rows!(values, coding),
            FieldValues::I32(values, coding) => float_rows!(values, coding),
            FieldValues::F64(values, coding) => float_rows!(values, coding),
        };

        Ok(viewport_sample_cache_from_rows(
            self.volume_ptr,
            self.sweep_index,
            self.field.clone(),
            width,
            height,
            row_builds,
        ))
    }

    pub fn build_geometry_cache(
        &self,
        volume: &Volume,
        options: ViewportRasterOptions,
    ) -> Result<ViewportGeometryCache> {
        let (_, view) = self.sweep_and_field(volume)?;
        let (width, height) = viewport_dimensions(options);
        let geometry = viewport_geometry(view.geometry, options);
        let lookup_table = ViewportLookupTable::new(view.geometry, geometry);
        let row_builds = build_geometry_cache_rows(height, &lookup_table, &self.row_lookup);
        let (sample_count, row_spans, samples) = flatten_cached_rows(height, row_builds);

        Ok(ViewportGeometryCache {
            width,
            height,
            geometry: view.geometry,
            sample_count,
            row_spans,
            samples,
        })
    }

    pub fn build_sample_cache_from_geometry_cache(
        &self,
        volume: &Volume,
        geometry_cache: &ViewportGeometryCache,
    ) -> Result<ViewportSampleCache> {
        let (_, view) = self.sweep_and_field(volume)?;
        if view.geometry != geometry_cache.geometry {
            return Err(RenderError::GeometryCacheMismatch);
        }
        let geometry = geometry_cache.geometry();
        let gate_count = view.gate_count();
        let row_lookup = &self.row_lookup;
        let height = geometry_cache.height;

        macro_rules! int_rows {
            ($values:expr, $coding:expr) => {
                build_sample_cache_rows_from_geometry(height, geometry, |sample| {
                    resolve_int_sample($values, &$coding, gate_count, row_lookup, sample)
                })
            };
        }
        macro_rules! float_rows {
            ($values:expr, $coding:expr) => {
                build_sample_cache_rows_from_geometry(height, geometry, |sample| {
                    resolve_float_sample($values, &$coding, gate_count, row_lookup, sample)
                })
            };
        }
        let row_builds = match field_values(view.field) {
            FieldValues::U8(values, coding) => int_rows!(values, coding),
            FieldValues::I8(values, coding) => int_rows!(values, coding),
            FieldValues::U16(values, coding) => int_rows!(values, coding),
            FieldValues::I16(values, coding) => int_rows!(values, coding),
            FieldValues::F32(values, coding) => float_rows!(values, coding),
            FieldValues::I32(values, coding) => float_rows!(values, coding),
            FieldValues::F64(values, coding) => float_rows!(values, coding),
        };

        Ok(viewport_sample_cache_from_rows(
            self.volume_ptr,
            self.sweep_index,
            self.field.clone(),
            geometry_cache.width,
            geometry_cache.height,
            row_builds,
        ))
    }

    pub fn sample_cache_storage_upper_bound(
        &self,
        volume: &Volume,
        options: ViewportRasterOptions,
    ) -> Result<usize> {
        let (_, view) = self.sweep_and_field(volume)?;
        Ok(sample_cache_storage_upper_bound(view.geometry, options))
    }

    pub fn render_field_rgba_with_sample_cache(
        &self,
        volume: &Volume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_field_rgba_with_sample_cache_impl(volume, sample_cache, pixels, true)
    }

    /// Renders over an existing RGBA buffer without clearing transparent pixels first.
    ///
    /// Callers must only use this when `pixels` was last rendered with the same
    /// volume, sweep, field, and viewport sample footprint. The app worker tracks
    /// that provenance before taking this path.
    pub fn render_field_rgba_with_sample_cache_reusing_transparency(
        &self,
        volume: &Volume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.render_field_rgba_with_sample_cache_impl(volume, sample_cache, pixels, false)
    }

    fn render_field_rgba_with_sample_cache_impl(
        &self,
        volume: &Volume,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
        clear_pixels: bool,
    ) -> Result<(u32, u32)> {
        let (_, view) = self.sweep_and_field(volume)?;
        self.ensure_sample_cache(sample_cache)?;
        ensure_rgba_buffer(pixels, sample_cache.width, sample_cache.height)?;
        render_field_sample_cache_into(
            view,
            &self.color_lookup,
            sample_cache,
            pixels,
            clear_pixels,
        )?;
        Ok(sample_cache.dimensions())
    }

    pub fn render_storm_relative_velocity_rgba_into(
        &self,
        volume: &Volume,
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
        volume: &Volume,
        storm_motion: StormMotion,
    ) -> Result<Option<StormRelativePaletteCache>> {
        self.ensure_velocity()?;

        let (sweep, view) = self.sweep_and_field(volume)?;
        let row_motion = || {
            self.storm_motion_basis
                .as_ref()
                .map(|basis| basis.row_motion_components(storm_motion))
                .unwrap_or_else(|| row_motion_components(sweep, view.field, storm_motion))
        };
        let row_palettes = match field_values(view.field) {
            FieldValues::U8(_, coding) => build_storm_relative_row_palettes(
                &coding,
                &row_motion(),
                self.color_lookup.color_table(),
            ),
            FieldValues::I8(_, coding) => build_storm_relative_row_palettes(
                &coding,
                &row_motion(),
                self.color_lookup.color_table(),
            ),
            _ => return Ok(None),
        };
        Ok(Some(StormRelativePaletteCache {
            volume_ptr: self.volume_ptr,
            sweep_index: self.sweep_index,
            row_palettes,
        }))
    }

    pub fn render_storm_relative_velocity_rgba_into_with_palette_cache(
        &self,
        volume: &Volume,
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
        volume: &Volume,
        storm_motion: StormMotion,
        palette_cache: Option<&StormRelativePaletteCache>,
        options: ViewportRasterOptions,
        pixels: &mut [u8],
    ) -> Result<(u32, u32)> {
        self.ensure_velocity()?;

        let (sweep, view) = self.sweep_and_field(volume)?;
        let (width, height) = viewport_dimensions(options);
        ensure_rgba_buffer(pixels, width, height)?;
        render_storm_relative_velocity_viewport_into(
            sweep,
            view,
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
        volume: &Volume,
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
        volume: &Volume,
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
        volume: &Volume,
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
        volume: &Volume,
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
        volume: &Volume,
        storm_motion: StormMotion,
        palette_cache: Option<&StormRelativePaletteCache>,
        sample_cache: &ViewportSampleCache,
        pixels: &mut [u8],
        clear_pixels: bool,
    ) -> Result<(u32, u32)> {
        self.ensure_velocity()?;

        let (sweep, view) = self.sweep_and_field(volume)?;
        self.ensure_sample_cache(sample_cache)?;
        ensure_rgba_buffer(pixels, sample_cache.width, sample_cache.height)?;
        render_storm_relative_velocity_sample_cache_into(
            sweep,
            view,
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

    fn ensure_velocity(&self) -> Result<()> {
        if self.velocity {
            Ok(())
        } else {
            Err(RenderError::NotRadialVelocity {
                field: self.field.clone(),
            })
        }
    }

    fn ensure_sample_cache(&self, sample_cache: &ViewportSampleCache) -> Result<()> {
        if self.volume_ptr != sample_cache.volume_ptr {
            return Err(RenderError::CacheVolumeMismatch);
        }
        if self.sweep_index != sample_cache.sweep_index {
            return Err(RenderError::CacheSweepMismatch {
                expected: self.sweep_index,
                actual: sample_cache.sweep_index,
            });
        }
        if self.field != sample_cache.field {
            return Err(RenderError::CacheFieldMismatch {
                expected: self.field.clone(),
                actual: sample_cache.field.clone(),
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
        if self.sweep_index != palette_cache.sweep_index {
            return Err(RenderError::CacheSweepMismatch {
                expected: self.sweep_index,
                actual: palette_cache.sweep_index,
            });
        }
        Ok(())
    }

    fn sweep_and_field<'a>(&'a self, volume: &'a Volume) -> Result<(&'a Sweep, FieldView<'a>)> {
        if self.volume_ptr != volume as *const Volume as usize {
            return Err(RenderError::CacheVolumeMismatch);
        }

        let sweep = sweep_at(volume, self.sweep_index)?;
        if let Some(owned) = &self.owned {
            return Ok((
                sweep,
                FieldView {
                    field: &owned.field,
                    geometry: owned.geometry,
                },
            ));
        }
        let field = field_in(sweep, self.sweep_index, &self.field)?;
        Ok((sweep, view_on(field, &sweep.range, self.sweep_index)?))
    }
}

fn render_field_viewport_into(
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    color_lookup: &CachedColorLookup,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
    clear_pixels: bool,
) -> Result<()> {
    if color_lookup.dtype() != view.field.data.dtype() {
        return Err(RenderError::CacheStorageMismatch);
    }
    let geometry = viewport_geometry(view.geometry, options);
    let lookup_table = ViewportLookupTable::new(view.geometry, geometry);
    let palette = color_lookup.palette();

    macro_rules! codes {
        ($values:expr) => {
            render_compact_viewport_storage(
                pixels,
                $values,
                palette,
                view,
                row_lookup,
                &lookup_table,
                clear_pixels,
            )
        };
    }
    macro_rules! floats {
        ($values:expr, $coding:expr) => {
            render_float_viewport_storage(
                pixels,
                $values,
                $coding,
                view,
                row_lookup,
                color_lookup.color_table(),
                &lookup_table,
                clear_pixels,
            )
        };
    }
    match field_values(view.field) {
        FieldValues::U8(values, _) => codes!(values),
        FieldValues::I8(values, _) => codes!(values),
        FieldValues::U16(values, _) => codes!(values),
        FieldValues::I16(values, _) => codes!(values),
        FieldValues::F32(values, coding) => floats!(values, coding),
        FieldValues::I32(values, coding) => floats!(values, coding),
        FieldValues::F64(values, coding) => floats!(values, coding),
    }
    Ok(())
}

fn render_field_sample_cache_into(
    view: FieldView<'_>,
    color_lookup: &CachedColorLookup,
    sample_cache: &ViewportSampleCache,
    pixels: &mut [u8],
    clear_pixels: bool,
) -> Result<()> {
    if color_lookup.dtype() != view.field.data.dtype() {
        return Err(RenderError::CacheStorageMismatch);
    }
    let palette = color_lookup.palette();

    macro_rules! codes {
        ($values:expr) => {
            render_compact_sample_cache_storage(
                pixels,
                $values,
                palette,
                view,
                sample_cache,
                clear_pixels,
            )
        };
    }
    macro_rules! floats {
        ($values:expr, $coding:expr) => {
            render_float_sample_cache_storage(
                pixels,
                $values,
                $coding,
                view,
                color_lookup.color_table(),
                sample_cache,
                clear_pixels,
            )
        };
    }
    match field_values(view.field) {
        FieldValues::U8(values, _) => codes!(values),
        FieldValues::I8(values, _) => codes!(values),
        FieldValues::U16(values, _) => codes!(values),
        FieldValues::I16(values, _) => codes!(values),
        FieldValues::F32(values, coding) => floats!(values, coding),
        FieldValues::I32(values, coding) => floats!(values, coding),
        FieldValues::F64(values, coding) => floats!(values, coding),
    }
    Ok(())
}

/// Storm-relative velocity of radial velocity field `field` of one sweep,
/// rendered to a simple radar raster.
pub fn render_storm_relative_velocity_image(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    storm_motion: StormMotion,
    options: RasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let sweep = sweep_at(volume, sweep_index)?;
    let view = drawable_field(sweep, sweep_index, field)?;
    if !is_radial_velocity(view.field.quantity) {
        return Err(RenderError::NotRadialVelocity {
            field: field.clone(),
        });
    }

    let row_lookup = AzimuthLookup::new(sweep, view);
    let row_motion = row_motion_components(sweep, view.field, storm_motion);
    let width = options.width.max(64);
    let height = options.height.max(64);
    let center_x = (width as f32 - 1.0) / 2.0;
    let center_y = (height as f32 - 1.0) / 2.0;
    let radius_px = center_x.min(center_y) * (f32::from(options.range_fraction) / 100.0);
    let max_range_m = view.geometry.max_range_m().max(1.0);

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
    let value_lookup = StormRelativeValueLookup {
        row_motion: &row_motion,
        color_table,
    };

    macro_rules! byte_codes {
        ($values:expr, $coding:expr) => {{
            let row_palettes =
                build_storm_relative_row_palettes(&$coding, &row_motion, color_table);
            render_storm_relative_byte_storage(
                &mut pixels,
                $values,
                view,
                &row_lookup,
                &row_palettes,
                geometry,
                false,
            );
        }};
    }
    macro_rules! wide_codes {
        ($values:expr, $coding:expr) => {
            render_storm_relative_storage(
                &mut pixels,
                $values,
                $coding,
                view,
                &row_lookup,
                value_lookup,
                geometry,
                false,
            )
        };
    }
    macro_rules! floats {
        ($values:expr, $coding:expr) => {
            render_storm_relative_float_storage(
                &mut pixels,
                $values,
                $coding,
                view,
                &row_lookup,
                value_lookup,
                geometry,
                false,
            )
        };
    }
    match field_values(view.field) {
        FieldValues::U8(values, coding) => byte_codes!(values, coding),
        FieldValues::I8(values, coding) => byte_codes!(values, coding),
        FieldValues::U16(values, coding) => wide_codes!(values, coding),
        FieldValues::I16(values, coding) => wide_codes!(values, coding),
        FieldValues::F32(values, coding) => floats!(values, coding),
        FieldValues::I32(values, coding) => floats!(values, coding),
        FieldValues::F64(values, coding) => floats!(values, coding),
    }

    rgba_image(width, height, pixels)
}

pub fn render_storm_relative_velocity_viewport_image(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let (width, height, pixels) = render_storm_relative_velocity_viewport_rgba(
        volume,
        sweep_index,
        field,
        storm_motion,
        options,
    )?;
    rgba_image(width, height, pixels)
}

pub fn render_storm_relative_velocity_viewport_rgba(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
) -> Result<(u32, u32, Vec<u8>)> {
    let (width, height) = viewport_dimensions(options);
    let mut pixels = vec![0; rgba_len(width, height)];
    render_storm_relative_velocity_viewport_rgba_into(
        volume,
        sweep_index,
        field,
        storm_motion,
        options,
        &mut pixels,
    )?;
    Ok((width, height, pixels))
}

pub fn render_storm_relative_velocity_viewport_rgba_into(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
) -> Result<(u32, u32)> {
    let cache = ViewportFieldCache::new(volume, sweep_index, field)?;
    cache.render_storm_relative_velocity_rgba_into(volume, storm_motion, options, pixels)
}

impl StormRelativeRenderCache<'_> {
    fn row_motion(&self, sweep: &Sweep, field: &Field, storm_motion: StormMotion) -> Vec<f32> {
        self.storm_motion_basis
            .map(|basis| basis.row_motion_components(storm_motion))
            .unwrap_or_else(|| row_motion_components(sweep, field, storm_motion))
    }
}

fn render_storm_relative_velocity_viewport_into(
    sweep: &Sweep,
    view: FieldView<'_>,
    render_cache: StormRelativeRenderCache<'_>,
    storm_motion: StormMotion,
    options: ViewportRasterOptions,
    pixels: &mut [u8],
    clear_pixels: bool,
) {
    let geometry = viewport_geometry(view.geometry, options);
    let lookup_table = ViewportLookupTable::new(view.geometry, geometry);

    macro_rules! byte_codes {
        ($values:expr, $coding:expr) => {{
            let built_palettes;
            let row_palettes = if let Some(palette_cache) = render_cache.palette_cache {
                &palette_cache.row_palettes
            } else {
                let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
                built_palettes = build_storm_relative_row_palettes(
                    &$coding,
                    &row_motion,
                    render_cache.color_table,
                );
                &built_palettes
            };
            render_storm_relative_byte_viewport_storage(
                pixels,
                $values,
                view,
                render_cache.row_lookup,
                row_palettes,
                &lookup_table,
                clear_pixels,
            );
        }};
    }
    macro_rules! wide_codes {
        ($values:expr, $coding:expr) => {{
            let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
            render_storm_relative_viewport_storage(
                pixels,
                $values,
                $coding,
                view,
                render_cache.row_lookup,
                StormRelativeValueLookup {
                    row_motion: &row_motion,
                    color_table: render_cache.color_table,
                },
                &lookup_table,
                clear_pixels,
            );
        }};
    }
    macro_rules! floats {
        ($values:expr, $coding:expr) => {{
            let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
            render_storm_relative_float_viewport_storage(
                pixels,
                $values,
                $coding,
                view,
                render_cache.row_lookup,
                StormRelativeValueLookup {
                    row_motion: &row_motion,
                    color_table: render_cache.color_table,
                },
                &lookup_table,
                clear_pixels,
            );
        }};
    }
    match field_values(view.field) {
        FieldValues::U8(values, coding) => byte_codes!(values, coding),
        FieldValues::I8(values, coding) => byte_codes!(values, coding),
        FieldValues::U16(values, coding) => wide_codes!(values, coding),
        FieldValues::I16(values, coding) => wide_codes!(values, coding),
        FieldValues::F32(values, coding) => floats!(values, coding),
        FieldValues::I32(values, coding) => floats!(values, coding),
        FieldValues::F64(values, coding) => floats!(values, coding),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_velocity_sample_cache_into(
    sweep: &Sweep,
    view: FieldView<'_>,
    render_cache: StormRelativeRenderCache<'_>,
    storm_motion: StormMotion,
    sample_cache: &ViewportSampleCache,
    pixels: &mut [u8],
    clear_pixels: bool,
) {
    macro_rules! byte_codes {
        ($values:expr, $coding:expr) => {{
            let built_palettes;
            let row_palettes = if let Some(palette_cache) = render_cache.palette_cache {
                &palette_cache.row_palettes
            } else {
                let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
                built_palettes = build_storm_relative_row_palettes(
                    &$coding,
                    &row_motion,
                    render_cache.color_table,
                );
                &built_palettes
            };
            render_storm_relative_byte_sample_cache_storage(
                pixels,
                $values,
                view,
                row_palettes,
                sample_cache,
                clear_pixels,
            );
        }};
    }
    macro_rules! wide_codes {
        ($values:expr, $coding:expr) => {{
            let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
            render_storm_relative_sample_cache_storage(
                pixels,
                $values,
                $coding,
                view,
                &row_motion,
                render_cache.color_table,
                sample_cache,
                clear_pixels,
            );
        }};
    }
    macro_rules! floats {
        ($values:expr, $coding:expr) => {{
            let row_motion = render_cache.row_motion(sweep, view.field, storm_motion);
            render_storm_relative_float_sample_cache_storage(
                pixels,
                $values,
                $coding,
                view,
                &row_motion,
                render_cache.color_table,
                sample_cache,
                clear_pixels,
            );
        }};
    }
    match field_values(view.field) {
        FieldValues::U8(values, coding) => byte_codes!(values, coding),
        FieldValues::I8(values, coding) => byte_codes!(values, coding),
        FieldValues::U16(values, coding) => wide_codes!(values, coding),
        FieldValues::I16(values, coding) => wide_codes!(values, coding),
        FieldValues::F32(values, coding) => floats!(values, coding),
        FieldValues::I32(values, coding) => floats!(values, coding),
        FieldValues::F64(values, coding) => floats!(values, coding),
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

fn viewport_geometry(gates: FieldGeometry, options: ViewportRasterOptions) -> ViewportGeometry {
    let (width, _) = viewport_dimensions(options);
    let max_range_km = gates.max_range_m().max(1.0) / 1000.0;
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
        gates: FieldGeometry,
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
        gates: FieldGeometry,
        row_lookup: &AzimuthLookup,
    ) -> Option<SampleLookup> {
        raster_lookup(x, y, gates, row_lookup, self)
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
        gates: FieldGeometry,
        row_lookup: &AzimuthLookup,
    ) -> Option<SampleLookup> {
        viewport_lookup(x, y, gates, row_lookup, self)
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
    fn new(gates: FieldGeometry, geometry: ViewportGeometry) -> Self {
        Self {
            geometry,
            first_gate_m: gates.first_gate_m(),
            gate_spacing_m: gates.lookup_spacing_m(),
            gate_count: gates.gate_count,
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

fn render_compact_storage<T: RawCode, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
    let gates = view.geometry;
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
                let Some(sample) = geometry.lookup(x, y, gates, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let color = palette[raw.palette_index()];
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

fn render_compact_viewport_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                    let color = palette[raw.palette_index()];
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

fn render_compact_sample_cache_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    palette: &[[u8; 4]],
    view: FieldView<'_>,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                let color = palette[values[index].palette_index()];
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn render_float_storage<T: FloatCode, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    color_table: &ColorTable,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
    let gates = view.geometry;
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
                let Some(sample) = geometry.lookup(x, y, gates, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(value) = values.get(index).and_then(|value| value.physical(&coding))
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

#[allow(clippy::too_many_arguments)]
fn render_float_viewport_storage<T: FloatCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    color_table: &ColorTable,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                    let Some(value) = values.get(index).and_then(|value| value.physical(&coding))
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

fn render_float_sample_cache_storage<T: FloatCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                if let Some(value) = values[index].physical(&coding) {
                    let color = sampler.color_for_value(value);
                    if color[3] != 0 {
                        row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                    }
                }
                pixel += 4;
            }
        });
}

/// Storm-relative color of a code: transparent when blank (the sample falls
/// through to the next candidate), the range-folded color, or the value less
/// the row's storm motion.
#[inline]
fn storm_relative_code_color<T: PackedInt>(
    coding: &IntCoding<T>,
    sampler: &ColorSampler,
    raw: T,
    row_motion: f32,
) -> [u8; 4] {
    match code_class(coding, raw) {
        CodeClass::Blank => [0, 0, 0, 0],
        CodeClass::RangeFolded => sampler.range_folded_color(),
        CodeClass::Value => sampler.color_for_value(code_value(coding, raw) - row_motion),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_storage<T: RawCode, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    coding: IntCoding<T>,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
    let gates = view.geometry;
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
                let Some(sample) = geometry.lookup(x, y, gates, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let motion = value_lookup
                        .row_motion
                        .get(candidate.row)
                        .copied()
                        .unwrap_or(0.0);
                    let color = match code_class(&coding, raw) {
                        CodeClass::Blank => continue,
                        CodeClass::RangeFolded => sampler.range_folded_color(),
                        CodeClass::Value => {
                            sampler.color_for_value(code_value(&coding, raw) - motion)
                        }
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

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_viewport_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: IntCoding<T>,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                    let Some(raw) = values.get(index).copied() else {
                        continue;
                    };
                    let motion = value_lookup
                        .row_motion
                        .get(candidate.row)
                        .copied()
                        .unwrap_or(0.0);
                    let color = match code_class(&coding, raw) {
                        CodeClass::Blank => continue,
                        CodeClass::RangeFolded => sampler.range_folded_color(),
                        CodeClass::Value => {
                            sampler.color_for_value(code_value(&coding, raw) - motion)
                        }
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

/// One 256-entry storm-relative palette per row of a byte-coded field.
fn build_storm_relative_row_palettes<T: RawCode>(
    coding: &IntCoding<T>,
    row_motion: &[f32],
    color_table: &ColorTable,
) -> Vec<[[u8; 4]; 256]> {
    let sampler = color_table.sampler();
    row_motion
        .par_iter()
        .map(|motion| {
            let mut palette = [[0, 0, 0, 0]; 256];
            for (index, slot) in palette.iter_mut().enumerate() {
                *slot = storm_relative_code_color(
                    coding,
                    &sampler,
                    T::from_palette_index(index),
                    *motion,
                );
            }
            palette
        })
        .collect()
}

fn render_storm_relative_byte_storage<T: RawCode, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    row_palettes: &[[[u8; 4]; 256]],
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
    let gates = view.geometry;
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
                let Some(sample) = geometry.lookup(x, y, gates, row_lookup) else {
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
                    let color = palette[raw.palette_index()];
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

fn render_storm_relative_byte_viewport_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    row_palettes: &[[[u8; 4]; 256]],
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                    let color = palette[raw.palette_index()];
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

fn render_storm_relative_byte_sample_cache_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    view: FieldView<'_>,
    row_palettes: &[[[u8; 4]; 256]],
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                let color = row_palettes[row][values[index].palette_index()];
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_sample_cache_storage<T: RawCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: IntCoding<T>,
    view: FieldView<'_>,
    row_motion: &[f32],
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                let raw = values[index];
                let color = match code_class(&coding, raw) {
                    CodeClass::Blank => {
                        pixel += 4;
                        continue;
                    }
                    CodeClass::RangeFolded => sampler.range_folded_color(),
                    CodeClass::Value => {
                        sampler.color_for_value(code_value(&coding, raw) - row_motion[row])
                    }
                };
                if color[3] != 0 {
                    row_pixels[pixel..pixel + 4].copy_from_slice(&color);
                }
                pixel += 4;
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_float_storage<T: FloatCode, G: LookupGeometry>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    geometry: G,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
    let gates = view.geometry;
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
                let Some(sample) = geometry.lookup(x, y, gates, row_lookup) else {
                    continue;
                };
                for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
                    let index = candidate.row * gate_count + sample.gate;
                    let Some(velocity) =
                        values.get(index).and_then(|value| value.physical(&coding))
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

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_float_viewport_storage<T: FloatCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    row_lookup: &AzimuthLookup,
    value_lookup: StormRelativeValueLookup<'_>,
    lookup_table: &ViewportLookupTable,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                        values.get(index).and_then(|value| value.physical(&coding))
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

#[allow(clippy::too_many_arguments)]
fn render_storm_relative_float_sample_cache_storage<T: FloatCode>(
    pixels: &mut [u8],
    values: &[T],
    coding: T::Coding,
    view: FieldView<'_>,
    row_motion: &[f32],
    color_table: &ColorTable,
    sample_cache: &ViewportSampleCache,
    clear_pixels: bool,
) {
    let gate_count = view.gate_count();
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
                if let Some(velocity) = values[index].physical(&coding) {
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
    sweep_index: usize,
    field: FieldName,
    width: u32,
    height: u32,
    row_builds: Vec<CachedRowBuild>,
) -> ViewportSampleCache {
    let (sample_count, row_spans, samples) = flatten_cached_rows(height, row_builds);
    ViewportSampleCache {
        volume_ptr,
        sweep_index,
        field,
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

/// First candidate row whose code at the sample's gate is not blank.
fn resolve_int_sample<T: RawCode>(
    values: &[T],
    coding: &IntCoding<T>,
    gate_count: usize,
    row_lookup: &AzimuthLookup,
    sample: SampleLookup,
) -> Option<ResolvedSample> {
    for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
        let index = candidate.row * gate_count + sample.gate;
        let Some(raw) = values.get(index).copied() else {
            continue;
        };
        if code_class(coding, raw) == CodeClass::Blank {
            continue;
        }
        return Some(ResolvedSample {
            row: candidate.row,
            gate: sample.gate,
        });
    }
    None
}

/// First candidate row with a finite physical value at the sample's gate.
fn resolve_float_sample<T: FloatCode>(
    values: &[T],
    coding: &T::Coding,
    gate_count: usize,
    row_lookup: &AzimuthLookup,
    sample: SampleLookup,
) -> Option<ResolvedSample> {
    for candidate in row_lookup.candidates_for_bin(sample.azimuth_bin) {
        let index = candidate.row * gate_count + sample.gate;
        if values
            .get(index)
            .is_some_and(|value| value.physical(coding).is_some())
        {
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
    gates: FieldGeometry,
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
    let gate = ((range_m - gates.first_gate_m()) / gates.lookup_spacing_m()).round() as isize;
    if gate < 0 || gate as usize >= gates.gate_count {
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
    gates: FieldGeometry,
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
    let gate = ((range_m - gates.first_gate_m()) / gates.lookup_spacing_m()).round() as isize;
    if gate < 0 || gate as usize >= gates.gate_count {
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

/// Palette of a byte-coded field: one color per code 0..=255.
fn build_byte_palette<T: RawCode>(
    coding: &IntCoding<T>,
    color_table: &ColorTable,
) -> [[u8; 4]; 256] {
    let sampler = color_table.sampler();
    let mut palette = [[0, 0, 0, 0]; 256];
    for (index, slot) in palette.iter_mut().enumerate() {
        *slot = color_for_code(coding, &sampler, T::from_palette_index(index));
    }
    palette
}

/// Palette of a 16-bit field, sized to the largest code present.
fn build_wide_palette<T: RawCode>(
    values: &[T],
    coding: &IntCoding<T>,
    color_table: &ColorTable,
) -> Vec<[u8; 4]> {
    let sampler = color_table.sampler();
    let max_index = values
        .iter()
        .map(|raw| raw.palette_index())
        .max()
        .unwrap_or(0);
    (0..=max_index)
        .map(|index| color_for_code(coding, &sampler, T::from_palette_index(index)))
        .collect()
}

fn color_for_code<T: PackedInt>(coding: &IntCoding<T>, sampler: &ColorSampler, raw: T) -> [u8; 4] {
    match code_class(coding, raw) {
        CodeClass::Blank => [0, 0, 0, 0],
        CodeClass::RangeFolded => sampler.range_folded_color(),
        CodeClass::Value => sampler.color_for_value(code_value(coding, raw)),
    }
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
    /// Lookup over the rows of a field on `sweep`'s rays. Rows the source did
    /// not provide take no azimuth slot.
    fn new(sweep: &Sweep, view: FieldView<'_>) -> Self {
        let field = view.field;
        Self::from_row_azimuths_iter(
            field,
            (0..field.nrays as usize)
                .filter(|row| !field.is_absent(*row))
                .filter_map(|row| {
                    sweep
                        .rays
                        .azimuth_deg
                        .get(row)
                        .map(|azimuth_deg| (row, *azimuth_deg))
                }),
        )
    }

    /// Lookup for a field whose rows do NOT correspond to sweep rays —
    /// the interpolated display field carries its own synthetic per-row
    /// azimuths (one entry per field row).
    fn from_row_azimuths(row_azimuths_deg: &[f32], view: FieldView<'_>) -> Self {
        Self::from_row_azimuths_iter(view.field, row_azimuths_deg.iter().copied().enumerate())
    }

    fn from_row_azimuths_iter(
        field: &Field,
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
                valid_extent: row_valid_extent(field, row),
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

/// One past the last gate of `row` that is not blank (0 when none is).
fn row_valid_extent(field: &Field, row: usize) -> usize {
    let gate_count = field.ngates as usize;
    let start = row.saturating_mul(gate_count);
    let Some(end) = start.checked_add(gate_count) else {
        return 0;
    };
    fn int_extent<T: RawCode>(row: Option<&[T]>, coding: IntCoding<T>) -> usize {
        row.and_then(|row| {
            row.iter()
                .rposition(|raw| code_class(&coding, *raw) != CodeClass::Blank)
        })
        .map(|gate| gate + 1)
        .unwrap_or(0)
    }
    fn float_extent<T: FloatCode>(row: Option<&[T]>, coding: T::Coding) -> usize {
        row.and_then(|row| {
            row.iter()
                .rposition(|value| value.physical(&coding).is_some())
        })
        .map(|gate| gate + 1)
        .unwrap_or(0)
    }
    match field_values(field) {
        FieldValues::U8(values, coding) => int_extent(values.get(start..end), coding),
        FieldValues::I8(values, coding) => int_extent(values.get(start..end), coding),
        FieldValues::U16(values, coding) => int_extent(values.get(start..end), coding),
        FieldValues::I16(values, coding) => int_extent(values.get(start..end), coding),
        FieldValues::F32(values, coding) => float_extent(values.get(start..end), coding),
        FieldValues::I32(values, coding) => float_extent(values.get(start..end), coding),
        FieldValues::F64(values, coding) => float_extent(values.get(start..end), coding),
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

/// Storm motion along the beam for every row of a field on `sweep`'s rays.
fn row_motion_components(sweep: &Sweep, field: &Field, storm_motion: StormMotion) -> Vec<f32> {
    (0..field.nrays as usize)
        .map(|row| {
            sweep
                .rays
                .azimuth_deg
                .get(row)
                .map(|azimuth_deg| motion_component_away_mps(storm_motion, *azimuth_deg))
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

/// Color table family of a field, from its quantity (`Field::quantity`);
/// unfiltered reflectivity ids without a known quantity use reflectivity
/// colors.
pub fn color_family_for_field(field: &Field) -> ColorTableFamily {
    color_family(field.quantity, &field.name)
}

/// Color table family for a field name alone, classifying it with
/// [`Quantity::classify`].
pub fn color_family_for_name(name: &FieldName) -> ColorTableFamily {
    color_family(Quantity::classify(name.as_str(), None).0, name)
}

fn color_family(quantity: Quantity, name: &FieldName) -> ColorTableFamily {
    match quantity {
        Quantity::Reflectivity => ColorTableFamily::Reflectivity,
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity => ColorTableFamily::Velocity,
        Quantity::SpectrumWidth => ColorTableFamily::SpectrumWidth,
        Quantity::CorrelationCoefficient => ColorTableFamily::CorrelationCoefficient,
        Quantity::DifferentialReflectivity => ColorTableFamily::DifferentialReflectivity,
        Quantity::DifferentialPhase => ColorTableFamily::DifferentialPhase,
        Quantity::SpecificDifferentialPhase => ColorTableFamily::SpecificDifferentialPhase,
        _ if unfiltered_reflectivity_name(name.as_str()) => ColorTableFamily::Reflectivity,
        _ => ColorTableFamily::Generic,
    }
}

/// A validation field carries a physical display scale that is independent
/// of the user's ordinary radar-family palette binding. Keeping this resolver
/// at the render seam means every cache path (native, smoothed, interpolated,
/// and direct PNG) sees the same true 0..1 quality ramp or centered residual
/// ramp even when the caller supplied the Generic family for a derived id.
pub fn validation_color_table_for_field(name: &FieldName) -> Option<ColorTable> {
    color::validation_table_for_moment_id(name.as_str())
}

fn unfiltered_reflectivity_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "DBUZ" | "UDBZ" | "UDBZH" | "DBZ_U" | "THU" | "TVU"
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use recast_radar_core::{Gate, GateMapping, LinearTransform};
    use serde_json::Value;

    // ---- real-data fixtures ----
    //
    // Expected values: `testdata/golden/render/*.json`, written by
    // `tools/render_bench_golden.py render` with Py-ART 2.2.5 (raw gate
    // codes, azimuths, gate geometry, `storm_relative_velocity`) and MetPy
    // 1.7.1 (scaled values), never with this workspace's readers.

    const KTLX_2024: &str = "l2-ktlx-20240315-000217-trim";
    const KTLX_2013: &str = "l2-ktlx-20130520-201643-trim";
    const KTLX_1999: &str = "l2-ktlx-19990504-002218-trim";
    /// Legacy-resolution Message 31 volume: sweeps 4-6 carry 1 km
    /// reflectivity (stride 4 on the sweep range) beside 250 m Doppler
    /// moments.
    const KPAH_2008: &str = "l2-kpah-20080415-235014";
    const KPAH_MIXED_SWEEP: usize = 4;

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
    fn level2(path: &Path) -> Volume {
        recast_radar_io_nexrad::read_volume_from_path(path)
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

    /// Field `name` of sweep `sweep_index`.
    fn field<'v>(volume: &'v Volume, sweep_index: usize, name: &FieldName) -> &'v Field {
        volume.sweeps[sweep_index]
            .field(name)
            .unwrap_or_else(|| panic!("sweep {sweep_index} has no {name}"))
    }

    /// Field `name` of sweep `sweep_index` with its gate geometry.
    fn view<'v>(volume: &'v Volume, sweep_index: usize, name: &FieldName) -> FieldView<'v> {
        view_on(
            field(volume, sweep_index, name),
            &volume.sweeps[sweep_index].range,
            sweep_index,
        )
        .expect("field geometry")
    }

    /// The `u8` codes and coding of a byte-coded field.
    fn u8_codes(field: &Field) -> (&[u8], IntCoding<u8>) {
        let FieldValues::U8(values, coding) = field_values(field) else {
            panic!("{} should be stored as u8 codes", field.name);
        };
        (values, coding)
    }

    fn icd_scale_offset<T: PackedInt>(coding: &IntCoding<T>) -> (f32, f32) {
        let LinearTransform::IcdScaleOffset { scale, offset } = coding.transform else {
            panic!("NEXRAD fields use the ICD transform");
        };
        (scale, offset)
    }

    /// The decoded field must carry the golden gate geometry (centre of the
    /// first gate and spacing) and, row by row, the golden ray azimuths (the
    /// f32 angle field of the file).
    fn assert_geometry(sweep: &Sweep, field: &Field, expected: &Value) {
        assert!(
            field.absent_rows.is_empty(),
            "every ray carries {}",
            field.name
        );
        assert_eq!(field.nrays as usize, as_usize(&expected["rows"]));
        assert_eq!(field.ngates as usize, as_usize(&expected["gates"]));
        let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range).expect("geometry");
        assert_eq!(
            first_gate_m,
            expected["first_gate_m"].as_i64().expect("first gate") as f64
        );
        assert_eq!(
            spacing_m,
            expected["gate_spacing_m"].as_i64().expect("gate spacing") as f64
        );
        let azimuths = array(&expected["azimuth_deg"]);
        assert_eq!(azimuths.len(), field.nrays as usize);
        for (row, azimuth) in azimuths.iter().enumerate() {
            let decoded = sweep.rays.azimuth_deg[row];
            assert!(
                (decoded - as_f32(azimuth)).abs() < 1e-4,
                "row {row}: azimuth {decoded} != {azimuth}"
            );
        }
    }

    /// A 65 x 65 viewport at 50 m per pixel whose centre pixel (32, 32) sits
    /// exactly on the centre of `gate` of `row`; returns the options and the
    /// centre pixel's index.
    fn gate_centre_viewport(
        sweep: &Sweep,
        field: &Field,
        row: usize,
        gate: usize,
    ) -> (ViewportRasterOptions, usize) {
        const SIZE: u32 = 65;
        const KM_PER_PX: f32 = 0.05;
        let centre_px = (SIZE / 2) as f32 + 0.5;
        let azimuth = sweep.rays.azimuth_deg[row].to_radians();
        let (first_gate_m, spacing_m) = field.native_geometry(&sweep.range).expect("geometry");
        let range_km = (first_gate_m + gate as f64 * spacing_m) as f32 / 1000.0;
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

    fn raster() -> RasterOptions {
        RasterOptions {
            width: 96,
            height: 96,
            range_fraction: 94,
        }
    }

    // ---- options, tables and codings (no radar data) ----

    #[test]
    fn base_layer_starts_visible() {
        assert!(RenderLayer::base(FieldName::Dbzh).visible);
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
            color_family_for_name(&FieldName::parse("dBuZ")),
            ColorTableFamily::Reflectivity
        );
        assert_eq!(
            color_family_for_name(&FieldName::parse("mystery")),
            ColorTableFamily::Generic
        );
        // Known names color by quantity, whatever the spelling.
        assert_eq!(
            color_family_for_name(&FieldName::Dbz),
            ColorTableFamily::Reflectivity
        );
        assert_eq!(
            color_family_for_name(&FieldName::Vraddh),
            ColorTableFamily::Velocity
        );
        assert_eq!(
            color_family_for_name(&FieldName::Rhohv),
            ColorTableFamily::CorrelationCoefficient
        );
    }

    #[test]
    fn synthetic_validation_fields_resolve_physical_palettes() {
        let quality =
            validation_color_table_for_field(&FieldName::parse("MCOV")).expect("quality palette");
        assert_eq!(quality.stops().first().unwrap().value, 0.0);
        assert_eq!(quality.stops().last().unwrap().value, 1.0);

        for id in [
            "DIF_REF", "DIF_VEL", "DIF_ZDR", "DIF_RHO", "DIF_PHI", "DIF_KDP",
        ] {
            let table = validation_color_table_for_field(&FieldName::parse(id))
                .expect("difference palette");
            assert_eq!(
                table.stops().first().unwrap().value,
                -table.stops().last().unwrap().value,
                "{id}"
            );
        }
        assert!(validation_color_table_for_field(&FieldName::parse("OTHER")).is_none());
        assert!(validation_color_table_for_field(&FieldName::Dbzh).is_none());
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
    fn nexrad_coding_blanks_undetect_and_out_of_range_codes() {
        // The NEXRAD coding: raw 0 is undetect and fill, raw 1 range folded,
        // valid_range [2, 255].
        let coding = IntCoding::<u8>::nexrad(2.0, 66.0);
        assert_eq!(code_class(&coding, 0u8), CodeClass::Blank);
        assert_eq!(code_class(&coding, 1u8), CodeClass::RangeFolded);
        assert_eq!(code_class(&coding, 2u8), CodeClass::Value);
        assert_eq!(code_value(&coding, 66u8), 0.0);
        let mut narrow = coding;
        narrow.valid_range = Some([2, 200]);
        assert_eq!(code_class(&narrow, 201u8), CodeClass::Blank);
        // A signed CfRadial packing: the fill is the only blank code.
        let cf = IntCoding::<i8> {
            transform: LinearTransform::CfScaleOffset {
                scale_factor: 0.5,
                add_offset: 32.0,
                attr_width: recast_radar_core::model::FloatWidth::F32,
            },
            fill_value: Some(-128),
            undetect: None,
            range_folded: None,
            valid_range: None,
        };
        assert_eq!(code_class(&cf, -128i8), CodeClass::Blank);
        assert_eq!(code_class(&cf, -127i8), CodeClass::Value);
        assert_eq!(code_value(&cf, 0i8), 32.0);
        assert_eq!((-128i8).palette_index(), 128);
        assert_eq!(<i8 as RawCode>::from_palette_index(128), -128);
    }

    #[test]
    fn float_codes_skip_fill_undetect_and_apply_transforms() {
        let plain = FloatCoding::<f32>::default();
        assert_eq!(12.5f32.physical(&plain), Some(12.5));
        assert_eq!(f32::NAN.physical(&plain), None);
        let with_fill = FloatCoding::<f32> {
            transform: None,
            fill_value: Some(-9999.0),
            undetect: Some(-32.0),
        };
        assert_eq!((-9999.0f32).physical(&with_fill), None);
        assert_eq!((-32.0f32).physical(&with_fill), None);
        assert_eq!(7.0f32.physical(&with_fill), Some(7.0));
        let scaled = FloatCoding::<f64> {
            transform: Some(LinearTransform::CfScaleOffset {
                scale_factor: 0.5,
                add_offset: -32.0,
                attr_width: recast_radar_core::model::FloatWidth::F64,
            }),
            fill_value: None,
            undetect: None,
        };
        assert_eq!(100.0f64.physical(&scaled), Some(18.0));
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&doppler["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let velocity = field(&volume, sweep_index, &FieldName::Vradh);
        assert_geometry(sweep, velocity, doppler);
        let (values, coding) = u8_codes(velocity);
        assert_eq!(
            coding.fill_value,
            Some(as_usize(&doppler["no_data_code"]) as u8)
        );
        assert_eq!(
            coding.range_folded,
            Some(as_usize(&doppler["range_folded_code"]) as u8)
        );
        let gates = velocity.ngates as usize;
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
            assert_eq!(velocity.gate(row, gate), Some(Gate::RangeFolded));
            assert_eq!(velocity.value(row, gate), None);
        }

        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Velocity);
        let range_folded = table.range_folded_color();
        assert_ne!(range_folded[3], 0);
        assert_eq!(color_for_code(&coding, &table.sampler(), 1u8), range_folded);
        assert_eq!(color_for_code(&coding, &table.sampler(), 0u8), [0, 0, 0, 0]);
        assert_eq!(build_byte_palette(&coding, table)[1], range_folded);

        for pair in array(&doppler["range_folded_interior"]) {
            let (row, gate) = row_gate(pair);
            let (options, centre) = gate_centre_viewport(sweep, velocity, row, gate);
            let (_, _, pixels) =
                render_field_viewport_rgba(&volume, sweep_index, &FieldName::Vradh, options)
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&doppler["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = field(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity, doppler);
        let (values, coding) = u8_codes(reflectivity);
        assert_eq!(coding.range_folded, Some(1));
        let gates = reflectivity.ngates as usize;
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
        assert_eq!(color_for_code(&coding, &table.sampler(), 1u8), range_folded);

        for pair in array(&doppler["range_folded_interior"]) {
            let (row, gate) = row_gate(pair);
            let (options, centre) = gate_centre_viewport(sweep, reflectivity, row, gate);
            let (_, _, pixels) =
                render_field_viewport_rgba(&volume, sweep_index, &FieldName::Dbzh, options)
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
    fn storm_relative_byte_row_palette_matches_pyart_storm_relative_velocity() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&doppler["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let velocity = field(&volume, sweep_index, &FieldName::Vradh);
        let (values, coding) = u8_codes(velocity);
        let gates = velocity.ngates as usize;
        let storm_motion = StormMotion {
            direction_deg: as_f32(&doppler["storm_motion"]["direction_deg"]),
            speed_mps: as_f32(&doppler["storm_motion"]["speed_mps"]),
        };
        let tables = ColorTableSet::default();
        let color_table = tables.for_family(ColorTableFamily::Velocity);
        let sampler = color_table.sampler();
        let row_motion = StormMotionBasis::new(sweep, velocity).row_motion_components(storm_motion);
        let palettes = build_storm_relative_row_palettes(&coding, &row_motion, color_table);
        assert_eq!(palettes.len(), velocity.nrays as usize);

        let samples = array(&doppler["storm_relative_samples"]);
        assert!(samples.len() >= 32);
        for sample in samples {
            let (row, gate) = (as_usize(&sample["row"]), as_usize(&sample["gate"]));
            let code = as_usize(&sample["code"]);
            let value = as_f32(&sample["velocity_mps"]);
            let storm_relative = as_f32(&sample["storm_relative_mps"]);
            assert_eq!(usize::from(values[row * gates + gate]), code);
            assert_eq!(velocity.value(row, gate), Some(value));
            let azimuth = sweep.rays.azimuth_deg[row];
            assert!(
                (storm_relative_velocity_mps(value, azimuth, storm_motion) - storm_relative).abs()
                    < 1e-3,
                "row {row} gate {gate}"
            );
            assert!((value - row_motion[row] - storm_relative).abs() < 1e-3);
            let color = palettes[row][code];
            assert_eq!(
                color,
                storm_relative_code_color(&coding, &sampler, code as u8, row_motion[row])
            );
            assert_color_close(
                color,
                sampler.color_for_value(storm_relative),
                &format!("row {row} code {code}"),
            );
        }
        let rows = velocity.nrays as usize;
        for row in [0, rows / 2, rows - 1] {
            assert_eq!(palettes[row][0], [0, 0, 0, 0]);
            assert_eq!(palettes[row][1], color_table.range_folded_color());
        }
    }

    /// A custom velocity ramp sampled through the byte palette of the real
    /// field (scale 2, offset 129 from the file's data block header): every
    /// code present in the sweep maps to the ramp colour of its physical
    /// velocity, including the exact stops at 0 m/s (code 129) and 20 m/s
    /// (code 169).
    #[test]
    fn custom_color_table_feeds_precomputed_byte_palette() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let velocity = field(&volume, as_usize(&doppler["sweep"]), &FieldName::Vradh);
        let (values, coding) = u8_codes(velocity);
        assert_eq!(
            icd_scale_offset(&coding),
            (as_f32(&doppler["scale"]), as_f32(&doppler["offset"]))
        );
        let table = ColorTable::parse(
            "unit test velocity",
            "units: m/s\ncolor: -20 1 2 3\ncolor: 0 10 20 30\ncolor: 20 40 50 60",
        )
        .expect("custom color table");

        let palette = build_byte_palette(&coding, &table);

        let samples = array(&doppler["custom_table_samples"]);
        assert!(samples.len() > 50);
        let mut exact_stops = 0;
        for sample in samples {
            let code = as_usize(&sample["code"]);
            let value = as_f32(&sample["velocity_mps"]);
            assert!(
                values.contains(&(code as u8)),
                "code {code} is in the sweep"
            );
            assert_eq!(palette[code], table.color_for_value(value), "code {code}");
            match value {
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&doppler["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let velocity = field(&volume, sweep_index, &FieldName::Vradh);
        assert_geometry(sweep, velocity, doppler);
        let basis = StormMotionBasis::new(sweep, velocity);
        let storm_motion = StormMotion {
            direction_deg: 225.0,
            speed_mps: 18.0,
        };
        let row_motion = basis.row_motion_components(storm_motion);
        assert_eq!(row_motion.len(), velocity.nrays as usize);

        for (row, azimuth) in array(&doppler["azimuth_deg"]).iter().enumerate() {
            let azimuth = as_f32(azimuth);
            let reference =
                storm_motion.speed_mps * (storm_motion.direction_deg - azimuth).to_radians().cos();
            assert!(
                (row_motion[row] - reference).abs() < 1e-4,
                "row {row}: {} != {reference}",
                row_motion[row]
            );
            let direct = motion_component_away_mps(storm_motion, sweep.rays.azimuth_deg[row]);
            assert!((row_motion[row] - direct).abs() < 1e-5);
        }
        for (basis, direct) in
            row_motion
                .iter()
                .zip(row_motion_components(sweep, velocity, storm_motion))
        {
            assert!((basis - direct).abs() < 1e-5);
        }
    }

    // ---- gate geometry ----

    /// KPAH 2008 sweep 4: 1 km reflectivity (centres from 500 m) beside
    /// 250 m Doppler moments (centres from 125 m). The reflectivity keeps its
    /// native 1 km gates at their true centres, every fourth gate of the
    /// sweep's 250 m range.
    #[test]
    fn field_geometry_follows_the_gate_mapping_on_the_sweep_range() {
        let path = recast_radar_testdata::require_file!(KPAH_2008);
        let volume = level2(&path);
        let sweep = &volume.sweeps[KPAH_MIXED_SWEEP];
        let reflectivity = field(&volume, KPAH_MIXED_SWEEP, &FieldName::Dbzh);
        let velocity = field(&volume, KPAH_MIXED_SWEEP, &FieldName::Vradh);
        assert_eq!(sweep.range.spacing_m(), Some(250.0));
        assert_eq!(sweep.range.center_m(0), Some(125.0));
        assert_eq!(
            reflectivity.gates,
            GateMapping {
                start: 0,
                stride: 4
            }
        );
        assert_eq!(velocity.gates, GateMapping::IDENTITY);

        let dbzh = FieldGeometry::of(reflectivity, &sweep.range).unwrap();
        assert_eq!(dbzh.first_gate_m(), 500.0);
        assert_eq!(dbzh.lookup_spacing_m(), 1000.0);
        assert_eq!(dbzh.gate_count, reflectivity.ngates as usize);
        assert_eq!(
            dbzh.max_range_m(),
            500.0 + 1000.0 * reflectivity.ngates as f32
        );
        let vradh = FieldGeometry::of(velocity, &sweep.range).unwrap();
        assert_eq!(vradh.first_gate_m(), 125.0);
        assert_eq!(vradh.lookup_spacing_m(), 250.0);
        assert_eq!(vradh.max_range_m(), 125.0 + 250.0 * velocity.ngates as f32);
    }

    // ---- viewport geometry on the KTLX 2024-03-15 surveillance cut ----

    /// The sample-cache bound follows the radar's 460 km footprint (1832 gates
    /// of 250 m from 2125 m): between the exact pixel count inside that circle
    /// and that count plus the row-span padding, and below the full viewport.
    #[test]
    fn field_sample_cache_upper_bound_tracks_actual_radar_footprint() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = field(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity, surveillance);
        let max_range_km = as_f32(&surveillance["max_range_m"]) / 1000.0;
        let geometry = view(&volume, sweep_index, &FieldName::Dbzh).geometry;
        assert_eq!(geometry.max_range_m() / 1000.0, max_range_km);
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
        let radar_footprint = viewport_sample_cache_storage_upper_bound_for_field(
            reflectivity,
            &sweep.range,
            options,
        );
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
        let cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh).unwrap();
        assert_eq!(
            cache
                .sample_cache_storage_upper_bound(&volume, options)
                .unwrap(),
            radar_footprint
        );
    }

    #[test]
    fn viewport_lookup_matches_reference_hypot_formula() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let row_lookup = AzimuthLookup::new(sweep, reflectivity);
        let max_range_km = reflectivity.geometry.max_range_m().max(1.0) / 1000.0;
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
            let sample = viewport_lookup(x, y, reflectivity.geometry, &row_lookup, geometry);
            assert_eq!(
                sample,
                viewport_lookup_reference(x, y, reflectivity.geometry, &row_lookup, geometry)
            );
            resolved += usize::from(sample.is_some());
        }
        assert_eq!(resolved, 5);
    }

    #[test]
    fn viewport_lookup_table_matches_reference_hypot_formula() {
        let expected = golden("ktlx2024.json");
        let surveillance = &expected["surveillance"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let row_lookup = AzimuthLookup::new(sweep, reflectivity);
        let geometry = viewport_geometry(reflectivity.geometry, window_viewport_options());
        let lookup_table = ViewportLookupTable::new(reflectivity.geometry, geometry);

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
                    viewport_lookup_reference(x, y, reflectivity.geometry, &row_lookup, geometry),
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let row_lookup = AzimuthLookup::new(sweep, reflectivity);
        for rotation_rad in [-0.21f32, 0.005, 0.35] {
            let geometry = viewport_geometry(
                reflectivity.geometry,
                ViewportRasterOptions {
                    rotation_rad,
                    ..window_viewport_options()
                },
            );
            let lookup_table = ViewportLookupTable::new(reflectivity.geometry, geometry);
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
                        viewport_lookup(x, y, reflectivity.geometry, &row_lookup, geometry),
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let row_lookup = AzimuthLookup::new(sweep, reflectivity);
        let options = |rotation_rad| ViewportRasterOptions {
            width: 96,
            height: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad,
        };
        let gates = reflectivity.geometry;
        let rotated = ViewportLookupTable::new(gates, viewport_geometry(gates, options(0.35)));
        let straight = ViewportLookupTable::new(gates, viewport_geometry(gates, options(0.0)));
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let row_lookup = AzimuthLookup::new(sweep, reflectivity);
        let max_range_km = reflectivity.geometry.max_range_m().max(1.0) / 1000.0;
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
                if viewport_lookup_reference(x, y, reflectivity.geometry, &row_lookup, geometry)
                    .is_some()
                {
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
        let path = recast_radar_testdata::require_file!(KTLX_1999);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert_geometry(sweep, reflectivity.field, surveillance);
        let spacing = as_f32(&surveillance["median_spacing_deg"]);
        assert!(spacing > 0.9 && spacing < 1.0, "{spacing} deg radials");

        let lookup = AzimuthLookup::new(sweep, reflectivity);
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&surveillance["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        let field = reflectivity.field;
        assert_geometry(sweep, field, surveillance);
        let (values, coding) = u8_codes(field);
        let gates = field.ngates as usize;
        for (row, extent) in array(&surveillance["valid_extent"]).iter().enumerate() {
            assert_eq!(row_valid_extent(field, row), as_usize(extent), "row {row}");
        }

        let lookup = AzimuthLookup::new(sweep, reflectivity);
        let pairs = array(&surveillance["longer_extent_neighbours"]);
        assert!(pairs.len() >= 16);
        for pair in pairs {
            let (first, second) = row_gate(&pair["rows"]);
            assert_eq!(second, first + 1);
            let longer = as_usize(&pair["longer_row"]);
            let shorter = if longer == first { second } else { first };
            let gate = as_usize(&pair["gate"]);
            assert_eq!(gate + 1, row_valid_extent(field, longer));
            assert!(gate >= row_valid_extent(field, shorter));
            assert_eq!(values[shorter * gates + gate], 0);
            assert_ne!(values[longer * gates + gate], 0);

            // Bins at the midpoint of the two radials' 0.1 deg bin centres.
            let first_bin = azimuth_bin(sweep.rays.azimuth_deg[first]);
            let mut second_bin = azimuth_bin(sweep.rays.azimuth_deg[second]);
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
                let resolved =
                    resolve_int_sample(values, &coding, reflectivity.gate_count(), &lookup, sample)
                        .expect("gate within the longer row resolves");
                assert_eq!(resolved.row, longer, "bin {bin} gate {gate}");
                assert_eq!(resolved.gate, gate);
            }
        }
    }

    /// A ray without a row never draws: on the real surveillance cut with
    /// ray 100 marked absent (its stored codes blanked, as the model keeps
    /// them), the ray takes no azimuth slot and its neighbours serve its bins.
    #[test]
    fn absent_rows_take_no_azimuth_slot_and_never_resolve() {
        let expected = golden("ktlx2024.json");
        let sweep_index = as_usize(&expected["surveillance"]["sweep"]);
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let mut volume = level2(&path);
        const ABSENT: usize = 100;
        let before = {
            let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
            let lookup = AzimuthLookup::new(&volume.sweeps[sweep_index], reflectivity);
            let azimuth = volume.sweeps[sweep_index].rays.azimuth_deg[ABSENT];
            assert_eq!(lookup.row_for_azimuth(azimuth), Some(ABSENT));
            azimuth
        };
        {
            let sweep = &mut volume.sweeps[sweep_index];
            let index = sweep.field_index(&FieldName::Dbzh).expect("DBZH");
            let field = &mut sweep.fields[index];
            let gates = field.ngates as usize;
            let FieldData::U8 { values, .. } = &mut field.data else {
                panic!("u8 reflectivity");
            };
            values[ABSENT * gates..(ABSENT + 1) * gates].fill(0);
            field.absent_rows = vec![ABSENT as u32];
            sweep.seal().expect("sealed edit");
        }
        let sweep = &volume.sweeps[sweep_index];
        let reflectivity = view(&volume, sweep_index, &FieldName::Dbzh);
        assert!(has_rows(reflectivity.field));
        let lookup = AzimuthLookup::new(sweep, reflectivity);
        let served = lookup
            .row_for_azimuth(before)
            .expect("a neighbour serves the bin");
        assert_ne!(served, ABSENT);
        assert_eq!(served.abs_diff(ABSENT), 1);
        for bin in 0..AZIMUTH_BINS {
            assert!(
                lookup
                    .candidates_for_bin(bin)
                    .iter()
                    .all(|candidate| candidate.row != ABSENT),
                "absent ray is a candidate of bin {bin}"
            );
        }
        // A field whose rows are all absent is empty for rendering.
        let mut empty = reflectivity.field.clone();
        empty.absent_rows = (0..empty.nrays).collect();
        assert!(!has_rows(&empty));
    }

    /// Range-folded gates count as valid data: the valid extent of each
    /// velocity row (one past the last non-zero code, from Py-ART) includes
    /// them, and a range-folded gate resolves to its own row.
    #[test]
    fn int_sample_resolution_keeps_visible_range_folded_candidates() {
        let expected = golden("ktlx2024.json");
        let doppler = &expected["doppler"];
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&doppler["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let velocity = view(&volume, sweep_index, &FieldName::Vradh);
        assert_geometry(sweep, velocity.field, doppler);
        let (values, coding) = u8_codes(velocity.field);
        let gates = velocity.gate_count();
        for (row, extent) in array(&doppler["valid_extent"]).iter().enumerate() {
            assert_eq!(
                row_valid_extent(velocity.field, row),
                as_usize(extent),
                "row {row}"
            );
        }

        let lookup = AzimuthLookup::new(sweep, velocity);
        for pair in array(&doppler["range_folded_first"]) {
            let (row, gate) = row_gate(pair);
            assert_eq!(values[row * gates + gate], 1);
            assert!(gate < row_valid_extent(velocity.field, row));
            let sample = SampleLookup {
                azimuth_bin: azimuth_bin(sweep.rays.azimuth_deg[row]),
                gate,
            };
            let resolved = resolve_int_sample(values, &coding, gates, &lookup, sample)
                .expect("range-folded sample should resolve");
            assert_eq!(resolved, ResolvedSample { row, gate });
        }
    }

    // ---- viewport rendering and caches ----

    #[test]
    fn viewport_render_uses_requested_screen_resolution() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();
        let storm_motion = StormMotion {
            direction_deg: 45.0,
            speed_mps: 10.0,
        };

        let reflectivity =
            render_field_viewport_image(&volume, sweep_index, &FieldName::Dbzh, options)
                .expect("viewport reflectivity");
        assert_eq!(reflectivity.dimensions(), (333, 217));
        assert!(has_visible_pixel(reflectivity.as_raw()));

        let mut reusable_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_field_viewport_rgba_into(
            &volume,
            sweep_index,
            &FieldName::Dbzh,
            options,
            &mut reusable_pixels,
        )
        .expect("viewport reflectivity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&reusable_pixels));
        assert!(has_transparent_pixel(&reusable_pixels));
        assert_eq!(reusable_pixels, *reflectivity.as_raw());

        let reflectivity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh)
            .expect("viewport reflectivity cache");
        assert_eq!(reflectivity_cache.field_name(), &FieldName::Dbzh);
        assert_eq!(reflectivity_cache.sweep_index(), sweep_index);
        reusable_pixels.fill(255);
        let dimensions = reflectivity_cache
            .render_field_rgba_into(&volume, options, &mut reusable_pixels)
            .expect("cached viewport reflectivity");
        assert_eq!(dimensions, (333, 217));
        assert_eq!(reusable_pixels, *reflectivity.as_raw());

        let storm_relative = render_storm_relative_velocity_viewport_image(
            &volume,
            sweep_index,
            &FieldName::Vradh,
            storm_motion,
            options,
        )
        .expect("viewport storm-relative velocity");
        assert_eq!(storm_relative.dimensions(), (333, 217));
        assert!(has_visible_pixel(storm_relative.as_raw()));

        let mut storm_relative_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_storm_relative_velocity_viewport_rgba_into(
            &volume,
            sweep_index,
            &FieldName::Vradh,
            storm_motion,
            options,
            &mut storm_relative_pixels,
        )
        .expect("viewport storm-relative velocity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&storm_relative_pixels));
        assert!(has_transparent_pixel(&storm_relative_pixels));
        assert_eq!(storm_relative_pixels, *storm_relative.as_raw());

        let velocity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Vradh)
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

    /// Storm-relative rendering and dealiasing need a radial velocity, and
    /// the caches report missing fields and sweeps (KTLX 2024 trim: sweep 0
    /// is the surveillance cut without spectrum width, two sweeps in all).
    #[test]
    fn storm_relative_rendering_needs_a_radial_velocity() {
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let options = sample_viewport_options();
        let err = render_storm_relative_velocity_viewport_image(
            &volume,
            1,
            &FieldName::Dbzh,
            StormMotion {
                direction_deg: 45.0,
                speed_mps: 10.0,
            },
            options,
        )
        .expect_err("reflectivity has no storm-relative form");
        assert!(matches!(
            err,
            RenderError::NotRadialVelocity {
                field: FieldName::Dbzh
            }
        ));
        let Err(err) = ViewportFieldCache::new_dealiased_velocity(&volume, 1, &FieldName::Dbzh)
        else {
            panic!("dealiasing needs a radial velocity");
        };
        assert!(matches!(err, RenderError::NotRadialVelocity { .. }));
        let Err(err) = ViewportFieldCache::new(&volume, 0, &FieldName::Wradh) else {
            panic!("the surveillance cut has no spectrum width");
        };
        assert!(matches!(
            err,
            RenderError::MissingField {
                sweep_index: 0,
                field: FieldName::Wradh
            }
        ));
        let Err(err) = ViewportFieldCache::new(&volume, 3, &FieldName::Dbzh) else {
            panic!("the trimmed volume has two sweeps");
        };
        assert!(matches!(
            err,
            RenderError::SweepOutOfRange {
                index: 3,
                sweep_count: 2
            }
        ));
    }

    /// The sample cache reproduces the direct render exactly when every
    /// measured code has a visible colour (an opaque reflectivity ramp).
    /// Under the default reflectivity palette, which hides low dBZ, the two
    /// paths can differ only where the cache's first candidate radial holds a
    /// hidden code: the direct render falls through to the next candidate of
    /// the azimuth bin, the cache leaves the pixel transparent.
    #[test]
    fn viewport_sample_cache_matches_direct_field_render() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
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
        let cache = ViewportFieldCache::new_with_color_tables(
            &volume,
            sweep_index,
            &FieldName::Dbzh,
            &tables,
        )
        .expect("viewport reflectivity cache");
        let sample_cache = cache
            .build_sample_cache(&volume, options)
            .expect("viewport sample cache");
        let mut direct_pixels = vec![0; viewport_rgba_buffer_len(options)];
        let mut sample_cache_pixels = vec![255; viewport_rgba_buffer_len(options)];

        cache
            .render_field_rgba_into(&volume, options, &mut direct_pixels)
            .expect("direct viewport render");
        let dimensions = cache
            .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut sample_cache_pixels)
            .expect("sample-cache viewport render");

        assert_eq!(dimensions, (333, 217));
        assert_eq!(sample_cache.dimensions(), (333, 217));
        assert!(sample_cache.sample_count() > 0);
        assert!(sample_cache.storage_bytes() < viewport_rgba_buffer_len(options));
        assert!(opaque_pixels(&direct_pixels) > 500);
        assert_eq!(sample_cache_pixels, direct_pixels);

        let mut reused_pixels = direct_pixels.clone();
        cache
            .render_field_rgba_with_sample_cache_reusing_transparency(
                &volume,
                &sample_cache,
                &mut reused_pixels,
            )
            .expect("sample-cache reuse viewport render");
        assert_eq!(reused_pixels, sample_cache_pixels);

        // Default palette: the same sample cache, transparent low dBZ.
        let cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh)
            .expect("default reflectivity cache");
        let sample_cache = cache
            .build_sample_cache(&volume, options)
            .expect("default sample cache");
        cache
            .render_field_rgba_into(&volume, options, &mut direct_pixels)
            .expect("direct default render");
        cache
            .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut sample_cache_pixels)
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
        assert!(
            fallthrough < opaque_pixels(&direct_pixels),
            "{fallthrough} fall-through pixels of {} opaque",
            opaque_pixels(&direct_pixels)
        );
    }

    #[test]
    fn viewport_geometry_cache_resolves_across_compatible_products() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        // The Doppler cut carries reflectivity and velocity on the same gates.
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
        assert_eq!(
            view(&volume, sweep_index, &FieldName::Dbzh).geometry,
            view(&volume, sweep_index, &FieldName::Vradh).geometry
        );
        let options = window_viewport_options();
        let reflectivity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh)
            .expect("reflectivity cache");
        let velocity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Vradh)
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
            .render_field_rgba_with_sample_cache(
                &volume,
                &geometry_sample_cache,
                &mut geometry_pixels,
            )
            .expect("geometry-derived sample render");
        velocity_cache
            .render_field_rgba_with_sample_cache(&volume, &direct_sample_cache, &mut direct_pixels)
            .expect("direct sample render");

        assert_eq!(geometry_cache.dimensions(), (333, 217));
        assert!(geometry_cache.sample_count() >= geometry_sample_cache.sample_count());
        assert!(opaque_pixels(&direct_pixels) > 1_000);
        assert_eq!(geometry_pixels, direct_pixels);
    }

    /// KPAH 2008 sweep 4: a geometry cache built for the 1 km reflectivity
    /// cannot serve the 250 m velocity of the same sweep.
    #[test]
    fn viewport_geometry_cache_rejects_a_different_gate_geometry() {
        let path = recast_radar_testdata::require_file!(KPAH_2008);
        let volume = level2(&path);
        assert_ne!(
            view(&volume, KPAH_MIXED_SWEEP, &FieldName::Dbzh).geometry,
            view(&volume, KPAH_MIXED_SWEEP, &FieldName::Vradh).geometry
        );
        let options = sample_viewport_options();
        let coarse = ViewportFieldCache::new(&volume, KPAH_MIXED_SWEEP, &FieldName::Dbzh).unwrap();
        let fine = ViewportFieldCache::new(&volume, KPAH_MIXED_SWEEP, &FieldName::Vradh).unwrap();
        let geometry_cache = coarse.build_geometry_cache(&volume, options).unwrap();
        assert!(matches!(
            fine.build_sample_cache_from_geometry_cache(&volume, &geometry_cache),
            Err(RenderError::GeometryCacheMismatch)
        ));
        assert!(
            coarse
                .build_sample_cache_from_geometry_cache(&volume, &geometry_cache)
                .is_ok()
        );
    }

    #[test]
    fn viewport_sample_cache_matches_direct_storm_relative_render() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();
        let storm_motion = StormMotion {
            direction_deg: 45.0,
            speed_mps: 10.0,
        };
        let cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Vradh)
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

        // The byte palette cache draws the same pixels.
        let palette_cache = cache
            .build_storm_relative_velocity_palette_cache(&volume, storm_motion)
            .expect("palette cache")
            .expect("u8 velocity has a palette cache");
        let mut palette_pixels = vec![255; viewport_rgba_buffer_len(options)];
        cache
            .render_storm_relative_velocity_rgba_into_with_palette_cache(
                &volume,
                storm_motion,
                &palette_cache,
                options,
                &mut palette_pixels,
            )
            .expect("palette-cache SRV render");
        assert_eq!(palette_pixels, direct_pixels);
    }

    #[test]
    fn viewport_sample_cache_rejects_mismatched_cache() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
        let options = ViewportRasterOptions {
            width: 64,
            height: 64,
            radar_x_px: 32.0,
            radar_y_px: 32.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let reflectivity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh)
            .expect("reflectivity cache");
        let velocity_cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Vradh)
            .expect("velocity cache");
        let sample_cache = reflectivity_cache
            .build_sample_cache(&volume, options)
            .expect("reflectivity sample cache");
        let mut pixels = vec![0; viewport_rgba_buffer_len(options)];

        let err = velocity_cache
            .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut pixels)
            .expect_err("sample cache should be field-bound");

        assert!(matches!(
            err,
            RenderError::CacheFieldMismatch {
                expected: FieldName::Vradh,
                actual: FieldName::Dbzh
            }
        ));
    }

    #[test]
    fn viewport_render_rejects_wrong_sized_reusable_buffer() {
        let expected = golden("ktlx2024.json");
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let sweep_index = as_usize(&expected["doppler"]["sweep"]);
        let options = window_viewport_options();

        let mut pixels = vec![0; viewport_rgba_buffer_len(options) - 4];
        let err = render_field_viewport_rgba_into(
            &volume,
            sweep_index,
            &FieldName::Dbzh,
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
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let other_path = recast_radar_testdata::require_file!(KTLX_2013);
        let other_volume = level2(&other_path);
        assert_ne!(volume.time_reference, other_volume.time_reference);
        let options = ViewportRasterOptions {
            width: 64,
            height: 64,
            radar_x_px: 32.0,
            radar_y_px: 32.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh)
            .expect("viewport reflectivity cache");
        let mut pixels = vec![0; viewport_rgba_buffer_len(options)];

        let err = cache
            .render_field_rgba_into(&other_volume, options, &mut pixels)
            .expect_err("cache should be bound to its source volume");

        assert!(matches!(err, RenderError::CacheVolumeMismatch));
        cache
            .render_field_rgba_into(&volume, options, &mut pixels)
            .expect("the source volume still renders");
        assert!(has_visible_pixel(&pixels));
    }

    /// Differential phase in the Build 13.2 KTLX 2013-05-20 volume is a
    /// 16-bit field (codes to 1022, scale 2.8361, offset 2): its wide palette
    /// colours every code by the physical value MetPy reports, and the cached
    /// viewport render equals the direct one.
    #[test]
    fn viewport_cache_renders_u16_palette_fields() {
        let expected = golden("ktlx2013.json");
        let phase = &expected["differential_phase"];
        let path = recast_radar_testdata::require_file!(KTLX_2013);
        let volume = level2(&path);
        let sweep_index = as_usize(&phase["sweep"]);
        let sweep = &volume.sweeps[sweep_index];
        let phidp = field(&volume, sweep_index, &FieldName::Phidp);
        assert_geometry(sweep, phidp, phase);
        let FieldValues::U16(values, coding) = field_values(phidp) else {
            panic!("16-bit differential phase should be stored as u16");
        };
        let gates = phidp.ngates as usize;
        let max_code = as_usize(&phase["max_code"]) as u16;
        assert_eq!(values.iter().copied().max(), Some(max_code));
        let (scale, offset) = icd_scale_offset(&coding);
        assert!((scale - as_f32(&phase["scale"])).abs() < 1e-4);
        assert!((offset - as_f32(&phase["offset"])).abs() < 1e-4);
        let finite = (0..phidp.nrays as usize)
            .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
            .filter(|&(row, gate)| phidp.value(row, gate).is_some())
            .count();
        assert_eq!(finite, as_usize(&phase["finite_gates"]));

        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::DifferentialPhase);
        assert_eq!(
            color_family_for_field(phidp),
            ColorTableFamily::DifferentialPhase
        );
        let palette = build_wide_palette(values, &coding, table);
        assert_eq!(palette.len(), usize::from(max_code) + 1);
        assert_eq!(palette[0], [0, 0, 0, 0]);
        for sample in array(&phase["samples"]) {
            let (row, gate) = (as_usize(&sample["row"]), as_usize(&sample["gate"]));
            let code = as_usize(&sample["code"]);
            let value = as_f32(&sample["value_deg"]);
            assert_eq!(usize::from(values[row * gates + gate]), code);
            let scaled = phidp.value(row, gate).expect("finite phase");
            assert!((scaled - value).abs() < 1e-3, "row {row} gate {gate}");
            assert_eq!(
                palette[code],
                color_for_code(&coding, &table.sampler(), code as u16)
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
        let cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Phidp)
            .expect("viewport u16 differential phase cache");
        let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = cache
            .render_field_rgba_into(&volume, options, &mut pixels)
            .expect("cached u16 viewport differential phase");
        assert_eq!(dimensions, (96, 96));
        assert!(has_visible_pixel(&pixels));
        assert!(has_transparent_pixel(&pixels));

        let mut direct = vec![0; viewport_rgba_buffer_len(options)];
        render_field_viewport_rgba_into(
            &volume,
            sweep_index,
            &FieldName::Phidp,
            options,
            &mut direct,
        )
        .expect("direct u16 viewport differential phase");
        assert_eq!(direct, pixels);
    }

    /// The KTLX 2024 Doppler-cut reflectivity (u8 codes, scale 2, offset
    /// 66, 342 range-folded gates) re-encoded losslessly in every other
    /// `FieldData` type draws the same pixels through the PNG raster, the
    /// viewport raster and the sample cache.
    #[test]
    fn every_storage_type_renders_the_same_physical_values() {
        let expected = golden("ktlx2024.json");
        let sweep_index = as_usize(&expected["doppler_reflectivity"]["sweep"]);
        let path = recast_radar_testdata::require_file!(KTLX_2024);
        let volume = level2(&path);
        let reference = field(&volume, sweep_index, &FieldName::Dbzh);
        let (codes, coding) = u8_codes(reference);
        assert_eq!(icd_scale_offset(&coding), (2.0, 66.0));
        let direct = render_field_image(&volume, sweep_index, &FieldName::Dbzh, raster()).unwrap();
        assert!(has_visible_pixel(direct.as_raw()));
        let options = sample_viewport_options();
        let (_, _, direct_viewport) =
            render_field_viewport_rgba(&volume, sweep_index, &FieldName::Dbzh, options).unwrap();
        assert!(has_visible_pixel(&direct_viewport));
        // The sample cache can leave pixels transparent where the direct
        // render falls through to a second candidate radial (see
        // `viewport_sample_cache_matches_direct_field_render`), so cached
        // renders compare with the u8 field's cached render.
        let direct_cached = {
            let cache = ViewportFieldCache::new(&volume, sweep_index, &FieldName::Dbzh).unwrap();
            let sample_cache = cache.build_sample_cache(&volume, options).unwrap();
            let mut cached = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut cached)
                .unwrap();
            cached
        };

        // Physical dBZ = (code - 66) / 2; codes 0 (undetect) and 1 (range
        // folded) have no value.
        let physical: Vec<Option<f32>> = codes
            .iter()
            .map(|code| (*code >= 2).then(|| (f32::from(*code) - 66.0) / 2.0))
            .collect();
        let range_folded: Vec<bool> = codes.iter().map(|code| *code == 1).collect();
        assert!(range_folded.iter().any(|folded| *folded));
        let variants: Vec<FieldData> = vec![
            // i16 half-dBZ steps with a CF transform and the NEXRAD sentinels.
            FieldData::I16 {
                values: physical
                    .iter()
                    .zip(&range_folded)
                    .map(|(value, folded)| match value {
                        Some(value) => (*value * 2.0) as i16,
                        None if *folded => -32767,
                        None => -32768,
                    })
                    .collect(),
                coding: IntCoding {
                    transform: LinearTransform::CfScaleOffset {
                        scale_factor: 0.5,
                        add_offset: 0.0,
                        attr_width: recast_radar_core::model::FloatWidth::F64,
                    },
                    fill_value: Some(-32768),
                    undetect: None,
                    range_folded: Some(-32767),
                    valid_range: None,
                },
            },
            // u16 codes with the ICD transform.
            FieldData::U16 {
                values: codes.iter().map(|code| u16::from(*code)).collect(),
                coding: IntCoding {
                    transform: coding.transform,
                    fill_value: Some(0),
                    undetect: Some(0),
                    range_folded: Some(1),
                    valid_range: Some([2, 255]),
                },
            },
        ];
        for data in variants {
            let dtype = data.dtype();
            let mut edited = volume.clone();
            let sweep = &mut edited.sweeps[sweep_index];
            let index = sweep.field_index(&FieldName::Dbzh).unwrap();
            sweep.fields[index].data = data;
            sweep.seal().unwrap();
            let image =
                render_field_image(&edited, sweep_index, &FieldName::Dbzh, raster()).unwrap();
            assert!(image.as_raw() == direct.as_raw(), "{dtype} PNG raster");
            let cache = ViewportFieldCache::new(&edited, sweep_index, &FieldName::Dbzh).unwrap();
            let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_into(&edited, options, &mut pixels)
                .unwrap();
            assert!(pixels == direct_viewport, "{dtype} viewport raster");
            let sample_cache = cache.build_sample_cache(&edited, options).unwrap();
            let mut cached = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_with_sample_cache(&edited, &sample_cache, &mut cached)
                .unwrap();
            assert!(cached == direct_cached, "{dtype} sample cache");
        }

        // Float storage has no range-folded code, and 32-bit integer codes
        // render through the float path, which blanks every sentinel: those
        // gates blank, so these variants match the u8 field with range folding
        // blanked too.
        let mut blanked = volume.clone();
        {
            let sweep = &mut blanked.sweeps[sweep_index];
            let index = sweep.field_index(&FieldName::Dbzh).unwrap();
            let FieldData::U8 { values, .. } = &mut sweep.fields[index].data else {
                unreachable!()
            };
            for code in values.iter_mut().filter(|code| **code == 1) {
                *code = 0;
            }
        }
        let expected_image =
            render_field_image(&blanked, sweep_index, &FieldName::Dbzh, raster()).unwrap();
        let (_, _, expected_viewport) =
            render_field_viewport_rgba(&blanked, sweep_index, &FieldName::Dbzh, options).unwrap();
        let floats: Vec<FieldData> = vec![
            // i32 half-dBZ steps with a CF transform (CfRadial `int`).
            FieldData::I32 {
                values: physical
                    .iter()
                    .zip(&range_folded)
                    .map(|(value, folded)| match value {
                        Some(value) => (*value * 2.0) as i32,
                        None if *folded => -2_147_483_647,
                        None => i32::MIN,
                    })
                    .collect(),
                coding: IntCoding {
                    transform: LinearTransform::CfScaleOffset {
                        scale_factor: 0.5,
                        add_offset: 0.0,
                        attr_width: recast_radar_core::model::FloatWidth::F32,
                    },
                    fill_value: Some(i32::MIN),
                    undetect: None,
                    range_folded: Some(-2_147_483_647),
                    valid_range: None,
                },
            },
            FieldData::F32 {
                values: physical
                    .iter()
                    .map(|value| value.unwrap_or(f32::NAN))
                    .collect(),
                coding: FloatCoding::default(),
            },
            FieldData::F64 {
                values: physical
                    .iter()
                    .map(|value| value.map_or(-9999.0, f64::from))
                    .collect(),
                coding: FloatCoding {
                    transform: None,
                    fill_value: Some(-9999.0),
                    undetect: None,
                },
            },
        ];
        for data in floats {
            let dtype = data.dtype();
            let mut edited = volume.clone();
            let sweep = &mut edited.sweeps[sweep_index];
            let index = sweep.field_index(&FieldName::Dbzh).unwrap();
            sweep.fields[index].data = data;
            sweep.seal().unwrap();
            let image =
                render_field_image(&edited, sweep_index, &FieldName::Dbzh, raster()).unwrap();
            assert!(
                image.as_raw() == expected_image.as_raw(),
                "{dtype} PNG raster"
            );
            let cache = ViewportFieldCache::new(&edited, sweep_index, &FieldName::Dbzh).unwrap();
            let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_into(&edited, options, &mut pixels)
                .unwrap();
            assert!(pixels == expected_viewport, "{dtype} viewport raster");
            let sample_cache = cache.build_sample_cache(&edited, options).unwrap();
            let mut cached = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_with_sample_cache(&edited, &sample_cache, &mut cached)
                .unwrap();
            assert!(has_visible_pixel(&cached), "{dtype} sample cache");
        }
    }

    fn viewport_lookup_reference(
        x: u32,
        y: u32,
        gates: FieldGeometry,
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

        let gate = ((range_m - gates.first_gate_m()) / gates.lookup_spacing_m()).round() as isize;
        if gate < 0 || gate as usize >= gates.gate_count {
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

/// Derived fields from `recast-radar-map` drawn through the viewport cache
/// (moved here from the volumetric tests when the algorithms left this crate).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod derived_product_tests {
    use recast_radar_core::{Field, FieldData, FieldName, Quantity, Volume};
    use recast_radar_map::{ECHO_TOP_THRESHOLD_DBZ, composite_reflectivity, echo_top, vil};
    use serde_json::Value;
    use std::path::Path;

    use crate::color::{ColorTableFamily, ColorTableSet};
    use crate::{ViewportFieldCache, ViewportRasterOptions, viewport_rgba_buffer_len};

    fn level2(path: &Path) -> Volume {
        recast_radar_io_nexrad::read_volume_from_path(path)
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

    fn max_value(field: &Field) -> f32 {
        let (rows, gates) = field.shape();
        (0..rows)
            .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
            .filter_map(|(row, gate)| field.value(row, gate))
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// End-to-end on the full KEWX 2016-04-13 volume (19 sweeps): compute
    /// each derived field and render it through the same ViewportFieldCache
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
        assert_eq!(volume.sweeps.len(), as_usize(&expected["sweeps"]));

        // Py-ART's first sweep: the 0.48 deg surveillance cut.
        let lowest = &expected["lowest_sweep"];
        let first = &volume.sweeps[0];
        let first_field = first.field(&FieldName::Dbzh).expect("first sweep DBZH");
        assert_eq!(first_field.nrays as usize, as_usize(&lowest["rows"]));
        assert_eq!(first_field.ngates as usize, as_usize(&lowest["gates"]));
        let (first_gate_m, spacing_m) = first_field.native_geometry(&first.range).unwrap();
        assert_eq!(first_gate_m as f32, as_f32(&lowest["first_gate_m"]));
        assert_eq!(spacing_m as f32, as_f32(&lowest["gate_spacing_m"]));
        let first_max = max_value(first_field);
        assert_eq!(first_max, as_f32(&lowest["max_dbz"]));

        // The derived fields lie on the reflectivity sweep with the lowest
        // tilt elevation. A Level II sweep's tilt elevation is its first
        // ray's, so among the four 0.48 deg passes of this SAILS volume
        // (sweeps 0, 1, 8 and 9 share the VCP cut angle; first rays at
        // 0.678, 0.527, 0.637 and 0.483 deg) the lowest is sweep 9.
        let (base_index, base) = volume
            .sweeps
            .iter()
            .enumerate()
            .filter(|(_, sweep)| sweep.find(Quantity::Reflectivity).is_some())
            .map(|(index, sweep)| (index, sweep, volume.tilt_elevation_deg(index).unwrap()))
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(index, sweep, _)| (index, sweep))
            .expect("a reflectivity sweep");
        assert_eq!(base_index, 9);
        assert_eq!(base.fixed_angle_deg, volume.sweeps[0].fixed_angle_deg);
        let base_elevation = volume.tilt_elevation_deg(base_index).unwrap();
        assert!(
            base_elevation < volume.tilt_elevation_deg(1).unwrap(),
            "{base_elevation} deg"
        );
        let base_field = base.find(Quantity::Reflectivity).unwrap();
        let (_, gates) = base_field.shape();
        let composite = composite_reflectivity(&volume).expect("composite field");
        assert_eq!(composite.shape(), base_field.shape());
        assert_eq!(composite.gates, base_field.gates);
        assert_eq!(composite.absent_rows, base_field.absent_rows);
        let FieldData::F32 { values, .. } = &composite.data else {
            panic!("derived fields are f32");
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
        let azimuth = base.rays.azimuth_deg[row];
        assert!(
            angular_distance(azimuth, as_f32(&volume_max["azimuth_deg"])) < 1.0,
            "composite peak at {azimuth} deg"
        );
        let (first_gate_m, spacing_m) = composite.native_geometry(&base.range).unwrap();
        let range_m = (first_gate_m + gate as f64 * spacing_m) as f32;
        assert!(
            (range_m - as_f32(&volume_max["ground_range_m"])).abs() < 400.0,
            "composite peak at {range_m} m"
        );

        let echo_top = echo_top(&volume, ECHO_TOP_THRESHOLD_DBZ).expect("echo top field");
        let top = echo_top
            .value(row, gate)
            .expect("echo top over the peak column");
        // The 76.5 dBZ gate itself clears the 18.3 dBZ threshold, so the top
        // is at least its beam height (Py-ART's ray elevation is up to 0.2 deg
        // from the sweep's fixed angle: 250 m at this range).
        assert!(
            top >= as_f32(&volume_max["height_above_radar_m"]) - 250.0,
            "{top} m"
        );
        let vil = vil(&volume).expect("VIL field");
        assert!(vil.value(row, gate).is_some_and(|vil| vil > 0.0));

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
        let range = base.range.clone();
        let cases = [
            (composite, ColorTableFamily::Reflectivity, Some(peak)),
            (echo_top, ColorTableFamily::EchoTops, None),
            (vil, ColorTableFamily::Vil, None),
        ];
        for (field, family, expected_value) in cases {
            let cache = ViewportFieldCache::new_derived(
                &volume, base_index, field, &range, family, &tables,
            )
            .expect("derived cache");
            let mut pixels = vec![0u8; viewport_rgba_buffer_len(opts)];
            cache
                .render_field_rgba_into(&volume, opts, &mut pixels)
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
