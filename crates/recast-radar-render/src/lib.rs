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
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

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
    #[error("velocity dealiasing failed: {0}")]
    Dealias(String),
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
/// Transitional: the dealiaser is `recast-radar-correct`'s pre-FM301
/// `dealias_velocity_grid`, run through the legacy shim until that crate
/// migrates; the result is identical.
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
    legacy_bridge::dealias_velocity(volume, sweep_index, view.field).map_err(RenderError::Dealias)
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
    F32(&'a [f32], FloatCoding<f32>),
    F64(&'a [f64], FloatCoding<f64>),
}

fn field_values(field: &Field) -> FieldValues<'_> {
    match &field.data {
        FieldData::U8 { values, coding } => FieldValues::U8(values, *coding),
        FieldData::I8 { values, coding } => FieldValues::I8(values, *coding),
        FieldData::U16 { values, coding } => FieldValues::U16(values, *coding),
        FieldData::I16 { values, coding } => FieldValues::I16(values, *coding),
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

/// Float storage: a stored value's finite physical value, `None` for NaN, the
/// coding's fill and undetect values, and non-finite results.
trait FloatCode: Copy + Sync + Send {
    fn physical(self, coding: &FloatCoding<Self>) -> Option<f32>;
}

impl FloatCode for f32 {
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
            FieldValues::F32(..) | FieldValues::F64(..) => Self::Float { color_table, dtype },
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
    coding: FloatCoding<T>,
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
    coding: FloatCoding<T>,
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
    coding: FloatCoding<T>,
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
    coding: FloatCoding<T>,
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
    coding: FloatCoding<T>,
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
    coding: FloatCoding<T>,
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
    coding: &FloatCoding<T>,
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
    fn float_extent<T: FloatCode>(row: Option<&[T]>, coding: FloatCoding<T>) -> usize {
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

/// Velocity dealiasing through `recast-radar-correct`'s pre-FM301 API, until
/// that crate's FM301 migration lands. The only place this crate names legacy
/// model types (docs/design/fm301-model.md section 13.3).
#[allow(deprecated)]
mod legacy_bridge {
    use recast_radar_core::legacy::{self, LegacyConvention};
    use recast_radar_core::{
        Field, FieldData, FieldName, FloatCoding, IntCoding, LinearTransform, MomentGrid,
        MomentStorage, Volume,
    };

    /// Region-based dealiasing (`recast_radar_correct::dealias_velocity_grid`)
    /// of `velocity`, a field of sweep `sweep_index`. Returns `VRADDH` on the
    /// same rays, gates and gate mapping.
    pub(crate) fn dealias_velocity(
        volume: &Volume,
        sweep_index: usize,
        velocity: &Field,
    ) -> Result<Field, String> {
        let sweep = volume
            .sweeps
            .get(sweep_index)
            .ok_or_else(|| format!("no sweep {sweep_index}"))?;
        // A one-sweep, one-field volume, so only the velocity buffer is copied.
        let mut single =
            recast_radar_core::Sweep::new(0, sweep.sweep_mode.clone(), sweep.fixed_angle_deg);
        single.elevation_number = sweep.elevation_number;
        single.rays = sweep.rays.clone();
        single.range = sweep.range.clone();
        single.ray_vars = sweep.ray_vars.clone();
        single.fields.push(velocity.clone());
        let mut scratch = Volume::new(volume.attrs.instrument_name.clone(), volume.time_reference);
        scratch.provenance.source_format = volume.provenance.source_format;
        scratch.sweeps.push(single);
        let convention = LegacyConvention::from(volume.provenance.source_format);
        let legacy_volume =
            legacy::legacy_from_volume(scratch, None, convention).map_err(|err| err.to_string())?;
        let cut = legacy_volume
            .cuts
            .first()
            .ok_or("velocity sweep did not convert")?;
        let grid = cut
            .moments
            .values()
            .next()
            .ok_or("velocity field did not convert")?;
        let dealiased = recast_radar_correct::dealias_velocity_grid(cut, grid);
        field_from_dealiased(dealiased, velocity)
    }

    fn field_from_dealiased(grid: MomentGrid, source: &Field) -> Result<Field, String> {
        let MomentGrid {
            gate_range,
            scale,
            offset,
            nodata,
            range_folded,
            radial_indices,
            storage,
            ..
        } = grid;
        if gate_range.gate_count != source.ngates as usize
            || radial_indices.len() != source.nrays as usize
        {
            return Err(format!(
                "dealiased grid is {} x {}, source field is {} x {}",
                radial_indices.len(),
                gate_range.gate_count,
                source.nrays,
                source.ngates
            ));
        }
        let transform = LinearTransform::IcdScaleOffset { scale, offset };
        let data = match storage {
            MomentStorage::U8(values) => FieldData::U8 {
                values,
                coding: IntCoding {
                    transform,
                    fill_value: nodata.and_then(|code| u8::try_from(code).ok()),
                    undetect: None,
                    range_folded: range_folded.and_then(|code| u8::try_from(code).ok()),
                    valid_range: None,
                },
            },
            MomentStorage::U16(values) => FieldData::U16 {
                values,
                coding: IntCoding {
                    transform,
                    fill_value: nodata,
                    undetect: None,
                    range_folded,
                    valid_range: None,
                },
            },
            MomentStorage::F32(values) => FieldData::F32 {
                values,
                coding: FloatCoding::default(),
            },
        };
        let mut field = Field::new(FieldName::Vraddh, source.gates, source.ngates, data);
        field.absent_rows = source.absent_rows.clone();
        Ok(field)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use recast_radar_core::{GateMapping, LinearTransform, SweepMode};

    #[test]
    fn base_layer_starts_visible() {
        assert!(RenderLayer::base(FieldName::Dbzh).visible);
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
    fn velocity_range_folded_bins_render_table_rf_color() {
        let volume = test_volume();
        let FieldValues::U8(_, coding) = field_values(field_of(&volume, &FieldName::Vradh)) else {
            panic!("u8 velocity");
        };
        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Velocity);

        assert_eq!(
            color_for_code(&coding, &table.sampler(), 1u8),
            table.range_folded_color()
        );
        assert_eq!(color_for_code(&coding, &table.sampler(), 0u8), [0, 0, 0, 0]);
    }

    #[test]
    fn reflectivity_range_folded_bins_render_table_rf_color() {
        let volume = test_volume();
        let FieldValues::U8(_, coding) = field_values(field_of(&volume, &FieldName::Dbzh)) else {
            panic!("u8 reflectivity");
        };
        let tables = ColorTableSet::default();
        let table = tables.for_family(ColorTableFamily::Reflectivity);

        assert_eq!(
            color_for_code(&coding, &table.sampler(), 1u8),
            table.range_folded_color()
        );
    }

    #[test]
    fn nexrad_coding_blanks_undetect_and_out_of_range_codes() {
        // A natively decoded NEXRAD field: raw 0 is undetect and fill, raw 1
        // range folded, valid_range [2, 255].
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
    fn storm_relative_byte_row_palette_matches_direct_color_math() {
        let volume = test_volume();
        let FieldValues::U8(_, coding) = field_values(field_of(&volume, &FieldName::Vradh)) else {
            panic!("u8 velocity");
        };
        let tables = ColorTableSet::default();
        let color_table = tables.for_family(ColorTableFamily::Velocity);
        let row_motion = [3.25];
        let palettes = build_storm_relative_row_palettes(&coding, &row_motion, color_table);

        for raw in [0u8, 1, 119, 129, 139] {
            assert_eq!(
                palettes[0][usize::from(raw)],
                storm_relative_code_color(&coding, &color_table.sampler(), raw, row_motion[0])
            );
        }
        assert_eq!(palettes[0][0], [0, 0, 0, 0]);
        assert_eq!(palettes[0][1], color_table.range_folded_color());
    }

    #[test]
    fn custom_color_table_feeds_precomputed_byte_palette() {
        let volume = test_volume();
        let FieldValues::U8(_, coding) = field_values(field_of(&volume, &FieldName::Vradh)) else {
            panic!("u8 velocity");
        };
        let table = ColorTable::parse(
            "unit test velocity",
            "units: m/s\ncolor: -20 1 2 3\ncolor: 0 10 20 30\ncolor: 20 40 50 60",
        )
        .expect("custom color table");

        let palette = build_byte_palette(&coding, &table);

        assert_eq!(palette[64], [10, 20, 30, 255]);
        assert_eq!(palette[74], [25, 35, 45, 255]);
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
    fn storm_motion_basis_matches_direct_projection() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let field = field_of(&volume, &FieldName::Vradh);
        let basis = StormMotionBasis::new(sweep, field);
        let storm_motion = StormMotion {
            direction_deg: 225.0,
            speed_mps: 18.0,
        };
        let row_motion = basis.row_motion_components(storm_motion);

        assert_eq!(row_motion.len(), field.nrays as usize);
        for (row, azimuth_deg) in sweep.rays.azimuth_deg.iter().enumerate() {
            let direct = motion_component_away_mps(storm_motion, *azimuth_deg);
            assert!((row_motion[row] - direct).abs() < 0.000_01);
        }
        for (basis, direct) in
            row_motion
                .iter()
                .zip(row_motion_components(sweep, field, storm_motion))
        {
            assert!((basis - direct).abs() < 0.000_01);
        }
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

    #[test]
    fn field_sample_cache_upper_bound_tracks_actual_radar_footprint() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let field = field_of(&volume, &FieldName::Dbzh);
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
        let radar_footprint =
            viewport_sample_cache_storage_upper_bound_for_field(field, &sweep.range, options);

        assert!(radar_footprint < full_viewport);
        assert!(radar_footprint > 1_080 * std::mem::size_of::<CachedRowSpan>());
        let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh).unwrap();
        assert_eq!(
            cache
                .sample_cache_storage_upper_bound(&volume, options)
                .unwrap(),
            radar_footprint
        );
    }

    #[test]
    fn field_geometry_follows_the_gate_mapping_on_the_sweep_range() {
        // KLIX 2005 sweep 2: 1 km reflectivity beside 250 m Doppler moments.
        // The reflectivity keeps its native 1 km gates at their true centres.
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 1.5);
        let reflectivity = sweep.attach_geometry(0.0, 1000.0, 137).unwrap();
        sweep
            .add_field(Field::new(
                FieldName::Dbzh,
                reflectivity,
                137,
                FieldData::U8 {
                    values: vec![70; 137],
                    coding: IntCoding::nexrad(2.0, 66.0),
                },
            ))
            .unwrap();
        let velocity = sweep.attach_geometry(-375.0, 250.0, 548).unwrap();
        sweep
            .add_field(Field::new(
                FieldName::Vradh,
                velocity,
                548,
                FieldData::U8 {
                    values: vec![129; 548],
                    coding: IntCoding::nexrad(2.0, 129.0),
                },
            ))
            .unwrap();
        let dbzh = FieldGeometry::of(&sweep.fields[0], &sweep.range).unwrap();
        assert_eq!(dbzh.first_gate_m(), 0.0);
        assert_eq!(dbzh.lookup_spacing_m(), 1000.0);
        assert_eq!(dbzh.gate_count, 137);
        assert_eq!(dbzh.max_range_m(), 137_000.0);
        let vradh = FieldGeometry::of(&sweep.fields[1], &sweep.range).unwrap();
        assert_eq!(vradh.first_gate_m(), -375.0);
        assert_eq!(vradh.lookup_spacing_m(), 250.0);
        assert_eq!(vradh.max_range_m(), -375.0 + 250.0 * 548.0);
        assert_eq!(
            sweep.fields[0].gates,
            GateMapping {
                start: 0,
                stride: 4
            }
        );
    }

    #[test]
    fn viewport_lookup_matches_reference_hypot_formula() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let view = view_of(&volume, &FieldName::Dbzh);
        let row_lookup = AzimuthLookup::new(sweep, view);
        let max_range_m = view.geometry.max_range_m().max(1.0);
        let max_range_km = max_range_m / 1000.0;
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

        for (x, y) in [(0, 0), (166, 108), (180, 110), (220, 70), (332, 216)] {
            assert_eq!(
                viewport_lookup(x, y, view.geometry, &row_lookup, geometry),
                viewport_lookup_reference(x, y, view.geometry, &row_lookup, geometry)
            );
        }
    }

    #[test]
    fn viewport_lookup_table_matches_reference_hypot_formula() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let view = view_of(&volume, &FieldName::Dbzh);
        let row_lookup = AzimuthLookup::new(sweep, view);
        let geometry = viewport_geometry(
            view.geometry,
            ViewportRasterOptions {
                width: 333,
                height: 217,
                radar_x_px: 166.5,
                radar_y_px: 108.5,
                km_per_px_x: 0.5,
                km_per_px_y: 0.5,
                rotation_rad: 0.0,
            },
        );
        let lookup_table = ViewportLookupTable::new(view.geometry, geometry);

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
                    viewport_lookup_reference(x, y, view.geometry, &row_lookup, geometry),
                    "lookup mismatch at {x},{y}"
                );
            }
        }
    }

    /// The fast table path must agree with `viewport_lookup` (whose
    /// rotation convention is pinned by `rotated_north_pixel_resolves_to_
    /// azimuth_zero`) when a convergence angle is baked in — the table
    /// used to drop the rotation entirely (field-reported skew).
    #[test]
    fn viewport_lookup_table_matches_rotated_viewport_lookup() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let view = view_of(&volume, &FieldName::Dbzh);
        let row_lookup = AzimuthLookup::new(sweep, view);
        for rotation_rad in [-0.21f32, 0.005, 0.35] {
            let geometry = viewport_geometry(
                view.geometry,
                ViewportRasterOptions {
                    width: 333,
                    height: 217,
                    radar_x_px: 166.5,
                    radar_y_px: 108.5,
                    km_per_px_x: 0.5,
                    km_per_px_y: 0.5,
                    rotation_rad,
                },
            );
            let lookup_table = ViewportLookupTable::new(view.geometry, geometry);
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
                        viewport_lookup(x, y, view.geometry, &row_lookup, geometry),
                        "rotated lookup mismatch at {x},{y} (gamma {rotation_rad})"
                    );
                }
            }
        }
    }

    /// Rotation must actually FLOW through the table path: a 0.35 rad
    /// baked convergence has to move some pixels into different azimuth
    /// bins than the unrotated table. The bug was the table silently
    /// ignoring the baked angle, which kept the two identical (so the
    /// parity test above passed at rotation 0 while the screen skewed).
    #[test]
    fn baked_rotation_changes_table_azimuth_bins() {
        // Full-circle 1°-radial sweep: the 4-ray `test_volume` leaves
        // most azimuth bins unfilled, which would no-op this sweep.
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 100.0, 60).unwrap();
        let mut reflectivity = u8_field(FieldName::Dbzh, mapping, 60, 1.0, 0.0);
        for i in 0..360 {
            let ray = sweep.push_ray(0.0, i as f32, 0.5);
            reflectivity.push_row_u8(ray, &[40u8; 60]).unwrap();
        }
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![32.0; 360]);
        sweep.add_field(reflectivity).unwrap();
        sweep.seal().unwrap();
        let view = view_on(&sweep.fields[0], &sweep.range, 0).unwrap();
        let row_lookup = AzimuthLookup::new(&sweep, view);
        let options = |rotation_rad| ViewportRasterOptions {
            width: 96,
            height: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.1,
            km_per_px_y: 0.1,
            rotation_rad,
        };
        let rotated = ViewportLookupTable::new(
            view.geometry,
            viewport_geometry(view.geometry, options(0.35)),
        );
        let straight = ViewportLookupTable::new(
            view.geometry,
            viewport_geometry(view.geometry, options(0.0)),
        );
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
        // 0.35 rad ≈ 20°: against 1° radials nearly every pixel must land
        // in a different radial than the unrotated table.
        assert!(
            moved_bins * 2 > resolved,
            "baked rotation moved only {moved_bins}/{resolved} pixels — rotation is not reaching the table path"
        );
    }

    #[test]
    fn viewport_row_span_covers_reference_samples() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let view = view_of(&volume, &FieldName::Dbzh);
        let row_lookup = AzimuthLookup::new(sweep, view);
        let max_range_m = view.geometry.max_range_m().max(1.0);
        let max_range_km = max_range_m / 1000.0;
        let geometry = ViewportGeometry {
            width: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            max_range_km_sq: max_range_km * max_range_km,
            rot_sin: 0.0,
            rot_cos: 1.0,
        };

        for y in 0..96 {
            let span = geometry.x_range_for_row(y);
            for x in 0..96 {
                if viewport_lookup_reference(x, y, view.geometry, &row_lookup, geometry).is_some() {
                    assert!(
                        span.as_ref().is_some_and(|range| range.contains(&x)),
                        "row span missed reference sample at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn azimuth_lookup_fills_wider_native_radial_sectors() {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 1).unwrap();
        let mut field = u8_field(FieldName::Dbzh, mapping, 1, 1.0, 0.0);
        for index in 0..180 {
            let ray = sweep.push_ray(0.0, index as f32 * 2.0, 0.5);
            field.push_row_u8(ray, &[20]).unwrap();
        }
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();

        let view = view_on(&sweep.fields[0], &sweep.range, 0).unwrap();
        let lookup = AzimuthLookup::new(&sweep, view);
        assert!(lookup.row_for_azimuth(1.0).is_some());
        assert!(lookup.row_for_azimuth(181.0).is_some());
    }

    #[test]
    fn azimuth_lookup_prefers_duplicate_row_with_longer_valid_extent() {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 4).unwrap();
        let mut field = u8_field(FieldName::Dbzh, mapping, 4, 1.0, 0.0);
        for azimuth_deg in [0.0, 0.0, 2.0, 4.0] {
            sweep.push_ray(0.0, azimuth_deg, 0.5);
        }
        field
            .push_row_u8(0, &[20, 0, 0, 0])
            .expect("short duplicate row");
        field
            .push_row_u8(1, &[20, 30, 40, 50])
            .expect("long duplicate row");
        field
            .push_row_u8(2, &[20, 30, 40, 50])
            .expect("neighbor row");
        field
            .push_row_u8(3, &[20, 30, 40, 50])
            .expect("neighbor row");
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();
        let field = &sweep.fields[0];

        let view = view_on(field, &sweep.range, 0).unwrap();
        let lookup = AzimuthLookup::new(&sweep, view);
        assert_eq!(lookup.row_for_azimuth(0.0), Some(1));
        assert_eq!(row_valid_extent(field, 0), 1);
        assert_eq!(row_valid_extent(field, 1), 4);

        let sample = SampleLookup {
            azimuth_bin: azimuth_bin(0.0),
            gate: 3,
        };
        let FieldValues::U8(values, coding) = field_values(field) else {
            panic!("test field should use u8 storage");
        };
        let resolved = resolve_int_sample(values, &coding, view.gate_count(), &lookup, sample)
            .expect("sample should resolve");
        assert_eq!(resolved.row, 1);
        assert_eq!(resolved.gate, 3);
    }

    #[test]
    fn absent_rows_take_no_azimuth_slot_and_never_resolve() {
        // Ray 1 never received a row: its azimuth must not draw, and the
        // fill code the model stored there is not a sample.
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 2).unwrap();
        let mut field = u8_field(FieldName::Dbzh, mapping, 2, 1.0, 0.0);
        for azimuth_deg in [0.0, 90.0, 180.0] {
            sweep.push_ray(0.0, azimuth_deg, 0.5);
        }
        field.push_row_u8(0, &[20, 30]).unwrap();
        field.push_row_u8(2, &[20, 30]).unwrap();
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();
        let field = &sweep.fields[0];
        assert_eq!(field.absent_rows, vec![1]);
        assert!(has_rows(field));

        let view = view_on(field, &sweep.range, 0).unwrap();
        let lookup = AzimuthLookup::new(&sweep, view);
        assert_eq!(lookup.row_for_azimuth(0.0), Some(0));
        assert_eq!(lookup.row_for_azimuth(180.0), Some(2));
        assert_eq!(lookup.row_for_azimuth(90.0), None);

        // A field whose rows are all absent is empty for rendering.
        let mut empty = u8_field(FieldName::Vradh, mapping, 2, 1.0, 64.0);
        empty.push_absent_rows_to(3).unwrap();
        assert!(!has_rows(&empty));
    }

    #[test]
    fn int_sample_resolution_keeps_visible_range_folded_candidates() {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 4).unwrap();
        let mut field = u8_field(FieldName::Vradh, mapping, 4, 1.0, 0.0);
        sweep.push_ray(0.0, 0.0, 0.5);
        field
            .push_row_u8(0, &[1, 1, 1, 1])
            .expect("range-folded row");
        sweep.add_field(field).unwrap();
        sweep.seal().unwrap();
        let field = &sweep.fields[0];

        let view = view_on(field, &sweep.range, 0).unwrap();
        let lookup = AzimuthLookup::new(&sweep, view);
        assert_eq!(row_valid_extent(field, 0), 4);

        let FieldValues::U8(values, coding) = field_values(field) else {
            panic!("test field should use u8 storage");
        };
        let resolved = resolve_int_sample(
            values,
            &coding,
            view.gate_count(),
            &lookup,
            SampleLookup {
                azimuth_bin: azimuth_bin(0.0),
                gate: 3,
            },
        )
        .expect("range-folded sample should resolve");

        assert_eq!(resolved.row, 0);
        assert_eq!(resolved.gate, 3);
    }

    #[test]
    fn viewport_render_uses_requested_screen_resolution() {
        let volume = test_volume();
        let options = ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };

        let reflectivity = render_field_viewport_image(&volume, 0, &FieldName::Dbzh, options)
            .expect("viewport reflectivity");
        assert_eq!(reflectivity.dimensions(), (333, 217));
        assert!(has_visible_pixel(reflectivity.as_raw()));

        let mut reusable_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_field_viewport_rgba_into(
            &volume,
            0,
            &FieldName::Dbzh,
            options,
            &mut reusable_pixels,
        )
        .expect("viewport reflectivity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&reusable_pixels));
        assert!(has_transparent_pixel(&reusable_pixels));

        let reflectivity_cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh)
            .expect("viewport reflectivity cache");
        assert_eq!(reflectivity_cache.field_name(), &FieldName::Dbzh);
        assert_eq!(reflectivity_cache.sweep_index(), 0);
        reusable_pixels.fill(255);
        let dimensions = reflectivity_cache
            .render_field_rgba_into(&volume, options, &mut reusable_pixels)
            .expect("cached viewport reflectivity");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&reusable_pixels));
        assert!(has_transparent_pixel(&reusable_pixels));

        let storm_relative = render_storm_relative_velocity_viewport_image(
            &volume,
            0,
            &FieldName::Vradh,
            StormMotion {
                direction_deg: 45.0,
                speed_mps: 10.0,
            },
            options,
        )
        .expect("viewport storm-relative velocity");
        assert_eq!(storm_relative.dimensions(), (333, 217));
        assert!(has_visible_pixel(storm_relative.as_raw()));

        let mut storm_relative_pixels = vec![255; viewport_rgba_buffer_len(options)];
        let dimensions = render_storm_relative_velocity_viewport_rgba_into(
            &volume,
            0,
            &FieldName::Vradh,
            StormMotion {
                direction_deg: 45.0,
                speed_mps: 10.0,
            },
            options,
            &mut storm_relative_pixels,
        )
        .expect("viewport storm-relative velocity into reusable buffer");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&storm_relative_pixels));
        assert!(has_transparent_pixel(&storm_relative_pixels));

        let velocity_cache = ViewportFieldCache::new(&volume, 0, &FieldName::Vradh)
            .expect("viewport velocity cache");
        storm_relative_pixels.fill(255);
        let dimensions = velocity_cache
            .render_storm_relative_velocity_rgba_into(
                &volume,
                StormMotion {
                    direction_deg: 45.0,
                    speed_mps: 10.0,
                },
                options,
                &mut storm_relative_pixels,
            )
            .expect("cached viewport storm-relative velocity");
        assert_eq!(dimensions, (333, 217));
        assert!(has_visible_pixel(&storm_relative_pixels));
        assert!(has_transparent_pixel(&storm_relative_pixels));
    }

    #[test]
    fn storm_relative_rendering_needs_a_radial_velocity() {
        let volume = test_volume();
        let options = sample_viewport_options();
        let err = render_storm_relative_velocity_viewport_image(
            &volume,
            0,
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
        let Err(err) = ViewportFieldCache::new_dealiased_velocity(&volume, 0, &FieldName::Dbzh)
        else {
            panic!("dealiasing needs a radial velocity");
        };
        assert!(matches!(err, RenderError::NotRadialVelocity { .. }));
        let Err(err) = ViewportFieldCache::new(&volume, 0, &FieldName::Wradh) else {
            panic!("the test sweep has no spectrum width");
        };
        assert!(matches!(
            err,
            RenderError::MissingField {
                sweep_index: 0,
                field: FieldName::Wradh
            }
        ));
        let Err(err) = ViewportFieldCache::new(&volume, 3, &FieldName::Dbzh) else {
            panic!("the test volume has one sweep");
        };
        assert!(matches!(
            err,
            RenderError::SweepOutOfRange {
                index: 3,
                sweep_count: 1
            }
        ));
    }

    #[test]
    fn viewport_sample_cache_matches_direct_field_render() {
        let volume = test_volume();
        let options = ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh)
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
    }

    #[test]
    fn viewport_geometry_cache_resolves_across_compatible_products() {
        let volume = test_volume();
        let options = ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let reflectivity_cache =
            ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh).expect("reflectivity cache");
        let velocity_cache =
            ViewportFieldCache::new(&volume, 0, &FieldName::Vradh).expect("velocity cache");
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
        assert_eq!(geometry_pixels, direct_pixels);
    }

    #[test]
    fn viewport_geometry_cache_rejects_a_different_gate_geometry() {
        // A field with other gates cannot reuse a geometry cache built for
        // the sweep's 1 km reflectivity.
        let mut volume = test_volume();
        let sweep = &mut volume.sweeps[0];
        // 500 m gates from a 250 m centre: their edges line up with the
        // 1 km gates, so the range refines to 500 m.
        let mapping = sweep.attach_geometry(250.0, 500.0, 12).unwrap();
        assert_eq!(
            mapping,
            GateMapping {
                start: 1,
                stride: 1
            }
        );
        assert_eq!(
            sweep.fields[0].gates,
            GateMapping {
                start: 0,
                stride: 2
            }
        );
        let mut fine = u8_field(FieldName::Wradh, mapping, 12, 1.0, 0.0);
        for ray in 0..4 {
            fine.push_row_u8(ray, &[30; 12]).unwrap();
        }
        sweep.add_field(fine).unwrap();
        sweep.seal().unwrap();
        let options = sample_viewport_options();
        let coarse = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh).unwrap();
        let fine = ViewportFieldCache::new(&volume, 0, &FieldName::Wradh).unwrap();
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
        let volume = test_volume();
        let options = ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let storm_motion = StormMotion {
            direction_deg: 45.0,
            speed_mps: 10.0,
        };
        let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Vradh).expect("velocity cache");
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
        let volume = test_volume();
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
            ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh).expect("reflectivity cache");
        let velocity_cache =
            ViewportFieldCache::new(&volume, 0, &FieldName::Vradh).expect("velocity cache");
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
        let volume = test_volume();
        let options = ViewportRasterOptions {
            width: 333,
            height: 217,
            radar_x_px: 166.5,
            radar_y_px: 108.5,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };

        let mut pixels = vec![0; viewport_rgba_buffer_len(options) - 4];
        let err =
            render_field_viewport_rgba_into(&volume, 0, &FieldName::Dbzh, options, &mut pixels)
                .expect_err("wrong buffer size should be rejected");

        assert!(matches!(err, RenderError::BufferSizeMismatch { .. }));
    }

    #[test]
    fn viewport_cache_rejects_different_volume() {
        let volume = test_volume();
        let other_volume = test_volume();
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
    }

    #[test]
    fn viewport_cache_renders_u16_palette_fields() {
        let volume = test_u16_volume();
        let options = ViewportRasterOptions {
            width: 96,
            height: 96,
            radar_x_px: 48.0,
            radar_y_px: 48.0,
            km_per_px_x: 0.5,
            km_per_px_y: 0.5,
            rotation_rad: 0.0,
        };
        let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh)
            .expect("viewport u16 reflectivity cache");
        let mut pixels = vec![255; viewport_rgba_buffer_len(options)];

        let dimensions = cache
            .render_field_rgba_into(&volume, options, &mut pixels)
            .expect("cached u16 viewport reflectivity");

        assert_eq!(dimensions, (96, 96));
        assert!(has_visible_pixel(&pixels));
        assert!(has_transparent_pixel(&pixels));

        // The wide palette covers exactly the codes present.
        let FieldValues::U16(values, coding) = field_values(field_of(&volume, &FieldName::Dbzh))
        else {
            panic!("u16 storage");
        };
        let tables = ColorTableSet::default();
        let palette = build_wide_palette(
            values,
            &coding,
            tables.for_family(ColorTableFamily::Reflectivity),
        );
        assert_eq!(palette.len(), 181);
    }

    #[test]
    fn every_storage_type_renders_the_same_physical_values() {
        // The same 4 x 6 reflectivity plane in every `FieldData` encoding
        // draws identical pixels through the PNG raster, the viewport
        // raster and the sample cache.
        let expected = render_field_image(&test_volume(), 0, &FieldName::Dbzh, raster()).unwrap();
        assert!(has_visible_pixel(expected.as_raw()));
        let options = sample_viewport_options();
        let (_, _, expected_viewport) =
            render_field_viewport_rgba(&test_volume(), 0, &FieldName::Dbzh, options).unwrap();
        assert!(has_visible_pixel(&expected_viewport));

        let rows: [[u8; 6]; 4] = [[20, 30, 40, 50, 60, 70]; 4];
        let physical: Vec<f32> = rows.iter().flatten().map(|raw| f32::from(*raw)).collect();
        let cf = |width| LinearTransform::CfScaleOffset {
            scale_factor: 0.5,
            add_offset: 0.0,
            attr_width: width,
        };
        let variants: Vec<FieldData> = vec![
            FieldData::I8 {
                values: physical.iter().map(|value| *value as i8).collect(),
                coding: IntCoding {
                    transform: LinearTransform::CfScaleOffset {
                        scale_factor: 1.0,
                        add_offset: 0.0,
                        attr_width: recast_radar_core::model::FloatWidth::F32,
                    },
                    fill_value: Some(-128),
                    undetect: None,
                    range_folded: None,
                    valid_range: None,
                },
            },
            FieldData::U16 {
                values: physical.iter().map(|value| (*value * 2.0) as u16).collect(),
                coding: IntCoding {
                    transform: LinearTransform::IcdScaleOffset {
                        scale: 2.0,
                        offset: 0.0,
                    },
                    fill_value: Some(0),
                    undetect: None,
                    range_folded: Some(1),
                    valid_range: None,
                },
            },
            FieldData::I16 {
                values: physical.iter().map(|value| (*value * 2.0) as i16).collect(),
                coding: IntCoding {
                    transform: cf(recast_radar_core::model::FloatWidth::F64),
                    fill_value: Some(-32768),
                    undetect: None,
                    range_folded: None,
                    valid_range: None,
                },
            },
            FieldData::F32 {
                values: physical.clone(),
                coding: FloatCoding::default(),
            },
            FieldData::F64 {
                values: physical.iter().map(|value| f64::from(*value)).collect(),
                coding: FloatCoding {
                    transform: None,
                    fill_value: Some(-9999.0),
                    undetect: None,
                },
            },
        ];
        for data in variants {
            let dtype = data.dtype();
            let mut volume = test_volume();
            let sweep = &mut volume.sweeps[0];
            let mapping = sweep.fields[0].gates;
            sweep.fields.clear();
            sweep
                .add_field(Field::new(FieldName::Dbzh, mapping, 6, data))
                .unwrap();
            sweep.seal().unwrap();
            let image = render_field_image(&volume, 0, &FieldName::Dbzh, raster()).unwrap();
            assert_eq!(image.as_raw(), expected.as_raw(), "{dtype} PNG raster");
            let cache = ViewportFieldCache::new(&volume, 0, &FieldName::Dbzh).unwrap();
            let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_into(&volume, options, &mut pixels)
                .unwrap();
            assert_eq!(pixels, expected_viewport, "{dtype} viewport raster");
            let sample_cache = cache.build_sample_cache(&volume, options).unwrap();
            let mut cached = vec![255; viewport_rgba_buffer_len(options)];
            cache
                .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut cached)
                .unwrap();
            assert_eq!(cached, expected_viewport, "{dtype} sample cache");
        }
    }

    #[test]
    fn derived_and_resampled_fields_render_through_the_cache() {
        let volume = test_volume();
        let sweep = &volume.sweeps[0];
        let tables = ColorTableSet::default();
        let options = sample_viewport_options();

        // A physical copy of the velocity drawn as a derived field.
        let source = field_of(&volume, &FieldName::Vradh);
        let derived = Field::new(
            FieldName::parse("VEL_F32"),
            source.gates,
            source.ngates,
            FieldData::F32 {
                values: source.to_physical(),
                coding: FloatCoding::default(),
            },
        );
        let cache = ViewportFieldCache::new_derived(
            &volume,
            0,
            derived,
            &sweep.range,
            ColorTableFamily::Velocity,
            &tables,
        )
        .unwrap();
        let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
        cache
            .render_field_rgba_into(&volume, options, &mut pixels)
            .unwrap();
        let mut native = vec![255; viewport_rgba_buffer_len(options)];
        ViewportFieldCache::new(&volume, 0, &FieldName::Vradh)
            .unwrap()
            .render_field_rgba_into(&volume, options, &mut native)
            .unwrap();
        // Raw 0 and 1 are blank / range folded in the native field and NaN
        // in the physical copy; the test rows hold neither, so the two agree.
        assert_eq!(pixels, native);

        // A display-resampled field: twice the rows on its own azimuths.
        let mut resampled = Field::new(
            FieldName::parse("DBZH_DISPLAY"),
            GateMapping::IDENTITY,
            6,
            FieldData::U8 {
                values: Vec::new(),
                coding: IntCoding::new(LinearTransform::IcdScaleOffset {
                    scale: 1.0,
                    offset: 0.0,
                }),
            },
        );
        let row_azimuths: Vec<f32> = (0..8).map(|row| row as f32 * 45.0).collect();
        for row in 0..8 {
            resampled
                .push_row_u8(row, &[20, 30, 40, 50, 60, 70])
                .unwrap();
        }
        let cache = ViewportFieldCache::new_resampled(
            &volume,
            0,
            resampled,
            &sweep.range,
            &row_azimuths,
            ColorTableFamily::Reflectivity,
            &tables,
        )
        .unwrap();
        let mut pixels = vec![255; viewport_rgba_buffer_len(options)];
        cache
            .render_field_rgba_into(&volume, options, &mut pixels)
            .unwrap();
        assert!(has_visible_pixel(&pixels));
        assert!(has_transparent_pixel(&pixels));
        let sample_cache = cache.build_sample_cache(&volume, options).unwrap();
        let mut cached = vec![255; viewport_rgba_buffer_len(options)];
        cache
            .render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut cached)
            .unwrap();
        assert_eq!(cached, pixels);
    }

    fn raster() -> RasterOptions {
        RasterOptions {
            width: 96,
            height: 96,
            range_fraction: 94,
        }
    }

    fn has_visible_pixel(pixels: &[u8]) -> bool {
        pixels.chunks_exact(4).any(|pixel| pixel[3] != 0)
    }

    fn has_transparent_pixel(pixels: &[u8]) -> bool {
        pixels.chunks_exact(4).any(|pixel| pixel[3] == 0)
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

    /// A `u8` field with the legacy unit-test coding: `(raw - offset) /
    /// scale`, raw 0 blank, raw 1 range folded.
    fn u8_field(
        name: FieldName,
        gates: GateMapping,
        ngates: u32,
        scale: f32,
        offset: f32,
    ) -> Field {
        Field::new(
            name,
            gates,
            ngates,
            FieldData::U8 {
                values: Vec::new(),
                coding: IntCoding {
                    transform: LinearTransform::IcdScaleOffset { scale, offset },
                    fill_value: Some(0),
                    undetect: None,
                    range_folded: Some(1),
                    valid_range: None,
                },
            },
        )
    }

    fn field_of<'a>(volume: &'a Volume, name: &FieldName) -> &'a Field {
        volume.sweeps[0].field(name).expect("test field")
    }

    fn view_of<'a>(volume: &'a Volume, name: &FieldName) -> FieldView<'a> {
        view_on(field_of(volume, name), &volume.sweeps[0].range, 0).expect("test field geometry")
    }

    /// One sweep at 0.5 deg with four rays (N, E, S, W), 6 gates of 1 km from
    /// 0 m, `u8` reflectivity and velocity.
    fn test_volume() -> Volume {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 6).unwrap();
        for azimuth_deg in [0.0, 90.0, 180.0, 270.0] {
            sweep.push_ray(0.0, azimuth_deg, 0.5);
        }
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![32.0; 4]);

        let mut reflectivity = u8_field(FieldName::Dbzh, mapping, 6, 1.0, 0.0);
        let mut velocity = u8_field(FieldName::Vradh, mapping, 6, 1.0, 64.0);
        for ray in 0..4 {
            reflectivity
                .push_row_u8(ray, &[20, 30, 40, 50, 60, 70])
                .expect("reflectivity row");
            velocity
                .push_row_u8(ray, &[44, 54, 64, 74, 84, 94])
                .expect("velocity row");
        }
        sweep.add_field(reflectivity).unwrap();
        sweep.add_field(velocity).unwrap();

        let mut volume = Volume::new("TST", chrono::Utc::now());
        volume.sweeps.push(sweep);
        volume.seal().unwrap();
        volume
    }

    fn test_u16_volume() -> Volume {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        sweep.elevation_number = Some(1);
        let mapping = sweep.attach_geometry(0.0, 1_000.0, 6).unwrap();
        for azimuth_deg in [0.0, 90.0, 180.0, 270.0] {
            sweep.push_ray(0.0, azimuth_deg, 0.5);
        }

        let mut reflectivity = Field::new(
            FieldName::Dbzh,
            mapping,
            6,
            FieldData::U16 {
                values: Vec::new(),
                coding: IntCoding {
                    transform: LinearTransform::IcdScaleOffset {
                        scale: 2.0,
                        offset: 64.0,
                    },
                    fill_value: Some(0),
                    undetect: None,
                    range_folded: Some(1),
                    valid_range: None,
                },
            },
        );
        for ray in 0..4 {
            let row: Vec<u8> = [80u16, 100, 120, 140, 160, 180]
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect();
            reflectivity
                .push_row_u16_be(ray, &row)
                .expect("u16 reflectivity row");
        }
        sweep.add_field(reflectivity).unwrap();

        let mut volume = Volume::new("U16", chrono::Utc::now());
        volume.sweeps.push(sweep);
        volume.seal().unwrap();
        volume
    }
}

