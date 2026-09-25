//! Shared helpers: real LDM records from the test corpus, the reference
//! decoder, a seeded generator and the differential check.
//!
//! The reference is the `bzip2` crate's pure-Rust backend (`libbz2-rs-sys`,
//! a port of libbzip2 1.0.8). Every test input is real data resolved through
//! `recast-radar-testdata` by manifest id, or real bytes recompressed by the
//! reference encoder.

#![allow(dead_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use bzip2::{Decompress, Status};
use recast_radar_bzip2::{Decoder, Encoder, Error};

/// Archive II volume header (`AR2V0006.123`, date, time, ICAO).
pub const VOLUME_HEADER_LEN: usize = 24;

/// Archive volumes whose every LDM record is checked, with their record
/// counts: the two `bench` volumes (WSR-88D, super-resolution, builds 22.0
/// and 23.1, with per-record SHA-256 lists under `tests/golden/`), a TDWR
/// volume (legacy resolution, records as small as 255 bytes) and the
/// status-only TBWI stub (three records of under 100 bytes).
pub const VOLUMES: &[(&str, usize)] = &[
    ("l2-ktlx-20240315-000217", 97),
    ("l2-kilx-20260418-013553", 106),
    ("l2-tstl-20230331-230314", 70),
    ("l2-tbwi-20230601-175101-stub", 3),
];

/// Committed real-time chunks of KIWA volume 307: one LDM record each. The
/// Start chunk has the volume header, the intermediate chunk does not.
pub const KIWA_START_CHUNK: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
pub const KIWA_CHUNK_2: &str = "l2chunk-kiwa-307-20260917-003629-002-i";

