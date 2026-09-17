//! DORADE sweepfile decoding and mobile-radar deployment archive ingest
//! (DOW/COW/RaXPol) into [`recast_radar_core::RadarVolume`].
//!
//! - [`dorade`]: native DORADE `swp.*` sweepfile decoder.
//! - [`mobile_archive`]: zip-archive and folder ingest that groups DORADE
//!   sweeps into volume scans. Level II (`.msg31`/`AR2V`) members inside
//!   those archives are decoded by a caller-supplied decoder, so this crate
//!   depends only on `recast-radar-core`.

pub mod dorade;
pub mod mobile_archive;

use thiserror::Error;

pub use dorade::{
    decode_dorade_sweep_volume, decode_dorade_volume_from_paths, decode_dorade_volume_from_slices,
    looks_like_dorade_bytes, peek_dorade_sweep,
};
pub use mobile_archive::{
    MobileVolume, decode_dorade_volume_for_path, decode_mobile_archive_from_path,
    decode_mobile_dir_from_path, looks_like_zip_bytes,
};

/// Result type for DORADE and mobile-archive decoding.
pub type Result<T> = std::result::Result<T, DoradeError>;

/// Errors from DORADE sweepfile and mobile-archive decoding.
#[derive(Debug, Error)]
pub enum DoradeError {
    /// A sweepfile, archive, or directory could not be read.
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A descriptor block or ray ended before its declared length.
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
    /// An archive member failed to expand within its size limits.
    #[error("unsupported or corrupt compression wrapper: {0}")]
    Compression(String),
    /// Structurally invalid sweepfile or archive content.
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
