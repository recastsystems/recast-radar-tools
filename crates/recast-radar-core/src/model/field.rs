//! Dataset variables: compact row-major storage with CF packing
//! (`docs/design/fm301-model.md` sections 4 and 7).

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::names::{FieldName, Polarization, Quantity};
use super::sweep::RangeCoord;
use super::values::{ArrayBuf, AttrValue, Scalar};

/// One dataset variable of a sweep: `<name>(time, range)` in FM301.
///
/// Values are stored row-major `[nrays × ngates]` in the source's encoding and
/// in the field's native gate geometry. Row `r` belongs to ray `r` of the sweep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Variable name in the sweep group (section 8).
    pub name: FieldName,
    /// Semantic class regardless of spelling (DBZH, DBZ, DBZHC_F all ->
    /// Reflectivity).
    pub quantity: Quantity,
    pub polarization: Polarization,
    /// CF / FM301 attributes: `standard_name`, `long_name`, `units`, Table
    /// 301-10.
    pub attrs: FieldAttrs,
    /// Rows. Equal to the sweep's ray count once the sweep is sealed.
    pub nrays: u32,
    /// Native gates per row.
    pub ngates: u32,
    /// Where the native gates sit on the sweep's `range` coordinate (section 6).
    pub gates: GateMapping,
    /// Row-major `[nrays × ngates]` values in the source encoding (section 7).
    pub data: FieldData,
    /// Rows the source did not provide for this field, ascending. Each is filled
    /// with the coding's fill code (NaN for floats without one). Empty in the
    /// common case, which costs no allocation.
    pub absent_rows: Vec<u32>,
}

/// Native gate `i` covers sweep-range gates
/// `start + i*stride ..= start + i*stride + stride - 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GateMapping {
    pub start: u32,
    /// 1: native spacing equals the sweep range spacing. 4: 1 km gates over a
    /// 250 m range.
    pub stride: u32,
}

impl GateMapping {
    /// `start = 0`, `stride = 1`.
    pub const IDENTITY: GateMapping = GateMapping {
        start: 0,
        stride: 1,
    };

    /// One past the last sweep-range gate covered by `ngates` native gates.
    pub fn end(self, ngates: u32) -> Option<u64> {
        u64::from(self.start).checked_add(u64::from(ngates) * u64::from(self.stride.max(1)))
    }
}

impl Default for GateMapping {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Field values in their source encoding with their coding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldData {
    U8 {
        values: Vec<u8>,
        coding: IntCoding<u8>,
    },
    U16 {
        values: Vec<u16>,
        coding: IntCoding<u16>,
    },
    I8 {
        values: Vec<i8>,
        coding: IntCoding<i8>,
    },
    I16 {
        values: Vec<i16>,
        coding: IntCoding<i16>,
    },
    /// Physical values (derived products, float32 sources), stored verbatim.
    F32 {
        values: Vec<f32>,
        coding: FloatCoding<f32>,
    },
    /// float64 sources (ODIM float64 planes, CfRadial `double` fields), stored
    /// verbatim.
    F64 {
        values: Vec<f64>,
        coding: FloatCoding<f64>,
    },
}

/// Coding of an integer field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntCoding<T> {
    pub transform: LinearTransform,
    /// CF `_FillValue`: the code for "no data" and the code the view pads with.
    /// NEXRAD 0, ODIM `nodata`, CfRadial `_FillValue`.
    pub fill_value: Option<T>,
    /// FM301 Table 301-10 `_Undetect`: radiated, but no valid echo. NEXRAD 0
    /// (below threshold, equal to `fill_value`), ODIM `undetect`.
    pub undetect: Option<T>,
    /// NEXRAD 1. Exported as `flag_values = [1]`, `flag_meanings =
    /// "range_folded"`.
    pub range_folded: Option<T>,
    /// CF `valid_range` in packed units (WMO-CF.5.2.15).
    pub valid_range: Option<[T; 2]>,
}

