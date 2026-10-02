//! The files around the BUFR messages.
//!
//! Meteo-France's radar files are gzip members laid end to end, one BUFR
//! message each; a PAM file's last message is a compress(1) (`.Z`) member
//! instead. A bare BUFR file (or one behind a WMO bulletin heading) is also
//! accepted.

use flate2::{Crc, Decompress, FlushDecompress, Status};

use crate::message::MAGIC;
use crate::{BufrError, lzw};

/// gzip magic and deflate method (RFC 1952).
const GZIP: [u8; 3] = [0x1F, 0x8B, 0x08];
/// Most bytes all members may expand to.
pub(crate) const MAX_EXPANDED: usize = 512 * 1024 * 1024;

/// Whether `bytes` start like a file this crate reads: a BUFR message, a
/// gzip member holding one, or a compress (`.Z`) stream holding one.
pub fn looks_like_bufr_bytes(bytes: &[u8]) -> bool {
    if bytes.starts_with(MAGIC) {
        return true;
    }
    if bytes.starts_with(&GZIP) {
        return first_gzip_bytes(bytes, MAGIC.len()).is_some_and(|head| head.starts_with(MAGIC));
    }
    if bytes.starts_with(&lzw::MAGIC) {
        return lzw::decompress(bytes, 4096)
            .ok()
            .or_else(|| partial_lzw(bytes))
            .is_some_and(|head| head.starts_with(MAGIC));
    }
    false
}

/// The first `n` bytes a gzip member expands to.
fn first_gzip_bytes(bytes: &[u8], n: usize) -> Option<Vec<u8>> {
    let header = gzip_header_length(bytes).ok()?;
    let mut inflate = Decompress::new(false);
    let mut out = Vec::with_capacity(n);
    inflate
        .decompress_vec(&bytes[header..], &mut out, FlushDecompress::None)
        .ok()?;
    Some(out)
}

/// The start of a `.Z` stream too long for the sniff's limit.
fn partial_lzw(bytes: &[u8]) -> Option<Vec<u8>> {
    let head = &bytes[..bytes.len().min(64)];
    lzw::decompress(head, 4096).ok()
}

/// Everything the members of `bytes` expand to, in order: gzip members,
/// a final compress member, or bytes that are BUFR already.
pub fn expand(bytes: &[u8]) -> Result<Vec<u8>, BufrError> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        if rest.starts_with(&GZIP) {
            let consumed = gzip_member(rest, &mut out)?;
            rest = &rest[consumed..];
        } else if rest.starts_with(&lzw::MAGIC) {
            let expanded = lzw::decompress(rest, MAX_EXPANDED.saturating_sub(out.len()))?;
            out.extend_from_slice(&expanded);
            break;
        } else if out.is_empty() {
            // Not compressed: BUFR (perhaps behind a bulletin heading).
            return Ok(bytes.to_vec());
        } else if rest.iter().all(|&b| b == 0 || b.is_ascii_whitespace()) {
            break;
        } else {
            return Err(BufrError::Container(format!(
                "{} bytes after the compressed members are neither gzip nor compress",
                rest.len()
            )));
        }
    }
    Ok(out)
}

/// Length of the gzip header at the start of `bytes`.
fn gzip_header_length(bytes: &[u8]) -> Result<usize, BufrError> {
    let truncated = || BufrError::Container("truncated gzip header".into());
    if bytes.len() < 10 || !bytes.starts_with(&GZIP) {
        return Err(truncated());
    }
    let flags = bytes[3];
    let mut at = 10;
    if flags & 0x04 != 0 {
        let extra = bytes.get(at..at + 2).ok_or_else(truncated)?;
        at += 2 + usize::from(u16::from_le_bytes([extra[0], extra[1]]));
    }
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            let end = bytes
                .get(at..)
                .and_then(|tail| tail.iter().position(|&b| b == 0))
                .ok_or_else(truncated)?;
            at += end + 1;
        }
    }
    if flags & 0x02 != 0 {
        at += 2;
    }
    if at > bytes.len() {
        return Err(truncated());
    }
    Ok(at)
}

/// Inflate the gzip member at the start of `bytes` onto `out`, checking
/// its CRC-32 and length; returns the bytes the member occupies.
fn gzip_member(bytes: &[u8], out: &mut Vec<u8>) -> Result<usize, BufrError> {
    let header = gzip_header_length(bytes)?;
    let start = out.len();
    let mut inflate = Decompress::new(false);
    loop {
        if out.len() >= MAX_EXPANDED {
            return Err(BufrError::Limit(format!(
                "gzip members expand past {MAX_EXPANDED} bytes"
            )));
        }
        if out.capacity() - out.len() < 64 * 1024 {
            out.reserve((out.len() - start).max(256 * 1024));
        }
        let input = &bytes[header + inflate.total_in() as usize..];
        let before = (inflate.total_in(), inflate.total_out());
        let status = inflate
            .decompress_vec(input, out, FlushDecompress::None)
            .map_err(|err| BufrError::Container(format!("gzip member: {err}")))?;
        if status == Status::StreamEnd {
            break;
        }
        if (inflate.total_in(), inflate.total_out()) == before && input.is_empty() {
            return Err(BufrError::Container("gzip member is truncated".into()));
        }
    }
    let end = header + inflate.total_in() as usize;
    let trailer = bytes
        .get(end..end + 8)
        .ok_or_else(|| BufrError::Container("gzip member has no trailer".into()))?;
    let expected_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let expected_len = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    let mut crc = Crc::new();
    crc.update(&out[start..]);
    if crc.sum() != expected_crc || (out.len() - start) as u32 != expected_len {
        return Err(BufrError::Container(
            "gzip member fails its CRC-32 or length check".into(),
        ));
    }
    Ok(end + 8)
}
