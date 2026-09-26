//! Model values to Python objects, keeping their types.
//!
//! CF needs `_FillValue`, `valid_range` and `flag_values` in the packed
//! variable's type, and xarray takes the decoded dtype from the type of
//! `scale_factor`, so attribute scalars become NumPy scalars of their own
//! dtype. Two exceptions follow what xradar 0.12 writes: 64-bit integers
//! become Python `int` and 64-bit floats Python `float` (a subclass of
//! `numpy.float64`'s base, so both compare and decode alike).
//!
//! Arrays move into NumPy without a copy ([`array()`]); text arrays become
//! lists of `str`.

use numpy::{PyArray1, PyArrayMethods};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyFloat, PyInt, PyList, PyString};
use recast_radar_core::model::{ArrayBuf, AttrValue, Scalar};

static NUMPY: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn numpy(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    NUMPY
        .get_or_try_init(py, || py.import("numpy").map(Bound::unbind))
        .map(|module| module.bind(py))
}

/// A NumPy scalar of `dtype` with `value`.
fn numpy_scalar<'py, T>(py: Python<'py>, dtype: &str, value: T) -> PyResult<Bound<'py, PyAny>>
where
    T: IntoPyObject<'py>,
{
    numpy(py)?.getattr(dtype)?.call1((value,))
}

/// A scalar with its type: NumPy scalars, except `int` for 64-bit integers
/// and `float` for float64.
pub fn scalar<'py>(py: Python<'py>, value: Scalar) -> PyResult<Bound<'py, PyAny>> {
    match value {
        Scalar::I8(v) => numpy_scalar(py, "int8", v),
        Scalar::U8(v) => numpy_scalar(py, "uint8", v),
        Scalar::I16(v) => numpy_scalar(py, "int16", v),
        Scalar::U16(v) => numpy_scalar(py, "uint16", v),
        Scalar::I32(v) => numpy_scalar(py, "int32", v),
        Scalar::U32(v) => numpy_scalar(py, "uint32", v),
        Scalar::I64(v) => Ok(PyInt::new(py, v).into_any()),
        Scalar::U64(v) => Ok(PyInt::new(py, v).into_any()),
        Scalar::F32(v) => numpy_scalar(py, "float32", f64::from(v)),
        Scalar::F64(v) => Ok(PyFloat::new(py, v).into_any()),
    }
}

/// A 0-d value of the scalar's exact dtype (a NumPy scalar even for 64-bit
/// types), for variables.
pub fn typed_scalar<'py>(py: Python<'py>, value: Scalar) -> PyResult<Bound<'py, PyAny>> {
    match value {
        Scalar::I64(v) => numpy_scalar(py, "int64", v),
        Scalar::U64(v) => numpy_scalar(py, "uint64", v),
        Scalar::F64(v) => numpy_scalar(py, "float64", v),
        other => scalar(py, other),
    }
}

/// A 1-D array, moved into NumPy without a copy; text becomes a list of
/// `str`.
pub fn array(py: Python<'_>, values: ArrayBuf) -> Bound<'_, PyAny> {
    match values {
        ArrayBuf::I8(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::U8(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::I16(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::U16(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::I32(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::U32(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::I64(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::F32(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::F64(v) => PyArray1::from_vec(py, v).into_any(),
        ArrayBuf::Text(v) => PyList::new(py, v.iter().map(|text| &**text))
            .map(Bound::into_any)
            .unwrap_or_else(|_| PyList::empty(py).into_any()),
    }
}

/// `values` as an array of `shape` (row-major), or 1-D when the lengths
/// disagree. Numeric arrays are moved, not copied; `reshape` of a
/// contiguous array is a view.
pub fn shaped_array<'py>(
    py: Python<'py>,
    values: ArrayBuf,
    shape: &[usize],
) -> PyResult<Bound<'py, PyAny>> {
    let len = values.len();
    let text = matches!(values, ArrayBuf::Text(_));
    let array = array(py, values);
    let expected: usize = shape.iter().product();
    if text || shape.len() <= 1 || expected != len {
        return Ok(array);
    }
    array.call_method1("reshape", (shape.to_vec(),))
}

/// A 2-D field buffer `[rows, gates]`, moved into NumPy without a copy.
pub fn field_array(
    py: Python<'_>,
    values: ArrayBuf,
    rows: usize,
    gates: usize,
) -> PyResult<Bound<'_, PyAny>> {
    macro_rules! reshape {
        ($v:expr) => {{
            let values = $v;
            let len = values.len();
            let array = PyArray1::from_vec(py, values);
            if rows.checked_mul(gates) == Some(len) {
                Ok(array.reshape([rows, gates])?.into_any())
            } else {
                Ok(array.into_any())
            }
        }};
    }
    match values {
        ArrayBuf::I8(v) => reshape!(v),
        ArrayBuf::U8(v) => reshape!(v),
        ArrayBuf::I16(v) => reshape!(v),
        ArrayBuf::U16(v) => reshape!(v),
        ArrayBuf::I32(v) => reshape!(v),
        ArrayBuf::U32(v) => reshape!(v),
        ArrayBuf::I64(v) => reshape!(v),
        ArrayBuf::F32(v) => reshape!(v),
        ArrayBuf::F64(v) => reshape!(v),
        ArrayBuf::Text(v) => Ok(PyList::new(py, v.iter().map(|text| &**text))?.into_any()),
    }
}

/// An attribute value: `str`, `bool`, a typed scalar or a 1-D array.
pub fn attr<'py>(py: Python<'py>, value: &AttrValue) -> PyResult<Bound<'py, PyAny>> {
    match value {
        AttrValue::Text(text) => Ok(PyString::new(py, text).into_any()),
        AttrValue::Bool(value) => Ok(PyBool::new(py, *value).to_owned().into_any()),
        AttrValue::Scalar(value) => scalar(py, *value),
        AttrValue::Array(values) => Ok(array(py, values.clone())),
    }
}

/// Attributes as a list of `(name, value)` pairs, in order.
pub fn attrs<'py, K: AsRef<str>>(
    py: Python<'py>,
    attrs: &[(K, AttrValue)],
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for (name, value) in attrs {
        list.append((name.as_ref(), attr(py, value)?))?;
    }
    Ok(list)
}
