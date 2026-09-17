//! Equivalence with the reference decoder on real NEXRAD Level II data.
//!
//! Inputs come from the test corpus (`recast-radar-testdata`): every LDM
//! record of four archive volumes and the committed KIWA real-time chunks,
//! plus multi-block streams made by recompressing real record bytes with the
//! reference encoder. Tests skip (with a note) when a downloadable volume is
//! unavailable offline.

mod common;

use common::{
    KIWA_CHUNK_2, KIWA_START_CHUNK, VOLUMES, block_count, ldm_records, ours, reference_decode,
    reference_encode, testdata, volume_records,
};
use recast_radar_bzip2::{Decoder, Error};
use recast_radar_testdata::sha256_hex;

/// Every record of every corpus volume decodes to the reference bytes,
/// through the single-stream API with one reused decoder and through
/// `decode_two_into` with every record on each side of a pair.
#[test]
fn every_record_matches_the_reference() {
    let mut dec = Decoder::new();
    let mut checked = 0usize;
    for &(id, count) in VOLUMES {
        let Some(records) = volume_records(id, count) else {
            continue;
        };
        let expected: Vec<Vec<u8>> = records
            .iter()
            .enumerate()
            .map(|(i, r)| reference_decode(r).unwrap_or_else(|e| panic!("{id} record {i}: {e}")))
            .collect();
        check_volume(&mut dec, id, &records, &expected);
        checked += records.len();
    }
    // Committed real-time chunks: always available, one record each.
    for id in [KIWA_START_CHUNK, KIWA_CHUNK_2] {
        let bytes = testdata(id).expect("committed fixture");
        let records = ldm_records(&bytes);
        assert_eq!(records.len(), 1, "{id}: one LDM record");
        let expected = vec![reference_decode(&records[0]).expect("reference decodes")];
        check_volume(&mut dec, id, &records, &expected);
        checked += 1;
    }
    eprintln!("{checked} records match the reference (single and paired)");
    assert!(checked >= 2);
}

fn check_volume(dec: &mut Decoder, id: &str, records: &[Vec<u8>], expected: &[Vec<u8>]) {
    // Single-stream API, one decoder reused across all records.
    let mut out = Vec::new();
    for (i, r) in records.iter().enumerate() {
        out.clear();
        dec.decode_stream_into(r, &mut out)
            .unwrap_or_else(|e| panic!("{id} record {i}: {e}"));
        assert!(out == expected[i], "{id} record {i}: output differs");
    }
    // Paired API over consecutive records, with both alignments so every
    // record is decoded both as `a` and as `b`.
    for shift in 0..2 {
        let mut i = shift;
        while i + 1 < records.len() {
            let (mut oa, mut ob) = (vec![0xAB], Vec::new());
            let (ra, rb) = dec.decode_two_into(&records[i], &mut oa, &records[i + 1], &mut ob);
            ra.unwrap_or_else(|e| panic!("{id} pair a {i}: {e}"));
            rb.unwrap_or_else(|e| panic!("{id} pair b {}: {e}", i + 1));
            assert_eq!(oa[0], 0xAB, "append semantics");
            assert!(
                oa[1..] == expected[i][..],
                "{id} pair a {i}: output differs"
            );
            assert!(
                ob == expected[i + 1],
                "{id} pair b {}: output differs",
                i + 1
            );
            i += 2;
        }
    }
    // The same record on both sides, and first with last (size mismatch).
    let last = records.len() - 1;
    for (x, y) in [(0usize, 0usize), (0, last), (last, 0)] {
        let (mut oa, mut ob) = (Vec::new(), Vec::new());
        let (ra, rb) = dec.decode_two_into(&records[x], &mut oa, &records[y], &mut ob);
        ra.unwrap_or_else(|e| panic!("{id} pair ({x}, {y}) a: {e}"));
        rb.unwrap_or_else(|e| panic!("{id} pair ({x}, {y}) b: {e}"));
        assert!(
            oa == expected[x] && ob == expected[y],
            "{id} pair ({x}, {y})"
        );
    }
}

