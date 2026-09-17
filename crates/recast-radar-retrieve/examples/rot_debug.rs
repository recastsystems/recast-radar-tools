//! Rotation-marker diagnostic: dump every detection with full per-site
//! numbers (field report: false markers on the tail of the KMKX line).
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use recast_radar_core::Volume;

/// Level II decoding through the un-migrated `recast-radar-io-nexrad`,
/// bridged to the FM301 model (design note 13.3) until `fm301-io` lands.
#[allow(deprecated)]
mod legacy_bridge {
    use super::Volume;

    pub fn decode_level2(bytes: &[u8]) -> Result<Volume, Box<dyn std::error::Error>> {
        let legacy = recast_radar_io_nexrad::decode_volume_from_bytes(bytes)?;
        Ok(recast_radar_core::legacy::volume_from_legacy(legacy)?.0)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for arg in std::env::args().skip(1) {
        let raw = std::fs::read(&arg)?;
        let volume = legacy_bridge::decode_level2(&raw)?;
        let sites = recast_radar_retrieve::detect_rotation_sites(&volume);
        println!("=== {arg}: {} sites", sites.len());
        let mut sorted = sites.clone();
        sorted.sort_by(|a, b| a.azimuth_deg.total_cmp(&b.azimuth_deg));
        for s in &sorted {
            println!(
                "  az={:6.1} rng={:6.1}km vrot={:5.1} gtg={:5.1} rank={} tilts={} depth={:5.1}km base={:.1}deg {:?}",
                s.azimuth_deg,
                s.ground_range_m / 1000.0,
                s.vrot_mps,
                s.gate_to_gate_dv_mps,
                s.rank,
                s.depth_tilts,
                s.depth_m / 1000.0,
                s.base_elevation_deg,
                s.strength,
            );
        }
        for (elev, count, best) in recast_radar_retrieve::rotation_features_per_tilt(&volume) {
            println!("  tilt {elev:4.1}deg: {count:3} feats best_rank={best}");
        }
    }
    Ok(())
}
