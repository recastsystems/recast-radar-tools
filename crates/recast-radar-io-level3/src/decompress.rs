//! Decompression: bzip2 product data (ICD 2620001 Figure 3-6 sheet 7 Note 3,
//! Appendix D) and the zlib frames of NOAAPort distributions
//! (`docs/level3/reference.md` section 3).

use crate::Level3Error;

/// Upper bound on decompressed output. The largest corpus product decompresses
/// to under 5 MB; this bounds allocation driven by hostile input.
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 64 << 20;

/// Minimum growth step for output buffers.
const CHUNK: usize = 1 << 16;

/// bzip2 block header magic (BCD pi) and end-of-stream magic (BCD sqrt(pi)).
const BZ_BLOCK_MAGIC: [u8; 6] = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59];
const BZ_EOS_MAGIC: [u8; 6] = [0x17, 0x72, 0x45, 0x38, 0x50, 0x90];

/// True when `data` starts a bzip2 stream: `BZh`, a block size digit 1-9, then
/// a block or end-of-stream magic.
///
/// Halfword 51 means "compression method" only for some products (it holds a
/// calibration constant for others), so detection uses the stream signature,
/// not the halfword.
pub(crate) fn is_bzip2(data: &[u8]) -> bool {
    match data {
        [b'B', b'Z', b'h', level, rest @ ..] if (b'1'..=b'9').contains(level) => {
            rest.starts_with(&BZ_BLOCK_MAGIC) || rest.starts_with(&BZ_EOS_MAGIC)
        }
        _ => false,
    }
}

/// Decompresses the single bzip2 stream at the start of `data`. Bytes after the
/// end of the stream are ignored.
pub(crate) fn bunzip2(data: &[u8]) -> Result<Vec<u8>, Level3Error> {
    let mut decoder = bzip2::Decompress::new(false);
    let mut out = Vec::new();
    loop {
        reserve(&mut out, "bzip2")?;
        let consumed = offset_of(decoder.total_in());
        let input = data.get(consumed..).unwrap_or_default();
        let before = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress_vec(input, &mut out)
            .map_err(|e| Level3Error::Bzip2 {
                reason: e.to_string(),
            })?;
        if status == bzip2::Status::StreamEnd {
            return Ok(out);
        }
        if (decoder.total_in(), decoder.total_out()) == before {
            return Err(Level3Error::Bzip2 {
                reason: "stream ends before its end-of-stream marker".into(),
            });
        }
    }
}

/// True when `data` starts with a zlib header (RFC 1950: deflate method, header
/// checksum a multiple of 31). NOAAPort frames observed start `78 DA`.
pub(crate) fn looks_like_zlib(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => *cmf == 0x78 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0,
        _ => false,
    }
}

/// Decompresses consecutive zlib frames filling all of `data`. Returns the
/// concatenated output and the number of frames.
pub(crate) fn inflate_zlib_frames(data: &[u8]) -> Result<(Vec<u8>, u32), Level3Error> {
    let mut out = Vec::new();
    let mut frames = 0u32;
    let mut rest = data;
    while !rest.is_empty() {
        let mut decoder = flate2::Decompress::new(true);
        loop {
            reserve(&mut out, "zlib")?;
            let consumed = offset_of(decoder.total_in());
            let input = rest.get(consumed..).unwrap_or_default();
            let before = (decoder.total_in(), decoder.total_out());
            let status = decoder
                .decompress_vec(input, &mut out, flate2::FlushDecompress::None)
                .map_err(|e| Level3Error::Zlib {
                    frame: frames,
                    reason: e.to_string(),
                })?;
            if status == flate2::Status::StreamEnd {
                break;
            }
            if (decoder.total_in(), decoder.total_out()) == before {
                return Err(Level3Error::Zlib {
                    frame: frames,
                    reason: "frame ends before its end-of-stream marker".into(),
                });
            }
        }
        rest = rest
            .get(offset_of(decoder.total_in())..)
            .unwrap_or_default();
        frames += 1;
    }
    Ok((out, frames))
}

/// Ensures spare capacity for the next decompression call, doubling the buffer
/// but never past the size limit.
fn reserve(out: &mut Vec<u8>, format: &'static str) -> Result<(), Level3Error> {
    if out.len() < out.capacity() {
        return Ok(());
    }
    let room = MAX_DECOMPRESSED_BYTES.saturating_sub(out.len());
    if room == 0 {
        return Err(Level3Error::DecompressedTooLarge {
            format,
            limit: MAX_DECOMPRESSED_BYTES,
        });
    }
    out.reserve_exact(out.len().max(CHUNK).min(room));
    Ok(())
}

/// Converts a decoder's `total_in` to a slice offset (saturating: an offset past
/// the input yields an empty remainder).
fn offset_of(total: u64) -> usize {
    usize::try_from(total).unwrap_or(usize::MAX)
}
