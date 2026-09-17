//! Build a max-DBZH swath from a real loop of Level II volumes and render it
//! (plus the newest single frame, for comparison) to PNG.
//!
//! Usage:
//!   cargo run -p recast-radar-render --example max_swath_png -- <out_dir> <file1> <file2> ...
//!
//! The swath PNG should show a BROADER reflectivity footprint than the single
//! newest frame — the union of where the storm has been across the loop.

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use std::path::PathBuf;

use recast_radar_core::{Quantity, Volume};
use recast_radar_render::{RasterOptions, render_field_png};

#[path = "legacy_bridge/mod.rs"]
mod legacy_bridge;
use legacy_bridge::swath::{SwathAggregation, base_tilt_sweep, max_value_swath};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(out_dir) = args.next().map(PathBuf::from) else {
        eprintln!("usage: max_swath_png <out_dir> <vol1> <vol2> ...");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let paths: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("no input volumes");
        std::process::exit(2);
    }

    let mut volumes = Vec::new();
    for path in &paths {
        match legacy_bridge::Decoded::from_path(path) {
            Ok(decoded) => {
                println!(
                    "decoded {} -> {} sweeps, {} @ {}",
                    path.display(),
                    decoded.volume.sweeps.len(),
                    decoded.volume.attrs.instrument_name,
                    decoded.volume.time_reference
                );
                volumes.push(decoded);
            }
            Err(err) => eprintln!("decode {} failed: {err}", path.display()),
        }
    }
    if volumes.is_empty() {
        eprintln!("nothing decoded");
        std::process::exit(1);
    }

    let refs: Vec<&legacy_bridge::Decoded> = volumes.iter().collect();
    let options = RasterOptions {
        width: 1200,
        height: 1200,
        range_fraction: 96,
    };

    // Newest single frame, base reflectivity — the "current scan" reference.
    let newest = &refs
        .iter()
        .max_by_key(|v| v.volume.time_reference)
        .copied()
        .unwrap()
        .volume;
    if let Some(sweep) = base_tilt_sweep(newest, Quantity::Reflectivity) {
        let name = &newest.sweeps[sweep]
            .find(Quantity::Reflectivity)
            .unwrap()
            .name;
        let out = out_dir.join("single_frame_ref.png");
        render_field_png(newest, sweep, name, &out, options).unwrap();
        println!("wrote {}", out.display());
    }

    // Max-DBZH swath over the whole loop.
    let swath =
        max_value_swath(&refs, Quantity::Reflectivity, SwathAggregation::Max).expect("swath");
    report_coverage("DBZH swath", &swath, Quantity::Reflectivity);
    let out = out_dir.join("max_ref_swath.png");
    let name = &swath.sweeps[0].find(Quantity::Reflectivity).unwrap().name;
    render_field_png(&swath, 0, name, &out, options).unwrap();
    println!("wrote {}", out.display());

    // Max-|V| swath (second toggle).
    if let Some(swath) = max_value_swath(
        &refs,
        Quantity::RadialVelocity,
        SwathAggregation::MaxMagnitude,
    ) {
        report_coverage("|V| swath", &swath, Quantity::RadialVelocity);
        let out = out_dir.join("max_vel_swath.png");
        let name = &swath.sweeps[0].find(Quantity::RadialVelocity).unwrap().name;
        render_field_png(&swath, 0, name, &out, options).unwrap();
        println!("wrote {}", out.display());
    }
}

/// Print how many gates carry a finite value — a swath should cover far more
/// than any single frame.
fn report_coverage(label: &str, volume: &Volume, quantity: Quantity) {
    let field = volume.sweeps[0].find(quantity).expect("swath field");
    let (rows, gates) = field.shape();
    let (mut finite, mut total) = (0usize, 0usize);
    for row in 0..rows {
        for gate in 0..gates {
            total += 1;
            if field.value(row, gate).is_some() {
                finite += 1;
            }
        }
    }
    println!(
        "{label}: {finite}/{total} gates finite ({:.1}%), {rows} rows x {gates} gates",
        100.0 * finite as f64 / total.max(1) as f64
    );
}
