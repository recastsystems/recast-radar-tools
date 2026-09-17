// Decode a NEXRAD Level II file and list its sweeps.
//
// cargo run --release -p recast-radar-tools --example decode_level2 -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::nexrad;

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);

    // Uncompressed, gzip, bzip2 and LDM block-bzip2 archives all decode here.
    let volume = nexrad::decode_volume_from_path(&path)?;

    println!("{} at {}", volume.site.id, volume.volume_time);
    if let Some(vcp) = &volume.vcp {
        println!("VCP {}", vcp.pattern);
    }
    for (index, cut) in volume.cuts.iter().enumerate() {
        let moments: Vec<String> = cut.moments.keys().map(ToString::to_string).collect();
        println!(
            "sweep {index:>2}: {:>5.2} deg, {} radials, {}",
            cut.elevation_deg,
            cut.radials.len(),
            moments.join(" ")
        );
    }
    Ok(())
}
