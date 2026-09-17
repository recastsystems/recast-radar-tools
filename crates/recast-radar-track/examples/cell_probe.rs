// Identify storm cells on real volumes: count, positions, timing.
// usage: cell_probe <l2-file> [...]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use recast_radar_track::identify_storm_cells;
use std::time::Instant;

/// Level II decoding through the un-migrated `recast-radar-io-nexrad`,
/// bridged to the FM301 model (design note 13.3) until `fm301-io` lands.
#[allow(deprecated)]
mod legacy_bridge {
    use recast_radar_core::Volume;
    use std::path::Path;

    pub fn decode_level2(path: &Path) -> Result<Volume, Box<dyn std::error::Error>> {
        let legacy = recast_radar_io_nexrad::decode_volume_from_path(path)?;
        Ok(recast_radar_core::legacy::volume_from_legacy(legacy)?.0)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for path in std::env::args().skip(1) {
        let volume = legacy_bridge::decode_level2(path.as_ref() as &std::path::Path)?;
        let start = Instant::now();
        let cells = identify_storm_cells(&volume);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{} -> {} cells in {ms:.1} ms",
            path.rsplit(['/', '\\']).next().unwrap_or(&path),
            cells.len()
        );
        for cell in cells.iter().take(8) {
            println!(
                "   ({:7.1}, {:7.1}) km  {:4.1} dBZ  {:6.1} km2  r_eq {:4.1}",
                cell.east_km, cell.north_km, cell.max_dbz, cell.area_km2, cell.eq_radius_km
            );
        }
    }
    Ok(())
}
