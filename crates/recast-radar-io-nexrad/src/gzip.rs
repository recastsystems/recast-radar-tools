//! gzip inflate for `.gz` Archive II volumes: a whole-buffer one-shot for
//! bytes already in memory and a streaming reader for the preview paths.
//!
//! Both decode every member of a multi-member gzip file and concatenate the
//! outputs, as `gzip -d` does, and both ignore bytes after the last member
//! that do not start another one (zero padding, for example). A
//! `flate2::read::GzDecoder` stops after the first member, which turned a
//! re-gzipped volume with several members into a silently partial volume;
//! `flate2::read::MultiGzDecoder` decodes every member but errors on
//! trailing bytes.
//!
//! The one-shot exists for speed. Reading a `GzDecoder` through a 64 KiB
//! chunk loop inflates in small output windows: every call re-enters
//! zlib-rs, which then copies the newest 32 KiB into its sliding window, and
//! every chunk is copied a second time into a growing `Vec`. When the whole
//! compressed file is in memory, zlib-rs can write straight into one output
//! buffer sized from the gzip ISIZE trailer, as zlib's `uncompress` does. On
//! the 9.5 MB KTLX20130520 volume that removes about a sixth of the decode's
//! instructions and about 5% of its wall time.

use std::io::{self, BufRead, Read};

use flate2::bufread::GzDecoder;
use flate2::{Decompress, FlushDecompress, Status};

/// gzip member magic bytes (RFC 1952 ID1, ID2).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Smallest possible gzip member: 10-byte header, a 2-byte empty final
/// fixed-Huffman block, and the 8-byte CRC-32 + ISIZE trailer.
const GZIP_MIN_MEMBER_LEN: usize = 20;

/// Deflate cannot expand input by more than about 1032:1 (a 258-byte match
/// costs at least 2 bits), so a trailer that claims more than this is not
/// trusted as an allocation size.
const MAX_DEFLATE_EXPANSION: usize = 1032;

/// Minimum growth step when the ISIZE hint turns out to be too small
/// (multi-member files, or a wrong or wrapped trailer).
const MIN_GROWTH: usize = 64 * 1024;

/// Inflate every gzip member in `raw` into one buffer of at most `limit`
/// bytes.
///
/// The output buffer is presized from the ISIZE field of the final trailer.
/// For a single-member file (every gzip-compressed Archive II volume seen so
/// far) that is the exact length, so the whole stream inflates in one
/// zlib-rs call with no chunk copies and no reallocation. ISIZE only
/// describes the *last* member, modulo 2^32. For a multi-member file the
/// hint is only a starting size, and the buffer grows when the output fills
/// it.
///
/// Members are decoded back to back and their outputs are concatenated, as
/// `gzip -d` and `flate2::read::MultiGzDecoder` do. zlib-rs checks each
/// member's header, CRC-32 and ISIZE. Once a member ends, any remaining bytes
/// that do not begin with the gzip magic are ignored, as
/// `flate2::read::GzDecoder` ignores data after its member (for example
/// zero padding).
///
/// Errors match `bounded_read::read_to_end_limited`: exactly `limit` bytes
/// is accepted, one more is "expands beyond the limit", and an allocation
/// failure is reported instead of aborting. The message is the complete
/// diagnostic, for the caller's compression-error variant.
pub fn inflate_gzip_members_limited(
    raw: &[u8],
    limit: usize,
    context: &'static str,
) -> Result<Vec<u8>, String> {
    // One byte of room past `limit` tells "ends exactly at the limit" apart
    // from "expands beyond it".
    let max_len = limit.saturating_add(1);
    let mut output = Vec::new();
    resize_zeroed(&mut output, gzip_size_hint(raw, limit), context)?;
    let mut filled = 0usize;
    let mut input = raw;
    loop {
        // One member: header, deflate body, CRC-32 + ISIZE trailer, all
        // parsed and verified by zlib-rs in gzip mode.
        let mut inflater = Decompress::new_gzip(15);
        loop {
            if filled == output.len() {
                let grown = output
                    .len()
                    .saturating_add(output.len().max(MIN_GROWTH))
                    .min(max_len);
                resize_zeroed(&mut output, grown, context)?;
            }
            let in_before = inflater.total_in();
            let out_before = inflater.total_out();
            let status = inflater
                .decompress(input, &mut output[filled..], FlushDecompress::Finish)
                .map_err(|err| format!("{context}: {err}"))?;
            // Both deltas are bounded by the lengths of the slices just
            // passed in, so they fit in usize.
            let consumed = (inflater.total_in() - in_before) as usize;
            input = &input[consumed..];
            filled += (inflater.total_out() - out_before) as usize;
            if filled > limit {
                return Err(format!("{context} expands beyond the {limit}-byte limit"));
            }
            match status {
                Status::StreamEnd => break,
                // With all remaining input supplied and `Finish`, inflate
                // stops before the end of the member only when it runs out
                // of output space or input. If the output is full, grow it
                // and continue. zlib-rs has saved its window, so the next
                // slice does not need to be contiguous with this one.
                _ if filled == output.len() => {}
                // zlib-rs takes at most u32::MAX input bytes per call, so
                // keep going while input remains and the call made progress.
                _ if consumed != 0 && !input.is_empty() => {}
                _ => return Err(format!("{context}: truncated gzip stream")),
            }
        }
        if !input.starts_with(&GZIP_MAGIC) {
            break;
        }
    }
    output.truncate(filled);
    Ok(output)
}

