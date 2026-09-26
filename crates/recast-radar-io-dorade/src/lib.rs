//! DORADE sweepfile decoding and mobile-radar deployment archive ingest
//! (DOW/COW/RaXPol) into the FM301 [`recast_radar_core::model::Volume`].
//!
//! - [`dorade`]: native DORADE `swp.*` sweepfile decoder
//!   ([`read_dorade_sweep_volume`], [`dorade::DoradeVolumeBuilder`]).
//! - [`mobile_archive`]: zip-archive and folder ingest that groups DORADE
//!   sweeps into volume scans. Level II (`.msg31`/`AR2V`) members inside
//!   those archives are decoded by a caller-supplied decoder, so this crate
//!   depends only on `recast-radar-core`.
//!
//! # Limits
//!
//! Sweepfiles:
//!
//! - **Gates**: CELV, CSFD, and extended PARM gate counts, run-length decoded
//!   rows, and uncompressed RDAT rows are limited to `MAX_GATES_PER_RADIAL`
//!   (16,384); real sweeps reach 1,002.
//! - **Cells per sweep**: at most 67,108,864 decoded cells are retained
//!   while a sweep's rays are collected.
//! - **Volume**: at most `MAX_SWEEPS_PER_VOLUME` (1,024) sweeps. The ray
//!   tables and fields (rows padded to the widest row of their field) of
//!   every appended sweep are charged to a `DecodeBudget` of
//!   `MAX_DECODED_VOLUME_BYTES` (1 GiB) before the fields are built.
//!
//! Mobile-radar archives and folders:
//!
//! - At most 4,096 candidate members of at most 256 MiB each, 1 GiB of
//!   member bytes in total, and folder recursion 4 levels deep.
//! - The decoded volumes of one archive or folder may retain at most
//!   `MAX_DECODED_BATCH_BYTES` (2 GiB). Members decode in parallel, so
//!   volumes still in flight on other threads can briefly add to that.
//!
//! Shared constants live in [`recast_radar_core::bounded_read`]. Gate,
//! cell, sweep, and budget violations are [`DoradeError::LimitExceeded`]
//! errors; archive size violations are [`DoradeError::InvalidMessage`] or
//! [`DoradeError::Compression`] errors.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod descriptors;
pub mod dorade;
pub mod mobile_archive;

use thiserror::Error;

pub use dorade::{
    DoradeVolumeBuilder, looks_like_dorade_bytes, peek_dorade_sweep, read_dorade_sweep_volume,
    read_dorade_volume_from_paths, read_dorade_volume_from_slices,
};
pub use mobile_archive::{
    MobileVolume, looks_like_zip_bytes, read_dorade_volume_for_path,
    read_mobile_archive_from_bytes, read_mobile_archive_from_path, read_mobile_dir_from_path,
};

/// Result type for DORADE and mobile-archive decoding.
pub type Result<T> = std::result::Result<T, DoradeError>;

/// Errors from DORADE sweepfile and mobile-archive decoding.
#[derive(Debug, Error)]
#[non_exhaustive]
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
    /// The input declares more data than a documented resource limit allows
    /// (see the crate-level `# Limits` section).
    #[error("decode limit exceeded: {0}")]
    LimitExceeded(String),
}