/// The two bench volumes against the per-record SHA-256 lists recorded from
/// C libbzip2 1.0.8 (`tests/golden/`), and the whole-volume digests of the
/// decoder shootout (RESULTS: d5ce40df... and a2c7122b...).
#[test]
fn records_match_the_golden_sha256_lists() {
    const GOLDEN: &[(&str, &str)] = &[
        (
            "l2-ktlx-20240315-000217",
            "d5ce40df21fddfc9efe7e412d6140b5139b7489efbb8d2042d5c8c71be2733b0",
        ),
        (
            "l2-kilx-20260418-013553",
            "a2c7122b4a9c668a9bdcce5aab206450858f2c5b1ee828906a63b700dc8cc509",
        ),
    ];
    let mut dec = Decoder::new();
    for &(id, whole) in GOLDEN {
        let golden = std::fs::read_to_string(format!(
            "{}/tests/golden/{id}.records.sha256",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("golden list");
        let golden: Vec<(usize, usize, &str)> = golden
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let mut f = l.split_whitespace();
                let index = f.next().and_then(|s| s.parse().ok()).expect("index");
                let len = f.next().and_then(|s| s.parse().ok()).expect("length");
                (index, len, f.next().expect("sha256"))
            })
            .collect();
        let Some(records) = volume_records(id, golden.len()) else {
            continue;
        };
        let mut all = Vec::new();
        for (i, r) in records.iter().enumerate() {
            let mut out = Vec::new();
            dec.decode_stream_into(r, &mut out)
                .unwrap_or_else(|e| panic!("{id} record {i}: {e}"));
            let (index, len, sha) = golden[i];
            assert_eq!(index, i);
            assert_eq!(out.len(), len, "{id} record {i}: decoded length");
            assert_eq!(sha256_hex(&out), sha, "{id} record {i}: sha256");
            all.extend_from_slice(&out);
        }
        assert_eq!(sha256_hex(&all), whole, "{id}: whole-volume sha256");
        eprintln!(
            "{id}: {} records match the golden sha256 list",
            records.len()
        );
    }
}

/// Real bytes to recompress: decoded records of the smallest archive volume
/// and of the KTLX bench volume, plus a stretch of the compressed KTLX file
/// itself (already-compressed data gives blocks with a very different symbol
/// mix). About 6 MB, so level 1 yields dozens of blocks.
fn real_bytes_for_recompression(dec: &mut Decoder) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    let tstl = volume_records("l2-tstl-20230331-230314", 70)?;
    let ktlx_file = testdata("l2-ktlx-20240315-000217")?;
    for r in tstl
        .iter()
        .take(30)
        .chain(ldm_records(&ktlx_file).iter().take(3))
    {
        dec.decode_stream_into(r, &mut raw)
            .unwrap_or_else(|e| panic!("real record: {e}"));
    }
    raw.extend_from_slice(&ktlx_file[..1_000_000]);
    Some(raw)
}

/// Multi-block streams: real bytes compressed by the reference encoder at
/// level 1 (100 kB blocks) and level 9 (900 kB blocks) decode to the input,
/// singly, paired (different block counts on the two sides) and when two
/// streams are concatenated (the first stream only, as the reference).
#[test]
fn multi_block_streams_at_levels_1_and_9() {
    let mut dec = Decoder::new();
    let Some(raw) = real_bytes_for_recompression(&mut dec) else {
        return;
    };
    let mut streams = Vec::new();
    let mut counts = Vec::new();
    for level in [1u32, 9] {
        let stream = reference_encode(level, &raw);
        let blocks = block_count(&stream);
        assert!(
            blocks >= 2,
            "level {level}: {blocks} block(s), want a multi-block stream"
        );
        assert!(reference_decode(&stream).expect("reference") == raw);
        let mut out = b"prefix".to_vec();
        dec.decode_stream_into(&stream, &mut out)
            .unwrap_or_else(|e| panic!("level {level}: {e}"));
        assert!(
            out[..6] == b"prefix"[..] && out[6..] == raw[..],
            "level {level}: output"
        );
        eprintln!(
            "bzip2 level {level}: {} bytes -> {} bytes, {blocks} blocks",
            stream.len(),
            raw.len()
        );
        streams.push(stream);
        counts.push(blocks);
    }
    assert!(
        counts[0] > counts[1],
        "level 1 has more blocks than level 9"
    );
    // Paired: level-1 and level-9 streams in lockstep.
    let (mut oa, mut ob) = (Vec::new(), Vec::new());
    let (ra, rb) = dec.decode_two_into(&streams[0], &mut oa, &streams[1], &mut ob);
    ra.expect("paired a");
    rb.expect("paired b");
    assert!(oa == raw && ob == raw);
    // Two streams back to back: only the first is decoded, like the
    // reference (which stops at BZ_STREAM_END).
    let mut cat = streams[1].clone();
    cat.extend_from_slice(&streams[0]);
    assert!(ours(&mut dec, &cat).expect("concatenated") == raw);
    assert!(reference_decode(&cat).expect("reference") == raw);
}

