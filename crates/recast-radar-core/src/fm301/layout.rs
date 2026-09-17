//! The borrow-free form of a [`VolumeView`] (design note 12.2) and the gate
//! mapping applied on read.

use super::{ArrayRef, FieldSource, Group, RowOrder, Values, ViewWarning, VolumeView};
use crate::model::{ArrayBuf, AttrValue, GateMapping, Scalar};

/// A [`VolumeView`] with no borrows: every dataset variable is a
/// [`DataRef::Field`], other values are copied (they are small).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeLayout {
    pub root: GroupLayout,
    pub warnings: Vec<ViewWarning>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupLayout {
    pub name: String,
    pub dims: Vec<(String, usize)>,
    pub variables: Vec<VariableLayout>,
    pub attrs: Vec<(String, AttrValue)>,
    pub children: Vec<GroupLayout>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableLayout {
    pub name: String,
    pub dims: Vec<String>,
    pub data: DataRef,
    pub attrs: Vec<(String, AttrValue)>,
}

/// Where a variable's values come from.
#[derive(Clone, Debug, PartialEq)]
pub enum DataRef {
    /// A model field's buffer, `[nrays × native_gates]` in storage row order,
    /// shown through `rows` and `mapping` on an `out_gates` range.
    Field {
        source: FieldSource,
        nrays: usize,
        native_gates: usize,
        mapping: GateMapping,
        out_gates: usize,
        fill: Scalar,
        rows: RowOrder,
    },
    Array(ArrayBuf),
    Scalar(Scalar),
    Text(String),
}

impl DataRef {
    /// `true` when the field's buffer, reshaped to `[nrays, native_gates]`, is
    /// already the variable (no reordering, padding or repetition).
    pub fn is_zero_copy(&self) -> bool {
        matches!(
            self,
            Self::Field { native_gates, mapping, out_gates, rows: RowOrder::Identity, .. }
                if *mapping == GateMapping::IDENTITY && native_gates == out_gates
        )
    }
}

impl GroupLayout {
    pub fn child(&self, name: &str) -> Option<&GroupLayout> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn variable(&self, name: &str) -> Option<&VariableLayout> {
        self.variables.iter().find(|variable| variable.name == name)
    }
}

pub(super) fn layout(view: &VolumeView<'_>) -> VolumeLayout {
    VolumeLayout {
        root: group_layout(view, &view.root),
        warnings: view.warnings.clone(),
    }
}

fn group_layout(view: &VolumeView<'_>, group: &Group<'_>) -> GroupLayout {
    GroupLayout {
        name: group.name.to_string(),
        dims: group
            .dims
            .iter()
            .map(|(name, len)| (name.to_string(), *len))
            .collect(),
        variables: group
            .variables
            .iter()
            .map(|variable| VariableLayout {
                name: variable.name.to_string(),
                dims: variable.dims.iter().map(|dim| dim.to_string()).collect(),
                data: data_ref(view, variable.source, &variable.values),
                attrs: variable
                    .attrs
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.clone()))
                    .collect(),
            })
            .collect(),
        attrs: group
            .attrs
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect(),
        children: group
            .children
            .iter()
            .map(|child| group_layout(view, child))
            .collect(),
    }
}

fn data_ref(view: &VolumeView<'_>, source: Option<FieldSource>, values: &Values<'_>) -> DataRef {
    match values {
        Values::Mapped {
            source,
            nrays,
            native_gates,
            mapping,
            out_gates,
            fill,
            rows,
            ..
        } => DataRef::Field {
            source: *source,
            nrays: *nrays,
            native_gates: *native_gates,
            mapping: *mapping,
            out_gates: *out_gates,
            fill: *fill,
            rows: rows.clone(),
        },
        Values::Borrowed(array) => {
            let field = source.and_then(|source| {
                view.volume
                    .sweeps
                    .get(source.sweep as usize)?
                    .fields
                    .get(source.field as usize)
                    .map(|field| (source, field))
            });
            match field {
                Some((source, field)) => DataRef::Field {
                    source,
                    nrays: field.nrays as usize,
                    native_gates: field.ngates as usize,
                    mapping: GateMapping::IDENTITY,
                    out_gates: field.ngates as usize,
                    fill: field.data.fill_scalar(),
                    rows: RowOrder::Identity,
                },
                None => DataRef::Array(array.to_array()),
            }
        }
        Values::Owned(array) => DataRef::Array(array.clone()),
        Values::Scalar(scalar) => DataRef::Scalar(*scalar),
        Values::Text(text) => DataRef::Text(text.to_string()),
    }
}

