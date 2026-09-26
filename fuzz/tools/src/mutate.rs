//! `fuzz-tools mutate`: a deterministic mutation fuzzer on the stable
//! toolchain, for machines without libFuzzer (Windows, or a quick check
//! before a campaign in `nexbench`).
//!
//! Each run takes one input file, applies one to four in-place mutations
//! (bit flips, random bytes, 2- and 4-byte boundary integers, IEEE special
//! and out-of-range floats in both byte orders, a copy of another stretch
//! of the same file, rarely a truncation) and calls the harness under
//! `catch_unwind`. The length is kept except for truncations, so the file
//! offsets of HDF5, netCDF and DORADE containers stay valid and mutations
//! reach the values behind them. Runs are reproducible from the PRNG seed.
//! A panicking input is written to `OUT_DIR/<target>-<rng seed>-<run>` with
//! its panic message printed; the summary lists each distinct message once
//! (digits masked).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use recast_radar_fuzz::Harness;

/// The message of the last panic, taken by the hook instead of printing it.
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// xorshift64*: small, fast, and the same stream on every platform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift.
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Doubles that decoders mishandle: specials, subnormals, values beyond the
/// f32 range and beyond any physical quantity.
const F64S: &[f64] = &[
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::MAX,
    -f64::MAX,
    1.556e185,
    -3.0e300,
    1.0e39,
    -1.0e39,
    4.0e-320,
    -0.0,
    0.0,
    360.0,
    -360.0,
    1.0e12,
];

/// Floats likewise.
const F32S: &[f32] = &[
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MAX,
    -f32::MAX,
    1.0e-40,
    -1.0e-40,
    -0.0,
    0.0,
    360.0,
    -360.0,
    1.0e9,
];

/// Integer boundaries.
const U32S: &[u32] = &[
    0,
    1,
    0x7F,
    0x80,
    0xFF,
    0x100,
    0x7FFF,
    0x8000,
    0xFFFF,
    0x1_0000,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

fn put(data: &mut [u8], offset: usize, bytes: &[u8]) {
    if let Some(slot) = data.get_mut(offset..offset + bytes.len()) {
        slot.copy_from_slice(bytes);
    }
}

fn mutate_once(data: &mut Vec<u8>, rng: &mut Rng) {
    if data.is_empty() {
        return;
    }
    let len = data.len();
    let at = rng.below(len);
    let big_endian = rng.next() & 1 == 1;
    match rng.below(100) {
        0..=19 => data[at] ^= 1 << rng.below(8),
        20..=34 => data[at] = rng.next() as u8,
        35..=54 => {
            let value = F64S[rng.below(F64S.len())];
            let bytes = if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            };
            put(data, at, &bytes);
        }
        55..=69 => {
            let value = F32S[rng.below(F32S.len())];
            let bytes = if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            };
            put(data, at, &bytes);
        }
        70..=79 => {
            let value = U32S[rng.below(U32S.len())];
            let bytes = if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            };
            put(data, at, &bytes);
        }
        80..=86 => {
            let value = U32S[rng.below(U32S.len())] as u16;
            let bytes = if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            };
            put(data, at, &bytes);
        }
        87..=97 => {
            // Another stretch of the same file over this one.
            let span = 1 + rng.below(64.min(len));
            let from = rng.below(len - span + 1);
            let to = rng.below(len - span + 1);
            data.copy_within(from..from + span, to);
        }
        _ => data.truncate(at.max(1)),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-text panic payload".to_owned()
    }
}

/// The panic message with digits masked, for grouping.
fn kind(message: &str) -> String {
    message
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// Run `runs` mutated inputs drawn from `inputs` through `harness`.
pub fn mutate(
    target: &str,
    harness: Harness,
    inputs: &[PathBuf],
    runs: u64,
    seed: u64,
    out_dir: &Path,
) -> io::Result<bool> {
    let mut seeds = Vec::with_capacity(inputs.len());
    for path in inputs {
        let bytes = fs::read(path)?;
        if !bytes.is_empty() {
            seeds.push((path.clone(), bytes));
        }
    }
    if seeds.is_empty() {
        return Err(io::Error::other("no non-empty input files"));
    }
    fs::create_dir_all(out_dir)?;
    panic::set_hook(Box::new(|info| {
        let message = format!("{} at {}", panic_message(info.payload()), {
            info.location()
                .map_or_else(String::new, |location| location.to_string())
        });
        if let Ok(mut slot) = LAST_PANIC.lock() {
            *slot = Some(message);
        }
    }));
    let started = Instant::now();
    let mut rng = Rng::new(seed);
    let mut decoded = 0u64;
    let mut kinds: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for run in 0..runs {
        let (path, source) = &seeds[rng.below(seeds.len())];
        let mut data = source.clone();
        for _ in 0..1 + rng.below(4) {
            mutate_once(&mut data, &mut rng);
        }
        match panic::catch_unwind(AssertUnwindSafe(|| harness(&data))) {
            Ok(true) => decoded += 1,
            Ok(false) => {}
            Err(_) => {
                let message = LAST_PANIC
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take())
                    .unwrap_or_default();
                let file = out_dir.join(format!("{target}-{seed}-{run}"));
                fs::write(&file, &data)?;
                println!(
                    "PANIC run {run} (from {}): {message}\n      input: {}",
                    path.display(),
                    file.display()
                );
                let entry = kinds.entry(kind(&message)).or_insert((0, message));
                entry.0 += 1;
            }
        }
        if (run + 1) % 500 == 0 {
            println!(
                "{target}: {} runs, {decoded} decoded, {} panics, {:.0} s",
                run + 1,
                kinds.values().map(|(count, _)| count).sum::<u64>(),
                started.elapsed().as_secs_f64()
            );
        }
    }
    let _ = panic::take_hook();
    let panics: u64 = kinds.values().map(|(count, _)| count).sum();
    println!(
        "{target}: {runs} runs from {} inputs (rng seed {seed}), {decoded} decoded, {panics} \
         panics of {} kinds, {:.0} s",
        seeds.len(),
        kinds.len(),
        started.elapsed().as_secs_f64()
    );
    for (count, message) in kinds.values() {
        println!("  {count:>5} x {message}");
    }
    Ok(panics == 0)
}
