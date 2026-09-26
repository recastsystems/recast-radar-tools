//! Download the latest NEXRAD Level II volume of a radar site from the public
//! `unidata-nexrad-level2` bucket on AWS, and decode it.
//!
//! cargo run --release -p recast-radar-tools --features net \
//!     --example fetch_aws -- <site> <out-dir>
//!
//! For example `-- KTLX downloads`. The volume is saved in `<out-dir>`; a
//! second run finds it there and does not download it again.

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::{data, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(site), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <site> <out-dir>".into());
    };

    // The newest object under today's and yesterday's prefixes
    // (`YYYY/MM/DD/SITE/`). `level2_objects_for_date` lists a whole day.
    let object = data::latest_level2_object(&site, 1)?;
    println!("{} ({} bytes)", object.key, object.size);

    // Saved as `<out-dir>/<file name>`; kept when the size already matches.
    let downloaded =
        data::download_object(data::LEVEL2_ARCHIVE_BUCKET, object, &PathBuf::from(out_dir))?;
    let status = if downloaded.cache_hit {
        "found"
    } else {
        "downloaded"
    };
    println!("{status} {}", downloaded.path.display());

    let volume = nexrad::read_volume_from_path(&downloaded.path)?;
    println!(
        "{} at {}: {} sweeps",
        volume.attrs.instrument_name,
        volume.time_reference,
        volume.sweeps.len()
    );
    Ok(())
}
