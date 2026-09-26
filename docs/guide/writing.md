# Writing radar files

Every writer takes a [`model::Volume`](data-model.md) from any decoder and
returns a file in another format. A writer refuses what its format cannot
hold with a typed error instead of writing something else, and every file it
writes reads back through the router.

| Format | Module (feature) | Entry points | Reference |
|---|---|---|---|
| NEXRAD Level II (Archive II) | `nexrad` (`write`) | `nexrad::write::write_volume`, `write_volume_with_source`, `write_volume_to`, `rewrite_level2` | [Level II writer](../level2/writer.md) |
| NEXRAD real-time chunks | `nexrad` (`write`) | `nexrad::write::realtime::write_realtime_chunks`, `ChunkWriter` | [Level II writer](../level2/writer.md#real-time-chunks) |
| GR2Analyst polling directory | `nexrad` (`write`) | `nexrad::write::polling::PollingDirectory` | [Level II writer](../level2/writer.md#polling-directory) |
| CfRadial 1.4 (classic netCDF) | `cfradial` | `cfradial::write_cfradial1` | [Writers](../design/writers.md) |
| CfRadial 2 / FM301 (netCDF-4) | `cfradial` | `cfradial::write_cfradial2` | [Writers](../design/writers.md) |
| ODIM_H5 polar volume (PVOL) | `odim` | `odim::write_odim_h5_volume` | [Writers](../design/writers.md) |
| HDF5 and netCDF-4 | `hdf5` | `hdf5::write::Writer`, `hdf5::write::netcdf4::NcWriter` | [Writers](../design/writers.md#hdf5-and-netcdf-4) |

## Every format

<!-- example: crates/recast-radar-tools/examples/write_formats.rs -->
```rust
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
```

On the ODIM_H5 file of the reading example:

<!-- output: write_formats repo:testdata/files/other/odim/espdg.pvol.20260707.dbzh_vradh.h5 dir:out -->
```text
Level II EPDG: 2 sweeps, 720 radials, 7 LDM records
  sweep 0 Ref from DBZH: 8-bit, scale 2, offset 66
  sweep 0 Vel from VRADH: 8-bit, scale 3.1833522, offset 119
out/volume.ar2v: NexradLevel2, 2 sweeps, 720 rays
out/volume.cf1.nc: CfRadial1, 2 sweeps, 720 rays
out/volume.fm301.nc: CfRadial2, 2 sweeps, 720 rays
out/volume.h5: OdimH5, 2 sweeps, 720 rays
```

## NEXRAD Level II

The Level II writer follows ICD 2620010 (Archive II) and ICD 2620002
(RDA/RPG messages): the volume header, a metadata record of 134 fixed frames
(Message 18 adaptation data, Message 5 volume coverage pattern and Message 2
RDA status at the frames NOAA's files use), Message 31 radials, the radar's
position, and each moment coded from the source's values (the `WriteSummary`
reports each moment's coding and any value error). It refuses a volume
without a site position, an RHI, more than 32 sweeps and geometry Level II
cannot hold (`WriteError`); nothing is written when it refuses. The LDM records are
compressed with `recast-radar-bzip2`'s encoder, which writes libbzip2
1.0.8's streams byte for byte, except which identical row `origPtr` names in
a block that repeats a shorter string (the stream decodes the same). For a Level II source, `write_volume_with_source` reuses its
metadata messages and constant blocks, and `rewrite_level2` re-encodes Level
II bytes. The writer is the `write` feature (off by default).

`nexrad::write::realtime` cuts the same records into the `S`, `I` and `E`
chunks of the `unidata-nexrad-level2-chunks` bucket, and
`nexrad::write::polling::PollingDirectory` publishes files the way GRLevelX
polling clients read a polling directory: `config.cfg` (`ListFile:
dir.list`, then the sites) and `grlevel2.cfg` listing the sites, and per site
`SITEYYYYMMDD_HHMMSS_V06.ar2v` files (the NWS archive's names) with a
`dir.list` of `<size> <file name>` lines, oldest first, every line ending in
LF. Every file is written
to a temporary name and renamed into place.

## CfRadial and ODIM_H5

`write_cfradial1` writes CfRadial 1.4 in classic netCDF (64-bit offset).
Sweeps with different gate geometries share one range coordinate when every
sweep fits the finest grid, and otherwise take a range per sweep
(`Cfradial1Options::with_range_layout`). `write_cfradial2` writes the FM301
view of the volume as netCDF-4, and `write_odim_h5_volume` an ODIM_H5 PVOL
(`ODIM_H5/V2_3`, or the source's own version for a volume read from
ODIM_H5). All three are deterministic: the same volume gives the same bytes.
A field coded by a Level III level table is refused by the CfRadial 1 and
ODIM_H5 writers (neither format can state such a coding) and written decoded,
with its codes beside it, by the FM301 writer.

## From the command line and Python

`recast-radar convert --to level2|cfradial1|odim|fm301` and `recast-radar
publish` run these writers ([command-line guide](cli.md#convert-and-publish));
`recast_radar.write`, `convert`, `write_chunks` and `publish` do so from
Python ([Python guide](python.md#writers-and-the-polling-publisher)).
