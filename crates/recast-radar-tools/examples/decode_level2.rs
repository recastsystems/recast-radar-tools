// Decode a NEXRAD Level II file and list its sweeps.
//
// cargo run --release -p recast-radar-tools --example decode_level2 -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::nexrad;

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);

    // Uncompressed, gzip, bzip2 and LDM block-bzip2 archives all decode here.
    let volume = nexrad::read_volume_from_path(&path)?;

    println!(
        "{} at {}",
        volume.attrs.instrument_name, volume.time_reference
    );
    if let Some(vcp) = volume.scan.vcp_pattern {
        println!("VCP {vcp}");
    }
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        let fields: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
        println!(
            "sweep {index:>2}: {:>5.2} deg, {} rays, {}",
            sweep.fixed_angle_deg,
            sweep.nrays(),
            fields.join(" ")
        );
    }
    Ok(())
}