/// Derived grids from `recast-radar-map` drawn through the viewport cache.
/// `recast-radar-map` still produces legacy `MomentGrid`s, so this test
/// bridges them into fields; it moves to the native API when `map` migrates.
#[cfg(test)]
#[allow(deprecated, clippy::unwrap_used, clippy::expect_used)]
mod legacy_bridge_tests {
    use recast_radar_core::legacy::{self, LegacyConvention};
    use recast_radar_core::{
        ElevationCut, GateRange, MomentGrid, MomentStorage, MomentType, RadarVolume, Radial, Volume,
    };
    use recast_radar_map::{
        ECHO_TOP_THRESHOLD_DBZ, composite_reflectivity_grid, echo_top_grid, vil_grid,
    };

    fn cut_with_ref(elev: f32, az_count: usize, gates: usize, dbz: f32) -> ElevationCut {
        let gate_range = GateRange {
            first_gate_m: 0,
            gate_spacing_m: 1_000,
            gate_count: gates,
        };
        let mut cut = ElevationCut::new(elev, None);
        for k in 0..az_count {
            cut.radials.push(Radial {
                azimuth_deg: k as f32 * (360.0 / az_count as f32),
                elevation_deg: elev,
                time_offset_ms: 0,
                gate_range: gate_range.clone(),
                nyquist_velocity_mps: None,
                radial_status: None,
            });
        }
        let grid = MomentGrid {
            moment: MomentType::Reflectivity,
            gate_range,
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices: (0..az_count).collect(),
            storage: MomentStorage::F32(vec![dbz; az_count * gates]),
        };
        cut.moments.insert(MomentType::Reflectivity, grid);
        cut
    }

