//! Column products of a radar volume: composite reflectivity, echo tops and
//! vertically integrated liquid, with the maximum of each and a PNG of the
//! composite.
//!
//! cargo run --release -p recast-radar-tools --features render \
//!     --example composite -- <radar-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::model::{Field, Sweep};
use recast_radar_tools::render::{self, RasterOptions};
use recast_radar_tools::{io, map};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <radar-file> <out-dir>".into());
    };
    let mut volume = io::read_supported_volume_bytes(&std::fs::read(&input)?)?;

    // Each product is a field on the rays and gates of the lowest tilt with
    // reflectivity, the column base. An RHI is not a tilt: on a volume of
    // RHIs only there is no base and no product.
    let base = map::column_base_sweep(&volume).ok_or("no tilt has reflectivity")?;
    let composite = map::composite_reflectivity(&volume).ok_or("no composite")?;
    let tops = map::echo_top(&volume, map::ECHO_TOP_THRESHOLD_DBZ).ok_or("no echo tops")?;
    let vil = map::vil(&volume).ok_or("no VIL")?;

    // The base is the sweep with the lowest `tilt_elevation_deg`, the
    // elevation the antenna scanned at, which can differ from the cut angle
    // in `fixed_angle_deg`.
    let sweep = &volume.sweeps[base];
    let elevation_deg = volume.tilt_elevation_deg(base).unwrap_or(f32::NAN);
    println!("column base: sweep {base} at {elevation_deg:.2} deg");
    print_maximum(sweep, &composite, "dBZ");
    print_maximum(sweep, &tops, "m");
    print_maximum(sweep, &vil, "kg m-2");

    // Added to the base sweep, the composite draws like any other field.
    let name = composite.name.clone();
    volume.sweeps[base].add_field(composite)?;
    let path = out_dir.join("composite.png");
    render::render_field_png(&volume, base, &name, &path, RasterOptions::default())?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The largest value of a column product and where it is.
fn print_maximum(sweep: &Sweep, field: &Field, units: &str) {
    let (rays, gates) = field.shape();
    let maximum = (0..rays)
        .flat_map(|ray| (0..gates).map(move |gate| (ray, gate)))
        .filter_map(|(ray, gate)| field.value(ray, gate).map(|value| (value, ray, gate)))
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let Some((value, ray, gate)) = maximum else {
        println!("{}: no values", field.name);
        return;
    };
    let range_km = field
        .native_geometry(&sweep.range)
        .map_or(f64::NAN, |(first, spacing)| {
            (first + gate as f64 * spacing) / 1000.0
        });
    println!(
        "{}: maximum {value:.1} {units} at azimuth {:.1} deg, {range_km:.1} km",
        field.name, sweep.rays.azimuth_deg[ray]
    );
}
