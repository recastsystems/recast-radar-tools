//! Python exception types.
//!
//! Each derives from the built-in exception a caller would catch without
//! knowing this package: a file that does not decode is a `ValueError`, a
//! writer this build lacks is a `NotImplementedError`, a download failure is
//! an `OSError`.

use pyo3::create_exception;
use pyo3::exceptions::{PyNotImplementedError, PyOSError, PyValueError};
use pyo3::prelude::*;

create_exception!(
    recast_radar,
    DecodeError,
    PyValueError,
    "The input is not a radar file this package decodes, or it is damaged."
);
create_exception!(
    recast_radar,
    UnavailableError,
    PyNotImplementedError,
    "This build has no writer for the format, or no polling-directory publisher."
);
create_exception!(
    recast_radar,
    UnrepresentableError,
    PyValueError,
    "The output format cannot represent something in the volume."
);
create_exception!(
    recast_radar,
    FetchError,
    PyOSError,
    "A download or a remote listing failed."
);

/// Add the exception types to the module.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add("DecodeError", py.get_type::<DecodeError>())?;
    module.add("UnavailableError", py.get_type::<UnavailableError>())?;
    module.add(
        "UnrepresentableError",
        py.get_type::<UnrepresentableError>(),
    )?;
    module.add("FetchError", py.get_type::<FetchError>())?;
    Ok(())
}

/// A [`DecodeError`] with `message`.
pub(crate) fn decode_error(message: impl Into<String>) -> PyErr {
    DecodeError::new_err(message.into())
}