/// Apply a field mapping: `nrays` output rows in `rows` order, native gates
/// repeated `mapping.stride` times from range gate `mapping.start`, padded with
/// `fill` to `out_gates`.
pub(super) fn apply_mapping(
    native: ArrayRef<'_>,
    nrays: usize,
    native_gates: usize,
    mapping: GateMapping,
    out_gates: usize,
    fill: Scalar,
    rows: &RowOrder,
) -> ArrayBuf {
    let map = MapSpec {
        nrays,
        native_gates,
        mapping,
        out_gates,
        rows,
    };
    match native {
        ArrayRef::U8(values) => ArrayBuf::U8(map.apply(values, scalar_or(fill, 0))),
        ArrayRef::U16(values) => ArrayBuf::U16(map.apply(values, scalar_or(fill, 0))),
        ArrayRef::I8(values) => ArrayBuf::I8(map.apply(values, scalar_or(fill, 0))),
        ArrayRef::I16(values) => ArrayBuf::I16(map.apply(values, scalar_or(fill, 0))),
        ArrayRef::I32(values) => ArrayBuf::I32(map.apply(values, scalar_or(fill, 0))),
        ArrayRef::F32(values) => ArrayBuf::F32(map.apply(values, scalar_or(fill, f32::NAN))),
        ArrayRef::F64(values) => ArrayBuf::F64(map.apply(values, scalar_or(fill, f64::NAN))),
    }
}

trait FromScalar: Sized {
    fn from_scalar(scalar: Scalar) -> Option<Self>;
}

macro_rules! from_scalar {
    ($t:ty, $variant:ident) => {
        impl FromScalar for $t {
            fn from_scalar(scalar: Scalar) -> Option<Self> {
                match scalar {
                    Scalar::$variant(value) => Some(value),
                    _ => None,
                }
            }
        }
    };
}

from_scalar!(u8, U8);
from_scalar!(u16, U16);
from_scalar!(i8, I8);
from_scalar!(i16, I16);
from_scalar!(i32, I32);
from_scalar!(f32, F32);
from_scalar!(f64, F64);

fn scalar_or<T: FromScalar>(scalar: Scalar, default: T) -> T {
    T::from_scalar(scalar).unwrap_or(default)
}

struct MapSpec<'r> {
    nrays: usize,
    native_gates: usize,
    mapping: GateMapping,
    out_gates: usize,
    rows: &'r RowOrder,
}

impl MapSpec<'_> {
    fn apply<T: Copy>(&self, native: &[T], fill: T) -> Vec<T> {
        let mut out = vec![fill; self.nrays.saturating_mul(self.out_gates)];
        let stride = self.mapping.stride.max(1) as usize;
        let start = self.mapping.start as usize;
        for (out_row, dest) in out.chunks_exact_mut(self.out_gates.max(1)).enumerate() {
            if self.out_gates == 0 {
                break;
            }
            let Some(source_row) = self.rows.source_row(out_row) else {
                continue;
            };
            let begin = source_row.saturating_mul(self.native_gates);
            let Some(row) = native.get(begin..begin.saturating_add(self.native_gates)) else {
                continue;
            };
            for (gate, value) in row.iter().enumerate() {
                let first = start + gate * stride;
                if first >= self.out_gates {
                    break;
                }
                dest[first..(first + stride).min(self.out_gates)].fill(*value);
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn mapping_pads_repeats_and_reorders() {
        let native = [1u8, 2, 3, 4];
        let out = apply_mapping(
            ArrayRef::U8(&native),
            2,
            2,
            GateMapping {
                start: 1,
                stride: 2,
            },
            6,
            Scalar::U8(0),
            &RowOrder::Permutation(Arc::from(vec![1u32, 0])),
        );
        assert_eq!(out, ArrayBuf::U8(vec![0, 3, 3, 4, 4, 0, 0, 1, 1, 2, 2, 0]));
    }
}