/// The output limit is exact and counted from the vector's starting length:
/// exactly the decoded size is `Ok`, one byte less is `Err(OutputLimit)`
/// with the vector restored, on single-block records and on a multi-block
/// stream (where the limit trips at the block that would cross it).
#[test]
fn output_limit_is_exact() {
    let mut dec = Decoder::new();
    let Some(records) = volume_records("l2-ktlx-20240315-000217", 97) else {
        return;
    };
    for r in [&records[0], &records[3], &records[96]] {
        let expected = reference_decode(r).expect("reference");
        let n = expected.len();
        for prefix_len in [0usize, 7] {
            let prefix = vec![0x5Au8; prefix_len];
            dec.set_max_output(n - 1);
            let mut out = prefix.clone();
            assert_eq!(dec.decode_stream_into(r, &mut out), Err(Error::OutputLimit));
            assert_eq!(out, prefix, "restored on error");
            dec.set_max_output(n);
            dec.decode_stream_into(r, &mut out)
                .expect("exactly the size");
            assert!(out[..prefix_len] == prefix[..] && out[prefix_len..] == expected[..]);
        }
        // Paired: the limit applies to each side separately.
        dec.set_max_output(n - 1);
        let (mut oa, mut ob) = (Vec::new(), vec![1u8]);
        let (ra, rb) = dec.decode_two_into(r, &mut oa, r, &mut ob);
        assert_eq!((ra, rb), (Err(Error::OutputLimit), Err(Error::OutputLimit)));
        assert!(oa.is_empty() && ob == [1]);
        dec.set_max_output(n);
        let (ra, rb) = dec.decode_two_into(r, &mut oa, r, &mut ob);
        assert_eq!((ra, rb), (Ok(()), Ok(())));
        assert!(oa == expected && ob[1..] == expected[..]);
    }
    // Multi-block: a level-1 stream of real bytes.
    dec.set_max_output(usize::MAX);
    let mut raw = Vec::new();
    for r in records.iter().take(2) {
        dec.decode_stream_into(r, &mut raw).expect("real record");
    }
    let stream = reference_encode(1, &raw);
    assert!(block_count(&stream) >= 2);
    let n = raw.len();
    for limit in [0usize, 1, n / 2, n - 1] {
        dec.set_max_output(limit);
        let mut out = Vec::new();
        assert_eq!(
            dec.decode_stream_into(&stream, &mut out),
            Err(Error::OutputLimit),
            "limit {limit}"
        );
        assert!(out.is_empty(), "limit {limit}: restored");
    }
    dec.set_max_output(n);
    let mut out = Vec::new();
    dec.decode_stream_into(&stream, &mut out)
        .expect("exactly the size");
    assert!(out == raw);
}

/// Bit-level helpers for building a valid legacy randomised block.
fn get_bits(d: &[u8], at: usize, n: usize) -> u64 {
    (0..n).fold(0u64, |acc, k| {
        (acc << 1) | u64::from((d[(at + k) / 8] >> (7 - (at + k) % 8)) & 1)
    })
}

fn set_bits(d: &mut [u8], at: usize, n: usize, v: u64) {
    for k in 0..n {
        let bit = ((v >> (n - 1 - k)) & 1) as u8;
        let (byte, sh) = ((at + k) / 8, 7 - (at + k) % 8);
        d[byte] = (d[byte] & !(1 << sh)) | (bit << sh);
    }
}

/// Randomised blocks (bzip2 0.9.0 and earlier). No real NEXRAD record has
/// the randomised bit set, so the test derives its inputs from real records:
/// set the bit on single-block records, then patch the stored block CRC and
/// the combined CRC to the CRC of the derandomised output. The reference
/// must accept the result, and both decoders must produce identical bytes.
#[test]
fn randomised_blocks_match_the_reference() {
    let mut dec = Decoder::new();
    let mut tested = 0;
    let mut changed = 0;
    for &(id, count) in VOLUMES {
        let Some(records) = volume_records(id, count) else {
            continue;
        };
        for (i, r) in records.iter().enumerate().filter(|(i, _)| i % 9 == 1) {
            if block_count(r) != 1 {
                continue;
            }
            let mut m = r.clone();
            let old_crc = get_bits(&m, 80, 32);
            assert_eq!(get_bits(&m, 112, 1), 0, "{id} record {i}: not randomised");
            set_bits(&mut m, 112, 1, 1);
            // Locate the end-of-stream marker + combined CRC (= block CRC).
            let total = m.len() * 8;
            let eos = (0..=total - 80)
                .rev()
                .find(|&p| {
                    get_bits(&m, p, 48) == 0x1772_4538_5090 && get_bits(&m, p + 48, 32) == old_crc
                })
                .expect("end-of-stream marker");
            dec.__set_check_crc(false);
            let mut out = Vec::new();
            let r0 = dec.decode_stream_into(&m, &mut out);
            let crcs = dec.__seen_block_crcs().to_vec();
            dec.__set_check_crc(true);
            if r0.is_err() {
                // For example a run of four at the very end after
                // derandomisation; the reference rejects it too.
                assert!(reference_decode(&m).is_err(), "{id} record {i}");
                continue;
            }
            assert_eq!(crcs.len(), 1);
            set_bits(&mut m, 80, 32, u64::from(crcs[0].0));
            set_bits(&mut m, eos + 48, 32, u64::from(crcs[0].0));
            let reference = reference_decode(&m).unwrap_or_else(|e| panic!("{id} record {i}: {e}"));
            let got = ours(&mut dec, &m).unwrap_or_else(|e| panic!("{id} record {i}: {e}"));
            assert!(
                got == reference,
                "{id} record {i}: randomised output differs"
            );
            // The mask first flips a byte at pre-RLE1 offset 618, so only a
            // block shorter than that (the TBWI stub) decodes unchanged.
            if got != reference_decode(r).expect("reference") {
                changed += 1;
            }
            tested += 1;
        }
    }
    eprintln!("randomised records verified: {tested}, {changed} with bytes changed by the mask");
    assert!(
        tested == 0 || (tested >= 10 && changed >= 10),
        "partial corpus"
    );
}