impl<T: PackedInt> IntCoding<T> {
    /// A coding with `transform` and no sentinels.
    pub fn new(transform: LinearTransform) -> Self {
        Self {
            transform,
            fill_value: None,
            undetect: None,
            range_folded: None,
            valid_range: None,
        }
    }

    /// NEXRAD Level II moment coding (section 7.1): ICD `value = (raw -
    /// offset) / scale`, raw 0 below threshold (`_FillValue` and `_Undetect`),
    /// raw 1 range folded, `valid_range = [2, MAX]`.
    pub fn nexrad(scale: f32, offset: f32) -> Self {
        Self {
            transform: LinearTransform::IcdScaleOffset { scale, offset },
            fill_value: Some(T::from_u8(0)),
            undetect: Some(T::from_u8(0)),
            range_folded: Some(T::from_u8(1)),
            valid_range: Some([T::from_u8(2), T::MAX]),
        }
    }

    /// The code used to fill padding and absent rows: `fill_value`, else 0.
    pub fn fill_code(&self) -> T {
        self.fill_value.unwrap_or(T::from_u8(0))
    }

    /// Resolve one raw code (section 4, `Field::gate` rules 2 to 6).
    pub fn resolve(&self, raw: T) -> Gate {
        if self.undetect == Some(raw) {
            Gate::Undetect
        } else if self.fill_value == Some(raw) {
            Gate::Missing
        } else if self.range_folded == Some(raw) {
            Gate::RangeFolded
        } else if matches!(self.valid_range, Some([lo, hi]) if raw < lo || raw > hi) {
            Gate::Missing
        } else {
            Gate::Value(self.transform.apply(raw.as_f64()))
        }
    }
}

/// A linear transform from packed to physical values.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinearTransform {
    /// `physical = (raw - offset) / scale`, evaluated in f32. NEXRAD ICD form;
    /// Py-ART evaluates exactly this expression. The
    /// view writes `scale_factor = 1/scale` and `add_offset = -offset/scale` as
    /// float64, as xradar does.
    IcdScaleOffset { scale: f32, offset: f32 },
    /// `physical = raw * scale_factor + add_offset`, evaluated in f64. CF and
    /// ODIM gain-offset form. `attr_width` is the type the source wrote the two
    /// attributes in; xarray derives the decoded dtype from it.
    CfScaleOffset {
        scale_factor: f64,
        add_offset: f64,
        attr_width: FloatWidth,
    },
}

impl LinearTransform {
    /// Apply to a raw value. Integer codes and f32 values are exact in f64.
    #[inline]
    pub fn apply(self, raw: f64) -> f32 {
        match self {
            Self::IcdScaleOffset { scale, offset } => (raw as f32 - offset) / scale,
            Self::CfScaleOffset {
                scale_factor,
                add_offset,
                ..
            } => (raw * scale_factor + add_offset) as f32,
        }
    }

    /// CF `scale_factor`.
    pub fn scale_factor(self) -> f64 {
        match self {
            Self::IcdScaleOffset { scale, .. } => 1.0 / f64::from(scale),
            Self::CfScaleOffset { scale_factor, .. } => scale_factor,
        }
    }

    /// CF `add_offset`.
    pub fn add_offset(self) -> f64 {
        match self {
            Self::IcdScaleOffset { scale, offset } => -f64::from(offset) / f64::from(scale),
            Self::CfScaleOffset { add_offset, .. } => add_offset,
        }
    }

