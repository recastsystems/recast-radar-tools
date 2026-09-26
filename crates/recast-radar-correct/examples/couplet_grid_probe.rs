//! Print raw, default region engine and Py-ART port velocities on an
//! azimuth/range box of the lowest velocity tilt, or of the given sweep: for
//! inspecting couplets and fold errors.
//!
//! Usage: `couplet_grid_probe <l2-file> <az_lo> <az_hi> <rng_lo_km> <rng_hi_km> [gate_step] [sweep]`

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::Quantity;
use recast_radar_correct::{dealias_velocity, dealias_velocity_pyart_region};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = &args[0];
    let az_lo: f32 = args[1].parse()?;
    let az_hi: f32 = args[2].parse()?;
    let r_lo: f64 = args[3].parse()?;
    let r_hi: f64 = args[4].parse()?;
    let step: usize = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(1);
    let sweep_pick: Option<usize> = args.get(6).map(|s| s.parse().unwrap());
    let volume = recast_radar_io_nexrad::read_volume_from_path(path.as_ref() as &std::path::Path)?;
    let (index, sweep) = match sweep_pick {
        Some(i) => (i, &volume.sweeps[i]),
        None => volume
            .sweeps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.find(Quantity::RadialVelocity).is_some())
            .min_by(|a, b| a.1.fixed_angle_deg.total_cmp(&b.1.fixed_angle_deg))
            .ok_or("no velocity")?,
    };
    let velocity = sweep.find(Quantity::RadialVelocity).unwrap();
    let region = dealias_velocity(sweep, velocity);
    let port = dealias_velocity_pyart_region(sweep, velocity);
    let (first_m, spacing_m) = velocity.native_geometry(&sweep.range).unwrap();
    let nyq = sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()
        .and_then(|n| n.first().copied());
    println!(
        "sweep #{index} elev {:.2} gates {} spacing {} m first {} m nyq {nyq:?}",
        sweep.fixed_angle_deg, velocity.ngates, spacing_m, first_m
    );
    let g_lo = ((r_lo * 1000.0 - first_m) / spacing_m).round().max(0.0) as usize;
    let g_hi = ((r_hi * 1000.0 - first_m) / spacing_m).round() as usize;
    let mut rows: Vec<(usize, f32)> = sweep
        .rays
        .azimuth_deg
        .iter()
        .enumerate()
        .filter(|(_, a)| **a >= az_lo && **a <= az_hi)
        .map(|(r, a)| (r, *a))
        .collect();
    rows.sort_by(|a, b| a.1.total_cmp(&b.1));
    let nrays = sweep.rays.azimuth_deg.len();
    println!(
        "rays {nrays}; first az {:?} last az {:?}; rows in box: {:?}",
        sweep.rays.azimuth_deg.first(),
        sweep.rays.azimuth_deg.last(),
        rows.iter().map(|r| r.0).collect::<Vec<_>>()
    );
    let fmt = |v: Option<f32>| v.map_or("   .".to_string(), |v| format!("{v:4.0}"));
    for (label, field) in [
        ("raw", velocity),
        ("region", &region),
        ("pyart port", &port),
    ] {
        println!("== {label} (rows az, cols range km)");
        print!("       ");
        for g in (g_lo..=g_hi).step_by(step) {
            print!("{:5.1}", (first_m + g as f64 * spacing_m) / 1000.0);
        }
        println!();
        for (row, az) in &rows {
            print!("{az:6.1} ");
            for g in (g_lo..=g_hi).step_by(step) {
                print!(" {}", fmt(field.value(*row, g)));
            }
            println!();
        }
    }
    Ok(())
}
