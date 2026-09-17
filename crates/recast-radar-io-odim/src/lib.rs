//! ODIM_H5 decoding through a pure-Rust HDF5 subset.
//!
//! - [`odim`]: polar `PVOL`/`SCAN` objects into
//!   [`recast_radar_core::RadarVolume`].
//! - [`odim_cartesian`]: Cartesian `IMAGE`/`MAX` products into a gridded
//!   [`odim_cartesian::OdimCartesianGrid`].
//! - [`hdf5lite`]: the minimal read-only HDF5 parser both decoders use.

pub mod hdf5lite;
pub mod odim;
pub mod odim_cartesian;

use thiserror::Error;

pub use odim::{decode_odim_h5_volume, looks_like_hdf5_bytes};
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
    /// Decoded gates did not fit the moment grid.
    #[error("moment grid error: {0}")]
    MomentGrid(#[from] recast_radar_core::MomentGridError),
}