    fn volume_with(cuts: Vec<ElevationCut>) -> RadarVolume {
        RadarVolume {
            cuts,
            ..Default::default()
        }
    }

    #[test]
    fn derived_products_render_through_viewport_cache() {
        // End-to-end: compute each derived grid and render it through the same
        // ViewportFieldCache path the GUI worker uses, with its dedicated
        // color family. Asserts the render produces opaque pixels (no panic,
        // correct plumbing).
        use crate::color::{ColorTableFamily, ColorTableSet};
        use crate::{ViewportFieldCache, ViewportRasterOptions, viewport_rgba_buffer_len};

        let legacy_volume = volume_with(vec![
            cut_with_ref(0.5, 360, 120, 45.0),
            cut_with_ref(3.0, 360, 120, 50.0),
        ]);
        let cases = [
            (
                composite_reflectivity_grid(&legacy_volume),
                ColorTableFamily::Reflectivity,
            ),
            (
                echo_top_grid(&legacy_volume, ECHO_TOP_THRESHOLD_DBZ),
                ColorTableFamily::EchoTops,
            ),
            (vil_grid(&legacy_volume), ColorTableFamily::Vil),
        ];
        let v = Volume::try_from(legacy_volume).expect("FM301 volume");
        let tables = ColorTableSet::default();
        let opts = ViewportRasterOptions {
            width: 256,
            height: 256,
            radar_x_px: 128.0,
            radar_y_px: 128.0,
            km_per_px_x: 1.0,
            km_per_px_y: 1.0,
            rotation_rad: 0.0,
        };
        for (grid, family) in cases {
            let grid = grid.expect("derived grid");
            // The derived field lies on sweep 0's rays and range.
            let mut scratch = v.sweeps[0].clone();
            scratch.fields.clear();
            let (index, _) =
                legacy::field_from_grid(&grid, &mut scratch, LegacyConvention::Generic)
                    .expect("derived field");
            let field = scratch.fields.swap_remove(index);
            let cache =
                ViewportFieldCache::new_derived(&v, 0, field, &scratch.range, family, &tables)
                    .expect("derived cache");
            let mut pixels = vec![0u8; viewport_rgba_buffer_len(opts)];
            cache
                .render_field_rgba_into(&v, opts, &mut pixels)
                .expect("render");
            assert!(
                pixels.chunks_exact(4).any(|p| p[3] > 0),
                "{family:?} derived product rendered no opaque pixels"
            );
        }
    }
}
