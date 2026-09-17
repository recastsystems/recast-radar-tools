//! NEXRAD Archive II / Level II decoder entry points.
//!
//! The `read_*` functions decode into the FM301 model
//! ([`recast_radar_core::model::Volume`], `docs/design/fm301-model.md`): one
//! sweep per elevation cut in acquisition order, fields named as xradar names
//! them (REF→`DBZH`, VEL→`VRADH`, SW→`WRADH`, ZDR, PHI→`PHIDP`, RHO→`RHOHV`,
//! CFP→`CCORH`), raw `u8`/`u16` gates with the ICD coding
//! ([`recast_radar_core::model::IntCoding::nexrad`]) and each moment in its
//! native gate geometry on the sweep range. `Sweep::fixed_angle_deg` is the
//! VCP cut angle from Message 5 when the file has one (what xradar and
//! Py-ART report), else the opening radial's elevation. A moment whose gates
//! cannot share the sweep range (garbage radials of misframed files) is
//! dropped for that sweep.
//!
//! Unsupported records stay non-fatal so an app can inspect partially decoded
//! volumes while the edge-case corpus grows.
//!
//! Other radar formats live in their own crates (`recast-radar-io-odim`,
//! `-io-cfradial`, `-io-dorade`, `-io-jma`); `recast-radar-io` routes byte
//! buffers of unknown format to the right decoder, and
//! `recast-radar-io-level3` decodes Level III products (including the VAD Wind
//! Profile, `recast_radar_io_level3::vwp`).
//! [`messages`] walks the Level II message stream and decodes the message
//! bodies (message 1 radials are left to the volume decoder).
//! [`read_volume_with_metadata`] ([`metadata`]) returns the decoded volume
//! together with its metadata messages and per-sweep message 31 constant
//! blocks.
//!
//! # Limits
//!
//! Allocations never follow header values past these caps (shared values in
//! [`recast_radar_core::bounded_read`]):
//!
//! - **Expanded input**: gzip, whole-file bzip2, and uncompressed buffers at
//!   most `MAX_DECODED_RADAR_BYTES` (512 MiB). LDM block-bzip2 volumes: at
//!   most 4,096 blocks, 16 MiB per decompressed block, 512 MiB in total.
//!   Violations are [`NexradError::Compression`] errors.
//! - **Messages**: sizes are 16-bit halfword counts (at most 131,070 bytes);
//!   every block pointer and gate array is checked against its message.
//! - **Gates**: a Message 1 row or Message 31 moment block may declare at
//!   most `MAX_GATES_PER_RADIAL` (16,384) gates; real volumes reach 1,840.
//! - **Cuts**: at most `MAX_SWEEPS_PER_VOLUME` (1,024) elevation cuts; real
//!   volumes reach 23.
//! - **Decoded moments**: field reservations and growth are checked against a
//!   `DecodeBudget` of `MAX_DECODED_VOLUME_BYTES` (1 GiB) per volume before
//!   allocating; real volumes need at most 80 MiB. Ray tables are not
//!   charged: every radial consumes at least 88 bytes of expanded input and
//!   occupies 40, so the expanded-input cap bounds them.
//!
//! Gate, cut, and budget violations are [`NexradError::LimitExceeded`]
//! errors. Preview callbacks receive a sealed copy of the partial volume, so
//! memory briefly doubles while one runs. [`read_volume_from_path`] reads the
//! whole file first; callers choose which files to open.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod builder;
pub mod messages;
pub mod metadata;

pub use metadata::{NexradMetadata, NexradVolume, SweepElevationData, read_volume_with_metadata};

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

use bzip2::bufread::BzDecoder;
use chrono::{DateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use recast_radar_core::bounded_read::{self, DecodeBudget, MAX_DECODED_RADAR_BYTES};
use recast_radar_core::model::Volume;
use thiserror::Error;

use crate::builder::{BlockGates, MomentBlock, MomentPayload, VolumeBuilder};
const VOLUME_HEADER_LEN: usize = 24;
const CONTROL_WORD_LEN: usize = 12;
const MESSAGE_HEADER_LEN: usize = 16;
const RECORD_BYTES: usize = 2432;
const MSG_31_HEADER_LEN: usize = 72;
const MSG_1_HEADER_LEN: usize = 100;
const GENERIC_DATA_BLOCK_LEN: usize = 28;
const VOLUME_CONSTANT_BLOCK_LEN: usize = 44;
const RADIAL_CONSTANT_BLOCK_LEN: usize = 20;
const HALF_DEGREE_RADIALS_PER_CUT: usize = 720;
const ONE_DEGREE_RADIALS_PER_CUT: usize = 360;
const FALLBACK_RADIALS_PER_CUT: usize = 760;
const MAX_MESSAGE_31_MOMENTS: usize = 10;
/// LDM block-bzip chunks are normally a few hundred KiB decoded. Keep a
/// separate per-block ceiling so many workers cannot each inflate an
/// attacker-controlled block all the way to the full-volume budget.
const MAX_BZIP_BLOCK_DECODED_BYTES: usize = 16 * 1024 * 1024;
const MAX_BZIP_BLOCKS: usize = 4096;

pub type Result<T> = std::result::Result<T, NexradError>;

#[derive(Debug, Error)]
pub enum NexradError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("input is too short for an Archive II volume header: {actual} bytes")]
    ShortVolumeHeader { actual: usize },
    /// The input does not start with an Archive II volume header (`AR2V` or
    /// `ARCHIVE2`, Table I of the Archive II ICD 2620010). Model-data
    /// (`_MDM`) files, intermediate real-time chunks and bare records have
    /// none: their messages are read with [`messages::record_bytes`] and
    /// [`messages::MessageWalker`], and their metadata with
    /// [`NexradMetadata::from_metadata_record`]. Without a header there is no
    /// site or volume time, so no [`Volume`] is built from them.
    #[error(
        "no Archive II volume header: the input starts with `{found}`, not AR2V or ARCHIVE2 (model-data _MDM files and intermediate real-time chunks have no header; read their messages with messages::MessageWalker)"
    )]
    MissingVolumeHeader {
        /// The first 8 input bytes, ASCII-escaped.
        found: String,
    },
    #[error("truncated {what} at offset {offset}: need {needed} bytes, have {available}")]
    Truncated {
        what: &'static str,
        offset: usize,
        needed: usize,
        available: usize,
    },
    #[error("unsupported or corrupt compression wrapper: {0}")]
    Compression(String),
    #[error("invalid message at offset {offset}: {reason}")]
    InvalidMessage { offset: usize, reason: String },
    /// The file declares more data than a documented resource limit allows
    /// (see the crate-level `# Limits` section).
    #[error("decode limit exceeded: {0}")]
    LimitExceeded(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveCompression {
    Gzip,
    Bzip2WholeFile,
    Bzip2Blocks,
    Uncompressed,
}

impl ArchiveCompression {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Bzip2WholeFile => "bzip2-whole-file",
            Self::Bzip2Blocks => "bzip2-blocks",
            Self::Uncompressed => "uncompressed",
        }
    }
}

/// Message 1 / Message 31 radial status (ICD 2620002): where a radial sits in
/// its elevation cut and volume scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RadialStatus {
    StartElevation,
    Intermediate,
    EndElevation,
    StartVolume,
    EndVolume,
    StartElevationLastCut,
    Unknown(u8),
}

impl From<u8> for RadialStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::StartElevation,
            1 => Self::Intermediate,
            2 => Self::EndElevation,
            3 => Self::StartVolume,
            4 => Self::EndVolume,
            5 => Self::StartElevationLastCut,
            other => Self::Unknown(other),
        }
    }
}

/// Decode a local Archive II / Level II file into the FM301 model.
pub fn read_volume_from_path(path: &Path) -> Result<Volume> {
    let mut volume = read_volume_from_bytes(&read_file(path)?)?;
    volume.provenance.source_path = Some(path.display().to_string());
    Ok(volume)
}

pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| NexradError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Decode an Archive II / Level II byte buffer (gzip, whole-file bzip2, LDM
/// block-bzip2 or uncompressed) into the FM301 model.
pub fn read_volume_from_bytes(bytes: &[u8]) -> Result<Volume> {
    Ok(builder_from_bytes(bytes)?.finish()?.0)
}

/// Receives each message 31 body the volume decoders turn into a radial,
/// right after the radial is added, with the volume as it then stands.
/// [`metadata::read_volume_with_metadata`] uses it to read the constant
/// blocks of each sweep's first radial during the single decode pass; the
/// plain decoders pass `()`, whose empty implementation compiles away.
trait RadialObserver {
    fn message_31(&mut self, body: &[u8], volume: &Volume);
}

impl RadialObserver for () {
    #[inline(always)]
    fn message_31(&mut self, _body: &[u8], _volume: &Volume) {}
}

pub(crate) fn builder_from_bytes(bytes: &[u8]) -> Result<VolumeBuilder> {
    builder_observed(bytes, &mut ())
}

/// [`builder_from_bytes`] with a [`RadialObserver`].
fn builder_observed(bytes: &[u8], observer: &mut impl RadialObserver) -> Result<VolumeBuilder> {
    if bytes.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader {
            actual: bytes.len(),
        });
    }
    if !bytes.starts_with(&[0x1f, 0x8b])
        && !bytes.starts_with(b"BZh")
        && let Some(blocks) = collect_bzip_block_slices(bytes)?
    {
        return decode_bzip_blocks_pipelined(bytes, blocks, None, false, |_| Ok(()), observer)
            .map(|outcome| outcome.builder);
    }

    let (bytes, compression) = normalize_archive_bytes(bytes)?;
    builder_from_normalized_observed(&bytes, compression, DecodeBudget::volume(), observer)
}

/// Decode a gzip-wrapped Archive II stream into the FM301 model.
pub fn read_gzip_volume_from_reader(reader: impl Read) -> Result<Volume> {
    Ok(builder_from_gzip_reader(reader)?.finish()?.0)
}

pub(crate) fn builder_from_gzip_reader(reader: impl Read) -> Result<VolumeBuilder> {
    let decoder = GzDecoder::new(reader);
    let mut decoder = ReadLimit::new(decoder, MAX_DECODED_RADAR_BYTES, "gzip radar payload");
    decode_volume_from_stream_until(&mut decoder, ArchiveCompression::Gzip, None).map(|result| {
        debug_assert!(!result.stopped_at_preview);
        result.builder
    })
}

/// Decode a gzip-wrapped volume, calling `on_preview` once with a sealed copy
/// of the volume as soon as its first displayable sweep completes (see
/// [`read_volume_from_bytes_with_bzip_preview`]). Other wrappers decode
/// normally without a preview.
pub fn read_gzip_volume_from_bytes_with_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_preview: F,
) -> Result<Volume>
where
    F: FnMut(Volume),
{
    builder_from_gzip_bytes_with_preview(raw, min_displayable_radials, |builder| {
        on_preview(builder.snapshot()?);
        Ok(())
    })?
    .finish()
    .map(|(volume, _)| volume)
}

pub(crate) fn builder_from_gzip_bytes_with_preview(
    raw: &[u8],
    min_displayable_radials: usize,
    on_preview: impl FnMut(&VolumeBuilder) -> Result<()>,
) -> Result<VolumeBuilder> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }
    if !raw.starts_with(&[0x1f, 0x8b]) {
        return builder_from_bytes(raw);
    }

    let decoder = GzDecoder::new(raw);
    let mut decoder = ReadLimit::new(decoder, MAX_DECODED_RADAR_BYTES, "gzip radar payload");
    decode_volume_from_stream(
        &mut decoder,
        ArchiveCompression::Gzip,
        Some(min_displayable_radials),
        false,
        on_preview,
    )
    .map(|result| {
        debug_assert!(!result.stopped_at_preview);
        result.builder
    })
}

/// Decode a gzip-wrapped volume only until its first displayable sweep
/// completes; `None` for other wrappers or when no sweep completes.
pub fn read_gzip_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<Volume>> {
    match builder_gzip_preview(raw, min_displayable_radials)? {
        Some(builder) => Ok(Some(builder.finish()?.0)),
        None => Ok(None),
    }
}

