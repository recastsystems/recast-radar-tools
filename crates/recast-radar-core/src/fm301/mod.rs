//! The FM301 group view over a [`Volume`]: the conformance surface for xradar
//! `DataTree` output and a future CfRadial 2 writer
//! (`docs/design/fm301-model.md` section 12).
//!
//! [`volume_view`] builds the FM301 group tree (`/`, `radar_parameters`,
//! `radar_calibration`, `georeferencing_correction`, `sweep_<n>`, `monitoring`)
//! with names, dimensions and typed attributes, borrowing field storage.
//! Fields are always written encoded (packed integers with CF attributes, or
//! floats with their `_FillValue`); decoding is the consumer's job. Ray order
//! and gate padding / repetition are described, not applied: a variable whose
//! storage is not already the FM301 array is [`Values::Mapped`], and
//! [`Values::materialize`] applies the mapping on request.
//!
//! [`VolumeView::layout`] detaches the same tree from the volume, so a binding
//! can record the layout and then move each field's buffer out of the volume.

mod build;
mod layout;

use std::borrow::Cow;
use std::sync::Arc;

use thiserror::Error;

pub use build::volume_view;
pub use layout::{DataRef, GroupLayout, VariableLayout, VolumeLayout};

use crate::model::{ArrayBuf, AttrValue, GateMapping, Scalar, Volume};

/// Which reference the view reproduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// xradar 0.12 names, attribute strings and attribute types
    /// (`sweep_fixed_angle`, "not_set", "meters per seconds", bool
    /// attributes). Used by conformance tests and the DataTree binding.
    Xradar012,
    /// The FM301-2022 text: `fixed_angle`, `Conventions`, `wmo__cf_profile`,
    /// UDUNITS units, Table 301-12a names. Used by a future CfRadial 2 writer.
    Wmo2022,
}

/// Ray dimension and ray order. Only a row permutation changes; no data moves.
///
/// A field whose storage order already is the view's ray order is
/// [`Values::Borrowed`] (in a layout, [`DataRef::is_zero_copy`]); otherwise the
/// view carries a [`RowOrder::Permutation`]. Storage keeps the source's order,
/// so which choice avoids the permutation depends on the format (design note
/// 12.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstDim {
    /// Dimension `time`; rays in acquisition order (stable sort by `time_s`).
    /// xradar's `first_dim="time"`, and the only choice for
    /// [`Flavor::Wmo2022`] (a CF coordinate must be monotonic). The permutation
    /// is the identity when storage order is acquisition order (NEXRAD,
    /// CfRadial, most DORADE files) or every ray time is equal (JMA, ODIM
    /// without per-ray times). It is not for ODIM with per-ray times (azimuth
    /// order, starting where the scan did not) or for DORADE files whose ray
    /// times run backwards (NOXP 2009).
    Time,
    /// xradar's default `first_dim="auto"`: dimension `azimuth` (or `elevation`
    /// for RHI sweeps of non-CfRadial sources, as observed in xradar 0.12), rays
    /// sorted by that angle. The permutation is the identity for azimuth-ordered
    /// storage (ODIM, NOXP 2009), and not for NEXRAD, CfRadial or JMA, whose
    /// sweeps start at an arbitrary azimuth.
    Auto,
}

/// Which unmodelled source items the view writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Passthrough {
    /// What the flavor's reference writes. Xradar012: sweep `extra_vars`,
    /// sweep `other` attributes, platform track and calibration `extra` yes,
    /// root `attrs.other` no. Wmo2022: FM301 names only.
    Flavor,
    /// Also every `other`, `extra_vars` and `extra` item, verbatim, for lossless
    /// CfRadial 2 output.
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewOptions {
    pub flavor: Flavor,
    pub first_dim: FirstDim,
    pub passthrough: Passthrough,
}

impl ViewOptions {
    /// xradar 0.12 defaults: `Xradar012`, `first_dim="auto"`.
    pub const XRADAR: ViewOptions = ViewOptions {
        flavor: Flavor::Xradar012,
        first_dim: FirstDim::Auto,
        passthrough: Passthrough::Flavor,
    };
    /// FM301-2022 text, acquisition order.
    pub const WMO: ViewOptions = ViewOptions {
        flavor: Flavor::Wmo2022,
        first_dim: FirstDim::Time,
        passthrough: Passthrough::Flavor,
    };
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self::XRADAR
    }
}

/// The FM301 group tree of one volume.
pub struct VolumeView<'a> {
    /// `/` with children `radar_parameters`, `radar_calibration`,
    /// `georeferencing_correction` (when present) and `sweep_<n>`. A sweep
    /// group's child is `monitoring` (Table 301-11).
    pub root: Group<'a>,
    pub warnings: Vec<ViewWarning>,
    volume: &'a Volume,
}

