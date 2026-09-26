//! Open an HDF5 file and read every dataset, timing both steps.
//!
//! ```text
//! cargo run --release -p recast-radar-hdf5 --example h5_read_all -- <file.h5> [repeats]
//! ```

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: h5_read_all <file.h5> [repeats]");
        std::process::exit(2);
    };
    let repeats: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{path}: {err}");
            std::process::exit(1);
        }
    };
    let mut best_open = f64::MAX;
    let mut best_read = f64::MAX;
    let mut summary = String::new();
    for _ in 0..repeats {
        let start = Instant::now();
        let file = match recast_radar_hdf5::H5File::open(&bytes) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("{path}: {err}");
                std::process::exit(1);
            }
        };
        let opened = start.elapsed().as_secs_f64();
        let (mut datasets, mut elements, mut failed) = (0usize, 0usize, 0usize);
        for (path, object) in file.objects() {
            if object.kind() == recast_radar_hdf5::ObjectKind::Dataset {
                match file.dataset(path) {
                    Ok(data) => {
                        datasets += 1;
                        elements += data.values.len();
                    }
                    Err(err) => {
                        failed += 1;
                        eprintln!("{path}: {err}");
                    }
                }
            }
        }
        let read = start.elapsed().as_secs_f64() - opened;
        best_open = best_open.min(opened);
        best_read = best_read.min(read);
        summary = format!(
            "{} objects, {datasets} datasets ({failed} failed), {elements} elements",
            file.objects().count()
        );
    }
    println!(
        "{path}: {summary}; open {:.3} ms, read all {:.3} ms (best of {repeats})",
        best_open * 1e3,
        best_read * 1e3
    );
}
