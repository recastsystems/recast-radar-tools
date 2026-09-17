// Sanity-check rotation detection on a real volume: prints detected sites.
// usage: rotation_probe <l2-file>
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use recast_radar_core::Volume;
use recast_radar_retrieve::{detect_rotation_sites, rotation_features_per_tilt};
use std::time::Instant;

/// Level II decoding through the un-migrated `recast-radar-io-nexrad`,
/// bridged to the FM301 model (design note 13.3) until `fm301-io` lands.
#[allow(deprecated)]
mod legacy_bridge {
    use super::Volume;
    use std::path::Path;

    pub fn decode_level2(path: &Path) -> Result<Volume, Box<dyn std::error::Error>> {
        let legacy = recast_radar_io_nexrad::decode_volume_from_path(path)?;
        Ok(recast_radar_core::legacy::volume_from_legacy(legacy)?.0)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: rotation_probe <l2-file>")?;
    let volume = legacy_bridge::decode_level2(path.as_ref())?;
    for (elev, count, best) in rotation_features_per_tilt(&volume) {
        println!("tilt {elev:5.2}: {count} 2D features, best rank {best}");
    }
    let start = Instant::now();
    let sites = detect_rotation_sites(&volume);
    println!(
        "{} site(s) in {:.0} ms",
        sites.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
    for site in &sites {
        println!(
            "  {:?} R{} az {:6.1} rng {:6.1} km Vrot {:4.1} m/s GTG {:4.1} depth {} tilts / {:.1} km",
            site.strength,
            site.rank,
            site.azimuth_deg,
            site.ground_range_m / 1000.0,
            site.vrot_mps,
            site.gate_to_gate_dv_mps,
            site.depth_tilts,
            site.depth_m / 1000.0,
        );
    }
    Ok(())
}
