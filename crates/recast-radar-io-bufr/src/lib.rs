//! Pure-Rust WMO BUFR decoder and Meteo-France polar radar reader.
//!
//! [`read_meteofrance_volume`] reads a Meteo-France radar file, the
//! gzip-compressed BUFR that the Meteo-France radar API and archives
//! distribute: a PAG (Doppler: reflectivity, its standard deviation and
//! radial velocity) or PAM (dual polarization: reflectivity, RHOHV, PHIDP,
//! ZDR) image of one elevation. Several files of one scan merge into one
//! volume with `recast_radar_core::model::merge_volumes`.
//!
//! Underneath is a general BUFR reader (WMO-No. 306, Manual on Codes,
//! FM 94 BUFR, editions 2 to 4), written from the WMO specification:
//! [`messages`] splits a file into messages, [`decode_message`] expands a
//! message's descriptors against [`Tables`] (the WMO master tables plus
//! Meteo-France's local tables, embedded) and reads its data, keeping a
//! replicated single element (a pixel array) as one run of codes. Data
//! compressed across subsets, associated fields and quality operators
//! (2-03, 2-04, 2-22 and later) are not read.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod container;
mod decode;
mod lzw;
mod message;
mod meteofrance;
mod tables;

pub use container::{expand, looks_like_bufr_bytes};
pub use decode::{Item, Value, decode_message, fxy};
pub use message::{Message, messages, parse as parse_message};
pub use meteofrance::read_meteofrance_volume;
pub use tables::{Element, ElementKind, Tables};

/// Why a BUFR file could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BufrError {
    /// The gzip or compress wrapping is damaged.
    #[error("BUFR container: {0}")]
    Container(String),
    /// A message breaks the BUFR format.
    #[error("BUFR: {0}")]
    Format(String),
    /// Valid BUFR this crate does not read.
    #[error("BUFR not supported: {0}")]
    Unsupported(String),
    /// The file exceeds a decoding limit.
    #[error("BUFR limit: {0}")]
    Limit(String),
}