pub(crate) fn builder_gzip_preview(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<VolumeBuilder>> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }
    if !raw.starts_with(&[0x1f, 0x8b]) {
        return Ok(None);
    }

    let decoder = GzDecoder::new(raw);
    let mut decoder = ReadLimit::new(decoder, MAX_DECODED_RADAR_BYTES, "gzip radar payload");
    let result = decode_volume_from_stream_until(
        &mut decoder,
        ArchiveCompression::Gzip,
        Some(min_displayable_radials),
    )?;
    Ok(result.stopped_at_preview.then_some(result.builder))
}

/// Decode a completed first displayable sweep from NEXRAD block-bzip Level II
/// bytes.
///
/// This is intended for UI preview on low-core machines: it returns `None` for
/// gzip, whole-file bzip, uncompressed, or malformed block-bzip inputs, and it
/// never substitutes for the final full-volume decode.
pub fn read_bzip_block_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<Volume>> {
    match builder_bzip_block_preview(raw, min_displayable_radials)? {
        Some(builder) => Ok(Some(builder.finish()?.0)),
        None => Ok(None),
    }
}

pub(crate) fn builder_bzip_block_preview(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<VolumeBuilder>> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }

    let Some(blocks) = collect_bzip_block_slices(raw)? else {
        return Ok(None);
    };

    let outcome = decode_bzip_blocks_pipelined(
        raw,
        blocks,
        Some(min_displayable_radials),
        true,
        |_| Ok(()),
        &mut (),
    )?;
    Ok(outcome.stopped_at_preview.then_some(outcome.builder))
}

/// Decode a full volume while optionally emitting an early completed
/// first-sweep preview.
///
/// For block-bzip Level II files, parsing streams behind the parallel block
/// decompression, so the preview is emitted as soon as the first displayable
/// sweep completes — without decompressing or parsing anything twice. Other
/// compression formats fall back to a normal full decode.
pub fn read_volume_from_bytes_with_bzip_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_preview: F,
) -> Result<Volume>
where
    F: FnMut(Volume),
{
    builder_with_bzip_preview(raw, min_displayable_radials, |builder| {
        on_preview(builder.snapshot()?);
        Ok(())
    })?
    .finish()
    .map(|(volume, _)| volume)
}

pub(crate) fn builder_with_bzip_preview(
    raw: &[u8],
    min_displayable_radials: usize,
    on_preview: impl FnMut(&VolumeBuilder) -> Result<()>,
) -> Result<VolumeBuilder> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }

    let Some(blocks) = collect_bzip_block_slices(raw)? else {
        return builder_from_bytes(raw);
    };

    let outcome = decode_bzip_blocks_pipelined(
        raw,
        blocks,
        Some(min_displayable_radials),
        false,
        on_preview,
        &mut (),
    )?;
    Ok(outcome.builder)
}

/// Decompress or normalize an Archive II byte slice before Level II parsing.
pub fn normalize_archive_bytes(raw: &[u8]) -> Result<(Vec<u8>, ArchiveCompression)> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }

    if raw.starts_with(&[0x1f, 0x8b]) {
        let decoded = decompress_gzip_bytes(raw)?;
        return Ok((decoded, ArchiveCompression::Gzip));
    }

    if raw.starts_with(b"BZh") {
        let decoded = read_to_end_limited(
            BzDecoder::new(Cursor::new(raw)),
            MAX_DECODED_RADAR_BYTES,
            "whole-file bzip2 radar payload",
        )?;
        return Ok((decoded, ArchiveCompression::Bzip2WholeFile));
    }

    if let Some(decoded) = try_decode_bzip_blocks(raw)? {
        return Ok((decoded, ArchiveCompression::Bzip2Blocks));
    }

    Ok((
        copy_bytes_limited(raw, MAX_DECODED_RADAR_BYTES, "uncompressed radar payload")?,
        ArchiveCompression::Uncompressed,
    ))
}

fn decompress_gzip_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    read_to_end_limited(
        GzDecoder::new(raw),
        MAX_DECODED_RADAR_BYTES,
        "gzip radar payload",
    )
}

/// [`bounded_read::read_to_end_limited`] with its message wrapped as a
/// compression error.
fn read_to_end_limited(reader: impl Read, limit: usize, context: &'static str) -> Result<Vec<u8>> {
    bounded_read::read_to_end_limited(reader, limit, context).map_err(NexradError::Compression)
}

/// [`bounded_read::copy_bytes_limited`] with its message wrapped as a
/// compression error.
fn copy_bytes_limited(bytes: &[u8], limit: usize, context: &'static str) -> Result<Vec<u8>> {
    bounded_read::copy_bytes_limited(bytes, limit, context).map_err(NexradError::Compression)
}

/// Streaming expansion guard used by the preview decoders. It permits EOF
/// exactly at the limit, but probes one byte further before reporting EOF so
/// an over-limit stream cannot be mistaken for a complete volume.
struct ReadLimit<R> {
    inner: R,
    remaining: usize,
    context: &'static str,
}

impl<R> ReadLimit<R> {
    fn new(inner: R, limit: usize, context: &'static str) -> Self {
        Self {
            inner,
            remaining: limit,
            context,
        }
    }
}

impl<R: Read> Read for ReadLimit<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{} expands beyond the {}-byte limit",
                        self.context, MAX_DECODED_RADAR_BYTES
                    ),
                )),
            };
        }
        let allowed = buffer.len().min(self.remaining);
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.remaining -= count;
        Ok(count)
    }
}

fn read_record_prefix<R: Read>(reader: &mut R, buffer: &mut [u8], offset: usize) -> Result<bool> {
    let mut read = 0;
    while read < buffer.len() {
        let count = reader
            .read(&mut buffer[read..])
            .map_err(|err| NexradError::Compression(err.to_string()))?;
        if count == 0 {
            if read == 0 {
                return Ok(false);
            }
            return Err(NexradError::Truncated {
                what: "record prefix",
                offset,
                needed: buffer.len(),
                available: read,
            });
        }
        read += count;
    }
    Ok(true)
}

fn read_exact_required<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    what: &'static str,
    offset: usize,
) -> Result<()> {
    let mut read = 0;
    while read < buffer.len() {
        let count = reader
            .read(&mut buffer[read..])
            .map_err(|err| NexradError::Compression(err.to_string()))?;
        if count == 0 {
            return Err(NexradError::Truncated {
                what,
                offset,
                needed: buffer.len(),
                available: read,
            });
        }
        read += count;
    }
    Ok(())
}

fn read_exact_into_buffer<R: Read>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    len: usize,
    what: &'static str,
    offset: usize,
) -> Result<()> {
    // The buffer is reused across records without clearing: `resize` only
    // zero-fills bytes past the previous length, and every byte below `len`
    // is overwritten by the read (or the caller discards the buffer on error).
    buffer.resize(len, 0);
    read_exact_required(reader, buffer, what, offset)
}

fn skip_record_padding<R: Read>(
    reader: &mut R,
    record_len: usize,
    consumed: usize,
    record_offset: usize,
) -> Result<()> {
    let padding = record_len.saturating_sub(consumed);
    skip_exact(reader, padding, "record padding", record_offset + consumed)
}

fn skip_exact<R: Read>(
    reader: &mut R,
    mut bytes: usize,
    what: &'static str,
    offset: usize,
) -> Result<()> {
    let mut buffer = [0; 8192];
    let mut skipped = 0;
    while bytes > 0 {
        let chunk = bytes.min(buffer.len());
        let target = &mut buffer[..chunk];
        read_exact_required(reader, target, what, offset + skipped)?;
        bytes -= chunk;
        skipped += chunk;
    }
    Ok(())
}

/// Parse already-normalized Archive II bytes into the FM301 model.
pub fn read_normalized_volume_bytes(
    bytes: &[u8],
    compression: ArchiveCompression,
) -> Result<Volume> {
    Ok(
        builder_from_normalized(bytes, compression, DecodeBudget::volume())?
            .finish()?
            .0,
    )
}

/// [`read_normalized_volume_bytes`] with an explicit output budget.
pub(crate) fn builder_from_normalized(
    bytes: &[u8],
    compression: ArchiveCompression,
    budget: DecodeBudget,
) -> Result<VolumeBuilder> {
    builder_from_normalized_observed(bytes, compression, budget, &mut ())
}

/// [`read_normalized_volume_bytes`] with an explicit output budget and a
/// [`RadialObserver`].
fn builder_from_normalized_observed(
    bytes: &[u8],
    compression: ArchiveCompression,
    budget: DecodeBudget,
    observer: &mut impl RadialObserver,
) -> Result<VolumeBuilder> {
    let volume_header = parse_volume_header(bytes)?;
    let mut builder = VolumeBuilder::new(
        volume_header.icao,
        volume_header.archive_version,
        volume_header.volume_time,
        compression,
        budget,
    );

    let mut cursor = VOLUME_HEADER_LEN;
    let mut record_index = 0usize;
    // GR2-style ".msg31" exports keep the AR2V header but carry only a few
    // metadata records before variable-framed message 31s — well before the
    // standard 134 fixed records. Detected once at the first early message
    // 31 and latched for the rest of the file.
    let mut early_variable_msg31 = false;
    while cursor + CONTROL_WORD_LEN + MESSAGE_HEADER_LEN <= bytes.len() {
        let header_offset = cursor + CONTROL_WORD_LEN;
        let header =
            parse_message_header_bytes(&bytes[header_offset..header_offset + MESSAGE_HEADER_LEN]);

        if header.size_halfwords == 0 && record_index < 134 {
            builder.count_skipped();
            cursor = cursor.saturating_add(RECORD_BYTES);
            record_index += 1;
            continue;
        } else if header.size_halfwords == 0 {
            break;
        }

        let (message_total_len, variable_framing) = message_framing(&header);
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        builder.count_message();
        match header.message_type {
            1 => {
                let message_end = header_offset + message_total_len;
                if message_end > bytes.len() {
                    if builder.decoded_radials() > 0 {
                        builder.count_skipped();
                        break;
                    }
                    return Err(NexradError::Truncated {
                        what: "message 1 body",
                        offset: header_offset,
                        needed: message_total_len,
                        available: bytes.len().saturating_sub(header_offset),
                    });
                }
                let body = &bytes[header_offset + MESSAGE_HEADER_LEN..message_end];
                parse_message_1(body, &header, &mut builder)?;
            }
            31 => {
                let message_end = header_offset + message_total_len;
                if message_end > bytes.len() {
                    if builder.decoded_radials() > 0 {
                        builder.count_skipped();
                        break;
                    }
                    return Err(NexradError::Truncated {
                        what: "message 31 body",
                        offset: header_offset,
                        needed: message_total_len,
                        available: bytes.len().saturating_sub(header_offset),
                    });
                }
                let body = &bytes[header_offset + MESSAGE_HEADER_LEN..message_end];
                parse_message_31(body, &header, &mut builder)?;
                observer.message_31(body, &builder.volume);
            }
            5 => {
                let body_offset = header_offset + MESSAGE_HEADER_LEN;
                let fixed_record_end = cursor.saturating_add(RECORD_BYTES).min(bytes.len());
                let message_end = header_offset.saturating_add(message_total_len);
                let body_end = message_end.min(fixed_record_end);
                if body_offset < body_end {
                    builder.set_vcp_message(&bytes[body_offset..body_end]);
                }
            }
            _ => builder.count_skipped(),
        }

        let record_len = if variable_framing {
            message_total_len + CONTROL_WORD_LEN
        } else if header.message_type != 31 {
            RECORD_BYTES
        } else if record_index >= 134 || early_variable_msg31 {
            message_total_len + CONTROL_WORD_LEN
        } else if message31_uses_variable_framing(bytes, cursor, message_total_len) {
            early_variable_msg31 = true;
            message_total_len + CONTROL_WORD_LEN
        } else {
            RECORD_BYTES
        };
        cursor = cursor.saturating_add(record_len);
        record_index += 1;
    }

    Ok(builder)
}