/// The bytes of a testdata entry, or `None` (with a note on stderr) when the
/// file is neither committed nor cached and cannot be downloaded right now.
/// Any other failure panics.
pub fn testdata(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes(id) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.is_offline() => {
            eprintln!("skipping {id}: {e}");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

/// The LDM block-bzip2 archive `id` split into its records, or `None` when
/// the file is unavailable offline: after the optional 24-byte volume header,
/// each record is a big-endian `i32` length (negative for the last record)
/// followed by one bzip2 stream.
pub fn ldm_records(id: &str) -> Option<Vec<Vec<u8>>> {
    let bytes = testdata(id)?;
    let bytes = bytes.as_slice();
    let mut cursor = if bytes.starts_with(b"AR2V") || bytes.starts_with(b"ARCH") {
        VOLUME_HEADER_LEN
    } else {
        0
    };
    let mut records = Vec::new();
    while let Some(word) = bytes.get(cursor..cursor + 4) {
        let len = i32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        let size = len.unsigned_abs() as usize;
        cursor += 4;
        if size == 0 {
            break;
        }
        let record = &bytes[cursor..cursor + size];
        assert!(
            record.starts_with(b"BZh"),
            "record at {} is not a bzip2 stream",
            cursor - 4
        );
        records.push(record.to_vec());
        cursor += size;
        if len < 0 {
            break;
        }
    }
    Some(records)
}

/// Every record of `id`, or `None` when the file is unavailable offline.
pub fn volume_records(id: &str, expected: usize) -> Option<Vec<Vec<u8>>> {
    let records = ldm_records(id)?;
    assert_eq!(records.len(), expected, "{id}: LDM record count");
    Some(records)
}

/// Reference decode: one stream from the start of `input`, trailing bytes
/// ignored (the decoder stops at `Status::StreamEnd`).
pub fn reference_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let (out, result) = reference_decode_partial(input, 1 << 16);
    result.map(|()| out)
}

/// Reference decode that also returns the bytes emitted before an error.
///
/// `chunk` is the output space guaranteed before each call into the
/// reference. When its RLE1 stage flags corruption, libbzip2 returns without
/// counting the bytes that call wrote, so the length of the partial output
/// depends on the buffer boundaries; callers that compare partial output
/// pass a chunk larger than any block's output.
pub fn reference_decode_partial(input: &[u8], chunk: usize) -> (Vec<u8>, Result<(), String>) {
    let mut d = Decompress::new(false);
    let mut out: Vec<u8> = Vec::with_capacity(chunk);
    loop {
        if out.capacity() - out.len() < chunk {
            out.reserve(chunk.max(out.capacity()));
        }
        let in_before = d.total_in();
        let out_before = d.total_out();
        match d.decompress_vec(&input[in_before as usize..], &mut out) {
            Err(e) => return (out, Err(format!("reference: {e}"))),
            Ok(Status::StreamEnd) => return (out, Ok(())),
            Ok(_) => {}
        }
        if d.total_in() == in_before && d.total_out() == out_before {
            return (out, Err("reference: unexpected end of input".to_owned()));
        }
    }
}

/// Reference encode with the pure-Rust encoder at `level` (1..=9, the block
/// size in units of 100 kB).
pub fn reference_encode(level: u32, data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
    encoder
        .write_all(data)
        .unwrap_or_else(|e| panic!("reference encode: {e}"));
    encoder
        .finish()
        .unwrap_or_else(|e| panic!("reference encode: {e}"))
}

/// Reference encode in one `BZ_FINISH` call, as `BZ2_bzBuffToBuffCompress`
/// does (default work factor). This is the call the encoder's block
/// boundaries follow; [`reference_encode`] feeds the input with `BZ_RUN`
/// first, which differs only when the input ends exactly as a block fills.
pub fn reference_compress(level: u32, data: &[u8]) -> Vec<u8> {
    let mut c = bzip2::Compress::new(bzip2::Compression::new(level), 30);
    let mut out = Vec::with_capacity(data.len() + data.len() / 50 + 1024);
    match c.compress_vec(data, &mut out, bzip2::Action::Finish) {
        Ok(Status::StreamEnd) => out,
        other => panic!("reference compress: {other:?}"),
    }
}

/// Our encoder as a function: one stream into a fresh vector.
pub fn encode(enc: &mut Encoder, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    enc.encode_into(data, &mut out);
    out
}

/// Check our stream against the reference encoder's for the same input:
/// byte-identical, except that the `origPtr` field of a periodic block
/// (reported by the encoder's test hook) may name another of its identical
/// rows.
pub fn assert_matches_reference(enc: &Encoder, ours: &[u8], reference: &[u8], what: &str) {
    if ours == reference {
        return;
    }
    assert_eq!(
        ours.len(),
        reference.len(),
        "{what}: stream length differs from the reference"
    );
    let fields = enc.__periodic_orig_ptr_bits();
    for (i, (a, b)) in ours.iter().zip(reference).enumerate() {
        let x = a ^ b;
        for bit in 0..8 {
            if x & (0x80 >> bit) != 0 {
                let at = (i * 8 + bit) as u64;
                assert!(
                    fields.iter().any(|&f| (f..f + 24).contains(&at)),
                    "{what}: bit {at} differs from the reference outside a periodic block's origPtr"
                );
            }
        }
    }
}

/// Our decoder as a function: one stream into a fresh vector.
pub fn ours(dec: &mut Decoder, input: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    dec.decode_stream_into(input, &mut out).map(|()| out)
}

/// Number of blocks in `stream`, counted with CRC checks off (each block
/// logs one CRC); the stream must be structurally valid.
pub fn block_count(stream: &[u8]) -> usize {
    let mut d = Decoder::new();
    d.__set_check_crc(false);
    let mut v = Vec::new();
    d.decode_stream_into(stream, &mut v)
        .unwrap_or_else(|e| panic!("block_count of an invalid stream: {e}"));
    d.__seen_block_crcs().len()
}

/// splitmix64
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Outcome counts of [`check_case`].
#[derive(Default, Debug)]
pub struct Tally {
    pub cases: usize,
    pub both_ok: usize,
    pub both_err: usize,
    /// Inputs we reject that the reference accepts (must stay 0).
    pub ours_err_ref_ok: usize,
    /// Corrupt inputs whose blocks still decode structurally: our output
    /// with CRC checks off equals the bytes the reference wrote before its
    /// CRC error.
    pub nocrc_equal: usize,
    /// Corrupt inputs where both decoders stop at the same block for a
    /// structural reason.
    pub nocrc_both_structural_err: usize,
}

/// Differential check of one (possibly corrupt) input against the
/// reference. Panics on any divergence: a panic inside our decoder,
/// accepting what the reference rejects, different output, or (with our CRC
/// checks off) a different structural stopping point or different emitted
/// bytes.
pub fn check_case(dec: &mut Decoder, input: &[u8], t: &mut Tally, what: &str) {
    t.cases += 1;
    let got = catch_unwind(AssertUnwindSafe(|| ours(dec, input)))
        .unwrap_or_else(|_| panic!("panic on {what}"));
    let reference = reference_decode(input);
    match (&got, &reference) {
        (Ok(a), Ok(b)) => {
            assert!(a == b, "output differs from the reference on {what}");
            t.both_ok += 1;
        }
        (Ok(_), Err(e)) => panic!("accepted input the reference rejects ({e}) on {what}"),
        (Err(e), Ok(_)) => {
            eprintln!("NOTE: we reject ({e}) but the reference accepts: {what}");
            t.ours_err_ref_ok += 1;
        }
        (Err(_), Err(_)) => {
            t.both_err += 1;
            // Decode-path equivalence on corrupt data: skip our CRC checks
            // and compare against what the reference emitted before failing.
            dec.__set_check_crc(false);
            let nocrc = catch_unwind(AssertUnwindSafe(|| ours(dec, input)))
                .unwrap_or_else(|_| panic!("panic (no-crc) on {what}"));
            let blocks = dec.__seen_block_crcs().to_vec();
            dec.__set_check_crc(true);
            // One buffer larger than any block's output (900k symbols, each
            // RLE1 quintet expanding to at most 259 bytes).
            let (partial, _) = reference_decode_partial(input, 64 << 20);
            match nocrc {
                Ok(v) => {
                    // The reference stops right after the first block whose
                    // CRC does not match (or at the end, for a stream CRC
                    // error).
                    let stop = blocks
                        .iter()
                        .find(|(c, s, _)| c != s)
                        .map_or(v.len(), |b| b.2);
                    assert!(
                        stop == partial.len() && v[..stop] == partial[..],
                        "no-crc output differs from the reference's partial output on {what} \
                         ({stop} vs {} bytes)",
                        partial.len()
                    );
                    t.nocrc_equal += 1;
                }
                Err(_) => {
                    // We stopped structurally in some block. The reference
                    // must stop no later: its emitted bytes end where our
                    // last completed block (or first CRC-mismatched block)
                    // ends.
                    let completed = blocks.last().map_or(0, |b| b.2);
                    let stop = blocks
                        .iter()
                        .find(|(c, s, _)| c != s)
                        .map_or(completed, |b| b.2);
                    assert!(
                        stop == partial.len(),
                        "the reference emitted {} bytes but we stopped structurally after \
                         {stop} on {what}",
                        partial.len()
                    );
                    t.nocrc_both_structural_err += 1;
                }
            }
        }
    }
}
