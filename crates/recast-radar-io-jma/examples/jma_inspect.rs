//! Print a decoded JMA radar GRIB2 tar in the exact text format of the
//! `jma-radar-bridge` crate's `inspect` subcommand, so the two decoders can
//! be cross-validated with a plain `diff` (station ids, sweep counts, gate
//! and radial counts, ranges, non-missing counts).
//!
//! With `--samples`, prints fixed-position sampled gate values instead
//! (the same positions a sibling harness prints from the bridge's own
//! `Sweep::value`), for gate-for-gate value comparison.
//!
//! Usage: cargo run -p recast-radar-io-jma --example jma_inspect -- <tar> [--samples]

use recast_radar_core::model::{FieldData, FieldName, Sweep};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: cargo run -p recast-radar-io-jma --example jma_inspect -- <jma-tar> [--samples]"
        );
        std::process::exit(2);
    };
    let samples = matches!(args.next().as_deref(), Some("--samples"));

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("read {path}: {err}");
            std::process::exit(1);
        }
    };
    let volumes = match recast_radar_io_jma::read_jma_tar_volumes(&bytes, None) {
        Ok(volumes) => volumes,
        Err(err) => {
            eprintln!("decode failed: {err}");
            std::process::exit(1);
        }
    };

    for volume in &volumes {
        if samples {
            for sweep in &volume.sweeps {
                print_sweep_samples(&volume.attrs.instrument_name, sweep);
            }
        } else {
            println!(
                "{}  station={}  sweeps={}",
                volume.time_reference.format("%Y-%m-%dT%H:%M:%SZ"),
                volume.attrs.instrument_name,
                volume.sweeps.len()
            );
            for sweep in &volume.sweeps {
                print_sweep_inspect_line(sweep);
            }
        }
    }
}

/// Mirror of the bridge's per-sweep `inspect` line.
fn print_sweep_inspect_line(sweep: &Sweep) {
    let Some(field) = sweep.fields.first() else {
        return;
    };
    let gates = field.ngates as usize;
    let rays = sweep.nrays();
    let (first, spacing) = field.native_geometry(&sweep.range).unwrap_or_default();
    let max_range_m = first.round() + gates as f64 * spacing.round();
    let non_missing = match &field.data {
        FieldData::F32 { values, .. } => values.iter().filter(|value| !value.is_nan()).count(),
        other => other.len(),
    };
    println!(
        "  sweep {:02} {:>3} elev={:>5.2} deg gates={gates} rays={rays} range={:.1} km non-missing={non_missing}",
        sweep.sweep_number,
        short3(&field.name),
        sweep.fixed_angle_deg,
        max_range_m / 1000.0,
    );
}

/// Fixed sample positions shared with the bridge-side harness.
fn print_sweep_samples(station: &str, sweep: &Sweep) {
    let Some(field) = sweep.fields.first() else {
        return;
    };
    let gates = field.ngates as usize;
    let rays = sweep.nrays();
    if gates == 0 || rays == 0 {
        return;
    }
    for (ray, gate) in [
        (0, 0),
        (0, gates / 2),
        (rays / 4, gates / 3),
        (rays / 2, 10.min(gates - 1)),
        (rays - 1, gates - 1),
    ] {
        let value = match field.value(ray, gate) {
            Some(value) if value.is_nan() => "NaN".to_owned(),
            Some(value) => format!("{value:.4}"),
            None => "NaN".to_owned(),
        };
        println!(
            "{station} s{:02} v[{ray},{gate}]={value}",
            sweep.sweep_number
        );
    }
}

fn short3(name: &FieldName) -> &'static str {
    match name {
        FieldName::Dbzh => "REF",
        FieldName::Vradh => "VEL",
        _ => "UNK",
    }
}