/// Starting output size: the final ISIZE field when it is plausible for
/// this input and within `limit`, otherwise 0 (grow on demand).
fn gzip_size_hint(raw: &[u8], limit: usize) -> usize {
    if raw.len() < GZIP_MIN_MEMBER_LEN {
        return 0;
    }
    let [.., b0, b1, b2, b3] = *raw else {
        return 0;
    };
    let trailer_len = usize::try_from(u32::from_le_bytes([b0, b1, b2, b3])).unwrap_or(usize::MAX);
    let plausible = raw.len().saturating_mul(MAX_DEFLATE_EXPANSION).min(limit);
    if trailer_len <= plausible {
        trailer_len
    } else {
        0
    }
}

/// Grow `output` to `len` zeroed bytes, reporting allocation failure as an
/// error instead of aborting.
fn resize_zeroed(output: &mut Vec<u8>, len: usize, context: &'static str) -> Result<(), String> {
    output
        .try_reserve_exact(len.saturating_sub(output.len()))
        .map_err(|err| format!("{context}: cannot reserve decoded buffer: {err}"))?;
    output.resize(len, 0);
    Ok(())
}

/// Streaming reader over every member of a gzip file, for the preview
/// decoders that stop before the end of the input.
///
/// Each member is read through a `flate2::bufread::GzDecoder` (header,
/// CRC-32 and ISIZE checked). When a member ends, the bytes that follow are
/// inspected: another gzip magic starts the next member, anything else ends
/// the stream and is ignored, so the reader agrees with
/// [`inflate_gzip_members_limited`] on the same input. Inflating stops as
/// soon as the caller stops reading.
pub(crate) struct MultiGzReader<R: BufRead> {
    /// The member being read; `None` once the input is exhausted or a
    /// non-member tail was found.
    member: Option<GzDecoder<R>>,
}

impl<R: BufRead> MultiGzReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            member: Some(GzDecoder::new(reader)),
        }
    }
}

impl<R: BufRead> Read for MultiGzReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(member) = self.member.as_mut() {
            let count = member.read(buf)?;
            if count > 0 || buf.is_empty() {
                return Ok(count);
            }
            // The member is finished and exactly its bytes were consumed
            // (`bufread::GzDecoder` reads the deflate body and the 8-byte
            // trailer through the inner `BufRead` without reading ahead).
            let Some(member) = self.member.take() else {
                break;
            };
            let mut inner = member.into_inner();
            // A one-byte fill cannot show the second magic byte; the header
            // parser of the next decoder decides that case.
            let next_member = matches!(inner.fill_buf()?, [0x1f, 0x8b, ..] | [0x1f]);
            if next_member {
                self.member = Some(GzDecoder::new(inner));
            }
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::read::{GzDecoder, MultiGzDecoder};
    use flate2::write::GzEncoder;
    use std::io::Write;