/// Headers, restoration and appending, level digits, padding bits in the
/// last byte, trailing bytes, and paired calls with invalid or empty sides.
#[test]
fn edge_cases() {
    let mut dec = Decoder::new();
    // An empty stream from the reference encoder.
    let empty = reference_encode(9, b"");
    assert_eq!(ours(&mut dec, &empty).expect("empty"), Vec::<u8>::new());
    assert_eq!(reference_decode(&empty).expect("empty"), Vec::<u8>::new());
    for bad in [
        &b""[..],
        b"B",
        b"BZh",
        b"BZh0",
        b"BZh9",
        b"BZhA",
        b"BZX9",
        b"BZh91AY&SY",
    ] {
        assert!(ours(&mut dec, bad).is_err(), "{bad:?}");
        assert!(reference_decode(bad).is_err(), "{bad:?}");
    }
    assert_eq!(ours(&mut dec, b"not bzip2"), Err(Error::BadStreamHeader));
    // A real record.
    let bytes = testdata(KIWA_START_CHUNK).expect("committed fixture");
    let records = ldm_records(&bytes);
    let good = &records[0];
    let expect = reference_decode(good).expect("reference");
    // Output restored on error; appended on success.
    let mut out = b"prefix".to_vec();
    assert!(
        dec.decode_stream_into(&good[..good.len() / 2], &mut out)
            .is_err()
    );
    assert_eq!(out, b"prefix");
    dec.decode_stream_into(good, &mut out).expect("good");
    assert!(out[..6] == b"prefix"[..] && out[6..] == expect[..]);
    // Trailing bytes after the end-of-stream marker are ignored.
    let mut trailing = good.clone();
    trailing.extend_from_slice(b"trailing garbage after the stream");
    assert!(ours(&mut dec, &trailing).expect("trailing") == expect);
    assert!(reference_decode(&trailing).expect("trailing") == expect);
    // Flipping bits of the last byte: padding bits are ignored, stream CRC
    // bits are not; both decoders agree bit by bit.
    let last = good.len() - 1;
    let mut padding = 0;
    for bit in 0..8 {
        let mut m = good.clone();
        m[last] ^= 1 << bit;
        let a = ours(&mut dec, &m);
        let b = reference_decode(&m);
        assert_eq!(a.is_ok(), b.is_ok(), "last byte bit {bit}");
        if let (Ok(x), Ok(y)) = (a, b) {
            assert!(x == y && x == expect);
            padding += 1;
        }
    }
    eprintln!("padding bits in the last byte: {padding}");
    // A different level digit changes only the block-size bound.
    let mut lvl = good.clone();
    for d in b'1'..=b'9' {
        lvl[3] = d;
        let a = ours(&mut dec, &lvl);
        let b = reference_decode(&lvl);
        assert_eq!(a.is_ok(), b.is_ok(), "level {}", d as char);
        if let (Ok(x), Ok(y)) = (a, b) {
            assert!(x == y);
        }
    }
    // Paired with both sides invalid, and with an empty side.
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let (ra, rb) = dec.decode_two_into(b"BZh9", &mut a, b"", &mut b);
    assert_eq!(
        (ra, rb),
        (Err(Error::UnexpectedEof), Err(Error::BadStreamHeader))
    );
    let (ra, rb) = dec.decode_two_into(&empty, &mut a, good, &mut b);
    assert_eq!((ra, rb), (Ok(()), Ok(())));
    assert!(a.is_empty() && b == expect);
    // Default is the same as new.
    let mut d2 = Decoder::default();
    assert!(ours(&mut d2, good).expect("default decoder") == expect);
    assert_eq!(
        Error::OutputLimit.to_string(),
        "bzip2: output limit exceeded"
    );
}
