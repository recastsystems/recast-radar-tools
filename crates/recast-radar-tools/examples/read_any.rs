//! Read a radar file of any supported format and list its sweeps and fields.
//!
//! cargo run --release -p recast-radar-tools --example read_any -- <radar-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::io;
use recast_radar_tools::model::{Field, RangeCoord};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <radar-file>")?);
    let bytes = std::fs::read(&path)?;

    // NEXRAD Level II, ODIM_H5, CfRadial 1, DORADE or a JMA GRIB2 tar, also
    // inside gzip or a single-file ZIP. The router sniffs the format.
    let volume = io::read_supported_volume_bytes(&bytes)?;

    println!(
        "{} ({:?}), {}",
        volume.attrs.instrument_name, volume.provenance.source_format, volume.time_reference
    );
    let location = volume.location;
    if let (Some(lat), Some(lon)) = (location.latitude_deg, location.longitude_deg) {
        let altitude = location.altitude_m.unwrap_or(f64::NAN);
        println!("latitude {lat:.4}, longitude {lon:.4}, altitude {altitude:.0} m");
    }

    for (index, sweep) in volume.sweeps.iter().enumerate() {
        println!(
            "sweep {index}: {} at {:.2} deg, {} rays, {}",
            sweep.sweep_mode.as_str(),
            sweep.fixed_angle_deg,
            sweep.nrays(),
            range_text(&sweep.range)
        );
        for field in &sweep.fields {
            println!("  {}", field_text(field));
        }
    }
    Ok(())
}

/// Gate count, first gate centre and spacing of a range coordinate.
fn range_text(range: &RangeCoord) -> String {
    let first_km = range.center_m(0).unwrap_or(f64::NAN) / 1000.0;
    match range.spacing_m() {
        Some(spacing) => format!(
            "{} gates from {first_km:.3} km every {spacing:.1} m",
            range.ngates()
        ),
        None => format!("{} gates from {first_km:.3} km", range.ngates()),
    }
}

/// Name, quantity, units and storage type of a field.
fn field_text(field: &Field) -> String {
    // Units the source stated, else the FM301 units of a known name.
    let units = field
        .attrs
        .units
        .as_deref()
        .or_else(|| field.name.info().map(|info| info.units))
        .unwrap_or("");
    let (rays, gates) = field.shape();
    format!(
        "{:<8} {:?} [{units}], {rays} x {gates} {}",
        field.name.as_str(),
        field.quantity,
        field.data.dtype()
    )
}
