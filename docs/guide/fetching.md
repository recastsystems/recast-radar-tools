# Fetching data

The `data` module (feature `net`, crate `recast-radar-data`) downloads radar
data from public sources. It uses a blocking HTTPS client (reqwest with
rustls). Its network-free parts (site catalogs, object-key and listing
parsers, the real-time chunk planner) also build without `net`, for callers
with their own transport.

## NEXRAD Level II from AWS

The NOAA Open Data Dissemination program publishes every NEXRAD Level II
volume in the public S3 bucket `unidata-nexrad-level2`, under
`YYYY/MM/DD/SITE/`.

<!-- example: crates/recast-radar-tools/examples/fetch_aws.rs -->
```rust
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
```

A run on 2026-09-24:

<!-- output-unchecked: fetch_aws KTLX downloads (a live listing: the newest volume changes) -->
```text
2026/09/24/KTLX/KTLX20260924_135511_V06 (2982012 bytes)
downloaded downloads/KTLX20260924_135511_V06
KTLX at 2026-09-24 13:55:11 UTC: 5 sweeps
```

Other listings:

- `data::level2_objects_for_date(site, date)`: every volume of one UTC day,
  oldest first (`date` is a `chrono::NaiveDate`).
- `data::recent_level2_objects(site, days_back, max_count)`: the newest
  volumes, newest first.
- `data::level2_objects_for_window(...)` and
  `data::select_level2_objects_for_window(...)`: the volumes of a time
  window, for loops and case studies.
- `data::fetch_volume_bytes(url)` downloads any volume URL into memory (with
  a size cap and one retry); decode the bytes with `io::read_supported_volume_bytes`.

## Real-time Level II chunks

While a volume is being scanned, the radar publishes it in chunks to the
bucket `unidata-nexrad-level2-chunks`, one every few seconds.

- `data::latest_realtime_level2_volume(site)` lists the chunks of the newest
  volume, and `data::download_realtime_volume(&volume, dir)` joins them into
  one Level II file, which decodes like an archive volume. An incomplete
  volume decodes too: its sweeps so far.
- `data::realtime::iterator::ChunkIterator` follows a site chunk by chunk:
  it joins the current or next volume, polls for new chunks, downloads them
  in order, retries under a policy and moves to the next volume after the
  End chunk. Its request planner (`ChunkPlanner`) does no I/O itself, so the
  same logic drives the async `ChunkStream` (feature `async` of
  `recast-radar-data`) and callers' own transports.
- `data::realtime::timing` models when the next chunk and the next volume
  are due, from the volume coverage pattern (`vcp_catalog`).

## Site catalogs

`data::sites` has one catalog of radar sites worldwide: the NEXRAD and TDWR
network, community research feeds and the international providers' stations,
compiled in, with locations. `data::fetch_level2_radar_sites(days_back)`
asks the AWS bucket which Level II sites have data.

## International and other feeds

`data::international` has one adapter per national or multi-country radar
feed: DWD, DMI, SMHI, FMI, GeoSphere Austria, SHMU, CHMI, KAIA (Estonia),
ARPA Piemonte and Lombardia, Meteo Romania, the EUMETNET Open Radar Data
cache (many European countries), the Australian radars on NCI, and JMA. Each implements `IntlProvider`:
`list_sites` and `latest`, which returns a `FramePlan` describing the files
of the newest frame. The caller downloads each part with
`data::fetch_volume_bytes`, decodes it with `io::read_supported_volume_bytes`
and, for plans with several parts, joins them with `model::merge_volumes`.
`intl_providers()` lists the providers and `intl_static_sites()` their
stations.

`data::gdex`, `data::grid_products`, `data::tropical` and
`data::community_feeds` cover NSF NCAR GDEX (THREDDS) archives, gridded and
composite products and warnings of national services, tropical cyclones (NHC
and GDACS), and community research radars that publish Level II over the
GR2Analyst `dir.list` polling convention.

## Being a good client

These are public services. Cache what you download (`download_object` keeps
a file whose size matches and prunes its directory by age and size), do not
poll faster than the data changes (a Level II volume takes 4 to 10 minutes;
a chunk arrives every few seconds), and prefer listing once and downloading
what is new over repeated listings.