/// Decide the framing of a message 31 seen before the standard 134 metadata
/// records. Real Archive II volumes never place message 31 that early, but
/// GR2-style ".msg31" exports (DOW/COW/RaXPol Level II twins) do, packing
/// them back to back with no fixed-record padding. Returns `true` when the
/// bytes directly after this message hold another message 31 (or the file
/// ends exactly there), which fixed 2432-byte framing cannot produce.
fn message31_uses_variable_framing(bytes: &[u8], cursor: usize, message_total_len: usize) -> bool {
    let variable_next = cursor + CONTROL_WORD_LEN + message_total_len;
    if variable_next == bytes.len() {
        return true;
    }
    let header_offset = variable_next + CONTROL_WORD_LEN;
    let Some(header_bytes) = bytes.get(header_offset..header_offset + MESSAGE_HEADER_LEN) else {
        return false;
    };
    let header = parse_message_header_bytes(header_bytes);
    header.message_type == 31
        && usize::from(header.size_halfwords) * 2 >= MESSAGE_HEADER_LEN + MSG_31_HEADER_LEN
}

struct StreamDecodeResult {
    builder: VolumeBuilder,
    stopped_at_preview: bool,
}

fn decode_volume_from_stream_until<R: Read>(
    reader: &mut R,
    compression: ArchiveCompression,
    preview_min_radials: Option<usize>,
) -> Result<StreamDecodeResult> {
    decode_volume_from_stream(reader, compression, preview_min_radials, true, |_| Ok(()))
}

