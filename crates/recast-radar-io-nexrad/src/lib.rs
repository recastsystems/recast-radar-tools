//! NEXRAD Archive II / Level II decoder entry points.
//!
//! This first slice focuses on the modern Message Type 31 radial format and
//! keeps unsupported records non-fatal so an app can inspect partially decoded
//! volumes while the edge-case corpus grows.
//!
//! Other radar formats live in their own crates (`recast-radar-io-odim`,
//! `-io-cfradial`, `-io-dorade`, `-io-jma`); `recast-radar-io` routes byte
//! buffers of unknown format to the right decoder. [`level3_vwp`] holds the
//! Level III VAD Wind Profile decoder until the full Level III crate
//! subsumes it.

pub mod level3_vwp;

use std::cell::UnsafeCell;
use std::collections::btree_map::Entry;
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use bzip2::bufread::BzDecoder;
use chrono::{DateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use recast_radar_core::bounded_read::{self, MAX_DECODED_RADAR_BYTES};
use recast_radar_core::{
    GateRange, MomentGrid, MomentType, RadarSite, RadarVolume, Radial, RadialStatus, VcpInfo,
};
use thiserror::Error;

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
    #[error("moment grid error: {0}")]
    MomentGrid(#[from] recast_radar_core::MomentGridError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveCompression {
    Gzip,
    Bzip2WholeFile,
    Bzip2Blocks,
    Uncompressed,
}

impl ArchiveCompression {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Bzip2WholeFile => "bzip2-whole-file",
            Self::Bzip2Blocks => "bzip2-blocks",
            Self::Uncompressed => "uncompressed",
        }
    }
}

/// Decode a local Archive II / Level II file into the shared radar model.
pub fn decode_volume_from_path(path: &Path) -> Result<RadarVolume> {
    let bytes = fs::read(path).map_err(|source| NexradError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut volume = decode_volume_from_bytes(&bytes)?;
    volume.metadata.source_path = Some(path.display().to_string());
    Ok(volume)
}

/// Decode a byte slice. This is public to support fixtures and embedded tests.
pub fn decode_volume_from_bytes(bytes: &[u8]) -> Result<RadarVolume> {
    if bytes.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader {
            actual: bytes.len(),
        });
    }
    if !bytes.starts_with(&[0x1f, 0x8b])
        && !bytes.starts_with(b"BZh")
        && let Some(blocks) = collect_bzip_block_slices(bytes)?
    {
        return decode_bzip_blocks_pipelined(bytes, blocks, None, false, |_| {})
            .map(|outcome| outcome.volume);
    }

    let (bytes, compression) = normalize_archive_bytes(bytes)?;
    decode_normalized_volume_bytes(&bytes, compression)
}

pub fn decode_gzip_volume_from_reader(reader: impl Read) -> Result<RadarVolume> {
    let decoder = GzDecoder::new(reader);
    let mut decoder = ReadLimit::new(decoder, MAX_DECODED_RADAR_BYTES, "gzip radar payload");
    decode_volume_from_stream_until(&mut decoder, ArchiveCompression::Gzip, None).map(|result| {
        debug_assert!(!result.stopped_at_preview);
        result.volume
    })
}

pub fn decode_gzip_volume_from_bytes_with_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    on_preview: F,
) -> Result<RadarVolume>
where
    F: FnMut(RadarVolume),
{
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }
    if !raw.starts_with(&[0x1f, 0x8b]) {
        return decode_volume_from_bytes(raw);
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
        result.volume
    })
}

pub fn decode_gzip_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<RadarVolume>> {
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
    Ok(result.stopped_at_preview.then_some(result.volume))
}

/// Decode a completed first displayable cut from NEXRAD block-bzip Level II bytes.
///
/// This is intended for UI preview on low-core machines: it returns `None` for
/// gzip, whole-file bzip, uncompressed, or malformed block-bzip inputs, and it
/// never substitutes for the final full-volume decode.
pub fn decode_bzip_block_preview_from_bytes(
    raw: &[u8],
    min_displayable_radials: usize,
) -> Result<Option<RadarVolume>> {
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }

    let Some(blocks) = collect_bzip_block_slices(raw)? else {
        return Ok(None);
    };

    let outcome =
        decode_bzip_blocks_pipelined(raw, blocks, Some(min_displayable_radials), true, |_| {})?;
    Ok(outcome.stopped_at_preview.then_some(outcome.volume))
}