    /// The type CF `scale_factor` / `add_offset` attributes are written in.
    pub fn attr_width(self) -> FloatWidth {
        match self {
            Self::IcdScaleOffset { .. } => FloatWidth::F64,
            Self::CfScaleOffset { attr_width, .. } => attr_width,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatWidth {
    F32,
    F64,
}

/// Coding of a float field. Values are stored as the source wrote them. NaN is
/// always missing. A non-NaN source fill (for example -9999) stays in the data
/// and resolves to `Missing`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloatCoding<T> {
    /// `None`: the values are physical. `Some`: a plane whose gain / offset is
    /// not 1 / 0, applied on read.
    pub transform: Option<LinearTransform>,
    pub fill_value: Option<T>,
    pub undetect: Option<T>,
}

/// Integer types a field can be packed in.
pub trait PackedInt: Copy + PartialEq + PartialOrd + std::fmt::Debug + private::Sealed {
    const MAX: Self;
    const DTYPE: &'static str;
    fn from_u8(value: u8) -> Self;
    fn as_f64(self) -> f64;
    fn to_scalar(self) -> Scalar;
    /// A typed array of this integer type.
    fn array(values: Vec<Self>) -> ArrayBuf;
    /// Narrow an `i64`, `None` when it does not fit.
    fn from_i64(value: i64) -> Option<Self>;
}

mod private {
    pub trait Sealed {}
    impl Sealed for u8 {}
    impl Sealed for u16 {}
    impl Sealed for i8 {}
    impl Sealed for i16 {}
}

macro_rules! packed_int {
    ($t:ty, $variant:ident, $dtype:literal) => {
        impl PackedInt for $t {
            const MAX: Self = <$t>::MAX;
            const DTYPE: &'static str = $dtype;
            #[inline]
            fn from_u8(value: u8) -> Self {
                value as $t
            }
            #[inline]
            fn as_f64(self) -> f64 {
                f64::from(self)
            }
            fn to_scalar(self) -> Scalar {
                Scalar::$variant(self)
            }
            fn array(values: Vec<Self>) -> ArrayBuf {
                ArrayBuf::$variant(values)
            }
            fn from_i64(value: i64) -> Option<Self> {
                <$t>::try_from(value).ok()
            }
        }
    };
}

packed_int!(u8, U8, "uint8");
packed_int!(u16, U16, "uint16");
packed_int!(i8, I8, "int8");
packed_int!(i16, I16, "int16");

fn resolve_float(
    raw: f64,
    transform: Option<LinearTransform>,
    is_fill: bool,
    is_undetect: bool,
) -> Gate {
    if raw.is_nan() {
        Gate::Missing
    } else if is_undetect {
        Gate::Undetect
    } else if is_fill {
        Gate::Missing
    } else {
        Gate::Value(match transform {
            Some(transform) => transform.apply(raw),
            None => raw as f32,
        })
    }
}

impl FloatCoding<f32> {
    pub fn resolve(&self, raw: f32) -> Gate {
        resolve_float(
            f64::from(raw),
            self.transform,
            self.fill_value
                .is_some_and(|fill| fill.to_bits() == raw.to_bits()),
            self.undetect
                .is_some_and(|undetect| undetect.to_bits() == raw.to_bits()),
        )
    }

    /// The code used to fill padding and absent rows: `fill_value`, else NaN.
    pub fn fill_code(&self) -> f32 {
        self.fill_value.unwrap_or(f32::NAN)
    }
}

impl FloatCoding<f64> {
    pub fn resolve(&self, raw: f64) -> Gate {
        resolve_float(
            raw,
            self.transform,
            self.fill_value
                .is_some_and(|fill| fill.to_bits() == raw.to_bits()),
            self.undetect
                .is_some_and(|undetect| undetect.to_bits() == raw.to_bits()),
        )
    }

    /// The code used to fill padding and absent rows: `fill_value`, else NaN.
    pub fn fill_code(&self) -> f64 {
        self.fill_value.unwrap_or(f64::NAN)
    }
}

/// CF and FM301 attributes of a field. `None` / empty means "not stated by the
/// source"; the FM301 view fills `standard_name`, `long_name` and `units` of
/// known names from the name table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldAttrs {
    pub standard_name: Option<Cow<'static, str>>,
    pub long_name: Option<Cow<'static, str>>,
    pub units: Option<Cow<'static, str>>,
    // Table 301-10, all optional
    pub sampling_ratio: Option<f32>,
    pub is_discrete: Option<bool>,
    pub field_folds: Option<bool>,
    pub fold_limit_lower: Option<f32>,
    pub fold_limit_upper: Option<f32>,
    pub is_quality_field: Option<bool>,
    pub qualified_variables: Vec<FieldName>,
    pub ancillary_variables: Vec<FieldName>,
    pub thresholding_xml: Option<String>,
    /// `flag_values`, `flag_masks` and `flag_meanings` of discrete fields,
    /// besides the range-folded flag, which comes from the coding. The view
    /// writes values and masks in the variable's packed type.
    pub flag_values: Vec<i64>,
    pub flag_masks: Vec<i64>,
    pub flag_meanings: Vec<Box<str>>,
    /// Source attributes with no slot above, verbatim, typed and in file order
    /// (for example CfRadial `grid_mapping`).
    pub other: Vec<(Box<str>, AttrValue)>,
}

/// One gate's value with its sentinel resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gate {
    Value(f32),
    Missing,
    Undetect,
    RangeFolded,
}

