//! The writer's compression seam: every bzip2 stream and the gzip wrapper
//! are produced here and nowhere else.
//!
//! TEMPORARY: bzip2 streams come from the `bzip2` crate with its pure-Rust
//! `libbz2-rs-sys` backend until `recast-radar-bzip2` has its encoder.
//! Replace [`bzip2_stream`] (and drop the `bzip2` dependency of this crate)
//! when that lands; nothing else in the writer calls a compressor.

use std::io::Write;

use rayon::prelude::*;

use super::WriteError;

/// Compress each record into an LDM record: a 4-byte big-endian control
/// word holding the compressed length, then the bzip2 stream. The control
/// word of the batch's last record is negated when `ends_file` (the file's
/// last record, as NOAA's files have it). Records are compressed in
/// parallel.
pub(crate) fn ldm_batch(records: &[Vec<u8>], ends_file: bool) -> Result<Vec<Vec<u8>>, WriteError> {
    let last = records.len().saturating_sub(1);
    records
        .par_iter()
        .enumerate()
        .map(|(index, record)| ldm_record(record, ends_file && index == last))
        .collect()
}

/// One LDM record: the control word (the compressed length, negated when
/// `last`), then the bzip2 stream of `record`.
pub(crate) fn ldm_record(record: &[u8], last: bool) -> Result<Vec<u8>, WriteError> {
    let stream = bzip2_stream(record)?;
    let len = i32::try_from(stream.len()).map_err(|_| {
        WriteError::LimitExceeded(format!(
            "compressed record of {} bytes does not fit its control word",
            stream.len()
        ))
    })?;
    let control = if last { -len } else { len };
    let mut out = Vec::with_capacity(4 + stream.len());
    out.extend_from_slice(&control.to_be_bytes());
    out.extend_from_slice(&stream);
    Ok(out)
}

/// One complete bzip2 stream (block size 900k, as NOAA's records use).
///
/// TEMPORARY seam: the `bzip2` crate until the `recast-radar-bzip2`
/// encoder is integrated.
pub(crate) fn bzip2_stream(input: &[u8]) -> Result<Vec<u8>, WriteError> {
    let mut encoder = bzip2::write::BzEncoder::new(
        Vec::with_capacity(input.len() / 4 + 64),
        bzip2::Compression::best(),
    );
    encoder
        .write_all(input)
        .map_err(|err| WriteError::Compression(format!("bzip2: {err}")))?;
    encoder
        .finish()
        .map_err(|err| WriteError::Compression(format!("bzip2: {err}")))
}

/// A writer that wraps everything written to it in one gzip member on
/// `out` (default level); `finish` ends the member.
pub(crate) fn gzip_writer<W: Write>(out: W) -> flate2::write::GzEncoder<W> {
    flate2::write::GzEncoder::new(out, flate2::Compression::default())
}
