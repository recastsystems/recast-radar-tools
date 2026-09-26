//! Write a radar file of any supported format as NEXRAD Level II, CfRadial 1,
//! CfRadial 2 / FM301 and ODIM_H5, and read each file back.
//!
//! cargo run --release -p recast-radar-tools --features write --example write_formats -- <radar-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::{cfradial, io, nexrad, odim};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(args.next().ok_or("usage: <radar-file> <out-dir>")?);
    let out = PathBuf::from(args.next().ok_or("usage: <radar-file> <out-dir>")?);
    let volume = io::read_supported_volume_bytes(&std::fs::read(&input)?)?;

    // NEXRAD Level II with bzip2 LDM records. The site id comes from the
    // instrument name (an ODIM node such as ESPDG gives EPDG) unless
    // `options.icao` sets one; the summary says how each moment was coded.
    let options = nexrad::write::WriteOptions::default();
    let (level2, summary) = nexrad::write::write_volume_with_source(
        &volume,
        nexrad::write::SourceMetadata::default(),
        &options,
    )?;
    println!(
        "Level II {}: {} sweeps, {} radials, {} LDM records",
        summary.icao, summary.sweeps, summary.radials, summary.records
    );
    for moment in summary.moments.iter().filter(|moment| moment.sweep == 0) {
        println!(
            "  sweep 0 {:?} from {}: {}-bit, scale {}, offset {}",
            moment.moment,
            moment.field.as_str(),
            moment.word_size,
            moment.scale,
            moment.offset
        );
    }

    let outputs = [
        ("volume.ar2v", level2),
        (
            "volume.cf1.nc",
            cfradial::write_cfradial1(&volume, &cfradial::Cfradial1Options::default())?,
        ),
        (
            "volume.fm301.nc",
            cfradial::write_cfradial2(&volume, &cfradial::Cfradial2Options::default())?,
        ),
        (
            "volume.h5",
            odim::write_odim_h5_volume(&volume, &odim::OdimWriteOptions::default())?,
        ),
    ];
    for (name, bytes) in outputs {
        let path = out.join(name);
        std::fs::write(&path, &bytes)?;
        // Every file reads back through the same router.
        let again = io::read_supported_volume_bytes(&bytes)?;
        let rays: usize = again.sweeps.iter().map(|sweep| sweep.nrays()).sum();
        println!(
            "{}: {:?}, {} sweeps, {} rays",
            path.display(),
            again.provenance.source_format,
            again.sweeps.len(),
            rays
        );
    }
    Ok(())
}