    const LIMIT: usize = 512 * 1024 * 1024;
    const CTX: &str = "gzip test payload";

    fn gzip(payload: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    /// Deterministic, moderately compressible bytes (radar-like runs plus noise).
    fn payload(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let run = (state % 7) as usize + 1;
            let value = (state >> 8) as u8 & 0x3f;
            out.extend(std::iter::repeat_n(value, run.min(len - out.len())));
        }
        out
    }

    fn multi_gz_reference(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        MultiGzDecoder::new(raw).read_to_end(&mut out).unwrap();
        out
    }

    /// The streaming reader's output, read through small buffers so member
    /// boundaries fall inside reads.
    fn streamed(raw: &[u8]) -> io::Result<Vec<u8>> {
        let mut reader = MultiGzReader::new(raw);
        let mut out = Vec::new();
        let mut chunk = [0u8; 4093];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&chunk[..count]);
        }
    }

    #[test]
    fn single_member_matches_gz_decoder_and_presizes_exactly() {
        let data = payload(300_000, 1);
        let raw = gzip(&data);
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        let mut reference = Vec::new();
        GzDecoder::new(raw.as_slice())
            .read_to_end(&mut reference)
            .unwrap();
        assert_eq!(out, reference);
        assert_eq!(out, data);
        assert_eq!(gzip_size_hint(&raw, LIMIT), data.len());
        // The ISIZE hint was exact: no growth step happened.
        assert_eq!(out.capacity(), data.len());
        assert_eq!(streamed(&raw).unwrap(), data);
    }

    #[test]
    fn empty_payload_decodes_to_empty() {
        let raw = gzip(&[]);
        assert!(
            inflate_gzip_members_limited(&raw, LIMIT, CTX)
                .unwrap()
                .is_empty()
        );
        assert!(streamed(&raw).unwrap().is_empty());
    }

    #[test]
    fn multi_member_decodes_every_member() {
        let parts = [payload(200_000, 2), payload(150_000, 3), payload(10, 4)];
        let mut raw = Vec::new();
        for part in &parts {
            raw.extend(gzip(part));
        }
        let expected = parts.concat();
        // The last member's ISIZE (10) is far below the total, so this
        // exercises the growth path.
        assert_eq!(gzip_size_hint(&raw, LIMIT), 10);
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        assert_eq!(out, expected);
        assert_eq!(out, multi_gz_reference(&raw));
        assert_eq!(streamed(&raw).unwrap(), expected);
    }

    #[test]
    fn multi_member_with_empty_members() {
        let first = payload(90_000, 5);
        let mut raw = gzip(&[]);
        raw.extend(gzip(&first));
        raw.extend(gzip(&[]));
        assert_eq!(gzip_size_hint(&raw, LIMIT), 0);
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        assert_eq!(out, first);
        assert_eq!(out, multi_gz_reference(&raw));
        assert_eq!(streamed(&raw).unwrap(), first);
    }

    #[test]
    fn hint_from_last_member_grows_once_for_earlier_members() {
        // ISIZE describes only the large last member, so the buffer fills
        // while that member is still inflating and grows once.
        let parts = [payload(5_000, 6), payload(400_000, 7)];
        let mut raw = gzip(&parts[0]);
        raw.extend(gzip(&parts[1]));
        assert_eq!(gzip_size_hint(&raw, LIMIT), 400_000);
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        assert_eq!(out, parts.concat());
    }

    #[test]
    fn trailing_non_gzip_bytes_are_ignored_like_gz_decoder() {
        let data = payload(70_000, 8);
        let mut raw = gzip(&data);
        raw.extend_from_slice(&[0u8; 512]);
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        assert_eq!(out, data);
        assert_eq!(streamed(&raw).unwrap(), data);

        let mut raw = gzip(&data);
        raw.extend_from_slice(b"not gzip");
        let out = inflate_gzip_members_limited(&raw, LIMIT, CTX).unwrap();
        assert_eq!(out, data);
        assert_eq!(streamed(&raw).unwrap(), data);
    }

    #[test]
    fn truncated_member_is_an_error() {
        let data = payload(120_000, 9);
        let raw = gzip(&data);
        for cut in [raw.len() - 1, raw.len() - 8, raw.len() / 2, 12] {
            let err = inflate_gzip_members_limited(&raw[..cut], LIMIT, CTX).unwrap_err();
            assert!(err.starts_with(CTX), "{cut}: {err}");
            assert!(streamed(&raw[..cut]).is_err(), "streamed {cut}");
        }
        // A truncated second member is an error too.
        let mut two = raw.clone();
        two.extend_from_slice(&raw[..raw.len() / 3]);
        assert!(inflate_gzip_members_limited(&two, LIMIT, CTX).is_err());
        assert!(streamed(&two).is_err());
    }

    #[test]
    fn corrupt_crc_or_isize_is_an_error() {
        let data = payload(50_000, 10);
        let raw = gzip(&data);
        let n = raw.len();
        let mut bad_crc = raw.clone();
        bad_crc[n - 8] ^= 1;
        assert!(inflate_gzip_members_limited(&bad_crc, LIMIT, CTX).is_err());
        assert!(streamed(&bad_crc).is_err());
        for bad_len in [data.len() as u32 - 1, data.len() as u32 + 1, 0, u32::MAX] {
            let mut bad_isize = raw.clone();
            bad_isize[n - 4..].copy_from_slice(&bad_len.to_le_bytes());
            assert!(
                inflate_gzip_members_limited(&bad_isize, LIMIT, CTX).is_err(),
                "ISIZE {bad_len}"
            );
            assert!(streamed(&bad_isize).is_err(), "streamed ISIZE {bad_len}");
        }
        let mut bad_body = raw.clone();
        bad_body[n / 2] ^= 0x55;
        assert!(inflate_gzip_members_limited(&bad_body, LIMIT, CTX).is_err());
        assert!(streamed(&bad_body).is_err());
    }

    #[test]
    fn limit_accepts_exact_size_and_rejects_one_more_byte() {
        let data = payload(100_000, 11);
        let raw = gzip(&data);
        assert_eq!(
            inflate_gzip_members_limited(&raw, data.len(), CTX).unwrap(),
            data
        );
        let err = inflate_gzip_members_limited(&raw, data.len() - 1, CTX).unwrap_err();
        assert!(err.contains("expands beyond"), "{err}");
        // Multi-member output is limited in aggregate.
        let mut two = raw.clone();
        two.extend(gzip(&data));
        let err = inflate_gzip_members_limited(&two, data.len() * 2 - 1, CTX).unwrap_err();
        assert!(err.contains("expands beyond"), "{err}");
        assert_eq!(
            inflate_gzip_members_limited(&two, data.len() * 2, CTX)
                .unwrap()
                .len(),
            data.len() * 2
        );
    }

    #[test]
    fn size_hint_ignores_implausible_or_over_limit_trailers() {
        let raw = gzip(&payload(1_000, 12));
        let n = raw.len();
        let mut huge = raw.clone();
        huge[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(gzip_size_hint(&huge, LIMIT), 0);
        assert_eq!(gzip_size_hint(&raw, 999), 0);
        assert_eq!(gzip_size_hint(&raw, 1_000), 1_000);
        assert_eq!(gzip_size_hint(&raw[..GZIP_MIN_MEMBER_LEN - 1], LIMIT), 0);
    }

    #[test]
    fn streaming_reader_handles_one_byte_fills() {
        // A reader that hands out one byte per fill: the magic of every
        // member after the first is seen one byte at a time.
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let count = buf.len().min(self.0.len()).min(1);
                buf[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        impl BufRead for OneByte<'_> {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Ok(&self.0[..self.0.len().min(1)])
            }
            fn consume(&mut self, amt: usize) {
                self.0 = &self.0[amt..];
            }
        }
        let parts = [payload(30_000, 13), payload(20_000, 14)];
        let mut raw = gzip(&parts[0]);
        raw.extend(gzip(&parts[1]));
        let mut out = Vec::new();
        MultiGzReader::new(OneByte(&raw))
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, parts.concat());
        // Trailing zero padding after the last member is ignored.
        raw.extend_from_slice(&[0u8; 100]);
        let mut out = Vec::new();
        MultiGzReader::new(OneByte(&raw))
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, parts.concat());
    }
}
