//! danielway/nexrad harness for tools/xlib-bench: each sample is
//! `std::fs::read` + `nexrad_data::volume::File::new` + `decompress()` (gzip
//! wrapper, a no-op otherwise) + `scan()`, which decompresses every LDM record
//! and decodes every radial's moments into `nexrad_model` sweeps.
//!
//! Protocol: `nexrad-crate-bench FORMAT FILE ITERS WARMUP [WAIT_STDIN]`, with
//! `RAYON_NUM_THREADS=1` building an inline one-thread global pool (the
//! calling thread, as `decode_bench --threads 1` does) and any other value or
//! none leaving rayon's default pool.

use std::hint::black_box;
use std::io::BufRead;
use std::time::Instant;

use nexrad_data::volume::File;

/// A `/proc/self/status` value in KiB (`VmRSS:`, `VmHWM:`), 0 if unknown.
fn status_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn rss_kb() -> u64 {
    status_kb("VmRSS:")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: nexrad-crate-bench FORMAT FILE ITERS WARMUP [1]");
        std::process::exit(2);
    }
    let path = &args[2];
    let iters: usize = args[3].parse().expect("ITERS");
    let warmup: usize = args[4].parse().expect("WARMUP");
    if args.get(5).is_some_and(|flag| flag == "1") {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).expect("stdin");
    }
    let threads = std::env::var("RAYON_NUM_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    if threads == Some(1) {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .use_current_thread()
            .build_global()
            .expect("rayon pool");
    }
    let rss_before = rss_kb();
    let mut samples = Vec::new();
    let (mut sweeps, mut radials) = (0, 0);
    let mut last = None;
    for iteration in 0..warmup + iters {
        let started = Instant::now();
        let raw = std::fs::read(path).expect("read");
        let scan = File::new(raw)
            .decompress()
            .and_then(|file| file.scan())
            .expect("decode");
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if iteration >= warmup {
            samples.push(elapsed);
        }
        sweeps = scan.sweeps().len();
        radials = scan
            .sweeps()
            .iter()
            .map(|sweep| sweep.radials().len())
            .sum();
        drop(last.replace(black_box(scan)));
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let text: Vec<String> = samples.iter().map(|v| format!("{v:.3}")).collect();
    println!(
        "{{\"lib\":\"nexrad-crate\",\"format\":\"{}\",\"iters\":{iters},\"median_ms\":{:.3},\
\"min_ms\":{:.3},\"samples_ms\":[{}],\"sweeps\":{sweeps},\"rays\":{radials},\"fields\":0,\
\"gates\":0,\"rss_before_kb\":{rss_before},\"self_hwm_kb\":{},\"threads\":\"{}\"}}",
        args[1],
        sorted[sorted.len() / 2],
        sorted[0],
        text.join(","),
        status_kb("VmHWM:"),
        threads.map_or("default".to_owned(), |t| t.to_string()),
    );
}
