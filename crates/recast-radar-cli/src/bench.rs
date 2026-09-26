//! `bench`: decode timing.
//!
//! Each file is read into memory once, decoded `--warmup` times untimed,
//! then `--iterations` times timed. The decode is the same one `info` runs
//! (the format router, or the Level III decoder), from memory, so disk
//! speed does not enter the numbers. `--threads 1` runs the decoders in a
//! worker pool of one thread for single-core timing (a pool of this run's
//! own, so [`crate::run`] can be called again in the same process); pinning
//! the process to one core is left to the operating system (`taskset`,
//! `start /affinity`).

use std::fs;
use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::open::decode_for_bench;
use crate::output::human_bytes;
use crate::{BenchArgs, CliError};

/// Decodes in a pool of the requested size, or in rayon's global pool.
struct Decoder {
    pool: Option<rayon::ThreadPool>,
    metadata: bool,
}

impl Decoder {
    fn threads(&self) -> usize {
        self.pool
            .as_ref()
            .map_or_else(rayon::current_num_threads, |pool| {
                pool.current_num_threads()
            })
    }

    /// Decode `bytes` once; the time covers the decode only.
    fn decode(&self, bytes: &[u8]) -> (Duration, Result<usize, String>) {
        let timed = || {
            let start = Instant::now();
            let decoded = decode_for_bench(bytes, self.metadata);
            (start.elapsed(), decoded)
        };
        match &self.pool {
            Some(pool) => pool.install(timed),
            None => timed(),
        }
    }
}

pub(crate) fn run(args: &BenchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let pool = match args.threads {
        Some(threads) => Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads as usize)
                .build()
                .map_err(|err| CliError::Failed(format!("cannot start the thread pool: {err}")))?,
        ),
        None => None,
    };
    let decoder = Decoder {
        pool,
        metadata: args.metadata,
    };
    let threads = decoder.threads();
    let mut results = Vec::new();
    if !args.json {
        writeln!(
            out,
            "{:<40} {:>10} {:>9} {:>9} {:>9} {:>9}  rays",
            "file", "size", "min ms", "median", "mean", "MB/s"
        )?;
    }
    for path in &args.files {
        let bytes = fs::read(path).map_err(|err| CliError::io(path, err))?;
        let mut rays = 0;
        for _ in 0..args.warmup {
            rays = decoder
                .decode(&bytes)
                .1
                .map_err(|message| CliError::Decode {
                    path: path.clone(),
                    message,
                })?;
        }
        let mut times = Vec::with_capacity(args.iterations as usize);
        for _ in 0..args.iterations {
            let (time, decoded) = decoder.decode(&bytes);
            times.push(time);
            rays = decoded.map_err(|message| CliError::Decode {
                path: path.clone(),
                message,
            })?;
        }
        let timing = Timing::new(&times);
        let mb_per_s = bytes.len() as f64 / 1e6 / timing.median.as_secs_f64().max(1e-9);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if args.json {
            results.push(json!({
                "path": path.display().to_string(),
                "size_bytes": bytes.len(),
                "iterations": args.iterations,
                "min_ms": ms(timing.min),
                "median_ms": ms(timing.median),
                "mean_ms": ms(timing.mean),
                "max_ms": ms(timing.max),
                "mb_per_s": mb_per_s,
                "rays": rays,
            }));
        } else {
            writeln!(
                out,
                "{:<40} {:>10} {:>9.2} {:>9.2} {:>9.2} {:>9.1}  {rays}",
                name,
                human_bytes(bytes.len() as u64),
                ms(timing.min),
                ms(timing.median),
                ms(timing.mean),
                mb_per_s
            )?;
        }
    }
    if args.json {
        let document = json!({
            "threads": threads,
            "metadata": args.metadata,
            "results": Value::Array(results),
        });
        serde_json::to_writer_pretty(&mut *out, &document).map_err(std::io::Error::from)?;
        writeln!(out)?;
    } else {
        writeln!(
            out,
            "{} iteration(s) per file, {threads} decoder thread(s); MB/s is input bytes over the median",
            args.iterations
        )?;
    }
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

struct Timing {
    min: Duration,
    median: Duration,
    mean: Duration,
    max: Duration,
}

impl Timing {
    fn new(times: &[Duration]) -> Self {
        let mut sorted = times.to_vec();
        sorted.sort();
        let count = sorted.len().max(1) as u32;
        let total: Duration = sorted.iter().sum();
        let median = match sorted.len() {
            0 => Duration::ZERO,
            n if n % 2 == 1 => sorted[n / 2],
            n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2,
        };
        Self {
            min: sorted.first().copied().unwrap_or_default(),
            median,
            mean: total / count,
            max: sorted.last().copied().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thread_count_can_change_between_runs_in_one_process() {
        use clap::Parser;
        let file = recast_radar_testdata::path("l2-ktlx-20240315-000217-trim")
            .unwrap_or_else(|err| panic!("{err}"));
        for threads in [1u64, 2, 1] {
            let count = threads.to_string();
            let args = [
                std::ffi::OsStr::new("recast-radar"),
                "bench".as_ref(),
                "--json".as_ref(),
                "-n".as_ref(),
                "1".as_ref(),
                "--warmup".as_ref(),
                "0".as_ref(),
                "--threads".as_ref(),
                count.as_ref(),
                file.as_os_str(),
            ];
            let cli = crate::Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{err}"));
            let mut out = Vec::new();
            crate::run(cli, &crate::backend::Backends::builtin(), &mut out)
                .unwrap_or_else(|err| panic!("{err}"));
            let report: Value = serde_json::from_slice(&out).unwrap_or_default();
            assert_eq!(report["threads"], threads);
            assert_eq!(report["results"][0]["rays"], 960);
        }
    }

    #[test]
    fn median_of_an_even_count_is_the_middle_pair_mean() {
        let times = [3, 1, 4, 2].map(Duration::from_millis);
        let timing = Timing::new(&times);
        assert_eq!(timing.min, Duration::from_millis(1));
        assert_eq!(timing.max, Duration::from_millis(4));
        assert_eq!(timing.median, Duration::from_micros(2500));
        assert_eq!(timing.mean, Duration::from_micros(2500));
    }
}