impl Gate {
    pub fn value(self) -> Option<f32> {
        match self {
            Self::Value(value) => Some(value),
            _ => None,
        }
    }
}

/// A borrowed row of a field in its storage type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowRef<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    I8(&'a [i8]),
    I16(&'a [i16]),
    F32(&'a [f32]),
    F64(&'a [f64]),
}

/// A field moved apart into its components (bindings hand `data` to NumPy
/// without copying).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldParts {
    pub name: FieldName,
    pub quantity: Quantity,
    pub polarization: Polarization,
    pub attrs: FieldAttrs,
    pub nrays: u32,
    pub ngates: u32,
    pub gates: GateMapping,
    pub data: FieldData,
    pub absent_rows: Vec<u32>,
}

/// A field's coding, detached from its buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Coding {
    U8(IntCoding<u8>),
    U16(IntCoding<u16>),
    I8(IntCoding<i8>),
    I16(IntCoding<i16>),
    F32(FloatCoding<f32>),
    F64(FloatCoding<f64>),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum FieldError {
    #[error("field storage is {expected}, row is {actual}")]
    StorageMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("row for ray {ray} pushed after {rows} rows (rows must be pushed in ray order)")]
    RowOrder { ray: usize, rows: usize },
    #[error("big-endian 16-bit row has odd byte length {byte_len}")]
    InvalidRowByteLength { byte_len: usize },
    #[error("field dimensions exceed u32 or addressable memory")]
    TooLarge,
    #[error("field has {rows} rows, more than the sweep's {nrays} rays")]
    TooManyRows { rows: usize, nrays: usize },
}

impl FieldData {
    /// Number of stored values (`nrays × ngates`).
    pub fn len(&self) -> usize {
        match self {
            Self::U8 { values, .. } => values.len(),
            Self::U16 { values, .. } => values.len(),
            Self::I8 { values, .. } => values.len(),
            Self::I16 { values, .. } => values.len(),
            Self::F32 { values, .. } => values.len(),
            Self::F64 { values, .. } => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// NumPy-style dtype name of the storage.
    pub fn dtype(&self) -> &'static str {
        match self {
            Self::U8 { .. } => "uint8",
            Self::U16 { .. } => "uint16",
            Self::I8 { .. } => "int8",
            Self::I16 { .. } => "int16",
            Self::F32 { .. } => "float32",
            Self::F64 { .. } => "float64",
        }
    }

    /// The fill code in the storage type (`_FillValue`, else 0 or NaN).
    pub fn fill_scalar(&self) -> Scalar {
        match self {
            Self::U8 { coding, .. } => Scalar::U8(coding.fill_code()),
            Self::U16 { coding, .. } => Scalar::U16(coding.fill_code()),
            Self::I8 { coding, .. } => Scalar::I8(coding.fill_code()),
            Self::I16 { coding, .. } => Scalar::I16(coding.fill_code()),
            Self::F32 { coding, .. } => Scalar::F32(coding.fill_code()),
            Self::F64 { coding, .. } => Scalar::F64(coding.fill_code()),
        }
    }

    /// The coding without the buffer.
    pub fn coding(&self) -> Coding {
        match self {
            Self::U8 { coding, .. } => Coding::U8(*coding),
            Self::U16 { coding, .. } => Coding::U16(*coding),
            Self::I8 { coding, .. } => Coding::I8(*coding),
            Self::I16 { coding, .. } => Coding::I16(*coding),
            Self::F32 { coding, .. } => Coding::F32(*coding),
            Self::F64 { coding, .. } => Coding::F64(*coding),
        }
    }

    /// The linear transform, if the coding has one.
    pub fn transform(&self) -> Option<LinearTransform> {
        match self {
            Self::U8 { coding, .. } => Some(coding.transform),
            Self::U16 { coding, .. } => Some(coding.transform),
            Self::I8 { coding, .. } => Some(coding.transform),
            Self::I16 { coding, .. } => Some(coding.transform),
            Self::F32 { coding, .. } => coding.transform,
            Self::F64 { coding, .. } => coding.transform,
        }
    }

    /// The buffer, moved, with its coding. No copy.
    pub fn into_array(self) -> (ArrayBuf, Coding) {
        match self {
            Self::U8 { values, coding } => (ArrayBuf::U8(values), Coding::U8(coding)),
            Self::U16 { values, coding } => (ArrayBuf::U16(values), Coding::U16(coding)),
            Self::I8 { values, coding } => (ArrayBuf::I8(values), Coding::I8(coding)),
            Self::I16 { values, coding } => (ArrayBuf::I16(values), Coding::I16(coding)),
            Self::F32 { values, coding } => (ArrayBuf::F32(values), Coding::F32(coding)),
            Self::F64 { values, coding } => (ArrayBuf::F64(values), Coding::F64(coding)),
        }
    }
}

/// Row bookkeeping shared by the typed push paths.
struct Rows<'a> {
    nrays: &'a mut u32,
    ngates: &'a mut u32,
    absent_rows: &'a mut Vec<u32>,
}

