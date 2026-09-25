//! The encoder against libbzip2 1.0.8 on real data.
//!
//! NOAA writes the LDM records of NEXRAD Level II archives with libbzip2,
//! so the published records are an independent golden: every record must
//! re-encode to exactly its published bytes. Other inputs (multi-block
//! streams, block-boundary cases, every test corpus file) are compared with
//! the reference encoder, the `bzip2` crate's port of libbzip2 1.0.8, and
//! every stream must decode to its input with both our decoder and the
//! reference decoder. Tests skip (with a note) when a downloadable volume is
//! unavailable offline; the committed KIWA chunks always run.

mod common;

use common::{
    KIWA_CHUNK_2, KIWA_START_CHUNK, VOLUMES, assert_matches_reference, block_count, encode,
    ldm_records, ours, reference_compress, reference_decode, testdata, volume_records,
};
use recast_radar_bzip2::{Decoder, Encoder, Level};

fn level_of(record: &[u8]) -> Level {
    let digit = u32::from(record[3] - b'0');
    Level::new(digit).unwrap_or_else(|| panic!("level digit {digit}"))
}

/// The bzip2 records of testdata file `id` when it is a Level II file stored
/// as LDM records (after the optional 24-byte volume header, a big-endian
/// length word before each record, negative for the last); `None` for any
/// other layout, or when the file is unavailable (see [`local_testdata`]).
fn ldm_bzip2_records(id: &str) -> Option<Vec<Vec<u8>>> {
    let bytes = local_testdata(id)?;
    let bytes = bytes.as_slice();
    let mut cursor = if bytes.starts_with(b"AR2V") || bytes.starts_with(b"ARCH") {
        24
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
        let record = bytes.get(cursor..cursor + size)?;
        if !record.starts_with(b"BZh") {
            return None;
        }
        records.push(record.to_vec());
        cursor += size;
        if len < 0 {
            break;
        }
    }
    (!records.is_empty()).then_some(records)
}

/// Run `f` on every item on a few threads.
fn in_parallel<T: Sync>(items: &[T], f: impl Fn(&T) + Sync) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    f(item);
                }
            });
        }
    });
}