/// Decode a full volume while optionally emitting an early completed first-cut preview.
///
/// For block-bzip Level II files, parsing streams behind the parallel block
/// decompression, so the preview is emitted as soon as the first displayable
/// cut completes — without decompressing or parsing anything twice. Other
/// compression formats fall back to a normal full decode.
pub fn decode_volume_from_bytes_with_bzip_preview<F>(
    raw: &[u8],
    min_displayable_radials: usize,
    mut on_preview: F,
) -> Result<RadarVolume>
where
    F: FnMut(RadarVolume),
{
    if raw.len() < VOLUME_HEADER_LEN {
        return Err(NexradError::ShortVolumeHeader { actual: raw.len() });
    }

    let Some(blocks) = collect_bzip_block_slices(raw)? else {
        return decode_volume_from_bytes(raw);
    };

    let outcome = decode_bzip_blocks_pipelined(
        raw,
        blocks,
        Some(min_displayable_radials),
        false,
        |preview| {
            on_preview(preview.clone());
        },
    )?;
    Ok(outcome.volume)
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
    buffer.clear();
    if buffer.capacity() < len {
        buffer.reserve_exact(len);
    }
    let spare = buffer.spare_capacity_mut();
    let target = &mut spare[..len];
    // SAFETY: u8 has no invalid bit patterns, and the slice is within spare capacity.
    let target = unsafe { std::slice::from_raw_parts_mut(target.as_mut_ptr().cast::<u8>(), len) };
    read_exact_required(reader, target, what, offset)?;
    // SAFETY: read_exact_required returned Ok, so every byte in target was initialized.
    unsafe {
        buffer.set_len(len);
    }
    Ok(())
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

/// Parse already-normalized Archive II bytes.
pub fn decode_normalized_volume_bytes(
    bytes: &[u8],
    compression: ArchiveCompression,
) -> Result<RadarVolume> {
    let volume_header = parse_volume_header(bytes)?;
    let mut volume = RadarVolume::new(
        RadarSite::new(volume_header.icao.clone()),
        volume_header.volume_time,
    );
    volume.metadata.archive_version = Some(volume_header.archive_version);
    volume.metadata.compression = Some(compression.as_str().to_owned());

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
            volume.metadata.skipped_message_count += 1;
            cursor = cursor.saturating_add(RECORD_BYTES);
            record_index += 1;
            continue;
        } else if header.size_halfwords == 0 {
            break;
        }

        let message_total_len = usize::from(header.size_halfwords) * 2;
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        volume.metadata.message_count += 1;
        match header.message_type {
            1 => {
                let message_end = header_offset + message_total_len;
                if message_end > bytes.len() {
                    if volume.metadata.decoded_radial_count > 0 {
                        volume.metadata.skipped_message_count += 1;
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
                parse_message_1(body, &header, &mut volume)?;
            }
            31 => {
                let message_end = header_offset + message_total_len;
                if message_end > bytes.len() {
                    if volume.metadata.decoded_radial_count > 0 {
                        volume.metadata.skipped_message_count += 1;
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
                parse_message_31(body, &header, &mut volume)?;
            }
            5 => {
                let body_offset = header_offset + MESSAGE_HEADER_LEN;
                let fixed_record_end = cursor.saturating_add(RECORD_BYTES).min(bytes.len());
                let message_end = header_offset.saturating_add(message_total_len);
                let body_end = message_end.min(fixed_record_end);
                if body_offset < body_end {
                    parse_message_5(&bytes[body_offset..body_end], &mut volume);
                }
            }
            _ => volume.metadata.skipped_message_count += 1,
        }

        let record_len = if header.message_type != 31 {
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

    Ok(volume)
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
    volume: RadarVolume,
    stopped_at_preview: bool,
}

fn decode_volume_from_stream_until<R: Read>(
    reader: &mut R,
    compression: ArchiveCompression,
    preview_min_radials: Option<usize>,
) -> Result<StreamDecodeResult> {
    decode_volume_from_stream(reader, compression, preview_min_radials, true, |_| {})
}

fn decode_volume_from_stream<R: Read, F>(
    reader: &mut R,
    compression: ArchiveCompression,
    preview_min_radials: Option<usize>,
    stop_at_preview: bool,
    mut on_preview: F,
) -> Result<StreamDecodeResult>
where
    F: FnMut(RadarVolume),
{
    let mut volume_header_bytes = [0; VOLUME_HEADER_LEN];
    read_exact_required(reader, &mut volume_header_bytes, "volume header", 0)?;
    let volume_header = parse_volume_header(&volume_header_bytes)?;
    let mut volume = RadarVolume::new(
        RadarSite::new(volume_header.icao.clone()),
        volume_header.volume_time,
    );
    volume.metadata.archive_version = Some(volume_header.archive_version);
    volume.metadata.compression = Some(compression.as_str().to_owned());

    let mut cursor = VOLUME_HEADER_LEN;
    let mut record_index = 0usize;
    let mut prefix = [0; CONTROL_WORD_LEN + MESSAGE_HEADER_LEN];
    let mut body_buffer = Vec::with_capacity(RECORD_BYTES);
    let mut preview_emitted = false;
    while read_record_prefix(reader, &mut prefix, cursor)? {
        let header_offset = cursor + CONTROL_WORD_LEN;
        let header = parse_message_header_bytes(&prefix[CONTROL_WORD_LEN..]);

        if header.size_halfwords == 0 && record_index < 134 {
            volume.metadata.skipped_message_count += 1;
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

        let message_total_len = usize::from(header.size_halfwords) * 2;
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        let record_len = if record_index < 134 || header.message_type != 31 {
            RECORD_BYTES
        } else {
            message_total_len + CONTROL_WORD_LEN
        };
        let body_len = message_total_len - MESSAGE_HEADER_LEN;
        volume.metadata.message_count += 1;

        match header.message_type {
            1 => {
                if let Err(err) = read_exact_into_buffer(
                    reader,
                    &mut body_buffer,
                    body_len,
                    "message 1 body",
                    header_offset,
                ) {
                    if volume.metadata.decoded_radial_count > 0 {
                        volume.metadata.skipped_message_count += 1;
                        break;
                    }
                    return Err(err);
                }
                parse_message_1(&body_buffer, &header, &mut volume)?;
                skip_record_padding(reader, record_len, prefix.len() + body_len, cursor)?;
                if let Some(min_radials) = preview_min_radials
                    && !preview_emitted
                    && has_complete_displayable_cut(&volume, min_radials)
                {
                    preview_emitted = true;
                    if stop_at_preview {
                        return Ok(StreamDecodeResult {
                            volume,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(volume.clone());
                }
            }
            31 => {
                if let Err(err) = read_exact_into_buffer(
                    reader,
                    &mut body_buffer,
                    body_len,
                    "message 31 body",
                    header_offset,
                ) {
                    if volume.metadata.decoded_radial_count > 0 {
                        volume.metadata.skipped_message_count += 1;
                        break;
                    }
                    return Err(err);
                }
                parse_message_31(&body_buffer, &header, &mut volume)?;
                skip_record_padding(reader, record_len, prefix.len() + body_len, cursor)?;
                if let Some(min_radials) = preview_min_radials
                    && !preview_emitted
                    && has_complete_displayable_cut(&volume, min_radials)
                {
                    preview_emitted = true;
                    if stop_at_preview {
                        return Ok(StreamDecodeResult {
                            volume,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(volume.clone());
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
                parse_message_5(&body_buffer, &mut volume);
                skip_record_padding(reader, record_len, prefix.len() + body_read_len, cursor)?;
            }
            _ => {
                volume.metadata.skipped_message_count += 1;
                skip_record_padding(reader, record_len, prefix.len(), cursor)?;
            }
        }

        cursor = cursor.saturating_add(record_len);
        record_index += 1;
    }

    Ok(StreamDecodeResult {
        volume,
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

#[derive(Clone, Debug, PartialEq)]
struct MomentBlock<'a> {
    moment: MomentType,
    gate_range: GateRange,
    scale: f32,
    offset: f32,
    row: MomentPayload<'a>,
}

#[derive(Clone, Debug, PartialEq)]
enum MomentPayload<'a> {
    U8(&'a [u8]),
    U16(&'a [u8]),
}

const BLOCK_PENDING: u8 = 0;
const BLOCK_READY: u8 = 1;
const BLOCK_FAILED: u8 = 2;

/// Slot store connecting parallel LDM-block decompression workers to the
/// in-order streaming parser.
///
/// Indices are claimed in parse order through `next_claim`, so each slot has
/// exactly one writer. A slot is published with a `Release` store on its state
/// flag and readers dereference it only after an `Acquire` load observes
/// `BLOCK_READY`, after which the slot is never written again.
struct BlockSlots<'a> {
    compressed: Vec<&'a [u8]>,
    slots: Box<[UnsafeCell<Vec<u8>>]>,
    states: Box<[AtomicU8]>,
    errors: Mutex<Vec<Option<String>>>,
    next_claim: AtomicUsize,
    decoded_bytes: AtomicUsize,
    canceled: AtomicBool,
    wakeup: Mutex<()>,
    published: Condvar,
}

// SAFETY: each `UnsafeCell` slot is written by exactly one thread (the unique
// claimant of its index) before being published via the matching `AtomicU8`
// with Release/Acquire ordering; every other field is already `Sync`.
unsafe impl Sync for BlockSlots<'_> {}

impl<'a> BlockSlots<'a> {
    fn new(compressed: Vec<&'a [u8]>) -> Self {
        let len = compressed.len();
        Self {
            compressed,
            slots: (0..len).map(|_| UnsafeCell::new(Vec::new())).collect(),
            states: (0..len).map(|_| AtomicU8::new(BLOCK_PENDING)).collect(),
            errors: Mutex::new(vec![None; len]),
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
        let state = match decompress_bzip_block(self.compressed[index]) {
            Ok(decoded) => {
                if !reserve_atomic_budget(
                    &self.decoded_bytes,
                    decoded.len(),
                    MAX_DECODED_RADAR_BYTES,
                ) {
                    self.errors.lock().unwrap()[index] = Some(format!(
                        "block-bzip radar payload expands beyond the {MAX_DECODED_RADAR_BYTES}-byte aggregate limit"
                    ));
                    self.states[index].store(BLOCK_FAILED, Ordering::Release);
                    drop(self.wakeup.lock().unwrap());
                    self.published.notify_all();
                    self.cancel();
                    return;
                }
                // SAFETY: `index` was claimed exactly once via `next_claim`,
                // so this thread is the slot's unique writer; readers wait for
                // the Release store below before touching it.
                unsafe {
                    *self.slots[index].get() = decoded;
                }
                BLOCK_READY
            }
            Err(err) => {
                self.errors.lock().unwrap()[index] = Some(err.to_string());
                BLOCK_FAILED
            }
        };
        self.states[index].store(state, Ordering::Release);
        // Take the wakeup lock so a parser that has checked the state but not
        // yet parked cannot miss this notification.
        drop(self.wakeup.lock().unwrap());
        self.published.notify_all();
    }

    /// Block until the decompressed contents of `index` are available.
    ///
    /// The caller participates in decompression while it waits (claims advance
    /// in parse order), so the pipeline makes progress even when no rayon
    /// worker ever runs — e.g. on a single-threaded pool.
    fn wait_block(&self, index: usize) -> Result<&[u8]> {
        loop {
            match self.states[index].load(Ordering::Acquire) {
                BLOCK_READY => {
                    // SAFETY: published with Release by the unique writer and
                    // never written again; the slot box itself is pre-sized
                    // and never reallocated.
                    return Ok(unsafe { (*self.slots[index].get()).as_slice() });
                }
                BLOCK_FAILED => {
                    let message = self.errors.lock().unwrap()[index]
                        .clone()
                        .unwrap_or_else(|| "bzip2 block decompression failed".to_owned());
                    return Err(NexradError::Compression(message));
                }
                _ => {}
            }
            let claimed = self.next_claim.fetch_add(1, Ordering::Relaxed);
            if claimed < self.len() {
                self.decompress_index(claimed);
                continue;
            }
            // Everything is claimed, so `index` is in flight on another
            // thread; park until the next publish.
            let mut guard = self.wakeup.lock().unwrap();
            while self.states[index].load(Ordering::Acquire) == BLOCK_PENDING {
                guard = self.published.wait(guard).unwrap();
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
    volume: RadarVolume,
    stopped_at_preview: bool,
}

/// Decode a block-bzip volume by parsing in lockstep with the parallel block
/// decompression: rayon workers fill `BlockSlots` while this thread parses
/// blocks in order, waiting (or stealing decompression work) only when the
/// next block is not ready yet. Total wall time is the decompression wall time
/// instead of decompression followed by a serial parse.
fn decode_bzip_blocks_pipelined(
    raw: &[u8],
    blocks: Vec<&[u8]>,
    min_displayable_radials: Option<usize>,
    stop_at_preview: bool,
    on_preview: impl FnMut(&RadarVolume),
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
        );
        // Stop idle claims if the parse returned early (preview-only or error).
        slots.cancel();
        outcome
    })
}

fn parse_bzip_block_volume(
    volume_header: &[u8],
    blocks: &BlockSlots<'_>,
    min_displayable_radials: Option<usize>,
    stop_at_preview: bool,
    mut on_preview: impl FnMut(&RadarVolume),
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
    let mut volume = RadarVolume::new(
        RadarSite::new(volume_header.icao.clone()),
        volume_header.volume_time,
    );
    volume.metadata.archive_version = Some(volume_header.archive_version);
    volume.metadata.compression = Some(ArchiveCompression::Bzip2Blocks.as_str().to_owned());

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
                if volume.metadata.decoded_radial_count > 0 {
                    volume.metadata.skipped_message_count += 1;
                    break;
                }
                return Err(err);
            }
        }
        let header_offset = cursor + CONTROL_WORD_LEN;
        let header = parse_message_header_bytes(&prefix[CONTROL_WORD_LEN..]);

        if header.size_halfwords == 0 && record_index < 134 {
            volume.metadata.skipped_message_count += 1;
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

        let message_total_len = usize::from(header.size_halfwords) * 2;
        if message_total_len < MESSAGE_HEADER_LEN {
            return Err(NexradError::InvalidMessage {
                offset: header_offset,
                reason: "message size is smaller than message header".to_owned(),
            });
        }

        let record_len = if record_index < 134 || header.message_type != 31 {
            RECORD_BYTES
        } else {
            message_total_len + CONTROL_WORD_LEN
        };
        let body_len = message_total_len - MESSAGE_HEADER_LEN;
        volume.metadata.message_count += 1;

        match header.message_type {
            1 => {
                let body = match cursor_reader.read_slice_or_copy(
                    &mut body_buffer,
                    body_len,
                    "message 1 body",
                    header_offset,
                ) {
                    Ok(body) => body,
                    Err(err) => {
                        if volume.metadata.decoded_radial_count > 0 {
                            volume.metadata.skipped_message_count += 1;
                            break;
                        }
                        return Err(err);
                    }
                };
                parse_message_1(body, &header, &mut volume)?;
                if let Some(min_radials) = preview_pending
                    && has_complete_displayable_cut(&volume, min_radials)
                {
                    preview_pending = None;
                    if stop_at_preview {
                        return Ok(BlockParseOutcome {
                            volume,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(&volume);
                }
                cursor_reader.skip_exact(
                    record_len.saturating_sub(prefix.len() + body_len),
                    "record padding",
                    cursor + prefix.len() + body_len,
                )?;
            }
            31 => {
                let body = match cursor_reader.read_slice_or_copy(
                    &mut body_buffer,
                    body_len,
                    "message 31 body",
                    header_offset,
                ) {
                    Ok(body) => body,
                    Err(err) => {
                        if volume.metadata.decoded_radial_count > 0 {
                            volume.metadata.skipped_message_count += 1;
                            break;
                        }
                        return Err(err);
                    }
                };
                parse_message_31(body, &header, &mut volume)?;
                if let Some(min_radials) = preview_pending
                    && has_complete_displayable_cut(&volume, min_radials)
                {
                    preview_pending = None;
                    if stop_at_preview {
                        return Ok(BlockParseOutcome {
                            volume,
                            stopped_at_preview: true,
                        });
                    }
                    on_preview(&volume);
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
                parse_message_5(body, &mut volume);
                cursor_reader.skip_exact(
                    record_len.saturating_sub(prefix.len() + body_read_len),
                    "record padding",
                    cursor + prefix.len() + body_read_len,
                )?;
            }
            _ => {
                volume.metadata.skipped_message_count += 1;
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
        volume,
        stopped_at_preview: false,
    })
}

fn has_complete_displayable_cut(volume: &RadarVolume, min_displayable_radials: usize) -> bool {
    volume.cuts.iter().enumerate().any(|(index, cut)| {
        if cut.radials.len() < min_displayable_radials {
            return false;
        }
        let has_displayable_moment = cut
            .moments
            .values()
            .any(|grid| grid.radial_count() >= min_displayable_radials);
        if !has_displayable_moment {
            return false;
        }
        let ended = cut.radials.last().is_some_and(|radial| {
            matches!(
                radial.radial_status,
                Some(RadialStatus::EndElevation | RadialStatus::EndVolume)
            )
        });
        ended || index + 1 < volume.cuts.len()
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

fn parse_volume_header(bytes: &[u8]) -> Result<VolumeHeader> {
    require_len(bytes, 0, VOLUME_HEADER_LEN, "volume header")?;
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

fn parse_message_5(body: &[u8], volume: &mut RadarVolume) {
    if body.len() >= 6 {
        let pattern = u16::from_be_bytes([body[4], body[5]]);
        if pattern != 0 {
            volume.vcp = Some(VcpInfo { pattern });
        }
    }
}

fn parse_message_1(
    body: &[u8],
    _message_header: &MessageHeader,
    volume: &mut RadarVolume,
) -> Result<()> {
    require_len(body, 0, MSG_1_HEADER_LEN, "message 1 header")?;

    let collect_ms = be_u32(body, 0);
    let collect_date = be_u16(body, 4);
    if volume.metadata.decoded_radial_count == 0 && collect_date > 0 {
        volume.volume_time = nexrad_date_ms_to_datetime(u32::from(collect_date), collect_ms);
    }

    let azimuth_angle = legacy_binary_angle_deg(be_u16(body, 8));
    let radial_status = RadialStatus::from(be_u16(body, 12) as u8);
    let elevation_angle = legacy_binary_angle_deg(be_u16(body, 14));
    let elevation_number = (be_u16(body, 16) as u8).max(1);

    let reflectivity_range = GateRange {
        first_gate_m: i32::from(be_i16(body, 18)),
        gate_spacing_m: i32::from(be_u16(body, 22).max(1)),
        gate_count: usize::from(be_u16(body, 26)),
    };
    let doppler_range = GateRange {
        first_gate_m: i32::from(be_i16(body, 20)),
        gate_spacing_m: i32::from(be_u16(body, 24).max(1)),
        gate_count: usize::from(be_u16(body, 28)),
    };
    let reflectivity_pointer = usize::from(be_u16(body, 36));
    let velocity_pointer = usize::from(be_u16(body, 38));
    let spectrum_width_pointer = usize::from(be_u16(body, 40));
    let velocity_resolution = be_u16(body, 42);

    let vcp = be_u16(body, 44);
    if vcp != 0 {
        volume.vcp = Some(VcpInfo { pattern: vcp });
    }
    // ICD 2620002 Table III: Nyquist velocity is halfword 31 (bytes 60-61),
    // after the spare halfwords 24-30 (MetPy and Py-ART read it there too).
    let nyquist_velocity_mps = match be_i16(body, 60) {
        raw if raw > 0 => Some(raw as f32 / 100.0),
        _ => None,
    };

    let reflectivity_row =
        legacy_message_1_row(body, reflectivity_pointer, reflectivity_range.gate_count);
    let velocity_row = legacy_message_1_row(body, velocity_pointer, doppler_range.gate_count);
    let spectrum_width_row =
        legacy_message_1_row(body, spectrum_width_pointer, doppler_range.gate_count);
    if reflectivity_row.is_none() && velocity_row.is_none() && spectrum_width_row.is_none() {
        volume.metadata.skipped_message_count += 1;
        return Ok(());
    }

    let gate_range = if reflectivity_row.is_some() {
        reflectivity_range.clone()
    } else {
        doppler_range.clone()
    };
    let radial = Radial {
        azimuth_deg: azimuth_angle,
        elevation_deg: elevation_angle,
        time_offset_ms: collect_ms as i32,
        gate_range,
        nyquist_velocity_mps,
        radial_status: Some(radial_status),
    };

    let starts_elevation = matches!(
        radial_status,
        RadialStatus::StartElevation
            | RadialStatus::StartVolume
            | RadialStatus::StartElevationLastCut
    );
    let last_cut_has_radials = volume
        .cuts
        .last()
        .is_some_and(|cut| !cut.radials.is_empty());
    let last_cut_matches = volume.cuts.last().is_some_and(|cut| {
        cut.elevation_number == Some(elevation_number)
            || (cut.elevation_deg - elevation_angle).abs() <= 0.05
    });
    let cut = if starts_elevation && last_cut_has_radials {
        volume.push_cut(elevation_angle, Some(elevation_number))
    } else if last_cut_matches {
        volume
            .cuts
            .last_mut()
            .expect("last cut existence was checked before borrowing")
    } else {
        volume.find_or_insert_cut(elevation_angle, Some(elevation_number))
    };
    if cut.radials.is_empty() {
        cut.radials.reserve(ONE_DEGREE_RADIALS_PER_CUT);
    }
    let radial_index = cut.radials.len();
    cut.radials.push(radial);

    if let Some(row) = reflectivity_row {
        let grid = legacy_u8_grid(
            cut,
            MomentType::Reflectivity,
            reflectivity_range,
            2.0,
            66.0,
            ONE_DEGREE_RADIALS_PER_CUT,
        );
        grid.push_u8_row_slice(radial_index, row)?;
    }
    if let Some(row) = velocity_row {
        let grid = legacy_u8_grid(
            cut,
            MomentType::Velocity,
            doppler_range.clone(),
            legacy_message_1_velocity_scale(velocity_resolution),
            129.0,
            ONE_DEGREE_RADIALS_PER_CUT,
        );
        grid.push_u8_row_slice(radial_index, row)?;
    }
    if let Some(row) = spectrum_width_row {
        // ICD 2620002 message 1 encodes SW like velocity at 0.5 m/s resolution:
        // value = (code - 129) / 2, with code 0 below threshold and 1 range folded.
        let grid = legacy_u8_grid(
            cut,
            MomentType::SpectrumWidth,
            doppler_range,
            2.0,
            129.0,
            ONE_DEGREE_RADIALS_PER_CUT,
        );
        grid.push_u8_row_slice(radial_index, row)?;
    }

    volume.metadata.decoded_radial_count += 1;
    Ok(())
}

fn legacy_u8_grid(
    cut: &mut recast_radar_core::ElevationCut,
    moment: MomentType,
    gate_range: GateRange,
    scale: f32,
    offset: f32,
    expected_radials: usize,
) -> &mut MomentGrid {
    match cut.moments.entry(moment) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let mut grid = MomentGrid::new_u8(
                entry.key().clone(),
                gate_range,
                scale,
                offset,
                Some(0),
                Some(1),
            );
            grid.reserve_rows(expected_radials);
            entry.insert(grid)
        }
    }
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
    volume: &mut RadarVolume,
) -> Result<()> {
    let header = parse_message_31_header(body, 0)?;
    let expected_radials = expected_radials_for_azimuth_resolution(header.azimuth_resolution);

    // GR2-style ".msg31" exports write a nonstandard volume-header date, so
    // the volume time parses as the epoch; recover it from the first
    // radial's collection time instead.
    if volume.volume_time == DateTime::<Utc>::UNIX_EPOCH && header.collect_date > 0 {
        volume.volume_time =
            nexrad_date_ms_to_datetime(u32::from(header.collect_date), header.collect_ms);
    }

    let mut nyquist_velocity_mps = None;
    let mut moments: [Option<MomentBlock<'_>>; MAX_MESSAGE_31_MOMENTS] =
        std::array::from_fn(|_| None);
    let mut moment_count = 0;
    let needs_volume_constants = volume_needs_constant_block(volume);

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
                    parse_volume_constant_block(body, pointer, volume)?;
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

    let gate_range = moments[..moment_count]
        .iter()
        .flatten()
        .next()
        .map(|moment| moment.gate_range.clone())
        .unwrap_or(GateRange {
            first_gate_m: 0,
            gate_spacing_m: 0,
            gate_count: 0,
        });
    let radial = Radial {
        azimuth_deg: header.azimuth_angle,
        elevation_deg: header.elevation_angle,
        time_offset_ms: header.collect_ms as i32,
        gate_range,
        nyquist_velocity_mps,
        radial_status: Some(header.radial_status),
    };

    let starts_elevation = matches!(
        header.radial_status,
        RadialStatus::StartElevation
            | RadialStatus::StartVolume
            | RadialStatus::StartElevationLastCut
    );
    let last_cut_has_radials = volume
        .cuts
        .last()
        .is_some_and(|cut| !cut.radials.is_empty());
    let last_cut_matches = volume.cuts.last().is_some_and(|cut| {
        cut.elevation_number == Some(header.elevation_number)
            || (cut.elevation_deg - header.elevation_angle).abs() <= 0.05
    });
    let cut = if starts_elevation && last_cut_has_radials {
        volume.push_cut(header.elevation_angle, Some(header.elevation_number))
    } else if last_cut_matches {
        volume
            .cuts
            .last_mut()
            .expect("last cut existence was checked before borrowing")
    } else {
        volume.find_or_insert_cut(header.elevation_angle, Some(header.elevation_number))
    };
    if cut.radials.is_empty() {
        cut.radials.reserve(expected_radials);
    }
    let radial_index = cut.radials.len();
    cut.radials.push(radial);

    for moment in moments.into_iter().take(moment_count).flatten() {
        let MomentBlock {
            moment,
            gate_range,
            scale,
            offset,
            row,
        } = moment;
        let grid = match cut.moments.entry(moment) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => match &row {
                MomentPayload::U8(_) => {
                    let mut grid = MomentGrid::new_u8(
                        entry.key().clone(),
                        gate_range.clone(),
                        scale,
                        offset,
                        Some(0),
                        Some(1),
                    );
                    grid.reserve_rows(expected_radials);
                    entry.insert(grid)
                }
                MomentPayload::U16(_) => {
                    let mut grid = MomentGrid::new_u16(
                        entry.key().clone(),
                        gate_range.clone(),
                        scale,
                        offset,
                        Some(0),
                        Some(1),
                    );
                    grid.reserve_rows(expected_radials);
                    entry.insert(grid)
                }
            },
        };
        match row {
            MomentPayload::U8(row) => grid.push_u8_row_slice(radial_index, row)?,
            MomentPayload::U16(row) => grid.push_u16_be_row_bytes(radial_index, row)?,
        }
    }

    volume.metadata.decoded_radial_count += 1;
    Ok(())
}

fn expected_radials_for_azimuth_resolution(azimuth_resolution: u8) -> usize {
    match azimuth_resolution {
        1 => HALF_DEGREE_RADIALS_PER_CUT,
        2 => ONE_DEGREE_RADIALS_PER_CUT,
        _ => FALLBACK_RADIALS_PER_CUT,
    }
}

fn volume_needs_constant_block(volume: &RadarVolume) -> bool {
    volume.site.latitude_deg.is_none()
        || volume.site.longitude_deg.is_none()
        || volume.site.elevation_m.is_none()
        || volume.vcp.is_none()
}

pub fn parse_message_31_header(bytes: &[u8], offset: usize) -> Result<Message31Header> {
    require_len(bytes, offset, MSG_31_HEADER_LEN, "message 31 header")?;
    let bytes = &bytes[offset..offset + MSG_31_HEADER_LEN];
    if bytes[..4]
        .iter()
        .all(|byte| *byte == 0 || byte.is_ascii_whitespace())
    {
        return Err(NexradError::InvalidMessage {
            offset,
            reason: "empty message 31 id".to_owned(),
        });
    }

    let mut block_pointers = [0; 10];
    for (index, pointer) in block_pointers.iter_mut().enumerate() {
        *pointer = be_u32(bytes, 32 + index * 4) as usize;
    }

    Ok(Message31Header {
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
    volume: &mut RadarVolume,
) -> Result<()> {
    require_len(
        bytes,
        offset,
        VOLUME_CONSTANT_BLOCK_LEN,
        "volume constant block",
    )?;
    let bytes = &bytes[offset..offset + VOLUME_CONSTANT_BLOCK_LEN];
    volume.site.latitude_deg = Some(be_f32(bytes, 8));
    volume.site.longitude_deg = Some(be_f32(bytes, 12));

    let tower_height_m = be_i16(bytes, 16) as f32;
    let feedhorn_height_m = be_u16(bytes, 18) as f32;
    volume.site.elevation_m = Some(tower_height_m + feedhorn_height_m);

    let vcp = be_u16(bytes, 40);
    if vcp != 0 {
        volume.vcp = Some(VcpInfo { pattern: vcp });
    }
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
    let moment = MomentType::from_nexrad_bytes(&header[1..4]);
    let gate_count = usize::from(be_u16(header, 8));
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
        moment,
        gate_range: GateRange {
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
    //! Decoder tests on real Level II files from the corpus
    //! (`recast-radar-testdata`, ids from `testdata/level2/manifest.toml`).
    //!
    //! Expected values come from `testdata/level2/golden/decode/<name>.json`,
    //! written by `tools/level2_decode_golden.py`: a byte walker written for the
    //! script (volume header, LDM record framing, message headers, Message 1
    //! and Message 31 radial headers and raw gate codes), MetPy 1.7.1
    //! `Level2File` (station, times, sweeps, radial headers, VOL block, Nyquist,
    //! VCP, scaled moments) and Py-ART 2.2.5 `NEXRADLevel2File` (rays per
    //! sweep, raw codes, Nyquist). The script fails unless the readers agree.
    //!
    //! Edge and corruption cases change bytes of the real files; each test
    //! shows the change. Inputs that are not committed are downloaded on first
    //! use, and their tests are skipped when that is not possible.

    use super::*;
    use recast_radar_core::{ElevationCut, MomentStorage};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::io::Write;

    /// Build 22.0 (VOL 52, RAD 28, ZDR and PHI 16-bit, CFP), LDM bzip2
    /// records: the metadata record and 480 + 480 radials of the split cut.
    const KTLX_2024_TRIM: &str = "l2-ktlx-20240315-000217-trim";
    /// The full volume the trim was cut from (downloaded).
    const KTLX_2024_FULL: &str = "l2-ktlx-20240315-000217";
    /// Build 13.2 (68-byte Message 31 header, PHI 16-bit, ZDR 8-bit).
    const KTLX_2013_TRIM: &str = "l2-ktlx-20130520-201643-trim";
    /// ARCHIVE2.001 with a blank ICAO, Message 1 radials.
    const KTLX_1991_TRIM: &str = "l2-ktlx-19910605-162126-trim";
    /// ARCHIVE2.036 with a NUL ICAO, Message 1 radials, VCP 11.
    const KTLX_1999_TRIM: &str = "l2-ktlx-19990504-002218-trim";
    /// gzip archive object, Message 31 at 1 degree, 7 sweeps (downloaded).
    const KPAH_2008_GZIP: &str = "l2-kpah-20080415-235014";
    /// gzip archive object that ends 68 radials into its first cut (downloaded).
    const KTLX_1999_TRUNCATED_GZIP: &str = "l2-ktlx-19990503-230052";
    /// Committed real-time chunks of KIWA volume 307: the Start chunk and the
    /// first intermediate chunk (the first 120 radials of a 720-radial cut).
    const KIWA_CHUNK_START: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
    const KIWA_CHUNK_002: &str = "l2chunk-kiwa-307-20260917-003629-002-i";
    const KIWA_CHUNKS_GOLDEN: &str = "l2chunk-kiwa-307-20260917-003629-001-s+002-i";

    // ------------------------------------------------------------ inputs ---

    /// Bytes of a corpus file, or `None` (with a message) when it is not
    /// committed and cannot be downloaded right now.
    fn corpus_bytes(id: &str) -> Option<Vec<u8>> {
        match recast_radar_testdata::bytes(id) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.is_offline() => {
                eprintln!("skipping: {error}");
                None
            }
            Err(error) => panic!("{error}"),
        }
    }

    /// Golden values for a corpus input (see the module docs).
    fn golden(name: &str) -> Value {
        let path = recast_radar_testdata::testdata_dir()
            .join("level2")
            .join("golden")
            .join("decode")
            .join(format!("{name}.json"));
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn int(value: &Value) -> i64 {
        value
            .as_i64()
            .unwrap_or_else(|| panic!("expected an integer, found {value}"))
    }

    fn count(value: &Value) -> usize {
        usize::try_from(int(value)).unwrap_or_else(|_| panic!("expected a count, found {value}"))
    }

    fn num(value: &Value) -> f64 {
        value
            .as_f64()
            .unwrap_or_else(|| panic!("expected a number, found {value}"))
    }

    fn text(value: &Value) -> &str {
        value
            .as_str()
            .unwrap_or_else(|| panic!("expected a string, found {value}"))
    }

    fn list(value: &Value) -> &[Value] {
        value
            .as_array()
            .unwrap_or_else(|| panic!("expected an array, found {value}"))
    }

    /// Header text as the decoder reports it: NUL padding and blanks trimmed.
    fn trimmed(text: &str) -> &str {
        text.trim_matches(|c: char| c == '\0' || c.is_whitespace())
    }

    fn hex_text(hex: &str) -> String {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{what}: {actual} vs {expected} (tolerance {tolerance})"
        );
    }

    /// `(control word offset, compressed payload)` of each LDM record after
    /// the volume header, read from the file's control words.
    fn ldm_records(file: &[u8]) -> Vec<(usize, &[u8])> {
        let mut records = Vec::new();
        let mut offset = VOLUME_HEADER_LEN;
        while offset + 4 <= file.len() {
            let control = i32::from_be_bytes(file[offset..offset + 4].try_into().unwrap());
            let len = control.unsigned_abs() as usize;
            records.push((offset, &file[offset + 4..offset + 4 + len]));
            offset += 4 + len;
            if control < 0 {
                break;
            }
        }
        records
    }

    fn bzip2_decompress(block: &[u8]) -> Vec<u8> {
        let mut decoded = Vec::new();
        BzDecoder::new(block)
            .read_to_end(&mut decoded)
            .expect("real LDM record decompresses");
        decoded
    }

    fn bzip2_compress(payload: &[u8]) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    /// Stream offsets of the messages in an uncompressed Message 31 run
    /// (control word + message, back to back), plus the end offset.
    fn message31_bounds(stream: &[u8]) -> Vec<usize> {
        let mut bounds = vec![0];
        let mut offset = 0;
        while offset < stream.len() {
            let header = parse_message_header(stream, offset + CONTROL_WORD_LEN).unwrap();
            assert_eq!(header.message_type, 31, "message at {offset}");
            offset += CONTROL_WORD_LEN + usize::from(header.size_halfwords) * 2;
            bounds.push(offset);
        }
        assert_eq!(offset, stream.len());
        bounds
    }

    // ------------------------------------------------------ golden checks ---

    fn code_at(grid: &MomentGrid, row: usize, gate: usize) -> u16 {
        let index = row * grid.gate_range.gate_count + gate;
        match &grid.storage {
            MomentStorage::U8(values) => u16::from(values[index]),
            MomentStorage::U16(values) => values[index],
            MomentStorage::F32(_) => panic!("Level II moments are stored as integer codes"),
        }
    }

    /// One moment against its golden summary: rows, word size, gate layout,
    /// scaling, the count and sum of valid raw codes, the scaled sum, minimum
    /// and maximum, and sampled gates (raw code and scaled value).
    fn assert_moment_matches_golden(grid: &MomentGrid, golden: &Value, label: &str) {
        let rows = count(&golden["rows"]);
        assert_eq!(grid.radial_count(), rows, "{label}: rows");
        let radial_indices: Vec<usize> = match golden["row_radials"].as_array() {
            Some(indices) => indices.iter().map(count).collect(),
            None => (0..rows).collect(),
        };
        assert_eq!(
            grid.radial_indices, radial_indices,
            "{label}: radial indices"
        );
        assert_eq!(
            i64::from(grid.storage.word_size_bits()),
            int(&golden["word_size"]),
            "{label}: word size"
        );
        assert_eq!(
            i64::from(grid.gate_range.first_gate_m),
            int(&golden["first_gate_m"]),
            "{label}: first gate"
        );
        assert_eq!(
            i64::from(grid.gate_range.gate_spacing_m),
            int(&golden["gate_width_m"]),
            "{label}: gate spacing"
        );
        assert_eq!(
            grid.gate_range.gate_count,
            count(&golden["gates_max"]),
            "{label}: gates"
        );
        assert_eq!(grid.scale, num(&golden["scale"]) as f32, "{label}: scale");
        assert_eq!(
            grid.offset,
            num(&golden["offset"]) as f32,
            "{label}: offset"
        );

        let mut valid = 0usize;
        let mut raw_sum = 0u64;
        let mut scaled_sum = 0.0f64;
        let mut abs_sum = 0.0f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for row in 0..rows {
            for gate in 0..grid.gate_range.gate_count {
                let code = code_at(grid, row, gate);
                match grid.scaled_value(row, gate) {
                    Some(value) => {
                        assert!(
                            code >= 2,
                            "{label}: code {code} at {row}/{gate} has a value"
                        );
                        let value = f64::from(value);
                        valid += 1;
                        raw_sum += u64::from(code);
                        scaled_sum += value;
                        abs_sum += value.abs();
                        min = min.min(value);
                        max = max.max(value);
                    }
                    None => assert!(code < 2, "{label}: code {code} at {row}/{gate} is missing"),
                }
            }
        }
        assert_eq!(valid, count(&golden["valid_count"]), "{label}: valid gates");
        assert_eq!(
            raw_sum,
            u64::try_from(int(&golden["raw_valid_sum"])).unwrap(),
            "{label}: raw code sum"
        );
        // Scaled values are f32 here and float64 in MetPy: allow the f32
        // rounding of each value.
        assert_close(
            scaled_sum,
            num(&golden["scaled_sum"]),
            2e-7 * abs_sum + 1e-9,
            &format!("{label}: scaled sum"),
        );
        if valid > 0 {
            assert_close(
                min,
                num(&golden["scaled_min"]),
                1e-4,
                &format!("{label}: min"),
            );
            assert_close(
                max,
                num(&golden["scaled_max"]),
                1e-4,
                &format!("{label}: max"),
            );
        }
        for sample in list(&golden["samples"]) {
            let sample = list(sample);
            let (row, gate) = (count(&sample[0]), count(&sample[1]));
            assert_eq!(
                i64::from(code_at(grid, row, gate)),
                int(&sample[2]),
                "{label}: code at {row}/{gate}"
            );
            match sample[3].as_f64() {
                Some(expected) => assert_close(
                    f64::from(grid.scaled_value(row, gate).expect("valid gate")),
                    expected,
                    1e-4,
                    &format!("{label}: value at {row}/{gate}"),
                ),
                None => assert_eq!(grid.scaled_value(row, gate), None, "{label}: {row}/{gate}"),
            }
        }
    }

    /// One elevation cut against a golden sweep: radial count, elevation
    /// number, first and last radial status, angles, Nyquist velocities and
    /// every moment.
    fn assert_cut_matches_golden(cut: &ElevationCut, sweep: &Value, label: &str) {
        let radials = count(&sweep["radials"]);
        assert_eq!(cut.radials.len(), radials, "{label}: radials");
        assert_eq!(
            cut.elevation_number.map(i64::from),
            Some(int(&sweep["elevation_number"])),
            "{label}: elevation number"
        );
        let status = |value: &Value| Some(RadialStatus::from(u8::try_from(int(value)).unwrap()));
        assert_eq!(
            cut.radials[0].radial_status,
            status(&sweep["first_status"]),
            "{label}: first status"
        );
        assert_eq!(
            cut.radials[radials - 1].radial_status,
            status(&sweep["last_status"]),
            "{label}: last status"
        );
        assert_close(
            f64::from(cut.elevation_deg),
            num(&sweep["first_elevation_deg"]),
            1e-4,
            &format!("{label}: elevation"),
        );
        assert_close(
            f64::from(cut.radials[0].azimuth_deg),
            num(&sweep["first_azimuth_deg"]),
            1e-4,
            &format!("{label}: first azimuth"),
        );
        assert_close(
            f64::from(cut.radials[radials - 1].azimuth_deg),
            num(&sweep["last_azimuth_deg"]),
            1e-4,
            &format!("{label}: last azimuth"),
        );
        let tolerance = 1e-4 * radials as f64;
        let azimuth_sum: f64 = cut.radials.iter().map(|r| f64::from(r.azimuth_deg)).sum();
        assert_close(
            azimuth_sum,
            num(&sweep["azimuth_sum_deg"]),
            tolerance,
            &format!("{label}: azimuth sum"),
        );
        let elevation_sum: f64 = cut.radials.iter().map(|r| f64::from(r.elevation_deg)).sum();
        assert_close(
            elevation_sum,
            num(&sweep["elevation_sum_deg"]),
            tolerance,
            &format!("{label}: elevation sum"),
        );

        let nyquist: Vec<f64> = cut
            .radials
            .iter()
            .filter_map(|r| r.nyquist_velocity_mps.map(f64::from))
            .collect();
        assert_eq!(
            nyquist.len(),
            count(&sweep["nyquist_count"]),
            "{label}: radials with a Nyquist velocity"
        );
        assert_close(
            nyquist.iter().sum(),
            num(&sweep["nyquist_sum_mps"]),
            tolerance,
            &format!("{label}: Nyquist sum"),
        );
        if !nyquist.is_empty() {
            let min = nyquist.iter().copied().fold(f64::INFINITY, f64::min);
            let max = nyquist.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert_close(
                min,
                num(&sweep["nyquist_min_mps"]),
                1e-4,
                &format!("{label}: Nyquist min"),
            );
            assert_close(
                max,
                num(&sweep["nyquist_max_mps"]),
                1e-4,
                &format!("{label}: Nyquist max"),
            );
        }

        let moments = sweep["moments"]
            .as_object()
            .unwrap_or_else(|| panic!("{label}: moments"));
        let names: BTreeSet<&str> = cut.moments.keys().map(MomentType::short_name).collect();
        let expected: BTreeSet<&str> = moments.keys().map(String::as_str).collect();
        assert_eq!(names, expected, "{label}: moments");
        for (moment, grid) in &cut.moments {
            let name = moment.short_name();
            assert_moment_matches_golden(grid, &moments[name], &format!("{label} {name}"));
        }
    }

    /// A decoded volume against its golden file: station id, archive version,
    /// volume time, VCP, site, radial count and every sweep.
    fn assert_volume_matches_golden(volume: &RadarVolume, golden: &Value) {
        let name = text(&golden["name"]);
        let header = &golden["volume_header"];
        assert_eq!(
            volume.site.id,
            trimmed(&hex_text(text(&header["icao_hex"]))),
            "{name}: station id"
        );
        let version = format!(
            "{}{}",
            trimmed(text(&header["tape"])),
            trimmed(text(&header["extension"]))
        );
        assert_eq!(
            volume.metadata.archive_version.as_deref(),
            Some(version.as_str()),
            "{name}: archive version"
        );
        let sweeps = list(&golden["sweeps"]);
        // Message 1 volumes take their time from the first radial; Message
        // 31 volumes from the volume header.
        let expected_time = if int(&golden["first_radial"]["message_type"]) == 1 {
            int(&sweeps[0]["first_epoch_ms"])
        } else {
            int(&header["metpy_epoch_ms"])
        };
        assert_eq!(
            volume.volume_time.timestamp_millis(),
            expected_time,
            "{name}: volume time"
        );
        // The decoder keeps the Message 1 VCP, else the VOL block VCP, else
        // the Message 5 pattern, ignoring zero.
        let expected_vcp = [
            &golden["message1_vcp"],
            &golden["site"]["vol_block_vcp"],
            &golden["message5_vcp"],
        ]
        .into_iter()
        .filter_map(Value::as_i64)
        .find(|pattern| *pattern != 0);
        assert_eq!(
            volume.vcp.as_ref().map(|vcp| i64::from(vcp.pattern)),
            expected_vcp,
            "{name}: VCP"
        );
        match golden["site"].as_object() {
            Some(site) => {
                assert_eq!(
                    volume.site.latitude_deg,
                    Some(num(&site["latitude_deg"]) as f32),
                    "{name}: latitude"
                );
                assert_eq!(
                    volume.site.longitude_deg,
                    Some(num(&site["longitude_deg"]) as f32),
                    "{name}: longitude"
                );
                assert_eq!(
                    volume.site.elevation_m,
                    Some((int(&site["site_amsl_m"]) + int(&site["feedhorn_agl_m"])) as f32),
                    "{name}: site height"
                );
            }
            None => {
                assert_eq!(volume.site.latitude_deg, None, "{name}: latitude");
                assert_eq!(volume.site.longitude_deg, None, "{name}: longitude");
                assert_eq!(volume.site.elevation_m, None, "{name}: site height");
            }
        }
        let radials: usize = sweeps.iter().map(|sweep| count(&sweep["radials"])).sum();
        assert_eq!(
            volume.metadata.decoded_radial_count, radials,
            "{name}: decoded radials"
        );
        assert_eq!(volume.cuts.len(), sweeps.len(), "{name}: cuts");
        for (index, (cut, sweep)) in volume.cuts.iter().zip(sweeps).enumerate() {
            assert_cut_matches_golden(cut, sweep, &format!("{name} sweep {index}"));
        }
    }

    // ------------------------------------------------ headers and messages ---

    #[test]
    fn parses_archive_volume_header() {
        for id in [KTLX_2024_TRIM, KTLX_1991_TRIM, KTLX_1999_TRIM] {
            let Some(bytes) = corpus_bytes(id) else {
                return;
            };
            let expected = &golden(id)["volume_header"];

            let header = parse_volume_header(&bytes).unwrap();

            assert_eq!(
                header.archive_version,
                format!(
                    "{}{}",
                    trimmed(text(&expected["tape"])),
                    trimmed(text(&expected["extension"]))
                ),
                "{id}"
            );
            // KTLX; blank (1991) and NUL (1999) ICAO fields decode as empty.
            assert_eq!(header.icao, trimmed(text(&expected["metpy_stid"])), "{id}");
            assert_eq!(
                header.volume_time.timestamp_millis(),
                int(&expected["metpy_epoch_ms"]),
                "{id}"
            );
        }
        assert!(parse_volume_header(&[0; VOLUME_HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn parses_message_header() {
        for id in [KTLX_2024_TRIM, KTLX_2013_TRIM, KTLX_1991_TRIM] {
            let Some(bytes) = corpus_bytes(id) else {
                return;
            };
            let golden = golden(id);
            let (normalized, compression) = normalize_archive_bytes(&bytes).unwrap();
            assert_eq!(compression, ArchiveCompression::Bzip2Blocks, "{id}");
            assert_eq!(
                normalized.len(),
                VOLUME_HEADER_LEN + count(&golden["stream_len"]),
                "{id}: decompressed length"
            );

            // Every metadata message header up to and including the first radial.
            let expected = list(&golden["messages_before_first_radial"]);
            assert!(!expected.is_empty());
            for message in expected {
                let fields: Vec<i64> = list(message).iter().map(int).collect();
                let offset = VOLUME_HEADER_LEN + usize::try_from(fields[0]).unwrap();
                let header = parse_message_header(&normalized, offset + CONTROL_WORD_LEN).unwrap();
                assert_eq!(
                    header,
                    MessageHeader {
                        size_halfwords: u16::try_from(fields[1]).unwrap(),
                        channels: u8::try_from(fields[2]).unwrap(),
                        message_type: u8::try_from(fields[3]).unwrap(),
                        sequence_id: u16::try_from(fields[4]).unwrap(),
                        date: u16::try_from(fields[5]).unwrap(),
                        milliseconds: u32::try_from(fields[6]).unwrap(),
                        segments: u16::try_from(fields[7]).unwrap(),
                        segment_number: u16::try_from(fields[8]).unwrap(),
                    },
                    "{id}: message at stream offset {}",
                    fields[0]
                );
            }

            // A header cut off by the end of the real data is an error.
            let end = normalized.len();
            assert!(matches!(
                parse_message_header(&normalized, end - MESSAGE_HEADER_LEN + 1),
                Err(NexradError::Truncated { .. })
            ));
        }
    }

    #[test]
    fn parses_message_31_header() {
        // Build 22.0 (72-byte header, ten pointers) and Build 13.2 (68-byte
        // header: the tenth "pointer" is the first word of the VOL block).
        for id in [KTLX_2024_TRIM, KTLX_2013_TRIM] {
            let Some(bytes) = corpus_bytes(id) else {
                return;
            };
            let expected = &golden(id)["first_radial"];
            assert_eq!(int(&expected["message_type"]), 31);
            let (normalized, _) = normalize_archive_bytes(&bytes).unwrap();
            let offset = VOLUME_HEADER_LEN + count(&expected["body_offset"]);

            let header = parse_message_31_header(&normalized, offset).unwrap();

            assert_eq!(
                i64::from(header.collect_ms),
                int(&expected["time_ms"]),
                "{id}"
            );
            assert_eq!(
                i64::from(header.collect_date),
                int(&expected["date"]),
                "{id}"
            );
            assert_eq!(
                i64::from(header.azimuth_number),
                int(&expected["azimuth_number"]),
                "{id}"
            );
            assert_eq!(
                header.azimuth_angle,
                num(&expected["azimuth_deg"]) as f32,
                "{id}"
            );
            assert_eq!(
                i64::from(header.radial_length),
                int(&expected["radial_length"]),
                "{id}"
            );
            assert_eq!(
                i64::from(header.azimuth_resolution),
                int(&expected["azimuth_spacing_code"]),
                "{id}"
            );
            assert_eq!(
                header.radial_status,
                RadialStatus::from(u8::try_from(int(&expected["status"])).unwrap()),
                "{id}"
            );
            assert_eq!(header.radial_status, RadialStatus::StartVolume, "{id}");
            assert_eq!(
                i64::from(header.elevation_number),
                int(&expected["elevation_number"]),
                "{id}"
            );
            assert_eq!(
                i64::from(header.cut_sector),
                int(&expected["cut_sector"]),
                "{id}"
            );
            assert_eq!(
                header.elevation_angle,
                num(&expected["elevation_deg"]) as f32,
                "{id}"
            );
            let pointers: Vec<usize> = list(&expected["block_pointers"])
                .iter()
                .map(count)
                .collect();
            assert_eq!(header.block_pointers.to_vec(), pointers, "{id}");
        }
    }

    // --------------------------------------------------- volume decoding ---

    #[test]
    fn decodes_message_31_volume() {
        let Some(bytes) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);

        let volume = decode_volume_from_bytes(&bytes).unwrap();

        assert_eq!(volume.site.id, "KTLX");
        assert_eq!(volume.vcp, Some(VcpInfo { pattern: 212 }));
        assert_eq!(volume.cuts.len(), 2);
        assert_eq!(volume.cuts[0].radials.len(), 480);
        assert_eq!(
            volume.metadata.message_count,
            count(&golden["message_count"])
        );
        assert_volume_matches_golden(&volume, &golden);
    }

    #[test]
    fn decodes_legacy_message_1_reflectivity_and_velocity() {
        let Some(bytes) = corpus_bytes(KTLX_1999_TRIM) else {
            return;
        };
        let golden = golden(KTLX_1999_TRIM);

        let volume = decode_volume_from_bytes(&bytes).unwrap();

        // REF-only surveillance cut, then the Doppler cut (VEL/SW at 250 m)
        // whose radials carry the Nyquist velocity (Message 1 halfword 31).
        assert_eq!(volume.vcp, Some(VcpInfo { pattern: 11 }));
        let surveillance = &volume.cuts[0];
        let doppler = &volume.cuts[1];
        assert!(surveillance.moments.contains_key(&MomentType::Reflectivity));
        assert!(!surveillance.moments.contains_key(&MomentType::Velocity));
        assert!(doppler.moments.contains_key(&MomentType::Velocity));
        assert!(
            surveillance
                .radials
                .iter()
                .all(|r| r.nyquist_velocity_mps.is_none())
        );
        assert_eq!(
            doppler.radials[0].nyquist_velocity_mps,
            Some(num(&golden["sweeps"][1]["nyquist_min_mps"]) as f32)
        );
        assert_volume_matches_golden(&volume, &golden);
    }

    #[test]
    fn decodes_legacy_message_1_spectrum_width_with_velocity_offset() {
        let Some(bytes) = corpus_bytes(KTLX_1999_TRIM) else {
            return;
        };
        let expected = &golden(KTLX_1999_TRIM)["sweeps"][1]["moments"]["SW"];

        let volume = decode_volume_from_bytes(&bytes).unwrap();

        // ICD 2620002: SW = (code - 129) / 2, as MetPy and Py-ART decode it.
        let spectrum_width = &volume.cuts[1].moments[&MomentType::SpectrumWidth];
        assert_eq!((spectrum_width.scale, spectrum_width.offset), (2.0, 129.0));
        for sample in list(&expected["samples"]) {
            let sample = list(sample);
            let (row, gate, code) = (count(&sample[0]), count(&sample[1]), int(&sample[2]));
            if code >= 2 {
                assert_eq!(
                    spectrum_width.scaled_value(row, gate),
                    Some((code as f32 - 129.0) / 2.0)
                );
            }
        }
        assert_moment_matches_golden(spectrum_width, expected, "KTLX 1999 SW");
    }

    #[test]
    fn decodes_16_bit_moments() {
        // KTLX 2013: PHI 16-bit, ZDR 8-bit. KTLX 2024: PHI and ZDR 16-bit.
        for (id, zdr_bits) in [(KTLX_2013_TRIM, 8), (KTLX_2024_TRIM, 16)] {
            let Some(bytes) = corpus_bytes(id) else {
                return;
            };
            let golden = golden(id);
            let volume = decode_volume_from_bytes(&bytes).unwrap();
            let cut = &volume.cuts[0];
            let moments = &golden["sweeps"][0]["moments"];

            let phi = &cut.moments[&MomentType::DifferentialPhase];
            let zdr = &cut.moments[&MomentType::DifferentialReflectivity];
            assert_eq!(phi.storage.word_size_bits(), 16, "{id}");
            assert_eq!(zdr.storage.word_size_bits(), zdr_bits, "{id}");
            assert_moment_matches_golden(phi, &moments["PHI"], &format!("{id} PHI"));
            assert_moment_matches_golden(zdr, &moments["ZDR"], &format!("{id} ZDR"));
        }
    }

    #[test]
    fn decodes_every_trimmed_fixture() {
        let ids = recast_radar_testdata::ids_with_tag("trimmed");
        assert!(ids.len() >= 16, "trimmed fixtures: {ids:?}");
        for id in ids {
            let Some(bytes) = corpus_bytes(id) else {
                return;
            };
            let volume =
                decode_volume_from_bytes(&bytes).unwrap_or_else(|error| panic!("{id}: {error}"));
            assert_eq!(volume.metadata.compression.as_deref(), Some("bzip2-blocks"));
            assert_volume_matches_golden(&volume, &golden(id));
        }
    }

    // --------------------------------------------------------------- gzip ---

    #[test]
    fn decodes_gzip_stream_without_normalized_buffer() {
        let Some(bytes) = corpus_bytes(KPAH_2008_GZIP) else {
            return;
        };
        let golden = golden(KPAH_2008_GZIP);
        assert_eq!(text(&golden["outer_compression"]), "gzip");

        let streamed = decode_gzip_volume_from_reader(bytes.as_slice()).unwrap();
        let buffered = decode_volume_from_bytes(&bytes).unwrap();

        assert_eq!(streamed.metadata.compression.as_deref(), Some("gzip"));
        assert_eq!(streamed, buffered);
        assert_volume_matches_golden(&streamed, &golden);
    }

    #[test]
    fn gzip_preview_waits_for_complete_displayable_cut() {
        let Some(bytes) = corpus_bytes(KTLX_1999_TRUNCATED_GZIP) else {
            return;
        };
        let golden = golden(KTLX_1999_TRUNCATED_GZIP);

        // The object ends 68 radials into its first cut: no cut completes.
        let preview = decode_gzip_preview_from_bytes(&bytes, 1).unwrap();

        assert!(preview.is_none());
        let volume = decode_volume_from_bytes(&bytes).unwrap();
        assert_eq!(volume.metadata.compression.as_deref(), Some("gzip"));
        assert_eq!(volume.cuts.len(), 1);
        assert_eq!(volume.cuts[0].radials.len(), 68);
        assert_volume_matches_golden(&volume, &golden);
    }

    #[test]
    fn gzip_preview_returns_completed_displayable_cut() {
        let Some(bytes) = corpus_bytes(KPAH_2008_GZIP) else {
            return;
        };
        let golden = golden(KPAH_2008_GZIP);
        let first = &golden["sweeps"][0];
        let radials = count(&first["radials"]);

        let preview = decode_gzip_preview_from_bytes(&bytes, radials)
            .unwrap()
            .expect("completed first cut preview");

        assert_eq!(preview.site.id, "KPAH");
        assert_eq!(preview.metadata.compression.as_deref(), Some("gzip"));
        // The first cut ends with an end-of-elevation radial (status 2), so
        // the preview stops right there.
        assert_eq!(preview.cuts.len(), 1);
        assert_cut_matches_golden(&preview.cuts[0], first, "KPAH preview sweep 0");
        let full = decode_volume_from_bytes(&bytes).unwrap();
        assert_eq!(preview.cuts[0], full.cuts[0]);

        // No cut of this volume has more radials than the first.
        assert!(
            list(&golden["sweeps"])
                .iter()
                .all(|sweep| count(&sweep["radials"]) <= radials)
        );
        assert!(
            decode_gzip_preview_from_bytes(&bytes, radials + 1)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn gzip_preview_callback_continues_to_full_volume() {
        let Some(bytes) = corpus_bytes(KPAH_2008_GZIP) else {
            return;
        };
        let golden = golden(KPAH_2008_GZIP);
        let mut previews = Vec::new();

        let volume = decode_gzip_volume_from_bytes_with_preview(&bytes, 1, |preview| {
            previews.push(preview);
        })
        .unwrap();

        assert_eq!(previews.len(), 1);
        assert_eq!(
            previews[0].metadata.decoded_radial_count,
            count(&golden["sweeps"][0]["radials"])
        );
        assert_eq!(volume, decode_volume_from_bytes(&bytes).unwrap());
        assert_eq!(volume.metadata.decoded_radial_count, 2520);
        assert_volume_matches_golden(&volume, &golden);
    }

    // ------------------------------------------------------ LDM bzip2 ---

    #[test]
    fn decodes_bzip_blocks_without_concatenated_normalized_buffer() {
        let Some(bytes) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        assert_eq!(list(&golden["ldm_records"]).len(), 9);

        let volume = decode_volume_from_bytes(&bytes).unwrap();

        assert_eq!(volume.metadata.compression.as_deref(), Some("bzip2-blocks"));
        assert_eq!(volume.metadata.decoded_radial_count, 960);
        assert_volume_matches_golden(&volume, &golden);
    }

    #[test]
    fn bzip_preview_waits_for_complete_displayable_cut() {
        let (Some(start), Some(chunk)) =
            (corpus_bytes(KIWA_CHUNK_START), corpus_bytes(KIWA_CHUNK_002))
        else {
            return;
        };
        // A real archive prefix: the Start chunk (volume header + metadata
        // record) and the first 120 radials of a 720-radial cut.
        let mut bytes = start;
        bytes.extend_from_slice(&chunk);

        let preview = decode_bzip_block_preview_from_bytes(&bytes, 1).unwrap();

        assert!(preview.is_none());
        let volume = decode_volume_from_bytes(&bytes).unwrap();
        assert_eq!(volume.cuts[0].radials.len(), 120);
        assert_volume_matches_golden(&volume, &golden(KIWA_CHUNKS_GOLDEN));
    }

    #[test]
    fn bzip_preview_returns_completed_displayable_cut() {
        let Some(bytes) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let trim_golden = golden(KTLX_2024_TRIM);
        let full = decode_volume_from_bytes(&bytes).unwrap();

        // The trim keeps 480 of sweep 1's radials: the cut counts as complete
        // once the first radial of sweep 2 arrives.
        let preview = decode_bzip_block_preview_from_bytes(&bytes, 1)
            .unwrap()
            .expect("completed first cut preview");

        assert_eq!(preview.site.id, "KTLX");
        assert_eq!(
            preview.metadata.compression.as_deref(),
            Some("bzip2-blocks")
        );
        assert_eq!(preview.cuts[0], full.cuts[0]);
        assert_cut_matches_golden(&preview.cuts[0], &trim_golden["sweeps"][0], "trim preview");
        assert_eq!(preview.cuts.len(), 2);
        assert_eq!(preview.cuts[1].radials.len(), 1);

        // The full volume's first cut ends with an end-of-elevation radial.
        let Some(bytes) = corpus_bytes(KTLX_2024_FULL) else {
            return;
        };
        let full_golden = golden(KTLX_2024_FULL);
        let preview = decode_bzip_block_preview_from_bytes(&bytes, 1)
            .unwrap()
            .expect("completed first cut preview");
        assert_eq!(preview.cuts.len(), 1);
        assert_eq!(preview.metadata.decoded_radial_count, 720);
        assert_cut_matches_golden(&preview.cuts[0], &full_golden["sweeps"][0], "full preview");
    }

    #[test]
    fn bzip_preview_full_decode_reuses_path_and_returns_full_volume() {
        let Some(bytes) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let mut previews = Vec::new();

        let volume = decode_volume_from_bytes_with_bzip_preview(&bytes, 1, |preview| {
            previews.push(preview);
        })
        .unwrap();

        assert_eq!(previews.len(), 1);
        assert_eq!(
            previews[0].cuts[0].radials.len(),
            count(&golden["sweeps"][0]["radials"])
        );
        assert_eq!(volume, decode_volume_from_bytes(&bytes).unwrap());
        assert_volume_matches_golden(&volume, &golden);
    }

    #[test]
    fn multi_block_bzip_decode_matches_uncompressed_reference() {
        let Some(file) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let expected_records = list(&golden["ldm_records"]);

        // Decompress each real LDM record on its own and frame the payloads
        // as an uncompressed Archive II file.
        let records = ldm_records(&file);
        assert_eq!(records.len(), expected_records.len());
        let mut uncompressed = file[..VOLUME_HEADER_LEN].to_vec();
        for ((offset, block), expected) in records.iter().zip(expected_records) {
            assert_eq!(*offset, count(&expected["offset"]));
            let payload = bzip2_decompress(block);
            assert_eq!(payload.len(), count(&expected["decompressed_len"]));
            uncompressed.extend_from_slice(&payload);
        }
        let reference = decode_volume_from_bytes(&uncompressed).unwrap();
        assert_eq!(
            reference.metadata.compression.as_deref(),
            Some("uncompressed")
        );

        let volume = decode_volume_from_bytes(&file).unwrap();

        assert_eq!(volume.metadata.compression.as_deref(), Some("bzip2-blocks"));
        let mut same = reference.clone();
        same.metadata.compression = volume.metadata.compression.clone();
        assert_eq!(volume, same);
        assert_volume_matches_golden(&volume, &golden);

        // Re-block the same message stream every 1,000,003 bytes, so records
        // and messages are split across bzip2 blocks.
        let stream = &uncompressed[VOLUME_HEADER_LEN..];
        let chunks: Vec<&[u8]> = stream.chunks(1_000_003).collect();
        let mut reblocked = file[..VOLUME_HEADER_LEN].to_vec();
        for (index, chunk) in chunks.iter().enumerate() {
            let block = bzip2_compress(chunk);
            let len = i32::try_from(block.len()).unwrap();
            let control = if index + 1 == chunks.len() { -len } else { len };
            reblocked.extend_from_slice(&control.to_be_bytes());
            reblocked.extend_from_slice(&block);
        }
        assert_eq!(decode_volume_from_bytes(&reblocked).unwrap(), volume);
    }

    #[test]
    fn bzip_preview_fires_past_legacy_block_window() {
        let Some(file) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let sweep_radials = count(&golden["sweeps"][0]["radials"]);
        let records = ldm_records(&file);
        assert_eq!(count(&golden["ldm_records"][1]["radials"]), 120);

        // Split record 1 (the first 120 radials of sweep 1) into 20 records
        // of 6 radials each and keep every other record byte for byte. Sweep
        // 1 then ends in the 24th record and sweep 2 starts in the 25th, past
        // the 16 records the old fixed preview scan window covered.
        let radials = bzip2_decompress(records[1].1);
        let bounds = message31_bounds(&radials);
        assert_eq!(bounds.len(), 121);
        let mut blocks = vec![records[0].1.to_vec()];
        for first in (0..120).step_by(6) {
            blocks.push(bzip2_compress(&radials[bounds[first]..bounds[first + 6]]));
        }
        blocks.extend(records[2..].iter().map(|(_, block)| block.to_vec()));
        assert_eq!(blocks.len(), 1 + 20 + 7);
        let mut reframed = file[..VOLUME_HEADER_LEN].to_vec();
        for (index, block) in blocks.iter().enumerate() {
            let len = i32::try_from(block.len()).unwrap();
            let control = if index + 1 == blocks.len() { -len } else { len };
            reframed.extend_from_slice(&control.to_be_bytes());
            reframed.extend_from_slice(block);
        }
        let original = decode_volume_from_bytes(&file).unwrap();

        let mut previews = Vec::new();
        let volume =
            decode_volume_from_bytes_with_bzip_preview(&reframed, sweep_radials, |preview| {
                previews.push(preview);
            })
            .unwrap();

        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].cuts[0].radials.len(), sweep_radials);
        assert_eq!(previews[0].cuts[0], original.cuts[0]);
        assert_eq!(volume, original);
    }

    #[test]
    fn corrupt_trailing_bzip_block_yields_partial_volume() {
        let Some(file) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let clean = decode_volume_from_bytes(&file).unwrap();

        // Zero the last LDM record's bzip2 data after its stream and block
        // magic ("BZh9" + "1AY&"), so it still frames as bzip2 but fails.
        let (offset, block) = *ldm_records(&file).last().unwrap();
        let start = offset + 4;
        let mut corrupt = file.clone();
        corrupt[start + 8..start + block.len()].fill(0);

        let volume = decode_volume_from_bytes(&corrupt).unwrap();

        // What MetPy reads from the file without that record: 480 + 360.
        let expected: Vec<usize> =
            list(&golden["layout_checks"]["without_last_record_sweep_radials"])
                .iter()
                .map(count)
                .collect();
        assert_eq!(expected, vec![480, 360]);
        let radials: Vec<usize> = volume.cuts.iter().map(|cut| cut.radials.len()).collect();
        assert_eq!(radials, expected);
        assert_eq!(volume.metadata.decoded_radial_count, 840);
        assert_eq!(volume.cuts[0], clean.cuts[0]);
        assert_eq!(
            volume.metadata.skipped_message_count,
            clean.metadata.skipped_message_count + 1
        );
    }

    #[test]
    fn corrupt_first_bzip_block_is_a_hard_error() {
        let Some(file) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        // Zero the metadata record's bzip2 data after its magic.
        let (offset, block) = ldm_records(&file)[0];
        let start = offset + 4;
        let mut corrupt = file.clone();
        corrupt[start + 8..start + block.len()].fill(0);

        assert!(matches!(
            decode_volume_from_bytes(&corrupt),
            Err(NexradError::Compression(_))
        ));
        assert!(decode_bzip_block_preview_from_bytes(&corrupt, 1).is_err());
    }

    #[test]
    fn pipelined_decode_works_on_single_thread_rayon_pool() {
        let Some(bytes) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();

        let volume = pool.install(|| decode_volume_from_bytes(&bytes)).unwrap();

        assert_eq!(volume, decode_volume_from_bytes(&bytes).unwrap());
        assert_volume_matches_golden(&volume, &golden);
    }

    // ----------------------------------------------------------- layouts ---

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
        // GR2 ".msg31" exports keep the AR2V volume header but carry only a
        // few fixed-size metadata records, then Message 31 records back to
        // back, uncompressed. The corpus has no GR2 export, so the layout is
        // cut from the real KTLX 2024 trim: its volume header, its Message 2
        // and 5 records, then all its Message 31 records. MetPy reads that
        // layout as the same two sweeps (golden `layout_checks`).
        let Some(file) = corpus_bytes(KTLX_2024_TRIM) else {
            return;
        };
        let golden = golden(KTLX_2024_TRIM);
        let checks = &golden["layout_checks"];
        let keep: Vec<u8> = list(&checks["gr2_metadata_types"])
            .iter()
            .map(|t| u8::try_from(int(t)).unwrap())
            .collect();
        let (normalized, _) = normalize_archive_bytes(&file).unwrap();
        let mut gr2 = normalized[..VOLUME_HEADER_LEN].to_vec();
        let mut cursor = VOLUME_HEADER_LEN;
        loop {
            let header = parse_message_header(&normalized, cursor + CONTROL_WORD_LEN).unwrap();
            if header.message_type == 31 {
                break;
            }
            if header.size_halfwords != 0 && keep.contains(&header.message_type) {
                gr2.extend_from_slice(&normalized[cursor..cursor + RECORD_BYTES]);
            }
            cursor += RECORD_BYTES;
        }
        message31_bounds(&normalized[cursor..]);
        gr2.extend_from_slice(&normalized[cursor..]);
        assert_eq!(gr2.len(), count(&checks["gr2_len"]));
        let original = decode_volume_from_bytes(&file).unwrap();

        let volume = decode_volume_from_bytes(&gr2).unwrap();

        assert_eq!(volume.metadata.compression.as_deref(), Some("uncompressed"));
        let radials: Vec<usize> = volume.cuts.iter().map(|cut| cut.radials.len()).collect();
        let expected: Vec<usize> = list(&checks["gr2_sweep_radials"])
            .iter()
            .map(count)
            .collect();
        assert_eq!(radials, expected);
        assert_eq!(volume.cuts, original.cuts);
        assert_eq!(volume.site, original.site);
        assert_eq!(volume.vcp, original.vcp);
        assert_eq!(volume.volume_time, original.volume_time);
        assert_eq!(volume.metadata.message_count, keep.len() + 960);

        // GR2 exports write a nonstandard volume header date: with the date
        // and time zeroed, the volume time comes from the first radial.
        gr2[12..20].fill(0);
        let volume = decode_volume_from_bytes(&gr2).unwrap();
        assert_eq!(
            volume.volume_time.timestamp_millis(),
            int(&golden["sweeps"][0]["first_epoch_ms"])
        );
        assert_eq!(volume.cuts, original.cuts);
    }
}