impl Rows<'_> {
    /// Prepare `values` for a row of `row_len` values for ray `ray`: check the
    /// order, widen existing rows if the row is longer, and append absent rows
    /// for skipped rays. Returns the gate count after widening.
    fn prepare<T: Copy>(
        &mut self,
        values: &mut Vec<T>,
        ray: usize,
        row_len: usize,
        fill: T,
    ) -> Result<usize, FieldError> {
        let rows = *self.nrays as usize;
        if ray < rows {
            return Err(FieldError::RowOrder { ray, rows });
        }
        u32::try_from(ray.checked_add(1).ok_or(FieldError::TooLarge)?)
            .map_err(|_| FieldError::TooLarge)?;
        let mut ngates = *self.ngates as usize;
        if row_len > ngates {
            let widened = u32::try_from(row_len).map_err(|_| FieldError::TooLarge)?;
            if rows > 0 && ngates > 0 {
                *values = widen_rows(values, rows, ngates, row_len, fill)?;
            } else if rows > 0 {
                values.clear();
                values.resize(rows.checked_mul(row_len).ok_or(FieldError::TooLarge)?, fill);
            }
            *self.ngates = widened;
            ngates = row_len;
        }
        if ray > rows {
            let extra = (ray - rows)
                .checked_mul(ngates)
                .ok_or(FieldError::TooLarge)?;
            values.reserve(extra);
            values.resize(values.len() + extra, fill);
            self.absent_rows.extend((rows..ray).map(|row| row as u32));
        }
        Ok(ngates)
    }

    fn finish<T: Copy>(
        &mut self,
        values: &mut Vec<T>,
        row_start: usize,
        ngates: usize,
        ray: usize,
        fill: T,
    ) {
        values.resize(row_start + ngates, fill);
        *self.nrays = (ray + 1) as u32;
    }
}

fn widen_rows<T: Copy>(
    values: &[T],
    rows: usize,
    old_gates: usize,
    new_gates: usize,
    fill: T,
) -> Result<Vec<T>, FieldError> {
    let mut widened = Vec::new();
    widened
        .try_reserve_exact(rows.checked_mul(new_gates).ok_or(FieldError::TooLarge)?)
        .map_err(|_| FieldError::TooLarge)?;
    for row in values.chunks(old_gates).take(rows) {
        widened.extend_from_slice(row);
        widened.resize(widened.len() + (new_gates - row.len()), fill);
    }
    Ok(widened)
}