/// Every LDM record re-encodes, at the level in its header, to the
/// published bytes: in release builds every record of every Level II file
/// and real-time chunk of the corpus that is committed or cached (NOAA
/// volumes from 2020 to 2026, TDWR volumes and real-time chunks as
/// published, and the trimmed fixtures of 1991 to 2026 volumes, whose
/// records the fixture tool recompressed at level 9), otherwise the four
/// corpus volumes and the two committed KIWA chunks. The 1991 to 2016
/// volumes themselves are published gzip-wrapped, and this test reads no
/// records from them.
#[test]
fn records_reencode_to_the_published_bytes() {
    let ids: Vec<&str> = if cfg!(debug_assertions) {
        VOLUMES
            .iter()
            .map(|&(id, _)| id)
            .chain([KIWA_START_CHUNK, KIWA_CHUNK_2])
            .collect()
    } else {
        recast_radar_testdata::manifest()
            .files
            .iter()
            .filter(|e| e.format.as_str().starts_with("nexrad-level2"))
            .map(|e| e.id.as_str())
            .collect()
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (records, files) = (AtomicUsize::new(0), AtomicUsize::new(0));
    in_parallel(&ids, |&id| {
        let Some(list) = ldm_bzip2_records(id) else {
            return;
        };
        let mut encoders: Vec<Encoder> = (1..=9)
            .map(|l| Encoder::new(Level::new(l).expect("level")))
            .collect();
        for (i, r) in list.iter().enumerate() {
            let payload = reference_decode(r).unwrap_or_else(|e| panic!("{id} record {i}: {e}"));
            let level = level_of(r);
            let got = encode(&mut encoders[level.get() as usize - 1], &payload);
            assert!(
                got == *r,
                "{id} record {i}: re-encoded stream differs from the published record \
                 ({} vs {} bytes)",
                got.len(),
                r.len()
            );
        }
        records.fetch_add(list.len(), Ordering::Relaxed);
        files.fetch_add(1, Ordering::Relaxed);
    });
    let (records, files) = (records.into_inner(), files.into_inner());
    eprintln!("{records} records of {files} files re-encode to their published bytes");
    // The committed KIWA chunks are always there.
    assert!(records >= 2);
}

/// A committed or already cached testdata file; `None` (with a note) when it
/// would have to be downloaded, in release builds, or cannot be right now.
fn local_testdata(id: &str) -> Option<Vec<u8>> {
    if cfg!(debug_assertions) {
        return testdata(id);
    }
    match recast_radar_testdata::local_path(id) {
        Ok(path) => Some(std::fs::read(&path).unwrap_or_else(|e| panic!("{id}: {e}"))),
        Err(e) if e.is_offline() => {
            eprintln!("skipping {id}: {e}");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

/// About 6 MB of real bytes: decoded records of the TDWR volume and of the
/// KTLX bench volume, plus 1 MB of the compressed KTLX file itself (already
/// compressed data: long blocks with a flat symbol mix).
fn real_bytes() -> Option<Vec<u8>> {
    let mut dec = Decoder::new();
    let mut raw = Vec::new();
    let tstl = volume_records("l2-tstl-20230331-230314", 70)?;
    let ktlx = volume_records("l2-ktlx-20240315-000217", 97)?;
    let ktlx_file = testdata("l2-ktlx-20240315-000217")?;
    for r in tstl.iter().take(30).chain(ktlx.iter().take(3)) {
        dec.decode_stream_into(r, &mut raw)
            .unwrap_or_else(|e| panic!("real record: {e}"));
    }
    raw.extend_from_slice(&ktlx_file[..1_000_000]);
    Some(raw)
}

fn round_trip(stream: &[u8], input: &[u8], what: &str) {
    let mut dec = Decoder::new();
    let got = ours(&mut dec, stream).unwrap_or_else(|e| panic!("{what}: our decoder: {e}"));
    assert!(
        got == input,
        "{what}: our decoder output differs from the input"
    );
    let reference =
        reference_decode(stream).unwrap_or_else(|e| panic!("{what}: reference decoder: {e}"));
    assert!(
        reference == input,
        "{what}: reference decoder output differs from the input"
    );
}

/// Multi-block streams at every level (levels 1, 2 and 9 in unoptimised
/// builds) match the reference encoder byte for byte and decode with both
/// decoders.
#[test]
fn multi_block_streams_match_the_reference_at_every_level() {
    let Some(raw) = real_bytes() else {
        return;
    };
    let levels: &[u32] = if cfg!(debug_assertions) {
        &[1, 2, 9]
    } else {
        &[1, 2, 3, 4, 5, 6, 7, 8, 9]
    };
    for &level in levels {
        let mut enc = Encoder::new(Level::new(level).expect("level"));
        let got = encode(&mut enc, &raw);
        let want = reference_compress(level, &raw);
        let what = format!("level {level}");
        assert_matches_reference(&enc, &got, &want, &what);
        assert!(
            got == want,
            "{what}: no periodic block expected in this input"
        );
        round_trip(&got, &raw, &what);
        let blocks = block_count(&got);
        assert!(blocks >= 2, "{what}: {blocks} block(s)");
        eprintln!(
            "{what}: {} -> {} bytes, {blocks} blocks",
            raw.len(),
            got.len()
        );
    }
}

/// Block boundaries follow `BZ2_bzBuffToBuffCompress`: every prefix length
/// of real data around the point where the first level-1 block fills. The
/// window includes the length at which the last input byte is the one whose
/// arrival fills the block (its one-byte run then joins that block).
#[test]
fn block_boundaries_match_the_reference() {
    let Some(raw) = real_bytes() else {
        return;
    };
    // Smallest prefix that the reference writes as two blocks.
    let (mut lo, mut hi) = (1usize, 100_000usize);
    assert_eq!(block_count(&reference_compress(1, &raw[..lo])), 1);
    while block_count(&reference_compress(1, &raw[..hi])) < 2 {
        lo = hi;
        hi = (2 * hi).min(raw.len());
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if block_count(&reference_compress(1, &raw[..mid])) >= 2 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut enc = Encoder::new(Level::FASTEST);
    for len in hi - 40..=hi + 40 {
        let input = &raw[..len];
        let got = encode(&mut enc, input);
        let what = format!("prefix of {len} bytes (first two-block prefix {hi})");
        assert!(
            got == reference_compress(1, input),
            "{what}: differs from the reference"
        );
        round_trip(&got, input, &what);
        assert_eq!(
            block_count(&got),
            if len < hi { 1 } else { 2 },
            "{what}: blocks"
        );
    }
}

/// Empty and very short inputs, at the lowest and highest level.
#[test]
fn empty_and_short_inputs() {
    let records = ldm_records(KIWA_CHUNK_2).expect("committed fixture");
    let payload = reference_decode(&records[0]).expect("reference");
    for level in [1u32, 9] {
        let mut enc = Encoder::new(Level::new(level).expect("level"));
        let empty = encode(&mut enc, b"");
        assert_eq!(empty.len(), 14, "level {level}: empty stream");
        assert!(
            empty == reference_compress(level, b""),
            "level {level}: empty stream"
        );
        round_trip(&empty, b"", "empty");
        for len in (1..=40).chain([255, 256, 1000]) {
            let input = &payload[..len];
            let got = encode(&mut enc, input);
            let what = format!("level {level}, {len} bytes");
            assert!(got == reference_compress(level, input), "{what}");
            round_trip(&got, input, &what);
        }
    }
}

/// `encode_into` appends, and a reused encoder gives the same bytes as a
/// fresh one for every input, in any order.
#[test]
fn appends_and_reuses_buffers() {
    let records = ldm_records(KIWA_START_CHUNK).expect("committed fixture");
    let a = reference_decode(&records[0]).expect("reference");
    let records = ldm_records(KIWA_CHUNK_2).expect("committed fixture");
    let b = reference_decode(&records[0]).expect("reference");
    let mut reused = Encoder::new(Level::BEST);
    assert_eq!(reused.level(), Level::BEST);
    let fresh_a = encode(&mut Encoder::new(Level::BEST), &a);
    let fresh_b = encode(&mut Encoder::new(Level::BEST), &b);
    let mut out = b"prefix".to_vec();
    reused.encode_into(&b, &mut out);
    reused.encode_into(&a, &mut out);
    reused.encode_into(&b, &mut out);
    let mut want = b"prefix".to_vec();
    want.extend_from_slice(&fresh_b);
    want.extend_from_slice(&fresh_a);
    want.extend_from_slice(&fresh_b);
    assert!(out == want);
}

/// Blocks that are exact repetitions of a shorter string: a stretch of a
/// real record repeated, and one byte value of it repeated. They decode to
/// the input with both decoders and differ from the reference at most in
/// the `origPtr` field of a periodic block.
#[test]
fn periodic_blocks() {
    let records = ldm_records(KIWA_CHUNK_2).expect("committed fixture");
    let payload = reference_decode(&records[0]).expect("reference");
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    for period in [1usize, 2, 7, 1000, 20_000] {
        let unit = &payload[5000..5000 + period];
        for len in [100_000usize, 2_000_000] {
            let input: Vec<u8> = unit.iter().copied().cycle().take(len).collect();
            cases.push((format!("period {period}, {len} bytes"), input));
        }
    }
    let mut periodic_blocks = 0;
    for level in [1u32, 9] {
        let mut enc = Encoder::new(Level::new(level).expect("level"));
        for (name, input) in &cases {
            let what = format!("level {level}, {name}");
            let got = encode(&mut enc, input);
            assert_matches_reference(&enc, &got, &reference_compress(level, input), &what);
            round_trip(&got, input, &what);
            periodic_blocks += enc.__periodic_orig_ptr_bits().len();
        }
    }
    assert!(periodic_blocks > 0, "the cases include periodic blocks");
}

/// Every test corpus file that is committed or already cached, compressed
/// at level 9: the reference's bytes (up to the periodic-block exception)
/// and both decoders return the file.
#[test]
#[cfg_attr(debug_assertions, ignore = "about 500 MB of input: run with --release")]
fn every_testdata_file_round_trips() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let ids: Vec<&str> = recast_radar_testdata::manifest()
        .files
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    let (files, bytes) = (AtomicUsize::new(0), AtomicUsize::new(0));
    in_parallel(&ids, |&id| {
        let Some(data) = local_testdata(id) else {
            return;
        };
        let mut enc = Encoder::new(Level::BEST);
        let got = encode(&mut enc, &data);
        assert_matches_reference(&enc, &got, &reference_compress(9, &data), id);
        round_trip(&got, &data, id);
        files.fetch_add(1, Ordering::Relaxed);
        bytes.fetch_add(data.len(), Ordering::Relaxed);
    });
    let (files, bytes) = (files.into_inner(), bytes.into_inner());
    eprintln!(
        "{files} of {} files ({bytes} bytes) round-trip; the rest are not cached",
        ids.len()
    );
    assert!(files > 0);
}

/// `encode_many` gives, for each input, the stream `encode_into` writes.
#[cfg(feature = "rayon")]
#[test]
fn encode_many_matches_encode_into() {
    let mut inputs = Vec::new();
    for id in [KIWA_START_CHUNK, KIWA_CHUNK_2] {
        for r in ldm_records(id).expect("committed fixture") {
            inputs.push(reference_decode(&r).expect("reference"));
        }
    }
    let payload = inputs[1].clone();
    for cut in [0usize, 1, 1000, 250_000] {
        inputs.push(payload[..cut.min(payload.len())].to_vec());
    }
    for level in [Level::FASTEST, Level::BEST] {
        let many = recast_radar_bzip2::encode_many(level, &inputs);
        assert_eq!(many.len(), inputs.len());
        let mut enc = Encoder::new(level);
        for (i, input) in inputs.iter().enumerate() {
            assert!(many[i] == encode(&mut enc, input), "input {i}");
        }
    }
}

/// An `EncoderPool` reuses its encoders across inputs and across calls: on
/// a pool of three threads, two calls over 64 inputs create at most three
/// encoders, and every result is what `encode_into` writes.
#[cfg(feature = "rayon")]
#[test]
fn encode_many_pool_reuses_encoders() {
    let records = ldm_records(KIWA_CHUNK_2).expect("committed fixture");
    let payload = reference_decode(&records[0]).expect("reference");
    let inputs: Vec<&[u8]> = payload.chunks(payload.len().div_ceil(64)).collect();
    assert_eq!(inputs.len(), 64);
    let mut enc = Encoder::new(Level::BEST);
    let want: Vec<Vec<u8>> = inputs.iter().map(|i| encode(&mut enc, i)).collect();
    let threads = 3;
    let rayon_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    let pool = recast_radar_bzip2::EncoderPool::new(Level::BEST);
    assert_eq!(pool.level(), Level::BEST);
    assert_eq!(pool.idle_encoders(), 0);
    for call in 0..2 {
        let got = rayon_pool.install(|| pool.encode_many(&inputs));
        assert!(got == want, "call {call}");
        let created = pool.idle_encoders();
        assert!(
            (1..=threads).contains(&created),
            "call {call}: {created} encoders for {threads} threads"
        );
    }
}

#[test]
fn levels() {
    assert_eq!(Level::new(0), None);
    assert_eq!(Level::new(10), None);
    assert_eq!(Level::new(1), Some(Level::FASTEST));
    assert_eq!(Level::new(9), Some(Level::BEST));
    assert_eq!(Level::default(), Level::BEST);
    assert_eq!(Level::new(5).map(Level::get), Some(5));
}
