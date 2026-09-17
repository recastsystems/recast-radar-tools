//! Decode every radar volume in a mobile-radar zip archive and summarize.
//!
//! Usage: cargo run -p recast-radar-io-dorade --example mobile_archive_probe -- deployment.zip

use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: mobile_archive_probe <archive.zip>");
        std::process::exit(2);
    };

    let started = Instant::now();
    let volumes = recast_radar_io_dorade::mobile_archive::read_mobile_archive_from_path(
        path.as_ref(),
        recast_radar_io_nexrad::read_volume_from_bytes,
    )?;
    let elapsed = started.elapsed();
    println!(
        "{} volumes decoded in {:.2}s",
        volumes.len(),
        elapsed.as_secs_f32()
    );
    for entry in &volumes {
        let volume = &entry.volume;
        let fields: Vec<String> = volume
            .sweeps
            .first()
            .map(|sweep| sweep.fields.iter().map(|f| f.name.to_string()).collect())
            .unwrap_or_default();
        println!(
            "  {} t={} sweeps={} rays={} members={} lat={:?} lon={:?} [{}] {}",
            volume.attrs.instrument_name,
            volume.time_reference.format("%Y-%m-%d %H:%M:%S"),
            volume.sweeps.len(),
            volume.provenance.decode.decoded_ray_count,
            entry.member_count,
            volume.location.latitude_deg,
            volume.location.longitude_deg,
            fields.join(","),
            entry.member_label,
        );
    }
    Ok(())
}
