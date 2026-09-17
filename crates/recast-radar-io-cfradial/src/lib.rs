//! CfRadial 1.x decoding through a pure-Rust classic netCDF reader.
//!
//! - [`cfradial`]: CfRadial 1.x volumes into
//!   [`recast_radar_core::RadarVolume`].
//! - [`netcdf3`]: the minimal read-only classic netCDF (CDF-1/CDF-2) parser.

pub mod cfradial;
pub mod netcdf3;

use thiserror::Error;

pub use cfradial::decode_cfradial1_volume;
pub use netcdf3::looks_like_netcdf3_bytes;

/// Result type for CfRadial and netCDF decoding.
pub type Result<T> = std::result::Result<T, CfRadialError>;

/// Errors from CfRadial and classic netCDF decoding.
#[derive(Debug, Error)]
pub enum CfRadialError {
    /// A netCDF structure ended before its declared length.
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
    /// Structurally invalid or unsupported netCDF/CfRadial content.
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