/// One group: dimensions, variables, attributes and child groups.
#[derive(Clone, Debug, PartialEq)]
pub struct Group<'a> {
    /// `""` for the root, `"sweep_0"`, `"monitoring"`, ...
    pub name: Cow<'a, str>,
    /// `("time", 720)`, `("range", 1832)`, `("frequency", 1)`.
    pub dims: Vec<(Cow<'a, str>, usize)>,
    pub variables: Vec<Variable<'a>>,
    pub attrs: Vec<(Cow<'a, str>, AttrValue)>,
    pub children: Vec<Group<'a>>,
}

/// One variable in its encoded form.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable<'a> {
    pub name: Cow<'a, str>,
    pub dims: Vec<Cow<'a, str>>,
    pub values: Values<'a>,
    /// Attributes of the encoded form, typed. Packing, fill and flag attributes
    /// are in the variable's packed type.
    pub attrs: Vec<(Cow<'a, str>, AttrValue)>,
    /// The model field this variable reads, for dataset variables.
    pub source: Option<FieldSource>,
}

/// Variable values.
#[derive(Clone, Debug, PartialEq)]
pub enum Values<'a> {
    /// Contiguous, same shape and ray order as the variable: zero-copy.
    Borrowed(ArrayRef<'a>),
    /// A field read through its mapping: rows taken in `rows` order, native
    /// gates padded with `fill` and repeated `mapping.stride` times, starting at
    /// range gate `mapping.start`.
    Mapped {
        source: FieldSource,
        native: ArrayRef<'a>,
        /// Output rows (the sweep's ray count).
        nrays: usize,
        native_gates: usize,
        mapping: GateMapping,
        out_gates: usize,
        fill: Scalar,
        rows: RowOrder,
    },
    /// Small computed or reordered arrays (range centres, ray coordinates in
    /// sorted order, volume constants broadcast to rays).
    Owned(ArrayBuf),
    Scalar(Scalar),
    Text(Cow<'a, str>),
}

/// Ray order of a sweep's variables relative to storage order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOrder {
    Identity,
    /// `permutation[i]` is the storage row shown at position `i`. One `Arc` is
    /// shared by every variable of the sweep.
    Permutation(Arc<[u32]>),
}

impl RowOrder {
    /// Storage row shown at position `row`.
    pub fn source_row(&self, row: usize) -> Option<usize> {
        match self {
            Self::Identity => Some(row),
            Self::Permutation(order) => order.get(row).map(|r| *r as usize),
        }
    }
}

/// A model field: `volume.sweeps[sweep].fields[field]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldSource {
    pub sweep: u32,
    pub field: u32,
}

/// A borrowed contiguous array.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrayRef<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    I8(&'a [i8]),
    I16(&'a [i16]),
    I32(&'a [i32]),
    F32(&'a [f32]),
    F64(&'a [f64]),
}

impl ArrayRef<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::I8(v) => v.len(),
            Self::I16(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// NumPy-style dtype name.
    pub fn dtype(&self) -> &'static str {
        match self {
            Self::U8(_) => "uint8",
            Self::U16(_) => "uint16",
            Self::I8(_) => "int8",
            Self::I16(_) => "int16",
            Self::I32(_) => "int32",
            Self::F32(_) => "float32",
            Self::F64(_) => "float64",
        }
    }

    /// Copy into an owned buffer.
    pub fn to_array(&self) -> ArrayBuf {
        match self {
            Self::U8(v) => ArrayBuf::U8(v.to_vec()),
            Self::U16(v) => ArrayBuf::U16(v.to_vec()),
            Self::I8(v) => ArrayBuf::I8(v.to_vec()),
            Self::I16(v) => ArrayBuf::I16(v.to_vec()),
            Self::I32(v) => ArrayBuf::I32(v.to_vec()),
            Self::F32(v) => ArrayBuf::F32(v.to_vec()),
            Self::F64(v) => ArrayBuf::F64(v.to_vec()),
        }
    }
}

/// Non-fatal conditions a CF writer reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewWarning {
    /// Ray times are not strictly increasing even in acquisition order (the
    /// source has no per-ray times). The `time` coordinate is written anyway,
    /// as xradar writes it.
    NonMonotonicTime { sweep: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ViewError {
    #[error("{path}: attribute {attr} value does not fit the variable's type")]
    OutOfRange { path: String, attr: &'static str },
}

/// Format-specific attributes a decoder crate contributes (for example xradar's
/// NEXRAD root attributes from `NexradMetadata`).
pub trait ExtraAttrs {
    fn root_attrs(&self, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
    fn sweep_attrs(&self, sweep: usize, flavor: Flavor) -> Vec<(Cow<'static, str>, AttrValue)>;
}

impl<'a> VolumeView<'a> {
    /// The volume the view borrows.
    pub fn volume(&self) -> &'a Volume {
        self.volume
    }

    /// A group by slash-separated path (`""` or `"/"` for the root,
    /// `"sweep_0"`, `"sweep_0/monitoring"`).
    pub fn group(&self, path: &str) -> Option<&Group<'a>> {
        let mut group = &self.root;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            group = group.child(part)?;
        }
        Some(group)
    }

    /// The same tree with no borrows (section 12.2).
    pub fn layout(&self) -> VolumeLayout {
        layout::layout(self)
    }
}

impl<'a> Group<'a> {
    pub fn child(&self, name: &str) -> Option<&Group<'a>> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn variable(&self, name: &str) -> Option<&Variable<'a>> {
        self.variables.iter().find(|variable| variable.name == name)
    }

    pub fn attr(&self, name: &str) -> Option<&AttrValue> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn dim(&self, name: &str) -> Option<usize> {
        self.dims
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, len)| *len)
    }
}

impl Variable<'_> {
    pub fn attr(&self, name: &str) -> Option<&AttrValue> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }
}

impl Values<'_> {
    /// The encoded values as one owned array in the variable's shape (row-major).
    /// `None` for scalars and text. Mapped fields are padded, repeated and
    /// reordered here; this is the only copy the view makes.
    pub fn materialize(&self) -> Option<ArrayBuf> {
        match self {
            Self::Borrowed(array) => Some(array.to_array()),
            Self::Owned(array) => Some(array.clone()),
            Self::Mapped {
                native,
                nrays,
                native_gates,
                mapping,
                out_gates,
                fill,
                rows,
                ..
            } => Some(layout::apply_mapping(
                *native,
                *nrays,
                *native_gates,
                *mapping,
                *out_gates,
                *fill,
                rows,
            )),
            Self::Scalar(_) | Self::Text(_) => None,
        }
    }
}
