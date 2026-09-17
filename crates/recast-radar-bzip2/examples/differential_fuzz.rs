//! Seeded differential fuzzing of the decoder against the reference on real
//! LDM records, for a time budget.
//!
//! ```text
//! cargo run --release -p recast-radar-bzip2 --example differential_fuzz -- [SECONDS] [SEED] [ID...]
//! ```
//!
//! Every case is a mutation of a real record of the given testdata volumes
//! (default: the four volumes the test suite uses): truncation, one to three
//! bit flips (half of them biased to the header and table region), a byte
//! burst, or a byte inserted or removed. Each case goes through the same
//! differential check as `tests/corruption.rs` (`common::check_case`), which
//! panics on any divergence: a panic in the decoder, accepting what the
//! reference rejects, different output, or a different structural stopping
//! point with CRC checks off. The exit status is non-zero when we rejected
//! an input the reference accepted.
//!
//! This is the stable-toolchain, seeded complement to the `bzip2` cargo-fuzz
//! target in `fuzz/`, which needs nightly and libFuzzer.

#[path = "../tests/common/mod.rs"]
mod common;

use std::process::ExitCode;
use std::time::Instant;

use common::{Rng, Tally, VOLUMES, check_case, ldm_records, testdata};
use recast_radar_bzip2::Decoder;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(60);
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let ids: Vec<&str> = if args.len() > 2 {
        args[2..].iter().map(String::as_str).collect()
    } else {
        VOLUMES.iter().map(|(id, _)| *id).collect()
    };

    let mut records: Vec<(String, usize, Vec<u8>)> = Vec::new();
    for id in ids {
        let Some(bytes) = testdata(id) else {
            continue;
        };
        for (i, r) in ldm_records(&bytes).into_iter().enumerate() {
            records.push((id.to_owned(), i, r));
        }
    }
    if records.is_empty() {
        eprintln!("no records available (testdata offline?)");
        return ExitCode::FAILURE;
    }
    eprintln!("{} real records, seed {seed}, {secs} s", records.len());

    let mut rng = Rng(seed);
    let mut dec = Decoder::new();
    let mut t = Tally::default();
    let start = Instant::now();
    while start.elapsed().as_secs() < secs {
        let (id, index, r) = &records[rng.below(records.len())];
        let len = r.len();
        let mut m = r.clone();
        let what = match rng.below(10) {
            0 => {
                let c = rng.below(len);
                m.truncate(c);
                format!("truncated to {c}")
            }
            1..=5 => {
                let flips = 1 + rng.below(3);
                let span = if rng.below(2) == 0 { len.min(900) } else { len };
                let mut bits = Vec::new();
                for _ in 0..flips {
                    let bit = rng.below(span * 8);
                    m[bit / 8] ^= 0x80 >> (bit % 8);
                    bits.push(bit);
                }
                format!("bits {bits:?} flipped")
            }
            6 | 7 => {
                let at = rng.below(len);
                let n = 1 + rng.below(16);
                for b in m[at..(at + n).min(len)].iter_mut() {
                    *b = rng.next() as u8;
                }
                format!("burst of {n} at {at}")
            }
            8 => {
                let at = rng.below(len);
                m.insert(at, rng.next() as u8);
                format!("byte inserted at {at}")
            }
            _ => {
                let at = rng.below(len);
                m.remove(at);
                format!("byte removed at {at}")
            }
        };
        check_case(
            &mut dec,
            &m,
            &mut t,
            &format!("seed {seed}: {id} record {index} {what}"),
        );
        if t.cases % 500 == 0 {
            eprintln!("{:>6}s {t:?}", start.elapsed().as_secs());
        }
    }
    println!("DONE seed={seed} secs={secs} {t:?}");
    if t.ours_err_ref_ok == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
