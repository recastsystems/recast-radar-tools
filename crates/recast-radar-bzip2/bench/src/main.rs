//! Encoder benchmark harness. Inputs are the decompressed payloads of the
//! LDM records of NEXRAD Level II archive files (one bzip2 stream each),
//! decoded before any timing, or whole files given as `raw:<path>`.
//!
//! ```text
//! bzip2-enc-bench time <ours|ref> <iters> <file>...   warm-up + timed passes
//! bzip2-enc-bench once <ours|ref> <file>...           one pass (callgrind)
//! bzip2-enc-bench prep <file>...                      input decoding only
//! bzip2-enc-bench patterns <iters> <file>              degenerate inputs
//! bzip2-enc-bench check <file>...                     ours == ref, both decode
//! bzip2-enc-bench par <threads> <iters> <file>...     encode_many, EncoderPool
//! bzip2-enc-bench small <iters> <record> <file>       per-call cost of prefixes
//! ```
//!
//! `ref` is libbzip2 1.0.8: C (feature `c`) or libbz2-rs-sys (default).

use std::hint::black_box;
use std::time::Instant;

use recast_radar_bzip2::{Decoder, Encoder, Level};

const REFERENCE: &str = if cfg!(feature = "c") {
    "C libbzip2 1.0.8 (bzip2-sys)"
} else {
    "libbz2-rs-sys"
};

/// The bzip2 streams of the LDM records of a Level II archive file.
fn ldm_records(bytes: &[u8]) -> Vec<&[u8]> {
    let mut cursor = if bytes.starts_with(b"AR2V") || bytes.starts_with(b"ARCH") {
        24
    } else {
        0
    };
    let mut out = Vec::new();
    while let Some(w) = bytes.get(cursor..cursor + 4) {
        let len = i32::from_be_bytes([w[0], w[1], w[2], w[3]]);
        let size = len.unsigned_abs() as usize;
        cursor += 4;
        if size == 0 {
            break;
        }
        let Some(r) = bytes.get(cursor..cursor + size) else {
            break;
        };
        if !r.starts_with(b"BZh") {
            break;
        }
        out.push(r);
        cursor += size;
        if len < 0 {
            break;
        }
    }
    out
}

/// (level, payload) of every record of every file. A file named
/// `raw:<path>` is one input as it is, and `cat:<path>` the decoded records
/// of a volume joined into one input, both at level 9.
fn payloads(files: &[String]) -> Vec<(u32, Vec<u8>)> {
    let mut dec = Decoder::new();
    let mut all = Vec::new();
    for f in files {
        if let Some(path) = f.strip_prefix("cat:") {
            // The decoded records of a volume as one input, at level 9.
            let joined: Vec<u8> = payloads(&[path.to_owned()])
                .into_iter()
                .flat_map(|(_, d)| d)
                .collect();
            all.push((9, joined));
            continue;
        }
        if let Some(path) = f.strip_prefix("raw:") {
            let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            all.push((9, bytes));
            continue;
        }
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let recs = ldm_records(&bytes);
        assert!(!recs.is_empty(), "{f}: no LDM bzip2 records");
        for r in recs {
            let mut out = Vec::new();
            dec.decode_stream_into(r, &mut out)
                .unwrap_or_else(|e| panic!("{f}: {e}"));
            all.push((u32::from(r[3] - b'0'), out));
        }
    }
    all
}

fn reference_encode(level: u32, data: &[u8], out: &mut Vec<u8>) {
    let mut c = bzip2::Compress::new(bzip2::Compression::new(level), 30);
    out.reserve(data.len() + data.len() / 50 + 1024);
    match c.compress_vec(data, out, bzip2::Action::Finish) {
        Ok(bzip2::Status::StreamEnd) => {}
        other => panic!("reference encode: {other:?}"),
    }
}

fn reference_decode(data: &[u8], size_hint: usize) -> Vec<u8> {
    let mut d = bzip2::Decompress::new(false);
    let mut out = Vec::with_capacity(size_hint + 1);
    match d.decompress_vec(data, &mut out) {
        Ok(bzip2::Status::StreamEnd) => out,
        other => panic!("reference decode: {other:?}"),
    }
}

/// Time this thread has spent on a CPU, in nanoseconds (Linux
/// `/proc/thread-self/schedstat`): unlike wall time, it excludes the time
/// the thread waits while the machine is busy with other work.
fn cpu_ns() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

fn median_min(v: &[f64]) -> (f64, f64) {
    let mut sorted = v.to_vec();
    sorted.sort_by(f64::total_cmp);
    (sorted[sorted.len() / 2], sorted[0])
}

