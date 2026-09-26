//! Python bindings for recast-radar-tools: the extension module
//! `recast_radar._native`, wrapped by the pure-Python package `recast_radar`
//! (`python/recast_radar/`). The user guide is `docs/guide/python.md`.
//!
//! The Rust side decodes, builds the FM301 view
//! (`recast_radar_core::fm301`, design note `docs/design/fm301-model.md`
//! section 12) and hands every field buffer to NumPy without copying it
//! ([`tree`]). The Python side turns that into an `xarray.DataTree`, a
//! `pyart.core.Radar` or files.
//!
//! What each module exports to Python:
//!
//! | Module | Python names |
//! |---|---|
//! | [`volume`] | `Volume`, `read`, `read_all`, `merge`, `sniff` |
//! | [`tree`] | `_open_tree`, `pyart_field_name` |
//! | [`mod@write`] | `_write`, `_to_bytes`, `_publish`, `_require_writer`, `writers`, `publisher_available` |
//! | [`fetch`] (feature `net`) | the private primitives behind `recast_radar.fetch` (`_level2_objects`, `_fetch_bytes`, ...) |
//! | [`errors`] | `DecodeError`, `UnavailableError`, `UnrepresentableError`, `FetchError` |
//!
//! There is no hand-written `unsafe` here: the crate keeps the workspace's
//! `forbid(unsafe_code)`. Buffers reach NumPy through
//! `numpy::PyArray::from_vec`, which takes ownership of the `Vec` (design
//! note 12.2), and PyO3's macros expand to code the lint accepts.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod errors;
#[cfg(feature = "net")]
pub mod fetch;
pub mod source;
pub mod tree;
pub mod values;
pub mod volume;
pub mod write;

use pyo3::prelude::*;

/// The `recast_radar._native` module.
#[pymodule]
#[pyo3(name = "_native")]
fn native_module(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add("NET", cfg!(feature = "net"))?;
    errors::register(module)?;
    volume::register(module)?;
    tree::register(module)?;
    write::register(module)?;
    #[cfg(feature = "net")]
    fetch::register(module)?;
    Ok(())
}