fn decode_volume_from_stream<R: Read, F>(
    reader: &mut R,
    compression: ArchiveCompression,
    preview_min_radials: Option<usize>,
    stop_at_preview: bool,
    mut on_preview: F,
) -> Result<StreamDecodeResult>
where
    F: FnMut(&VolumeBuilder) -> Result<()>,
{
    let mut volume_header_bytes = [0; VOLUME_HEADER_LEN];
    read_exact_required(reader, &mut volume_header_bytes, "volume header", 0)?;
    let volume_header = parse_volume_header(&volume_header_bytes)?;
    let mut builder = VolumeBuilder::new(
        volume_header.icao,
        volume_header.archive_version,
        volume_header.volume_time,
        compression,
        DecodeBudget::volume(),
    );

    let mut cursor = VOLUME_HEADER_LEN;
    let mut record_index = 0usize;
    let mut prefix = [0; CONTROL_WORD_LEN + MESSAGE_HEADER_LEN];
    let mut body_buffer = Vec::with_capacity(RECORD_BYTES);
    let mut preview_emitted = false;
    while read_record_prefix(reader, &mut prefix, cursor)? {
        let header_offset = cursor + CONTROL_WORD_LEN;
        let header = parse_message_header_bytes(&prefix[CONTROL_WORD_LEN..]);

        if header.size_halfwords == 0 && record_index < 134 {
            builder.count_skipped();
            skip_exact(
                reader,
                RECORD_BYTES - prefix.len(),
                "empty fixed record",
                cursor + prefix.len(),
            )?;
            cursor = cursor.saturating_add(RECORD_BYTES);
            record_index += 1;
            continue;
        } else if header.size_halfwords == 0 {
            break;
        }

        let (message_total_len, variable_framing) = message_framing(&header);
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        let record_len = if variable_framing {
            message_total_len + CONTROL_WORD_LEN
        } else if record_index < 134 || header.message_type != 31 {
            RECORD_BYTES
        } else {
            message_total_len + CONTROL_WORD_LEN
        };
        let body_len = message_total_len - MESSAGE_HEADER_LEN;
        builder.count_message();

        match header.message_type {
            1 | 31 => {
                let what = if header.message_type == 1 {
                    "message 1 body"
                } else {
                    "message 31 body"
                };
                if let Err(err) =
                    read_exact_into_buffer(reader, &mut body_buffer, body_len, what, header_offset)
                {
                    if builder.decoded_radials() > 0 {
                        builder.count_skipped();
                        break;
                    }
                    return Err(err);
                }
                if header.message_type == 1 {
                    parse_message_1(&body_buffer, &header, &mut builder)?;
                } else {
                    parse_message_31(&body_buffer, &header, &mut builder)?;
                }
                skip_record_padding(reader, record_len, prefix.len() + body_len, cursor)?;
                if let Some(min_radials) = preview_min_radials
                    && !preview_emitted
                    && builder.has_complete_displayable_sweep(min_radials)
                {
                    preview_emitted = true;
                    if stop_at_preview {
                        return Ok(StreamDecodeResult {
                            builder,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(&builder)?;
                }
            }
            5 => {
                let fixed_body_len = RECORD_BYTES.saturating_sub(prefix.len());
                let body_read_len = body_len.min(fixed_body_len);
                read_exact_into_buffer(
                    reader,
                    &mut body_buffer,
                    body_read_len,
                    "message 5 body",
                    header_offset,
                )?;
                builder.set_vcp_message(&body_buffer);
                skip_record_padding(reader, record_len, prefix.len() + body_read_len, cursor)?;
            }
            _ => {
                builder.count_skipped();
                skip_record_padding(reader, record_len, prefix.len(), cursor)?;
            }
        }

        cursor = cursor.saturating_add(record_len);
        record_index += 1;
    }

    Ok(StreamDecodeResult {
        builder,
        stopped_at_preview: false,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VolumeHeader {
    archive_version: String,
    volume_time: DateTime<Utc>,
    icao: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageHeader {
    pub size_halfwords: u16,
    pub channels: u8,
    pub message_type: u8,
    pub sequence_id: u16,
    pub date: u16,
    pub milliseconds: u32,
    pub segments: u16,
    pub segment_number: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message31Header {
    /// Bytes 0-3: radar identifier as recorded. It can be blank (KVWX
    /// 2008-04-15 records four spaces); see [`Self::radar_identifier_or`].
    pub radar_identifier: [u8; 4],
    pub collect_ms: u32,
    pub collect_date: u16,
    pub azimuth_number: u16,
    pub azimuth_angle: f32,
    pub radial_length: u16,
    pub azimuth_resolution: u8,
    pub radial_status: RadialStatus,
    pub elevation_number: u8,
    pub cut_sector: u8,
    pub elevation_angle: f32,
    pub block_pointers: [usize; 10],
}

/// Outcome of decompressing one LDM block: its bytes, or the error message.
type BlockResult = std::result::Result<Vec<u8>, String>;

/// Slot store connecting parallel LDM-block decompression workers to the
/// in-order streaming parser.
///
/// Indices are claimed in parse order through `next_claim`, so each slot is
/// filled by exactly one thread. A slot is a `OnceLock` that is set once with
/// the block's bytes (or error) and never written again, so the parser can
/// borrow published bytes for the lifetime of the store without copying.
struct BlockSlots<'a> {
    compressed: Vec<&'a [u8]>,
    slots: Box<[OnceLock<BlockResult>]>,
    next_claim: AtomicUsize,
    decoded_bytes: AtomicUsize,
    canceled: AtomicBool,
    wakeup: Mutex<()>,
    published: Condvar,
}

impl<'a> BlockSlots<'a> {
    fn new(compressed: Vec<&'a [u8]>) -> Self {
        let len = compressed.len();
        Self {
            compressed,
            slots: (0..len).map(|_| OnceLock::new()).collect(),
            next_claim: AtomicUsize::new(0),
            decoded_bytes: AtomicUsize::new(0),
            canceled: AtomicBool::new(false),
            wakeup: Mutex::new(()),
            published: Condvar::new(),
        }
    }

    fn len(&self) -> usize {
        self.compressed.len()
    }

    fn cancel(&self) {
        self.canceled.store(true, Ordering::Relaxed);
    }

    fn run_worker(&self) {
        while !self.canceled.load(Ordering::Relaxed) {
            let index = self.next_claim.fetch_add(1, Ordering::Relaxed);
            if index >= self.len() {
                break;
            }
            self.decompress_index(index);
        }
    }

    fn decompress_index(&self, index: usize) {
        match decompress_bzip_block(self.compressed[index]) {
            Ok(decoded) => {
                if reserve_atomic_budget(
                    &self.decoded_bytes,
                    decoded.len(),
                    MAX_DECODED_RADAR_BYTES,
                ) {
                    self.publish(index, Ok(decoded));
                } else {
                    self.publish(
                        index,
                        Err(format!(
                            "block-bzip radar payload expands beyond the {MAX_DECODED_RADAR_BYTES}-byte aggregate limit"
                        )),
                    );
                    self.cancel();
                }
            }
            Err(err) => self.publish(index, Err(err.to_string())),
        }
    }

    fn publish(&self, index: usize, result: BlockResult) {
        // `index` was claimed exactly once via `next_claim`, so the slot is
        // still empty; `set` cannot fail here and would never overwrite.
        let _ = self.slots[index].set(result);
        // Take the wakeup lock so a parser that has checked the slot but not
        // yet parked cannot miss this notification. The mutex guards no data,
        // so a poisoned lock is still usable.
        drop(self.wakeup.lock().unwrap_or_else(PoisonError::into_inner));
        self.published.notify_all();
    }

    /// Block until the decompressed contents of `index` are available.
    ///
    /// The caller participates in decompression while it waits (claims advance
    /// in parse order), so the pipeline makes progress even when no rayon
    /// worker ever runs — e.g. on a single-threaded pool.
    fn wait_block(&self, index: usize) -> Result<&[u8]> {
        loop {
            if let Some(result) = self.slots[index].get() {
                return match result {
                    Ok(bytes) => Ok(bytes.as_slice()),
                    Err(message) => Err(NexradError::Compression(message.clone())),
                };
            }
            let claimed = self.next_claim.fetch_add(1, Ordering::Relaxed);
            if claimed < self.len() {
                self.decompress_index(claimed);
                continue;
            }
            // Everything is claimed, so `index` is in flight on another
            // thread; park until the next publish.
            let mut guard = self.wakeup.lock().unwrap_or_else(PoisonError::into_inner);
            while self.slots[index].get().is_none() {
                guard = self
                    .published
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

struct BzipBlockCursor<'a> {
    volume_header: &'a [u8],
    blocks: &'a BlockSlots<'a>,
    chunk_index: usize,
    chunk_offset: usize,
    absolute_offset: usize,
    current: Option<&'a [u8]>,
}

impl<'a> BzipBlockCursor<'a> {
    fn new(volume_header: &'a [u8], blocks: &'a BlockSlots<'a>) -> Self {
        Self {
            volume_header,
            blocks,
            chunk_index: 0,
            chunk_offset: 0,
            absolute_offset: 0,
            current: None,
        }
    }

    fn current_chunk(&mut self) -> Result<Option<&'a [u8]>> {
        if let Some(chunk) = self.current {
            return Ok(Some(chunk));
        }
        let chunk = match self.chunk_index {
            0 => Some(self.volume_header),
            index if index - 1 < self.blocks.len() => Some(self.blocks.wait_block(index - 1)?),
            _ => None,
        };
        self.current = chunk;
        Ok(chunk)
    }

    fn advance_chunk(&mut self) {
        self.chunk_index += 1;
        self.chunk_offset = 0;
        self.current = None;
    }

    fn skip_empty_chunks(&mut self) -> Result<()> {
        while let Some(chunk) = self.current_chunk()? {
            if self.chunk_offset < chunk.len() {
                break;
            }
            self.advance_chunk();
        }
        Ok(())
    }

    fn read_exact_into(
        &mut self,
        mut output: &mut [u8],
        what: &'static str,
        offset: usize,
    ) -> Result<()> {
        let mut written = 0;
        while !output.is_empty() {
            self.skip_empty_chunks()?;
            let Some(chunk) = self.current_chunk()? else {
                return Err(NexradError::Truncated {
                    what,
                    offset,
                    needed: written + output.len(),
                    available: written,
                });
            };
            let available = &chunk[self.chunk_offset..];
            let count = available.len().min(output.len());
            output[..count].copy_from_slice(&available[..count]);
            self.chunk_offset += count;
            self.absolute_offset += count;
            let (_, rest) = output.split_at_mut(count);
            output = rest;
            written += count;
        }
        Ok(())
    }

    fn read_optional_prefix(&mut self, output: &mut [u8], offset: usize) -> Result<bool> {
        self.skip_empty_chunks()?;
        if self.current_chunk()?.is_none() {
            return Ok(false);
        }
        self.read_exact_into(output, "record prefix", offset)?;
        Ok(true)
    }

    fn read_slice_or_copy<'b>(
        &'b mut self,
        scratch: &'b mut Vec<u8>,
        len: usize,
        what: &'static str,
        offset: usize,
    ) -> Result<&'b [u8]> {
        scratch.clear();
        if len == 0 {
            return Ok(&[]);
        }
        self.skip_empty_chunks()?;
        let Some(chunk) = self.current_chunk()? else {
            return Err(NexradError::Truncated {
                what,
                offset,
                needed: len,
                available: 0,
            });
        };
        if self.chunk_offset + len <= chunk.len() {
            let start = self.chunk_offset;
            self.chunk_offset += len;
            self.absolute_offset += len;
            return Ok(&chunk[start..start + len]);
        }

        if scratch.capacity() < len {
            scratch.reserve_exact(len - scratch.capacity());
        }
        let mut remaining = len;
        while remaining > 0 {
            self.skip_empty_chunks()?;
            let Some(chunk) = self.current_chunk()? else {
                return Err(NexradError::Truncated {
                    what,
                    offset,
                    needed: len,
                    available: scratch.len(),
                });
            };
            let available = &chunk[self.chunk_offset..];
            let count = available.len().min(remaining);
            scratch.extend_from_slice(&available[..count]);
            self.chunk_offset += count;
            self.absolute_offset += count;
            remaining -= count;
        }
        Ok(scratch.as_slice())
    }

    fn skip_exact(&mut self, len: usize, what: &'static str, offset: usize) -> Result<()> {
        let mut skipped = 0;
        while skipped < len {
            self.skip_empty_chunks()?;
            let Some(chunk) = self.current_chunk()? else {
                return Err(NexradError::Truncated {
                    what,
                    offset,
                    needed: len,
                    available: skipped,
                });
            };
            let count = (len - skipped).min(chunk.len() - self.chunk_offset);
            self.chunk_offset += count;
            self.absolute_offset += count;
            skipped += count;
        }
        Ok(())
    }
}

struct BlockParseOutcome {
    builder: VolumeBuilder,
    stopped_at_preview: bool,
}

/// Decode a block-bzip volume by parsing in lockstep with the parallel block
/// decompression: rayon workers fill `BlockSlots` while this thread parses
/// blocks in order, waiting (or stealing decompression work) only when the
/// next block is not ready yet. Total wall time is the decompression wall time
/// instead of decompression followed by a serial parse.
#[allow(clippy::too_many_arguments)]
fn decode_bzip_blocks_pipelined(
    raw: &[u8],
    blocks: Vec<&[u8]>,
    min_displayable_radials: Option<usize>,
    stop_at_preview: bool,
    on_preview: impl FnMut(&VolumeBuilder) -> Result<()>,
    observer: &mut impl RadialObserver,
) -> Result<BlockParseOutcome> {
    let slots = BlockSlots::new(blocks);
    rayon::in_place_scope(|scope| {
        // Leave one hardware thread for the parsing thread below; it also
        // steals decompression work whenever it would otherwise wait.
        let workers = rayon::current_num_threads()
            .saturating_sub(1)
            .max(1)
            .min(slots.len());
        for _ in 0..workers {
            scope.spawn(|_| slots.run_worker());
        }
        let outcome = parse_bzip_block_volume(
            &raw[..VOLUME_HEADER_LEN],
            &slots,
            min_displayable_radials,
            stop_at_preview,
            on_preview,
            observer,
        );
        // Stop idle claims if the parse returned early (preview-only or error).
        slots.cancel();
        outcome
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_bzip_block_volume(
    volume_header: &[u8],
    blocks: &BlockSlots<'_>,
    min_displayable_radials: Option<usize>,
    stop_at_preview: bool,
    mut on_preview: impl FnMut(&VolumeBuilder) -> Result<()>,
    observer: &mut impl RadialObserver,
) -> Result<BlockParseOutcome> {
    let mut preview_pending = min_displayable_radials;
    let mut cursor_reader = BzipBlockCursor::new(volume_header, blocks);
    let mut volume_header_buffer = Vec::new();
    let volume_header_bytes = cursor_reader.read_slice_or_copy(
        &mut volume_header_buffer,
        VOLUME_HEADER_LEN,
        "volume header",
        0,
    )?;
    let volume_header = parse_volume_header(volume_header_bytes)?;
    let mut builder = VolumeBuilder::new(
        volume_header.icao,
        volume_header.archive_version,
        volume_header.volume_time,
        ArchiveCompression::Bzip2Blocks,
        DecodeBudget::volume(),
    );

    let mut cursor = VOLUME_HEADER_LEN;
    let mut record_index = 0usize;
    let mut prefix = [0; CONTROL_WORD_LEN + MESSAGE_HEADER_LEN];
    let mut body_buffer = Vec::with_capacity(RECORD_BYTES);
    loop {
        match cursor_reader.read_optional_prefix(&mut prefix, cursor) {
            Ok(true) => {}
            Ok(false) => break,
            // A damaged block after at least one decoded radial degrades to a
            // partial volume, mirroring the message-31 body handling below.
            Err(err) => {
                if builder.decoded_radials() > 0 {
                    builder.count_skipped();
                    break;
                }
                return Err(err);
            }
        }
        let header_offset = cursor + CONTROL_WORD_LEN;
        let header = parse_message_header_bytes(&prefix[CONTROL_WORD_LEN..]);

        if header.size_halfwords == 0 && record_index < 134 {
            builder.count_skipped();
            cursor_reader.skip_exact(
                RECORD_BYTES - prefix.len(),
                "empty fixed record",
                cursor + prefix.len(),
            )?;
            cursor = cursor.saturating_add(RECORD_BYTES);
            record_index += 1;
            continue;
        } else if header.size_halfwords == 0 {
            break;
        }

        let (message_total_len, variable_framing) = message_framing(&header);
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        let record_len = if variable_framing {
            message_total_len + CONTROL_WORD_LEN
        } else if record_index < 134 || header.message_type != 31 {
            RECORD_BYTES
        } else {
            message_total_len + CONTROL_WORD_LEN
        };
        let body_len = message_total_len - MESSAGE_HEADER_LEN;
        builder.count_message();

        match header.message_type {
            1 | 31 => {
                let what = if header.message_type == 1 {
                    "message 1 body"
                } else {
                    "message 31 body"
                };
                let body = match cursor_reader.read_slice_or_copy(
                    &mut body_buffer,
                    body_len,
                    what,
                    header_offset,
                ) {
                    Ok(body) => body,
                    Err(err) => {
                        if builder.decoded_radials() > 0 {
                            builder.count_skipped();
                            break;
                        }
                        return Err(err);
                    }
                };
                if header.message_type == 1 {
                    parse_message_1(body, &header, &mut builder)?;
                } else {
                    parse_message_31(body, &header, &mut builder)?;
                    observer.message_31(body, &builder.volume);
                }
                if let Some(min_radials) = preview_pending
                    && builder.has_complete_displayable_sweep(min_radials)
                {
                    preview_pending = None;
                    if stop_at_preview {
                        return Ok(BlockParseOutcome {
                            builder,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(&builder)?;
                }
                cursor_reader.skip_exact(
                    record_len.saturating_sub(prefix.len() + body_len),
                    "record padding",
                    cursor + prefix.len() + body_len,
                )?;
            }
            5 => {
                let fixed_body_len = RECORD_BYTES.saturating_sub(prefix.len());
                let body_read_len = body_len.min(fixed_body_len);
                let body = cursor_reader.read_slice_or_copy(
                    &mut body_buffer,
                    body_read_len,
                    "message 5 body",
                    header_offset,
                )?;
                builder.set_vcp_message(body);
                cursor_reader.skip_exact(
                    record_len.saturating_sub(prefix.len() + body_read_len),
                    "record padding",
                    cursor + prefix.len() + body_read_len,
                )?;
            }
            _ => {
                builder.count_skipped();
                cursor_reader.skip_exact(
                    record_len.saturating_sub(prefix.len()),
                    "record padding",
                    cursor + prefix.len(),
                )?;
            }
        }

        cursor = cursor.saturating_add(record_len);
        record_index += 1;
    }

    Ok(BlockParseOutcome {
        builder,
        stopped_at_preview: false,
    })
}

fn try_decode_bzip_blocks(raw: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(decoded_blocks) = try_decompress_bzip_blocks(raw)? else {
        return Ok(None);
    };

    let decoded_len = decoded_blocks.iter().try_fold(0usize, |total, block| {
        total.checked_add(block.len()).filter(|sum| *sum <= MAX_DECODED_RADAR_BYTES)
    }).ok_or_else(|| {
        NexradError::Compression(format!(
            "block-bzip radar payload expands beyond the {MAX_DECODED_RADAR_BYTES}-byte aggregate limit"
        ))
    })?;
    let output_len = VOLUME_HEADER_LEN
        .checked_add(decoded_len)
        .ok_or_else(|| NexradError::Compression("block-bzip output size overflow".to_owned()))?;
    let mut output = Vec::new();
    output.try_reserve_exact(output_len).map_err(|err| {
        NexradError::Compression(format!("cannot reserve block-bzip output: {err}"))
    })?;
    output.extend_from_slice(&raw[..VOLUME_HEADER_LEN]);
    for block in decoded_blocks {
        output.extend(block);
    }

    Ok(Some(output))
}

fn try_decompress_bzip_blocks(raw: &[u8]) -> Result<Option<Vec<Vec<u8>>>> {
    let Some(blocks) = collect_bzip_block_slices(raw)? else {
        return Ok(None);
    };

    let decoded_bytes = AtomicUsize::new(0);
    let decoded_blocks = blocks
        .par_iter()
        .map(|compressed| {
            let decoded = decompress_bzip_block(compressed)?;
            if !reserve_atomic_budget(
                &decoded_bytes,
                decoded.len(),
                MAX_DECODED_RADAR_BYTES,
            ) {
                return Err(NexradError::Compression(format!(
                    "block-bzip radar payload expands beyond the {MAX_DECODED_RADAR_BYTES}-byte aggregate limit"
                )));
            }
            Ok(decoded)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Some(decoded_blocks))
}

fn collect_bzip_block_slices(raw: &[u8]) -> Result<Option<Vec<&[u8]>>> {
    if raw.len() < VOLUME_HEADER_LEN + 4 {
        return Ok(None);
    }

    let mut cursor = VOLUME_HEADER_LEN;
    let mut blocks = Vec::new();

    while cursor + 4 <= raw.len() {
        let signed_block_size = i32_at(raw, cursor)?;
        if signed_block_size == -1 && cursor + 4 == raw.len() {
            break;
        }
        if signed_block_size == 0 {
            return Ok(None);
        }

        cursor += 4;
        let is_last_block = signed_block_size < 0;
        let block_size = usize::try_from(signed_block_size.unsigned_abs())
            .map_err(|_| NexradError::Compression("bzip2 block size overflow".to_owned()))?;
        if cursor + block_size > raw.len() {
            return Ok(None);
        }

        let compressed = &raw[cursor..cursor + block_size];
        if !compressed.starts_with(b"BZh") {
            return Ok(None);
        }

        blocks.push(compressed);
        if blocks.len() > MAX_BZIP_BLOCKS {
            return Err(NexradError::Compression(format!(
                "block-bzip volume contains more than {MAX_BZIP_BLOCKS} blocks"
            )));
        }
        cursor += block_size;
        if is_last_block {
            break;
        }
    }

    if blocks.is_empty() {
        return Ok(None);
    }

    Ok(Some(blocks))
}

fn decompress_bzip_block(compressed: &[u8]) -> Result<Vec<u8>> {
    read_to_end_limited(
        BzDecoder::new(Cursor::new(compressed)),
        MAX_BZIP_BLOCK_DECODED_BYTES,
        "block-bzip chunk",
    )
}

fn reserve_atomic_budget(total: &AtomicUsize, additional: usize, limit: usize) -> bool {
    total
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(additional)
                .filter(|next| *next <= limit)
        })
        .is_ok()
}

/// True when `bytes` start with an Archive II volume header tape name:
/// `AR2V` (Build 5 on, 2004) or `ARCHIVE2` (1991-2003).
pub(crate) fn starts_with_volume_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"AR2V") || bytes.starts_with(b"ARCHIVE2")
}

/// Parse the 24-byte volume header. Bytes that do not start with `AR2V` or
/// `ARCHIVE2` are [`NexradError::MissingVolumeHeader`]: the volume decoders
/// never treat headerless bytes (a model-data file, an intermediate
/// real-time chunk, compressed data) as a header followed by records.
fn parse_volume_header(bytes: &[u8]) -> Result<VolumeHeader> {
    require_len(bytes, 0, VOLUME_HEADER_LEN, "volume header")?;
    if !starts_with_volume_header(bytes) {
        return Err(NexradError::MissingVolumeHeader {
            found: bytes[..8].escape_ascii().to_string(),
        });
    }
    let tape = ascii_trim(&bytes[0..9]);
    let extension = ascii_trim(&bytes[9..12]);
    let date = u32_at(bytes, 12)?;
    let milliseconds = u32_at(bytes, 16)?;
    let icao = ascii_trim(&bytes[20..24]);

    Ok(VolumeHeader {
        archive_version: format!("{tape}{extension}"),
        volume_time: nexrad_date_ms_to_datetime(date, milliseconds),
        icao,
    })
}

/// Framing of one message in the volume decoders: the message length from
/// its header (Table II: the size halfwords, or the byte count in the segment
/// fields when the size is the 0xFFFF sentinel, notes 6 and 7) and whether
/// the message has its own variable-length record instead of a fixed
/// 2432-byte frame. Message 29 (model data) and every extended-size message
/// are variable length, like the walker treats them; message 31 framing is
/// decided by the caller (fixed inside the metadata record, variable after
/// it). Without this, a message 29 was skipped as one 2432-byte frame and
/// its remaining bytes were parsed as message headers and radials.
fn message_framing(header: &MessageHeader) -> (usize, bool) {
    let total_len = header.message_len();
    let variable = header.has_extended_size() || header.message_type == 29;
    (total_len, variable)
}

pub fn parse_message_header(bytes: &[u8], offset: usize) -> Result<MessageHeader> {
    require_len(bytes, offset, MESSAGE_HEADER_LEN, "message header")?;
    Ok(parse_message_header_bytes(
        &bytes[offset..offset + MESSAGE_HEADER_LEN],
    ))
}

fn parse_message_header_bytes(bytes: &[u8]) -> MessageHeader {
    debug_assert!(bytes.len() >= MESSAGE_HEADER_LEN);
    MessageHeader {
        size_halfwords: be_u16(bytes, 0),
        channels: bytes[2],
        message_type: bytes[3],
        sequence_id: be_u16(bytes, 4),
        date: be_u16(bytes, 6),
        milliseconds: be_u32(bytes, 8),
        segments: be_u16(bytes, 12),
        segment_number: be_u16(bytes, 14),
    }
}

fn parse_message_1(
    body: &[u8],
    _message_header: &MessageHeader,
    builder: &mut VolumeBuilder,
) -> Result<()> {
    require_len(body, 0, MSG_1_HEADER_LEN, "message 1 header")?;

    let collect_ms = be_u32(body, 0);
    let collect_date = be_u16(body, 4);
    if builder.decoded_radials() == 0 && collect_date > 0 {
        builder.set_header_time(nexrad_date_ms_to_datetime(
            u32::from(collect_date),
            collect_ms,
        ));
    }

    let azimuth_angle = legacy_binary_angle_deg(be_u16(body, 8));
    let radial_status = RadialStatus::from(be_u16(body, 12) as u8);
    let elevation_angle = legacy_binary_angle_deg(be_u16(body, 14));
    let elevation_number = (be_u16(body, 16) as u8).max(1);

    let reflectivity_gates = BlockGates {
        first_gate_m: i32::from(be_i16(body, 18)),
        gate_spacing_m: i32::from(be_u16(body, 22).max(1)),
        gate_count: usize::from(be_u16(body, 26)),
    };
    let doppler_gates = BlockGates {
        first_gate_m: i32::from(be_i16(body, 20)),
        gate_spacing_m: i32::from(be_u16(body, 24).max(1)),
        gate_count: usize::from(be_u16(body, 28)),
    };
    let reflectivity_pointer = usize::from(be_u16(body, 36));
    let velocity_pointer = usize::from(be_u16(body, 38));
    let spectrum_width_pointer = usize::from(be_u16(body, 40));
    let velocity_resolution = be_u16(body, 42);

    builder.set_vcp(be_u16(body, 44));
    let nyquist_velocity_mps = match be_i16(body, 46) {
        raw if raw > 0 => Some(raw as f32 / 100.0),
        _ => None,
    };

    let reflectivity_row =
        legacy_message_1_row(body, reflectivity_pointer, reflectivity_gates.gate_count);
    let velocity_row = legacy_message_1_row(body, velocity_pointer, doppler_gates.gate_count);
    let spectrum_width_row =
        legacy_message_1_row(body, spectrum_width_pointer, doppler_gates.gate_count);
    if reflectivity_row.is_none() && velocity_row.is_none() && spectrum_width_row.is_none() {
        builder.count_skipped();
        return Ok(());
    }
    for row in [reflectivity_row, velocity_row, spectrum_width_row]
        .into_iter()
        .flatten()
    {
        bounded_read::check_gate_count(row.len(), "message 1 moment")
            .map_err(NexradError::LimitExceeded)?;
    }

    let sweep = builder.sweep_for_radial(radial_status, elevation_angle, elevation_number)?;
    let ray = builder.push_ray(
        sweep,
        collect_date,
        collect_ms,
        azimuth_angle,
        elevation_angle,
        nyquist_velocity_mps,
        radial_status,
        ONE_DEGREE_RADIALS_PER_CUT,
    );

    // ICD 2620002 message 1 encodes REF as (code - 66) / 2 and VEL and SW
    // like velocity at 0.5 m/s resolution ((code - 129) / 2), with code 0
    // below threshold and 1 range folded.
    let moments = [
        (reflectivity_row, *b"REF", reflectivity_gates, 2.0),
        (
            velocity_row,
            *b"VEL",
            doppler_gates,
            legacy_message_1_velocity_scale(velocity_resolution),
        ),
        (spectrum_width_row, *b"SW ", doppler_gates, 2.0),
    ];
    for (row, name, gates, scale) in moments {
        let Some(row) = row else {
            continue;
        };
        let block = MomentBlock {
            name,
            gates,
            scale,
            offset: if name == *b"REF" { 66.0 } else { 129.0 },
            row: MomentPayload::U8(row),
        };
        builder.push_moment(sweep, ray, &block, ONE_DEGREE_RADIALS_PER_CUT)?;
    }

    builder.count_radial();
    Ok(())
}

fn legacy_message_1_row(body: &[u8], pointer: usize, gate_count: usize) -> Option<&[u8]> {
    if pointer == 0 || gate_count == 0 {
        return None;
    }
    body.get(pointer..pointer.checked_add(gate_count)?)
}

fn legacy_message_1_velocity_scale(velocity_resolution: u16) -> f32 {
    match velocity_resolution {
        2 => 2.0,
        4 => 1.0,
        _ => 2.0,
    }
}

fn legacy_binary_angle_deg(raw: u16) -> f32 {
    raw as f32 * 360.0 / 65_536.0
}

fn parse_message_31(
    body: &[u8],
    _message_header: &MessageHeader,
    builder: &mut VolumeBuilder,
) -> Result<()> {
    let header = parse_message_31_header(body, 0)?;
    let expected_radials = expected_radials_for_azimuth_resolution(header.azimuth_resolution);

    // GR2-style ".msg31" exports write a nonstandard volume-header date, so
    // the volume time parses as the epoch; recover it from the first
    // radial's collection time instead.
    if builder.header_time == DateTime::<Utc>::UNIX_EPOCH && header.collect_date > 0 {
        builder.set_header_time(nexrad_date_ms_to_datetime(
            u32::from(header.collect_date),
            header.collect_ms,
        ));
    }

    let mut nyquist_velocity_mps = None;
    let mut moments: [Option<MomentBlock<'_>>; MAX_MESSAGE_31_MOMENTS] =
        std::array::from_fn(|_| None);
    let mut moment_count = 0;
    let needs_volume_constants = builder.needs_volume_constants();

    for pointer in &header.block_pointers {
        if *pointer == 0 {
            continue;
        }
        let pointer = *pointer;
        if pointer > body.len().saturating_sub(4) {
            continue;
        }

        match body[pointer] {
            b'R' if &body[pointer + 1..pointer + 4] == b"VOL" => {
                if needs_volume_constants {
                    parse_volume_constant_block(body, pointer, builder)?;
                }
            }
            b'R' if &body[pointer + 1..pointer + 4] == b"RAD" => {
                nyquist_velocity_mps = parse_radial_constant_block(body, pointer)?;
            }
            b'D' if moment_count < moments.len() => {
                moments[moment_count] = Some(parse_generic_moment_block(body, pointer)?);
                moment_count += 1;
            }
            _ => {}
        }
    }

    let sweep = builder.sweep_for_radial(
        header.radial_status,
        header.elevation_angle,
        header.elevation_number,
    )?;
    let ray = builder.push_ray(
        sweep,
        header.collect_date,
        header.collect_ms,
        header.azimuth_angle,
        header.elevation_angle,
        nyquist_velocity_mps,
        header.radial_status,
        expected_radials,
    );

    for moment in moments.iter().take(moment_count).flatten() {
        builder.push_moment(sweep, ray, moment, expected_radials)?;
    }

    builder.count_radial();
    Ok(())
}

fn expected_radials_for_azimuth_resolution(azimuth_resolution: u8) -> usize {
    match azimuth_resolution {
        1 => HALF_DEGREE_RADIALS_PER_CUT,
        2 => ONE_DEGREE_RADIALS_PER_CUT,
        _ => FALLBACK_RADIALS_PER_CUT,
    }
}

impl Message31Header {
    /// The radar identifier: the message 31 identifier with spaces and NULs
    /// trimmed, or, when that is blank, `volume_header_icao` trimmed (which
    /// may be empty too).
    pub fn radar_identifier_or(&self, volume_header_icao: &str) -> String {
        radar_identifier_or(&self.radar_identifier, volume_header_icao)
    }
}

/// A message 31 radar identifier, falling back to the volume header ICAO when
/// the identifier is blank (spaces or NULs), and then to the empty string.
pub(crate) fn radar_identifier_or(identifier: &[u8; 4], volume_header_icao: &str) -> String {
    let identifier = ascii_trim(identifier);
    if identifier.is_empty() {
        volume_header_icao
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_owned()
    } else {
        identifier
    }
}

/// Parse the Data Header Block of a message 31 body. A blank radar
/// identifier (spaces or NULs) is accepted: real files have one (KVWX
/// 2008-04-15), and MetPy and Py-ART read them.
pub fn parse_message_31_header(bytes: &[u8], offset: usize) -> Result<Message31Header> {
    require_len(bytes, offset, MSG_31_HEADER_LEN, "message 31 header")?;
    let bytes = &bytes[offset..offset + MSG_31_HEADER_LEN];

    let mut block_pointers = [0; 10];
    for (index, pointer) in block_pointers.iter_mut().enumerate() {
        *pointer = be_u32(bytes, 32 + index * 4) as usize;
    }

    Ok(Message31Header {
        radar_identifier: [bytes[0], bytes[1], bytes[2], bytes[3]],
        collect_ms: be_u32(bytes, 4),
        collect_date: be_u16(bytes, 8),
        azimuth_number: be_u16(bytes, 10),
        azimuth_angle: be_f32(bytes, 12),
        radial_length: be_u16(bytes, 18),
        azimuth_resolution: bytes[20],
        radial_status: RadialStatus::from(bytes[21]),
        elevation_number: bytes[22],
        cut_sector: bytes[23],
        elevation_angle: be_f32(bytes, 24),
        block_pointers,
    })
}

fn parse_volume_constant_block(
    bytes: &[u8],
    offset: usize,
    builder: &mut VolumeBuilder,
) -> Result<()> {
    require_len(
        bytes,
        offset,
        VOLUME_CONSTANT_BLOCK_LEN,
        "volume constant block",
    )?;
    let bytes = &bytes[offset..offset + VOLUME_CONSTANT_BLOCK_LEN];
    let location = &mut builder.volume.location;
    location.latitude_deg = Some(f64::from(be_f32(bytes, 8)));
    location.longitude_deg = Some(f64::from(be_f32(bytes, 12)));

    // Antenna height above MSL: tower plus feedhorn, as xradar and Py-ART.
    let tower_height_m = be_i16(bytes, 16) as f32;
    let feedhorn_height_m = be_u16(bytes, 18) as f32;
    location.altitude_m = Some(f64::from(tower_height_m + feedhorn_height_m));

    builder.set_vcp(be_u16(bytes, 40));
    Ok(())
}

fn parse_radial_constant_block(bytes: &[u8], offset: usize) -> Result<Option<f32>> {
    require_len(
        bytes,
        offset,
        RADIAL_CONSTANT_BLOCK_LEN,
        "radial constant block",
    )?;
    let raw = be_i16(&bytes[offset..offset + RADIAL_CONSTANT_BLOCK_LEN], 16);
    Ok((raw > 0).then_some(raw as f32 / 100.0))
}

fn parse_generic_moment_block(bytes: &[u8], offset: usize) -> Result<MomentBlock<'_>> {
    require_len(
        bytes,
        offset,
        GENERIC_DATA_BLOCK_LEN,
        "generic moment block",
    )?;
    let header = &bytes[offset..offset + GENERIC_DATA_BLOCK_LEN];
    let name = [header[1], header[2], header[3]];
    let gate_count = usize::from(be_u16(header, 8));
    bounded_read::check_gate_count(gate_count, "message 31 moment block")
        .map_err(NexradError::LimitExceeded)?;
    let first_gate_m = i32::from(be_i16(header, 10));
    let gate_spacing_m = i32::from(be_i16(header, 12));
    let word_size = header[19];
    let scale = be_f32(header, 20);
    let offset_value = be_f32(header, 24);
    let data_offset = offset + GENERIC_DATA_BLOCK_LEN;

    let row = match word_size {
        8 => {
            require_len(bytes, data_offset, gate_count, "8-bit moment gates")?;
            MomentPayload::U8(&bytes[data_offset..data_offset + gate_count])
        }
        16 => {
            let byte_count = gate_count
                .checked_mul(2)
                .ok_or(NexradError::InvalidMessage {
                    offset,
                    reason: "16-bit moment gate count overflow".to_owned(),
                })?;
            require_len(bytes, data_offset, byte_count, "16-bit moment gates")?;
            MomentPayload::U16(&bytes[data_offset..data_offset + byte_count])
        }
        other => {
            return Err(NexradError::InvalidMessage {
                offset,
                reason: format!("unsupported moment word size {other}"),
            });
        }
    };

    Ok(MomentBlock {
        name,
        gates: BlockGates {
            first_gate_m,
            gate_spacing_m,
            gate_count,
        },
        scale,
        offset: offset_value,
        row,
    })
}

fn nexrad_date_ms_to_datetime(date: u32, milliseconds: u32) -> DateTime<Utc> {
    let days = i64::from(date.saturating_sub(1));
    let seconds = days * 86_400 + i64::from(milliseconds / 1000);
    let nanos = (milliseconds % 1000) * 1_000_000;
    Utc.timestamp_opt(seconds, nanos)
        .single()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn ascii_trim(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

fn require_len(bytes: &[u8], offset: usize, needed: usize, what: &'static str) -> Result<()> {
    let available = bytes.len().saturating_sub(offset);
    if available < needed {
        Err(NexradError::Truncated {
            what,
            offset,
            needed,
            available,
        })
    } else {
        Ok(())
    }
}

fn be_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn be_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn be_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_bits(be_u32(bytes, offset))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    require_len(bytes, offset, 4, "u32")?;
    Ok(u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn i32_at(bytes: &[u8], offset: usize) -> Result<i32> {
    require_len(bytes, offset, 4, "i32")?;
    Ok(i32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bzip2::write::BzEncoder;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use recast_radar_core::model::{FieldName, Quantity};
    use std::io::Write;

    fn field(volume: &Volume, sweep: usize, name: FieldName) -> &recast_radar_core::Field {
        volume.sweeps[sweep]
            .field(&name)
            .unwrap_or_else(|| panic!("sweep {sweep} has no {name}"))
    }

    #[test]
    fn parses_archive_volume_header() {
        let bytes = synthetic_archive(false);
        let header = parse_volume_header(&bytes).unwrap();

        assert_eq!(header.archive_version, "AR2V000001");
        assert_eq!(header.icao, "KTLX");
        assert_eq!(
            header.volume_time,
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 1).unwrap()
        );
    }

    #[test]
    fn parses_message_header() {
        let bytes = synthetic_archive(false);
        let header = parse_message_header(&bytes, VOLUME_HEADER_LEN + CONTROL_WORD_LEN).unwrap();

        assert_eq!(header.message_type, 31);
        assert_eq!(header.sequence_id, 7);
        assert!(usize::from(header.size_halfwords) * 2 >= MESSAGE_HEADER_LEN + MSG_31_HEADER_LEN);
    }

    #[test]
    fn parses_message_31_header() {
        let body = synthetic_message_31_body(false);
        let header = parse_message_31_header(&body, 0).unwrap();

        assert_eq!(&header.radar_identifier, b"AR2V");
        assert_eq!(header.radar_identifier_or("KTLX"), "AR2V");
        assert_eq!(header.azimuth_number, 1);
        assert_eq!(header.azimuth_angle, 180.5);
        assert_eq!(header.elevation_angle, 0.5);
        assert_eq!(header.radial_status, RadialStatus::StartVolume);
        assert_eq!(header.block_pointers[0], 72);
        assert_eq!(header.block_pointers[3], 136);
    }

    #[test]
    fn decodes_synthetic_message_31_volume() {
        let bytes = synthetic_archive(false);
        let volume = read_volume_from_bytes(&bytes).unwrap();

        assert_eq!(volume.attrs.instrument_name, "KTLX");
        assert_eq!(volume.location.latitude_deg, Some(f64::from(35.333f32)));
        assert_eq!(volume.scan.vcp_pattern, Some(212));
        assert_eq!(volume.scan.name.as_deref(), Some("VCP-212"));
        assert_eq!(volume.scan.id, Some(212));
        assert_eq!(volume.sweeps.len(), 1);
        assert_eq!(volume.sweeps[0].nrays(), 1);
        assert_eq!(volume.sweeps[0].elevation_number, Some(1));
        assert_eq!(volume.sweeps[0].rays.azimuth_deg, vec![180.5]);
        assert_eq!(volume.sweeps[0].rays.time_s, vec![0.0]);
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 1).unwrap()
        );

        let reflectivity = field(&volume, 0, FieldName::Dbzh);
        assert_eq!(reflectivity.quantity, Quantity::Reflectivity);
        assert_eq!(reflectivity.shape(), (1, 3));
        assert_eq!(reflectivity.value(0, 1), Some(0.0));
        assert_eq!(reflectivity.value(0, 2), Some(7.0));
        assert_eq!(
            reflectivity.native_geometry(&volume.sweeps[0].range),
            Some((0.0, 250.0))
        );
    }

    #[test]
    fn decodes_legacy_message_1_reflectivity_and_velocity() {
        let mut body = vec![0u8; 106];
        body[0..4].copy_from_slice(&1_000u32.to_be_bytes());
        body[4..6].copy_from_slice(&19_724u16.to_be_bytes());
        body[8..10].copy_from_slice(&0u16.to_be_bytes());
        body[12..14].copy_from_slice(&3u16.to_be_bytes());
        body[14..16].copy_from_slice(&91u16.to_be_bytes());
        body[16..18].copy_from_slice(&1u16.to_be_bytes());
        body[18..20].copy_from_slice(&0i16.to_be_bytes());
        body[20..22].copy_from_slice(&(-375i16).to_be_bytes());
        body[22..24].copy_from_slice(&1000u16.to_be_bytes());
        body[24..26].copy_from_slice(&250u16.to_be_bytes());
        body[26..28].copy_from_slice(&3u16.to_be_bytes());
        body[28..30].copy_from_slice(&3u16.to_be_bytes());
        body[36..38].copy_from_slice(&100u16.to_be_bytes());
        body[38..40].copy_from_slice(&103u16.to_be_bytes());
        body[42..44].copy_from_slice(&2u16.to_be_bytes());
        body[44..46].copy_from_slice(&31u16.to_be_bytes());
        body[46..48].copy_from_slice(&1500i16.to_be_bytes());
        body[100..103].copy_from_slice(&[0, 66, 86]);
        body[103..106].copy_from_slice(&[129, 131, 127]);
        let header = MessageHeader {
            size_halfwords: ((MESSAGE_HEADER_LEN + body.len()) / 2) as u16,
            channels: 0,
            message_type: 1,
            sequence_id: 1,
            date: 19_724,
            milliseconds: 1_000,
            segments: 1,
            segment_number: 1,
        };
        let mut builder = VolumeBuilder::new(
            "KBPP".to_owned(),
            "AR2V0001".to_owned(),
            DateTime::<Utc>::UNIX_EPOCH,
            ArchiveCompression::Uncompressed,
            DecodeBudget::volume(),
        );

        parse_message_1(&body, &header, &mut builder).unwrap();
        let expected_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 1).unwrap();
        assert_eq!(builder.header_time, expected_time);
        let volume = builder.finish().unwrap().0;

        assert_eq!(volume.time_reference, expected_time);
        assert_eq!(volume.scan.vcp_pattern, Some(31));
        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        assert_eq!(volume.sweeps.len(), 1);
        assert_eq!(volume.sweeps[0].nrays(), 1);
        assert_eq!(
            volume.sweeps[0].ray_vars.nyquist_velocity_mps,
            Some(vec![15.0])
        );

        // REF 1 km gates centred from 0 m and Doppler 250 m gates centred
        // from -375 m (the ICD layout) share one 250 m range from -375 m: the
        // reflectivity maps with stride 4 (design note 6.5).
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.range.spacing_m(), Some(250.0));
        assert_eq!(sweep.range.center_m(0), Some(-375.0));
        let reflectivity = field(&volume, 0, FieldName::Dbzh);
        assert_eq!(
            reflectivity.gates,
            recast_radar_core::GateMapping {
                start: 0,
                stride: 4
            }
        );
        assert_eq!(
            reflectivity.native_geometry(&sweep.range),
            Some((0.0, 1000.0))
        );
        assert_eq!(reflectivity.value(0, 0), None);
        assert_eq!(reflectivity.value(0, 1), Some(0.0));
        assert_eq!(reflectivity.value(0, 2), Some(10.0));

        let velocity = field(&volume, 0, FieldName::Vradh);
        assert_eq!(velocity.gates.stride, 1);
        assert_eq!(velocity.value(0, 0), Some(0.0));
        assert_eq!(velocity.value(0, 1), Some(1.0));
        assert_eq!(velocity.value(0, 2), Some(-1.0));
    }

    #[test]
    fn decodes_legacy_message_1_spectrum_width_with_velocity_offset() {
        let mut body = vec![0u8; 103];
        body[12..14].copy_from_slice(&3u16.to_be_bytes());
        body[24..26].copy_from_slice(&250u16.to_be_bytes());
        body[28..30].copy_from_slice(&3u16.to_be_bytes());
        body[40..42].copy_from_slice(&100u16.to_be_bytes());
        body[100..103].copy_from_slice(&[0, 133, 129]);
        let header = MessageHeader {
            size_halfwords: ((MESSAGE_HEADER_LEN + body.len()) / 2) as u16,
            channels: 0,
            message_type: 1,
            sequence_id: 1,
            date: 19_724,
            milliseconds: 1_000,
            segments: 1,
            segment_number: 1,
        };
        let mut builder = VolumeBuilder::new(
            "KCRI".to_owned(),
            "AR2V0001".to_owned(),
            DateTime::<Utc>::UNIX_EPOCH,
            ArchiveCompression::Uncompressed,
            DecodeBudget::volume(),
        );

        parse_message_1(&body, &header, &mut builder).unwrap();
        let volume = builder.finish().unwrap().0;

        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        let spectrum_width = field(&volume, 0, FieldName::Wradh);
        // ICD 2620002: SW = (code - 129) / 2, so code 133 -> 2.0 m/s and code
        // 129 -> 0.0 m/s. The old offset of 2.0 biased both by +63.5 m/s.
        assert_eq!(spectrum_width.value(0, 0), None);
        assert_eq!(spectrum_width.value(0, 1), Some(2.0));
        assert_eq!(spectrum_width.value(0, 2), Some(0.0));
    }

    #[test]
    fn decodes_gzip_stream_without_normalized_buffer() {
        let bytes = synthetic_archive(false);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let volume = read_volume_from_bytes(&compressed).unwrap();

        assert_eq!(volume.attrs.instrument_name, "KTLX");
        assert_eq!(volume.provenance.compression.as_deref(), Some("gzip"));
        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        assert!(volume.sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn gzip_preview_waits_for_complete_displayable_cut() {
        let bytes = synthetic_archive(false);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let preview = read_gzip_preview_from_bytes(&compressed, 1).unwrap();

        assert!(preview.is_none());
    }

    #[test]
    fn gzip_preview_returns_completed_displayable_cut() {
        let mut bytes = synthetic_archive(false);
        set_first_synthetic_radial_status(&mut bytes, RadialStatus::EndElevation);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let preview = read_gzip_preview_from_bytes(&compressed, 1)
            .unwrap()
            .expect("completed first cut preview");

        assert_eq!(preview.attrs.instrument_name, "KTLX");
        assert_eq!(preview.provenance.compression.as_deref(), Some("gzip"));
        assert_eq!(preview.sweeps.len(), 1);
        assert_eq!(preview.sweeps[0].nrays(), 1);
        assert!(preview.sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn gzip_preview_callback_continues_to_full_volume() {
        let mut bytes = synthetic_archive(false);
        set_first_synthetic_radial_status(&mut bytes, RadialStatus::EndElevation);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut preview_radials = None;

        let volume = read_gzip_volume_from_bytes_with_preview(&compressed, 1, |preview| {
            preview_radials = Some(preview.provenance.decode.decoded_ray_count);
        })
        .unwrap();

        assert_eq!(preview_radials, Some(1));
        assert_eq!(volume.attrs.instrument_name, "KTLX");
        assert_eq!(volume.provenance.compression.as_deref(), Some("gzip"));
        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        assert!(volume.sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn decodes_bzip_blocks_without_concatenated_normalized_buffer() {
        let bytes = synthetic_archive(false);
        let compressed = synthetic_bzip_block_archive(&bytes);

        let volume = read_volume_from_bytes(&compressed).unwrap();

        assert_eq!(volume.attrs.instrument_name, "KTLX");
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some("bzip2-blocks")
        );
        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        assert!(volume.sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn bzip_preview_waits_for_complete_displayable_cut() {
        let bytes = synthetic_archive(false);
        let compressed = synthetic_bzip_block_archive(&bytes);

        let preview = read_bzip_block_preview_from_bytes(&compressed, 1).unwrap();

        assert!(preview.is_none());
    }

    #[test]
    fn bzip_preview_returns_completed_displayable_cut() {
        let mut bytes = synthetic_archive(false);
        set_first_synthetic_radial_status(&mut bytes, RadialStatus::EndElevation);
        let compressed = synthetic_bzip_block_archive(&bytes);

        let preview = read_bzip_block_preview_from_bytes(&compressed, 1)
            .unwrap()
            .expect("completed first cut preview");

        assert_eq!(preview.attrs.instrument_name, "KTLX");
        assert_eq!(
            preview.provenance.compression.as_deref(),
            Some("bzip2-blocks")
        );
        assert_eq!(preview.sweeps.len(), 1);
        assert_eq!(preview.sweeps[0].nrays(), 1);
        assert!(preview.sweeps[0].field(&FieldName::Dbzh).is_some());
    }

    #[test]
    fn bzip_preview_full_decode_reuses_path_and_returns_full_volume() {
        let mut bytes = synthetic_archive(false);
        set_first_synthetic_radial_status(&mut bytes, RadialStatus::EndElevation);
        let compressed = synthetic_bzip_block_archive(&bytes);
        let mut preview_radials = None;

        let volume = read_volume_from_bytes_with_bzip_preview(&compressed, 1, |preview| {
            preview_radials = Some(preview.provenance.decode.decoded_ray_count);
        })
        .unwrap();

        assert_eq!(preview_radials, Some(1));
        assert_eq!(volume.attrs.instrument_name, "KTLX");
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some("bzip2-blocks")
        );
        assert_eq!(volume.provenance.decode.decoded_ray_count, 1);
        assert!(volume.sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn multi_block_bzip_decode_matches_uncompressed_reference() {
        let radials = [
            (1, 1, RadialStatus::StartVolume),
            (2, 1, RadialStatus::Intermediate),
            (3, 1, RadialStatus::EndElevation),
            (1, 2, RadialStatus::StartElevation),
            (2, 2, RadialStatus::Intermediate),
            (3, 2, RadialStatus::EndVolume),
        ];
        let archive = synthetic_multi_radial_archive(&radials);
        let reference = read_volume_from_bytes(&archive).unwrap();

        let payload = &archive[VOLUME_HEADER_LEN..];
        let chunks: Vec<&[u8]> = payload.chunks(RECORD_BYTES * 2).collect();
        let compressed = synthetic_bzip_blocks_from_chunks(&archive, &chunks);
        let volume = read_volume_from_bytes(&compressed).unwrap();

        assert_eq!(volume.location, reference.location);
        assert_eq!(volume.sweeps, reference.sweeps);
        assert_eq!(
            volume.provenance.decode.decoded_ray_count,
            reference.provenance.decode.decoded_ray_count
        );
    }

    #[test]
    fn bzip_preview_fires_past_legacy_block_window() {
        // First cut only completes at the 16th record, one bzip block per
        // record: more blocks than the old fixed preview scan window, so the
        // preview must come from the streaming parse, not a block prefix.
        let mut radials: Vec<(u16, u8, RadialStatus)> = vec![(1, 1, RadialStatus::StartVolume)];
        for az in 2..=15 {
            radials.push((az, 1, RadialStatus::Intermediate));
        }
        radials.push((16, 1, RadialStatus::EndElevation));
        radials.push((1, 2, RadialStatus::StartElevation));
        radials.push((2, 2, RadialStatus::EndVolume));
        let archive = synthetic_multi_radial_archive(&radials);
        let payload = &archive[VOLUME_HEADER_LEN..];
        let chunks: Vec<&[u8]> = payload.chunks(RECORD_BYTES).collect();
        let compressed = synthetic_bzip_blocks_from_chunks(&archive, &chunks);

        let mut preview_radials = None;
        let volume = read_volume_from_bytes_with_bzip_preview(&compressed, 16, |preview| {
            preview_radials = Some(preview.provenance.decode.decoded_ray_count);
        })
        .unwrap();

        assert_eq!(preview_radials, Some(16));
        assert_eq!(volume.provenance.decode.decoded_ray_count, 18);
        assert_eq!(volume.sweeps.len(), 2);
    }

    #[test]
    fn corrupt_trailing_bzip_block_yields_partial_volume() {
        let radials = [
            (1, 1, RadialStatus::StartVolume),
            (2, 1, RadialStatus::Intermediate),
            (3, 1, RadialStatus::Intermediate),
            (4, 1, RadialStatus::EndVolume),
        ];
        let archive = synthetic_multi_radial_archive(&radials);
        let payload = &archive[VOLUME_HEADER_LEN..];
        // Records 1-2 plus the third record's prefix land in the good block;
        // the third radial's message body crosses into the corrupted block.
        let split = 2 * RECORD_BYTES + CONTROL_WORD_LEN + MESSAGE_HEADER_LEN + 2;
        let good = bzip_compress(&payload[..split]);
        let mut bad = bzip_compress(&payload[split..]);
        for byte in bad.iter_mut().skip(8) {
            *byte = 0;
        }
        let mut compressed = archive[..VOLUME_HEADER_LEN].to_vec();
        compressed.extend_from_slice(&i32::try_from(good.len()).unwrap().to_be_bytes());
        compressed.extend_from_slice(&good);
        compressed.extend_from_slice(&(-i32::try_from(bad.len()).unwrap()).to_be_bytes());
        compressed.extend_from_slice(&bad);

        let volume = read_volume_from_bytes(&compressed).unwrap();

        assert_eq!(volume.provenance.decode.decoded_ray_count, 2);
        assert!(volume.provenance.decode.skipped_message_count >= 1);
    }

    #[test]
    fn corrupt_first_bzip_block_is_a_hard_error() {
        let archive = synthetic_archive(false);
        let payload = &archive[VOLUME_HEADER_LEN..];
        let mut bad = bzip_compress(payload);
        for byte in bad.iter_mut().skip(8) {
            *byte = 0;
        }
        let mut compressed = archive[..VOLUME_HEADER_LEN].to_vec();
        compressed.extend_from_slice(&(-i32::try_from(bad.len()).unwrap()).to_be_bytes());
        compressed.extend_from_slice(&bad);

        assert!(read_volume_from_bytes(&compressed).is_err());
    }

    #[test]
    fn pipelined_decode_works_on_single_thread_rayon_pool() {
        let radials = [
            (1, 1, RadialStatus::StartVolume),
            (2, 1, RadialStatus::Intermediate),
            (3, 1, RadialStatus::Intermediate),
            (4, 1, RadialStatus::EndVolume),
        ];
        let archive = synthetic_multi_radial_archive(&radials);
        let payload = &archive[VOLUME_HEADER_LEN..];
        let chunks: Vec<&[u8]> = payload.chunks(RECORD_BYTES).collect();
        let compressed = synthetic_bzip_blocks_from_chunks(&archive, &chunks);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let volume = pool
            .install(|| read_volume_from_bytes(&compressed))
            .unwrap();

        assert_eq!(volume.provenance.decode.decoded_ray_count, 4);
    }

    #[test]
    fn decodes_synthetic_16_bit_moment() {
        let bytes = synthetic_archive(true);
        let volume = read_volume_from_bytes(&bytes).unwrap();
        let phi = field(&volume, 0, FieldName::Phidp);

        assert_eq!(phi.data.dtype(), "uint16");
        assert_eq!(phi.value(0, 1), Some(20.0));
    }

    #[test]
    fn expected_radials_follow_message31_azimuth_resolution_code() {
        assert_eq!(expected_radials_for_azimuth_resolution(1), 720);
        assert_eq!(expected_radials_for_azimuth_resolution(2), 360);
        assert_eq!(
            expected_radials_for_azimuth_resolution(0),
            FALLBACK_RADIALS_PER_CUT
        );
    }

    #[test]
    fn decodes_gr2_style_variable_framed_msg31_records() {
        // GR2 ".msg31" exports: AR2V header, then message 31 records packed
        // back to back (no 2432-byte fixed-record padding, no 134 metadata
        // records).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AR2V00000");
        bytes.extend_from_slice(b"1  ");
        bytes.extend_from_slice(&19_724u32.to_be_bytes());
        bytes.extend_from_slice(&1_000u32.to_be_bytes());
        bytes.extend_from_slice(b"COW2");
        for (azimuth_number, status) in [
            (1u16, RadialStatus::StartVolume),
            (2, RadialStatus::Intermediate),
            (3, RadialStatus::EndVolume),
        ] {
            bytes.extend_from_slice(&[0u8; CONTROL_WORD_LEN]);
            let mut body = synthetic_message_31_body(false);
            body[10..12].copy_from_slice(&azimuth_number.to_be_bytes());
            body[21] = radial_status_code(status);
            let message_size = u16::try_from((MESSAGE_HEADER_LEN + body.len()) / 2).unwrap();
            bytes.extend_from_slice(&message_size.to_be_bytes());
            bytes.push(0);
            bytes.push(31);
            bytes.extend_from_slice(&7u16.to_be_bytes());
            bytes.extend_from_slice(&19_724u16.to_be_bytes());
            bytes.extend_from_slice(&1_000u32.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&body);
        }

        let volume = read_volume_from_bytes(&bytes).unwrap();

        assert_eq!(volume.attrs.instrument_name, "COW2");
        assert_eq!(volume.provenance.decode.decoded_ray_count, 3);
        assert_eq!(volume.sweeps[0].nrays(), 3);
    }

    #[ignore = "set NEXRAD_LEVEL2_SAMPLE to a public Archive II file path to run manually"]
    #[test]
    fn decodes_real_public_level2_file_from_env() {
        let path = std::env::var("NEXRAD_LEVEL2_SAMPLE").expect("NEXRAD_LEVEL2_SAMPLE is not set");
        let volume = read_volume_from_path(Path::new(&path)).unwrap();

        assert!(!volume.attrs.instrument_name.is_empty());
        assert!(
            !volume.sweeps.is_empty(),
            "expected at least one decoded sweep"
        );
    }

    /// Committed KIWA volume 307 Start chunk plus its first two intermediate
    /// chunks, decompressed (240 real Message 31 radials).
    pub(crate) fn kiwa_chunk_prefix_normalized() -> Vec<u8> {
        let mut raw = Vec::new();
        for id in [
            "l2chunk-kiwa-307-20260917-003629-001-s",
            "l2chunk-kiwa-307-20260917-003629-002-i",
            "l2chunk-kiwa-307-20260917-003629-003-i",
        ] {
            let path = recast_radar_testdata::path(id).unwrap_or_else(|e| panic!("{e}"));
            raw.extend(fs::read(path).expect("read committed chunk"));
        }
        normalize_archive_bytes(&raw)
            .expect("real chunks decompress")
            .0
    }

    #[test]
    fn real_volume_exceeding_the_output_budget_is_rejected() {
        let bytes = kiwa_chunk_prefix_normalized();
        let volume = builder_from_normalized(
            &bytes,
            ArchiveCompression::Bzip2Blocks,
            DecodeBudget::volume(),
        )
        .expect("real chunks fit the default budget")
        .finish()
        .unwrap()
        .0;
        let needed: usize = volume
            .sweeps
            .iter()
            .flat_map(|sweep| sweep.fields.iter())
            .map(builder::field_capacity_bytes)
            .sum();
        assert!(
            needed > 1024 * 1024,
            "240 super-resolution radials: {needed}"
        );

        let error = builder_from_normalized(
            &bytes,
            ArchiveCompression::Bzip2Blocks,
            DecodeBudget::new(needed / 2),
        )
        .expect_err("half the needed budget must fail");
        assert!(
            matches!(&error, NexradError::LimitExceeded(reason) if reason.contains("limit")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn real_chunks_decode_natively_with_vcp_fixed_angles() {
        // The Start chunk carries the metadata record (Message 5), so the
        // sweep's fixed angle is the VCP cut angle rather than the opening
        // radial's elevation; the legacy wrapper keeps the latter.
        let bytes = kiwa_chunk_prefix_normalized();
        let volume = read_normalized_volume_bytes(&bytes, ArchiveCompression::Bzip2Blocks).unwrap();
        assert_eq!(volume.sweeps.len(), 1);
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.nrays(), 240);
        assert_eq!(volume.scan.vcp_pattern, Some(215));
        assert_eq!(volume.scan.name.as_deref(), Some("VCP-215"));
        assert!(
            (sweep.fixed_angle_deg - 0.5).abs() < 0.05,
            "fixed angle {}",
            sweep.fixed_angle_deg
        );
        assert_ne!(sweep.fixed_angle_deg, sweep.rays.elevation_deg[0]);
        assert!(sweep.rays.time_s.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(sweep.range.spacing_m(), Some(250.0));
        let reflectivity = field(&volume, 0, FieldName::Dbzh);
        assert_eq!(reflectivity.nrays, 240);
        assert!(reflectivity.absent_rows.is_empty());
        assert_eq!(volume.time_coverage.map(|c| c.start), volume.ray_time(0, 0));
    }

    fn synthetic_archive(include_phi_16: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AR2V00000");
        bytes.extend_from_slice(b"1  ");
        bytes.extend_from_slice(&19_724u32.to_be_bytes());
        bytes.extend_from_slice(&1_000u32.to_be_bytes());
        bytes.extend_from_slice(b"KTLX");

        bytes.extend_from_slice(&[0u8; CONTROL_WORD_LEN]);
        let body = synthetic_message_31_body(include_phi_16);
        let message_size = u16::try_from((MESSAGE_HEADER_LEN + body.len()) / 2).unwrap();
        bytes.extend_from_slice(&message_size.to_be_bytes());
        bytes.push(0);
        bytes.push(31);
        bytes.extend_from_slice(&7u16.to_be_bytes());
        bytes.extend_from_slice(&19_724u16.to_be_bytes());
        bytes.extend_from_slice(&1_000u32.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&body);
        bytes.resize(VOLUME_HEADER_LEN + RECORD_BYTES, 0);
        bytes
    }

    fn radial_status_code(status: RadialStatus) -> u8 {
        match status {
            RadialStatus::StartElevation => 0,
            RadialStatus::Intermediate => 1,
            RadialStatus::EndElevation => 2,
            RadialStatus::StartVolume => 3,
            RadialStatus::EndVolume => 4,
            RadialStatus::StartElevationLastCut => 5,
            RadialStatus::Unknown(value) => value,
        }
    }

    fn set_first_synthetic_radial_status(bytes: &mut [u8], status: RadialStatus) {
        let offset = VOLUME_HEADER_LEN + CONTROL_WORD_LEN + MESSAGE_HEADER_LEN + 21;
        bytes[offset] = radial_status_code(status);
    }

    /// One fixed-length record per radial: (azimuth_number, elevation_number, status).
    fn synthetic_multi_radial_archive(radials: &[(u16, u8, RadialStatus)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AR2V00000");
        bytes.extend_from_slice(b"1  ");
        bytes.extend_from_slice(&19_724u32.to_be_bytes());
        bytes.extend_from_slice(&1_000u32.to_be_bytes());
        bytes.extend_from_slice(b"KTLX");

        for (azimuth_number, elevation_number, status) in radials {
            let record_start = bytes.len();
            bytes.extend_from_slice(&[0u8; CONTROL_WORD_LEN]);
            let mut body = synthetic_message_31_body(false);
            body[10..12].copy_from_slice(&azimuth_number.to_be_bytes());
            let azimuth_deg = f32::from(*azimuth_number) * 0.5;
            body[12..16].copy_from_slice(&azimuth_deg.to_bits().to_be_bytes());
            body[21] = radial_status_code(*status);
            body[22] = *elevation_number;
            let message_size = u16::try_from((MESSAGE_HEADER_LEN + body.len()) / 2).unwrap();
            bytes.extend_from_slice(&message_size.to_be_bytes());
            bytes.push(0);
            bytes.push(31);
            bytes.extend_from_slice(&7u16.to_be_bytes());
            bytes.extend_from_slice(&19_724u16.to_be_bytes());
            bytes.extend_from_slice(&1_000u32.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&body);
            bytes.resize(record_start + RECORD_BYTES, 0);
        }
        bytes
    }

    fn bzip_compress(payload: &[u8]) -> Vec<u8> {
        let mut encoder = BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    /// Assemble an LDM block-bzip archive from pre-split payload chunks, with
    /// the real-file convention of a negative size on the final block.
    fn synthetic_bzip_blocks_from_chunks(archive: &[u8], chunks: &[&[u8]]) -> Vec<u8> {
        let mut bytes = archive[..VOLUME_HEADER_LEN].to_vec();
        for (index, chunk) in chunks.iter().enumerate() {
            let compressed = bzip_compress(chunk);
            let len = i32::try_from(compressed.len()).expect("compressed block length fits");
            let signed = if index + 1 == chunks.len() { -len } else { len };
            bytes.extend_from_slice(&signed.to_be_bytes());
            bytes.extend_from_slice(&compressed);
        }
        bytes
    }

    fn synthetic_bzip_block_archive(normalized: &[u8]) -> Vec<u8> {
        let mut encoder = BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(&normalized[VOLUME_HEADER_LEN..]).unwrap();
        let compressed_block = encoder.finish().unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&normalized[..VOLUME_HEADER_LEN]);
        bytes.extend_from_slice(
            &i32::try_from(compressed_block.len())
                .expect("compressed block length fits")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&compressed_block);
        bytes.extend_from_slice(&(-1_i32).to_be_bytes());
        bytes
    }

    fn synthetic_message_31_body(include_phi_16: bool) -> Vec<u8> {
        let mut body = vec![0u8; MSG_31_HEADER_LEN];
        body[0..4].copy_from_slice(b"AR2V");
        body[4..8].copy_from_slice(&1_000u32.to_be_bytes());
        body[8..10].copy_from_slice(&19_724u16.to_be_bytes());
        body[10..12].copy_from_slice(&1u16.to_be_bytes());
        body[12..16].copy_from_slice(&180.5f32.to_bits().to_be_bytes());
        body[18..20].copy_from_slice(&1u16.to_be_bytes());
        body[20] = 2;
        body[21] = 3;
        body[22] = 1;
        body[23] = 1;
        body[24..28].copy_from_slice(&0.5f32.to_bits().to_be_bytes());
        body[30..32].copy_from_slice(&(if include_phi_16 { 5u16 } else { 4u16 }).to_be_bytes());

        let vol_pointer = body.len();
        push_volume_block(&mut body);
        let rad_pointer = body.len();
        push_radial_block(&mut body);
        let ref_pointer = body.len();
        push_u8_moment(&mut body, b"DREF", &[0, 66, 80]);
        let vel_pointer = body.len();
        push_u8_moment(&mut body, b"DVEL", &[129, 139, 119]);
        let phi_pointer = body.len();
        if include_phi_16 {
            push_u16_moment(&mut body, b"DPHI", &[0, 20, 40]);
        }

        set_pointer(&mut body, 0, vol_pointer);
        set_pointer(&mut body, 2, rad_pointer);
        set_pointer(&mut body, 3, ref_pointer);
        set_pointer(&mut body, 4, vel_pointer);
        if include_phi_16 {
            set_pointer(&mut body, 7, phi_pointer);
        }
        body
    }

    fn push_volume_block(body: &mut Vec<u8>) {
        body.extend_from_slice(b"RVOL");
        body.extend_from_slice(&1u16.to_be_bytes());
        body.push(1);
        body.push(0);
        body.extend_from_slice(&35.333f32.to_bits().to_be_bytes());
        body.extend_from_slice(&(-97.277f32).to_bits().to_be_bytes());
        body.extend_from_slice(&370i16.to_be_bytes());
        body.extend_from_slice(&20u16.to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&212u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
    }

    fn push_radial_block(body: &mut Vec<u8>) {
        body.extend_from_slice(b"RRAD");
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&2_500i16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
    }

    fn push_u8_moment(body: &mut Vec<u8>, id: &[u8; 4], gates: &[u8]) {
        body.extend_from_slice(id);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(gates.len() as u16).to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&250i16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.push(0);
        body.push(8);
        body.extend_from_slice(&2.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&66.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(gates);
        if !body.len().is_multiple_of(2) {
            body.push(0);
        }
    }

    fn push_u16_moment(body: &mut Vec<u8>, id: &[u8; 4], gates: &[u16]) {
        body.extend_from_slice(id);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(gates.len() as u16).to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&250i16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.push(0);
        body.push(16);
        body.extend_from_slice(&1.0f32.to_bits().to_be_bytes());
        body.extend_from_slice(&0.0f32.to_bits().to_be_bytes());
        for gate in gates {
            body.extend_from_slice(&gate.to_be_bytes());
        }
    }

    fn set_pointer(body: &mut [u8], pointer_index: usize, value: usize) {
        let offset = 32 + pointer_index * 4;
        body[offset..offset + 4].copy_from_slice(&(value as u32).to_be_bytes());
    }
}