#[inline(never)]
fn encode_all(
    ours: bool,
    inputs: &[(u32, Vec<u8>)],
    encoders: &mut [Encoder],
    out: &mut Vec<u8>,
) -> usize {
    let mut total = 0;
    for (level, data) in inputs {
        out.clear();
        if ours {
            encoders[*level as usize - 1].encode_into(data, out);
        } else {
            reference_encode(*level, data, out);
        }
        total += out.len();
        black_box(&out);
    }
    total
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let mut encoders: Vec<Encoder> = (1..=9)
        .map(|l| Encoder::new(Level::new(l).unwrap_or_default()))
        .collect();
    let mut out = Vec::with_capacity(4 << 20);
    match mode {
        "time" => {
            let ours = args[2] == "ours";
            let iters: usize = args[3].parse().expect("iters");
            let inputs = payloads(&args[4..]);
            let bytes: usize = inputs.iter().map(|(_, d)| d.len()).sum();
            let name = if ours {
                "recast-radar-bzip2"
            } else {
                REFERENCE
            };
            encode_all(ours, &inputs, &mut encoders, &mut out);
            let mut wall = Vec::new();
            let mut cpu = Vec::new();
            let mut z = 0;
            for _ in 0..iters {
                let c = cpu_ns();
                let t = Instant::now();
                z = encode_all(ours, &inputs, &mut encoders, &mut out);
                wall.push(t.elapsed().as_secs_f64() * 1e3);
                if let (Some(c0), Some(c1)) = (c, cpu_ns()) {
                    cpu.push((c1 - c0) as f64 / 1e6);
                }
            }
            let (wmed, wmin) = median_min(&wall);
            let runs: Vec<f64> = wall.iter().map(|t| (t * 10.0).round() / 10.0).collect();
            let cpu_note = if cpu.is_empty() {
                String::new()
            } else {
                let (cmed, cmin) = median_min(&cpu);
                format!(" on-CPU median {cmed:.1} ms, min {cmin:.1} ms;")
            };
            println!(
                "{name}: {} records, {bytes} -> {z} bytes, wall median {wmed:.1} ms, min {wmin:.1} ms, \
                 {:.1} MB/s;{cpu_note} runs {runs:?}",
                inputs.len(),
                bytes as f64 / wmed / 1e3,
            );
        }
        "patterns" => {
            // Degenerate inputs built from a real volume: worst cases for
            // block sorting, timed for ours and the reference at level 9.
            let iters: usize = args[2].parse().expect("iters");
            let file = &args[3];
            let raw = std::fs::read(file).unwrap_or_else(|e| panic!("{file}: {e}"));
            let inputs = payloads(&args[3..4]);
            let payload: Vec<u8> = inputs.iter().flat_map(|(_, d)| d.iter().copied()).collect();
            let size = 8 << 20;
            let mut hist = [0usize; 256];
            for &b in &payload {
                hist[b as usize] += 1;
            }
            let common = (0..256).max_by_key(|&b| hist[b]).unwrap_or(0) as u8;
            let cases: Vec<(&str, Vec<u8>)> = vec![
                (
                    "real records (payload)",
                    payload[..size.min(payload.len())].to_vec(),
                ),
                ("one byte value", vec![common; size]),
                (
                    "long runs (each byte x64)",
                    payload
                        .iter()
                        .flat_map(|&b| std::iter::repeat_n(b, 64))
                        .take(size)
                        .collect(),
                ),
                (
                    "period 7",
                    payload[4096..4103]
                        .iter()
                        .copied()
                        .cycle()
                        .take(size)
                        .collect(),
                ),
                (
                    "period 1000",
                    payload[4096..5096]
                        .iter()
                        .copied()
                        .cycle()
                        .take(size)
                        .collect(),
                ),
                (
                    "period 100000",
                    payload[4096..104_096]
                        .iter()
                        .copied()
                        .cycle()
                        .take(size)
                        .collect(),
                ),
                ("compressed bytes", raw[..size.min(raw.len())].to_vec()),
            ];
            for (name, data) in &cases {
                for ours in [true, false] {
                    let mut best = f64::MAX;
                    for _ in 0..=iters {
                        out.clear();
                        let t = Instant::now();
                        if ours {
                            encoders[8].encode_into(data, &mut out);
                        } else {
                            reference_encode(9, data, &mut out);
                        }
                        best = best.min(t.elapsed().as_secs_f64());
                    }
                    let who = if ours {
                        "recast-radar-bzip2"
                    } else {
                        REFERENCE
                    };
                    println!(
                        "{name:<28} {who:<30} {:>9} B -> {:>9} B  {:>8.2} ns/B",
                        data.len(),
                        out.len(),
                        best * 1e9 / data.len() as f64
                    );
                }
            }
        }
        "prep" => {
            // Only the decoding of the inputs: subtract from `once` counts.
            let inputs = payloads(&args[2..]);
            println!("{} inputs", inputs.len());
        }
        "once" => {
            let ours = args[2] == "ours";
            let inputs = payloads(&args[3..]);
            let z = encode_all(ours, &inputs, &mut encoders, &mut out);
            println!("{}: {} records -> {z} bytes", args[2], inputs.len());
        }
        "check" => {
            let inputs = payloads(&args[2..]);
            let mut dec = Decoder::new();
            let (mut same, mut ours_total, mut ref_total) = (0, 0, 0);
            for (i, (level, data)) in inputs.iter().enumerate() {
                let mut ours = Vec::new();
                encoders[*level as usize - 1].encode_into(data, &mut ours);
                let mut reference = Vec::new();
                reference_encode(*level, data, &mut reference);
                let mut back = Vec::new();
                dec.decode_stream_into(&ours, &mut back)
                    .expect("our decoder");
                assert!(back == *data, "record {i}: our decoder output differs");
                assert!(
                    reference_decode(&ours, data.len()) == *data,
                    "record {i}: reference decoder output differs"
                );
                same += usize::from(ours == reference);
                ours_total += ours.len();
                ref_total += reference.len();
            }
            println!(
                "{} records decode with both decoders; {same} byte-identical to {REFERENCE}; \
                 {ours_total} vs {ref_total} bytes",
                inputs.len()
            );
        }
        "par" => {
            let threads: usize = args[2].parse().expect("threads");
            let iters: usize = args[3].parse().expect("iters");
            let inputs = payloads(&args[4..]);
            let bytes: usize = inputs.iter().map(|(_, d)| d.len()).sum();
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("pool");
            let level = Level::new(inputs[0].0).unwrap_or_default();
            let refs: Vec<&[u8]> = inputs.iter().map(|(_, d)| d.as_slice()).collect();
            // `encode_many` (a fresh pool per call), one EncoderPool kept
            // across calls, and for comparison a new encoder per rayon job
            // (`map_init`, which calls its init once per job, not once per
            // thread). Interleaved pass by pass; the first pass is a warm-up.
            let kept = recast_radar_bzip2::EncoderPool::new(level);
            let names = ["encode_many", "EncoderPool (kept)", "new encoder per job"];
            let mut times = vec![Vec::new(); names.len()];
            for pass in 0..=iters {
                for (mode, mode_times) in times.iter_mut().enumerate() {
                    let t = Instant::now();
                    let outs = pool.install(|| match mode {
                        0 => recast_radar_bzip2::encode_many(level, &refs),
                        1 => kept.encode_many(&refs),
                        _ => {
                            use rayon::prelude::*;
                            refs.par_iter()
                                .map_init(
                                    || Encoder::new(level),
                                    |e, r| {
                                        let mut o = Vec::new();
                                        e.encode_into(r, &mut o);
                                        o
                                    },
                                )
                                .collect()
                        }
                    });
                    if pass > 0 {
                        mode_times.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    black_box(outs);
                }
            }
            for (name, mode_times) in names.iter().zip(&times) {
                let (median, min) = median_min(mode_times);
                println!(
                    "{name} on {threads} threads: {} records, median {median:.1} ms ({:.1} MB/s), min {min:.1} ms",
                    inputs.len(),
                    bytes as f64 / median / 1e3
                );
            }
            println!("the kept pool holds {} encoders", kept.idle_encoders());
        }
        "small" => {
            // The fixed cost of a call: prefixes of the contents of one
            // record, ours with a reused encoder against the reference's
            // one-shot compression, level 9, interleaved call by call.
            let iters: usize = args[2].parse().expect("iters");
            let record: usize = args[3].parse().expect("record");
            let inputs = payloads(&args[4..5]);
            let data = &inputs[record].1;
            for size in [16usize, 100, 1000, 2432, 10_000, 30_000, 100_000, 300_000] {
                let input = &data[..size.min(data.len())];
                let (mut ours, mut reference) = (Vec::new(), Vec::new());
                for _ in 0..=iters {
                    for is_ours in [true, false] {
                        out.clear();
                        let t = Instant::now();
                        if is_ours {
                            encoders[8].encode_into(input, &mut out);
                        } else {
                            reference_encode(9, input, &mut out);
                        }
                        let us = t.elapsed().as_secs_f64() * 1e6;
                        black_box(&out);
                        if is_ours { &mut ours } else { &mut reference }.push(us);
                    }
                }
                let (om, omin) = median_min(&ours[1..]);
                let (rm, rmin) = median_min(&reference[1..]);
                println!(
                    "{:>7} B: recast-radar-bzip2 median {om:.1} us, min {omin:.1} us; {REFERENCE} median {rm:.1} us, min {rmin:.1} us",
                    input.len()
                );
            }
        }
        _ => {
            eprintln!(
                "usage: time <ours|ref> <iters> <file>... | once <ours|ref> <file>... \
                 | check <file>... | par <threads> <iters> <file>...                  | small <iters> <record> <file> | patterns <iters> <file>"
            );
            std::process::exit(2);
        }
    }
}
