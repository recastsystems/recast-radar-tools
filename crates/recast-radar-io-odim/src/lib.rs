//! ODIM_H5 decoding through a pure-Rust HDF5 subset.
//!
//! - [`odim`]: polar `PVOL`/`SCAN` objects into the FM301
//!   [`recast_radar_core::model::Volume`] ([`odim::read_odim_h5_volume`]);
//!   the pre-FM301 [`decode_odim_h5_volume`] lives in [`legacy_api`] during
//!   the migration.
//! - [`odim_cartesian`]: Cartesian `IMAGE`/`MAX` products into a gridded
//!   [`odim_cartesian::OdimCartesianGrid`].
//! - [`hdf5lite`]: the minimal read-only HDF5 parser both decoders use.
//!
//! # Limits
//!
//! The HDF5 reader bounds every structure a file header can inflate:
//!
//! | Structure | Limit |
//! |---|---|
//! | Group nesting depth | 16 |
//! | Objects indexed per file (real files: 18-283) | 16,384 |
//! | B-tree nodes per walk | 65,536 |
//! | Entries per group | 1,048,576 |
//! | Messages per object header | 4,096 |
//! | Message bytes per object header, and per header block | 64 MiB |
//! | Header continuation blocks per object | 1,024 |
//! | Dataspace rank / dimension size | 32 / 104,857,600 |
//! | Dataset bytes, stored and after type conversion | 256 MiB each |
//! | Chunks per dataset / stored chunk size | 262,144 / 256 MiB |
//! | Inflated chunk | its declared chunk size |
//! | Attribute value | 16 MiB |
//! | Filters per pipeline / client values per filter | 32 / 1,024 |
//!
//! Reading a dataset holds its raw bytes and converted elements at the same
//! time (at most 512 MiB); datasets are read one at a time.
//!
//! The polar decoder accepts at most `MAX_SWEEPS_PER_VOLUME` (1,024)
//! `datasetN` groups and `MAX_GATES_PER_RADIAL` (16,384) bins per ray, and
//! charges each sweep's radial table and moment grids to a `DecodeBudget` of
//! `MAX_DECODED_VOLUME_BYTES` (1 GiB) before allocating them (constants in
//! [`recast_radar_core::bounded_read`]). The Cartesian decoder caps its
//! physical-value grid at the same 1 GiB.
//!
//! Every limit violation is an [`OdimError::LimitExceeded`] error.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// Migrated to the FM301 model (F.3): only `legacy_api` names legacy items.
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

pub mod hdf5lite;
#[allow(deprecated)]
pub mod legacy_api;
pub mod odim;
pub mod odim_cartesian;

use thiserror::Error;

#[allow(deprecated)]
pub use legacy_api::decode_odim_h5_volume;
pub use odim::{looks_like_hdf5_bytes, read_odim_h5_volume};
pub use odim_cartesian::{OdimCartesianGrid, decode_odim_h5_cartesian_max};

/// Result type for ODIM_H5 and HDF5 decoding.
pub type Result<T> = std::result::Result<T, OdimError>;

/// Errors from ODIM_H5 and HDF5 decoding.
#[derive(Debug, Error)]
pub enum OdimError {
    /// An HDF5 structure ended before its declared length.
    #[error("truncated {what} at offset {offset}: need {needed} bytes, have {available}")]
    Truncated {
        /// Structure being read.
        what: &'static str,
        /// Byte offset of the structure.
        offset: usize,
        /// Bytes the structure needs.
        needed: usize,
        /// Bytes actually available.
        available: usize,
    },
    /// Structurally invalid or unsupported HDF5/ODIM content.
    #[error("invalid message at offset {offset}: {reason}")]
    InvalidMessage {
        /// Byte offset of the problem (0 when not meaningful).
        offset: usize,
        /// Human-readable description.
        reason: String,
    },
    /// The file declares more data than a documented resource limit allows
    /// (see the crate-level `# Limits` section).
    #[error("decode limit exceeded: {0}")]
    LimitExceeded(String),
}
