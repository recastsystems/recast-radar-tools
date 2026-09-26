//! Read physical values from a radar file: the strongest reflectivity of the
//! lowest sweep, where it is, and how the sweep's gates split into values,
//! no-echo, missing and range-folded gates.
//!
//! cargo run --release -p recast-radar-tools --example physical_values -- <radar-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::io;
use recast_radar_tools::model::{Gate, Quantity, beam_ground_range_m, beam_height_above_radar_m};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <radar-file>")?);
    let volume = io::read_supported_volume_bytes(&std::fs::read(&path)?)?;

    // The lowest sweep that has reflectivity. `tilt_elevation_deg` is the
    // elevation the antenna actually scanned at.
    let (index, sweep) = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, sweep)| sweep.find(Quantity::Reflectivity).is_some())
        .min_by(|(a, _), (b, _)| {
            let a = volume.tilt_elevation_deg(*a).unwrap_or(f32::INFINITY);
            let b = volume.tilt_elevation_deg(*b).unwrap_or(f32::INFINITY);
            a.total_cmp(&b)
        })
        .ok_or("no sweep has reflectivity")?;
    let field = sweep
        .find(Quantity::Reflectivity)
        .ok_or("no reflectivity")?;
    let elevation_deg = volume.tilt_elevation_deg(index).unwrap_or(f32::NAN);
    println!(
        "sweep {index} at {elevation_deg:.2} deg: {} ({} x {} gates)",
        field.name, field.nrays, field.ngates
    );

    // A field stores the source's packed values. `gate` resolves one of them
    // to a physical value or a sentinel; `value` keeps only physical values.
    let (mut values, mut undetect, mut missing, mut folded) = (0, 0, 0, 0);
    let mut strongest: Option<(f32, usize, usize)> = None;
    let (rays, gates) = field.shape();
    for ray in 0..rays {
        for gate in 0..gates {
            match field.gate(ray, gate) {
                Some(Gate::Value(dbz)) => {
                    values += 1;
                    if strongest.is_none_or(|(max, _, _)| dbz > max) {
                        strongest = Some((dbz, ray, gate));
                    }
                }
                Some(Gate::Undetect) => undetect += 1,
                Some(Gate::RangeFolded) => folded += 1,
                // `Missing`, and any sentinel class a later version adds (`Gate`
                // is `#[non_exhaustive]`).
                _ => missing += 1,
            }
        }
    }
    println!("{values} values, {undetect} no echo, {missing} missing, {folded} range folded");

    let (dbz, ray, gate) = strongest.ok_or("no reflectivity values")?;
    // Gate centres come from the field's native geometry on the sweep's range
    // coordinate (a field may have coarser gates than the sweep).
    let (first_m, spacing_m) = field
        .native_geometry(&sweep.range)
        .ok_or("no gate geometry")?;
    let slant_m = first_m + gate as f64 * spacing_m;
    let azimuth_deg = sweep.rays.azimuth_deg[ray];
    let ray_elevation_deg = f64::from(sweep.rays.elevation_deg[ray]);
    let ground_km = beam_ground_range_m(slant_m, ray_elevation_deg) / 1000.0;
    let height_m = beam_height_above_radar_m(slant_m, ray_elevation_deg)
        + volume.location.altitude_m.unwrap_or(0.0);
    println!(
        "strongest: {dbz:.1} dBZ at azimuth {azimuth_deg:.1} deg, {ground_km:.1} km, \
         beam centre {height_m:.0} m above sea level"
    );
    if let Some(time) = volume.ray_time(index, ray) {
        println!("ray time {time}");
    }

    // All values at once, NaN for every sentinel.
    let physical = field.to_physical();
    let mean = physical.iter().filter(|v| v.is_finite()).sum::<f32>() / values.max(1) as f32;
    println!("mean of the {values} values: {mean:.1} dBZ");
    Ok(())
}
