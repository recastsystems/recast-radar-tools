//! Rotation-marker diagnostic: dump every detection with full per-site
//! numbers (field report: false markers on the tail of the KMKX line).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for arg in std::env::args().skip(1) {
        let raw = std::fs::read(&arg)?;
        let volume = recast_radar_io_nexrad::decode_volume_from_bytes(&raw)?;
        let sites = recast_radar_render::detect_rotation_sites(&volume);
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
        for (elev, count, best) in recast_radar_render::rotation_features_per_tilt(&volume) {
            println!("  tilt {elev:4.1}deg: {count:3} feats best_rank={best}");
        }
    }
    Ok(())
}
