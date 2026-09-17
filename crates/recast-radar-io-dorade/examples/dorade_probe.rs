//! Decode one or more DORADE sweepfiles and print volume geometry.
//!
//! Usage: cargo run -p recast-radar-io-dorade --example dorade_probe -- swp.file [swp.file...]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if paths.is_empty() {
        eprintln!("usage: dorade_probe <swp.file> [swp.file...]");
        std::process::exit(2);
    }

    let volume = recast_radar_io_dorade::dorade::read_dorade_volume_from_paths(&paths)?;
    println!(
        "site {} lat {:?} lon {:?} alt {:?} m",
        volume.attrs.instrument_name,
        volume.location.latitude_deg,
        volume.location.longitude_deg,
        volume.location.altitude_m
    );
    println!(
        "time reference {} | {} sweeps | compression {:?} | skipped {}",
        volume.time_reference,
        volume.sweeps.len(),
        volume.provenance.compression,
        volume.provenance.decode.skipped_message_count
    );
    for sweep in &volume.sweeps {
        println!(
            "  sweep {}: {} fixed {:.2} deg, {} rays, nyquist {:?}",
            sweep.sweep_number,
            sweep.sweep_mode.as_str(),
            sweep.fixed_angle_deg,
            sweep.nrays(),
            sweep
                .ray_vars
                .nyquist_velocity_mps
                .as_ref()
                .and_then(|values| values.first().copied()),
        );
        for field in &sweep.fields {
            let mut finite = 0usize;
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            let (rows, gates) = field.shape();
            for row in 0..rows {
                for gate in 0..gates {
                    if let Some(value) = field.value(row, gate) {
                        finite += 1;
                        min = min.min(value);
                        max = max.max(value);
                    }
                }
            }
            let (first, spacing) = field.native_geometry(&sweep.range).unwrap_or_default();
            println!(
                "    {}: {} rows x {} gates ({}, first centre {first:.1} m, spacing {spacing:.1} m), {} finite, range [{min:.2}, {max:.2}]",
                field.name,
                rows,
                gates,
                field.data.dtype(),
                finite
            );
        }
    }
    Ok(())
}
