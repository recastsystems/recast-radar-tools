//! CfRadial 1.x decoding through a pure-Rust classic netCDF reader.
//!
//! - [`cfradial`]: CfRadial 1.x volumes into
//!   [`recast_radar_core::RadarVolume`].
//! - [`netcdf3`]: the minimal read-only classic netCDF (CDF-1/CDF-2) parser.
//!
//! # Limits
//!
//! The classic netCDF reader accepts at most 1,024 dimensions, 4,096
//! variables, 4,096 attributes per list, rank 32, 64 KiB names, 16 MiB
//! attribute values, dimension lengths and record counts of 104,857,600, and
//! 256 MiB per variable array (one record slab, and a whole record variable).
//! A record variable's full byte range is checked against the file before
//! its array is reserved.
//!
//! The CfRadial decoder accepts at most `MAX_GATES_PER_RADIAL` (16,384)
//! gates and `MAX_SWEEPS_PER_VOLUME` (1,024) sweeps, and charges every
//! numeric coordinate array it widens to f64, each field's transient f32
//! plane, the radial and per-ray metadata tables, and every moment grid to a
//! `DecodeBudget` of `MAX_DECODED_VOLUME_BYTES` (1 GiB) before allocating
//! (constants in [`recast_radar_core::bounded_read`]). Sweeps must not share
//! rays: overlapping `sweep_start_ray_index`/`sweep_end_ray_index` ranges are
//! a [`CfRadialError::InvalidMessage`] error, so each ray's gates are copied
//! into at most one sweep and a moment's grids never outgrow its field. A
//! sweep whose ray index is not a non-negative integer (a fill value, for
//! example) is skipped.
//!
//! Every limit violation is a [`CfRadialError::LimitExceeded`] error. An
//! optional variable that is missing or malformed is ignored, but one that
//! exceeds a limit fails the decode.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

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
    /// The file declares more data than a documented resource limit allows
    /// (see the crate-level `# Limits` section).
    #[error("decode limit exceeded: {0}")]
    LimitExceeded(String),
}