macro_rules! push_slice_row {
    ($(#[$doc:meta])* $fn_name:ident, $variant:ident, $t:ty) => {
        $(#[$doc])*
        pub fn $fn_name(&mut self, ray: usize, row: &[$t]) -> Result<(), FieldError> {
            let Field { data, nrays, ngates, absent_rows, .. } = self;
            let FieldData::$variant { values, coding } = data else {
                return Err(FieldError::StorageMismatch {
                    expected: data.dtype(),
                    actual: stringify!($t),
                });
            };
            let fill = coding.fill_code();
            let mut rows = Rows { nrays, ngates, absent_rows };
            let ngates = rows.prepare(values, ray, row.len(), fill)?;
            let row_start = values.len();
            values.extend_from_slice(row);
            rows.finish(values, row_start, ngates, ray, fill);
            Ok(())
        }
    };
}

macro_rules! push_be_row {
    ($(#[$doc:meta])* $fn_name:ident, $variant:ident, $t:ty) => {
        $(#[$doc])*
        pub fn $fn_name(&mut self, ray: usize, row_be: &[u8]) -> Result<(), FieldError> {
            if !row_be.len().is_multiple_of(2) {
                return Err(FieldError::InvalidRowByteLength { byte_len: row_be.len() });
            }
            let Field { data, nrays, ngates, absent_rows, .. } = self;
            let FieldData::$variant { values, coding } = data else {
                return Err(FieldError::StorageMismatch {
                    expected: data.dtype(),
                    actual: stringify!($t),
                });
            };
            let fill = coding.fill_code();
            let mut rows = Rows { nrays, ngates, absent_rows };
            let ngates = rows.prepare(values, ray, row_be.len() / 2, fill)?;
            let row_start = values.len();
            values.extend(
                row_be
                    .chunks_exact(2)
                    .map(|pair| <$t>::from_be_bytes([pair[0], pair[1]])),
            );
            rows.finish(values, row_start, ngates, ray, fill);
            Ok(())
        }
    };
}

impl Field {
    /// A field over existing (possibly empty) row-major data. `nrays` is
    /// `data.len() / ngates`; `quantity` and `polarization` come from
    /// [`Quantity::classify`] on the name; attributes start empty.
    pub fn new(name: FieldName, gates: GateMapping, ngates: u32, data: FieldData) -> Self {
        let (quantity, polarization) = Quantity::classify(name.as_str(), None);
        let nrays = if ngates == 0 {
            0
        } else {
            u32::try_from(data.len() / ngates as usize).unwrap_or(u32::MAX)
        };
        Self {
            name,
            quantity,
            polarization,
            attrs: FieldAttrs::default(),
            nrays,
            ngates,
            gates,
            data,
            absent_rows: Vec::new(),
        }
    }

    /// `(nrays, ngates)`.
    pub fn shape(&self) -> (usize, usize) {
        (self.nrays as usize, self.ngates as usize)
    }

    /// `true` when the source did not provide row `ray`.
    #[inline]
    pub fn is_absent(&self, ray: usize) -> bool {
        !self.absent_rows.is_empty()
            && u32::try_from(ray).is_ok_and(|ray| self.absent_rows.binary_search(&ray).is_ok())
    }

    /// Row `ray` in its storage type.
    pub fn row(&self, ray: usize) -> Option<RowRef<'_>> {
        let ngates = self.ngates as usize;
        if ray >= self.nrays as usize {
            return None;
        }
        let start = ray.checked_mul(ngates)?;
        let end = start.checked_add(ngates)?;
        Some(match &self.data {
            FieldData::U8 { values, .. } => RowRef::U8(values.get(start..end)?),
            FieldData::U16 { values, .. } => RowRef::U16(values.get(start..end)?),
            FieldData::I8 { values, .. } => RowRef::I8(values.get(start..end)?),
            FieldData::I16 { values, .. } => RowRef::I16(values.get(start..end)?),
            FieldData::F32 { values, .. } => RowRef::F32(values.get(start..end)?),
            FieldData::F64 { values, .. } => RowRef::F64(values.get(start..end)?),
        })
    }

    /// Resolve a native gate, in this order:
    /// 1. `ray` is in `absent_rows`: `Missing`.
    /// 2. The raw value equals `undetect`: `Undetect` (so NEXRAD raw 0 reads as
    ///    `Undetect` in a provided row).
    /// 3. It equals `fill_value` or is NaN: `Missing`.
    /// 4. It equals `range_folded`: `RangeFolded`.
    /// 5. It is outside `valid_range`: `Missing`.
    /// 6. Otherwise: `Value(physical)`.
    ///
    /// Gates past the native extent are not native gates (`None`).
    pub fn gate(&self, ray: usize, gate: usize) -> Option<Gate> {
        let ngates = self.ngates as usize;
        if ray >= self.nrays as usize || gate >= ngates {
            return None;
        }
        if self.is_absent(ray) {
            return Some(Gate::Missing);
        }
        let index = ray.checked_mul(ngates)?.checked_add(gate)?;
        Some(match &self.data {
            FieldData::U8 { values, coding } => coding.resolve(*values.get(index)?),
            FieldData::U16 { values, coding } => coding.resolve(*values.get(index)?),
            FieldData::I8 { values, coding } => coding.resolve(*values.get(index)?),
            FieldData::I16 { values, coding } => coding.resolve(*values.get(index)?),
            FieldData::F32 { values, coding } => coding.resolve(*values.get(index)?),
            FieldData::F64 { values, coding } => coding.resolve(*values.get(index)?),
        })
    }

    /// Physical value of a native gate; `None` for every sentinel and outside
    /// the native extent.
    #[inline]
    pub fn value(&self, ray: usize, gate: usize) -> Option<f32> {
        self.gate(ray, gate)?.value()
    }

    /// 256-entry decode table for `u8` / `i8` fields, indexed by the raw byte
    /// (`raw as u8` for `i8`), NaN for every sentinel. Absent rows hold the fill
    /// code, so they decode to NaN only when the coding has a `fill_value`;
    /// check [`Field::is_absent`] otherwise.
    pub fn lut8(&self) -> Option<[f32; 256]> {
        let mut table = [f32::NAN; 256];
        match &self.data {
            FieldData::U8 { coding, .. } => {
                for (code, slot) in table.iter_mut().enumerate() {
                    *slot = coding.resolve(code as u8).value().unwrap_or(f32::NAN);
                }
            }
            FieldData::I8 { coding, .. } => {
                for (code, slot) in table.iter_mut().enumerate() {
                    *slot = coding.resolve(code as u8 as i8).value().unwrap_or(f32::NAN);
                }
            }
            _ => return None,
        }
        Some(table)
    }

    /// Explicit float expansion, row-major `[nrays × ngates]`, NaN for every
    /// sentinel. Decoders never call this.
    pub fn to_physical(&self) -> Vec<f32> {
        let (nrays, ngates) = self.shape();
        let mut out = Vec::with_capacity(nrays.saturating_mul(ngates));
        for ray in 0..nrays {
            for gate in 0..ngates {
                out.push(
                    self.gate(ray, gate)
                        .and_then(Gate::value)
                        .unwrap_or(f32::NAN),
                );
            }
        }
        out
    }

    /// Native geometry on `range`: (centre of native gate 0 in metres, native
    /// spacing in metres). For an explicit range the spacing is the distance to
    /// the next centre (0 for a single gate).
    pub fn native_geometry(&self, range: &RangeCoord) -> Option<(f64, f64)> {
        match range {
            RangeCoord::Uniform {
                first_center_m,
                spacing_m,
                ..
            } => {
                let stride = f64::from(self.gates.stride.max(1));
                let start_edge =
                    first_center_m - spacing_m / 2.0 + f64::from(self.gates.start) * spacing_m;
                let spacing = spacing_m * stride;
                Some((start_edge + spacing / 2.0, spacing))
            }
            RangeCoord::Explicit { centers_m } => {
                let start = self.gates.start as usize;
                let center = f64::from(*centers_m.get(start)?);
                let spacing = centers_m
                    .get(start + 1)
                    .map_or(0.0, |next| f64::from(*next) - center);
                Some((center, spacing))
            }
        }
    }

    /// Reserve storage for `rows` more rows of `ngates` values.
    pub fn reserve_rows(&mut self, rows: usize) {
        let additional = rows.saturating_mul(self.ngates as usize);
        match &mut self.data {
            FieldData::U8 { values, .. } => values.reserve(additional),
            FieldData::U16 { values, .. } => values.reserve(additional),
            FieldData::I8 { values, .. } => values.reserve(additional),
            FieldData::I16 { values, .. } => values.reserve(additional),
            FieldData::F32 { values, .. } => values.reserve(additional),
            FieldData::F64 { values, .. } => values.reserve(additional),
        }
    }

    push_slice_row!(
        /// Push the `u8` row of ray `ray` (the index `Sweep::push_ray` returned).
        /// Rays skipped since the last row become absent rows; a short row is
        /// padded with the fill code; a longer row widens every existing row.
        push_row_u8, U8, u8
    );
    push_slice_row!(
        /// Push a `u16` row; see [`Field::push_row_u8`].
        push_row_u16, U16, u16
    );
    push_be_row!(
        /// Push a big-endian `u16` row (NEXRAD 16-bit moments); see
        /// [`Field::push_row_u8`].
        push_row_u16_be, U16, u16
    );
    push_slice_row!(
        /// Push an `i8` row; see [`Field::push_row_u8`].
        push_row_i8, I8, i8
    );
    push_slice_row!(
        /// Push an `i16` row (DORADE 16-bit fields); see
        /// [`Field::push_row_u8`].
        push_row_i16, I16, i16
    );
    push_be_row!(
        /// Push a big-endian `i16` row; see [`Field::push_row_u8`].
        push_row_i16_be, I16, i16
    );
    push_slice_row!(
        /// Push an `f32` row; see [`Field::push_row_u8`].
        push_row_f32, F32, f32
    );
    push_slice_row!(
        /// Push an `f64` row; see [`Field::push_row_u8`].
        push_row_f64, F64, f64
    );

    /// Append absent rows until the field has `rays` rows (`Sweep::seal` calls
    /// this for rays at the end that the field never received).
    pub fn push_absent_rows_to(&mut self, rays: usize) -> Result<(), FieldError> {
        let rows = self.nrays as usize;
        if rows > rays {
            return Err(FieldError::TooManyRows { rows, nrays: rays });
        }
        if rows == rays {
            return Ok(());
        }
        let Field {
            data,
            nrays,
            ngates,
            absent_rows,
            ..
        } = self;
        let mut state = Rows {
            nrays,
            ngates,
            absent_rows,
        };
        macro_rules! fill_to {
            ($values:expr, $fill:expr) => {{
                let values = $values;
                let fill = $fill;
                // Rows `rows..rays-1` are absent; the last one is pushed as an
                // absent row too.
                let ngates = state.prepare(values, rays - 1, 0, fill)?;
                let row_start = values.len();
                state.finish(values, row_start, ngates, rays - 1, fill);
                state.absent_rows.push((rays - 1) as u32);
            }};
        }
        match data {
            FieldData::U8 { values, coding } => fill_to!(values, coding.fill_code()),
            FieldData::U16 { values, coding } => fill_to!(values, coding.fill_code()),
            FieldData::I8 { values, coding } => fill_to!(values, coding.fill_code()),
            FieldData::I16 { values, coding } => fill_to!(values, coding.fill_code()),
            FieldData::F32 { values, coding } => fill_to!(values, coding.fill_code()),
            FieldData::F64 { values, coding } => fill_to!(values, coding.fill_code()),
        }
        Ok(())
    }

    /// Move the field apart. No copy.
    pub fn into_parts(self) -> FieldParts {
        FieldParts {
            name: self.name,
            quantity: self.quantity,
            polarization: self.polarization,
            attrs: self.attrs,
            nrays: self.nrays,
            ngates: self.ngates,
            gates: self.gates,
            data: self.data,
            absent_rows: self.absent_rows,
        }
    }
}
