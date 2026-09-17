# Data access (`recast-radar-data`): design note

Stream E of the wave 2 plan (spec section 4.6). This note fixes the crate's
dependency and feature boundaries, which E.2-E.4 build on.

## Dependencies

Normal dependencies are external only: `chrono`, `quick-xml`, `serde`,
`serde_json`, `thiserror`, and `reqwest` (optional, `net`). No internal crate:
decoding is the caller's choice (spec 4.1 dependency rule).

`recast-radar-io-jma` was a normal dependency only so `JmaProvider::list_sites`
could download the newest N5 tar and read the station headers from it. That
is now a caller-provided table:

- `JmaProvider::new()` serves the embedded 20-station table, which was
  decoded from real tar headers.
- `JmaProvider::with_stations(rows)` serves a caller's table instead. A caller
  that wants the live network downloads the N5 tar named by
  `latest(..).parts[0].url`, maps `recast_radar_io_jma::jma_tar_station_headers`
  rows into `JmaStation`s, and builds the provider from them.

Moving the parser into this crate was the other option. It would have
duplicated about 150 lines of ustar and GRIB2 section walking from `io-jma`.
A side effect of the change: `list_sites` no longer downloads a multi-MB tar.
`io-jma` stays a dev-dependency for the test that builds a caller table from a
real tar member (`jma-n5-20191012-090000-rs47773`, testdata) and for the
ignored live probes.

## Features

| Feature | Default | Contents |
|---|---|---|
| `net` | yes | The blocking HTTPS client (reqwest + rustls) and every function that sends a request |
| `async` | no | E.3: a `futures::Stream` over the same request planning |

Gating is at compile time. A function that would always fail without a
client is not compiled at all. It does not return a "network disabled" error.
Under `net`:

- Transport: `fetch_text`, `fetch_listing_text`, `fetch_bytes`,
  `fetch_volume_bytes`, `url_exists`, the shared clients, and
  `DataSourceError::Http`.
- NEXRAD: archive listing, real-time volume resolution, and downloads
  (`latest_level2_object`, `latest_realtime_level2_volume*`,
  `download_object`, `download_realtime_volume`, and related functions).
- International providers: the network trait methods
  (`IntlProvider::{list_sites, latest, recent}`,
  `RecentFrames::recent_frames`,
  `ArchiveFrames::{day_plans, day_plans_with_progress, window_plans}`), their
  implementations, and the provider helpers that fetch.
  `recent_source`/`archive_source` and the derived capability flags stay. The
  `RecentFrames`/`ArchiveFrames` traits stay too, as empty marker traits.
- GDEX crawl, NCSS, and downloads; grid-product and tropical feed fetches.
- Examples (`required-features = ["net"]`) and tests that call any of the
  above, including the ignored live probes.

Without `net` the crate builds for `wasm32-unknown-unknown`. It still provides:
types, embedded site catalogs (`sites`, `fallback_sites`, `intl_static_sites`,
`static_sites`, capability cards), key/listing/feed parsers, archive window
selection, real-time chunk key parsing and validation, and local cache
helpers.

Private helpers that only network paths call are not gated one by one. They
stay compiled and unit-tested in both configurations. The crate root allows
`dead_code` and `unused_imports` only when `net` is off. The default build
still lints them.

## Placement for E.2-E.4

- E.2 `src/realtime/{timing.rs, vcp_catalog.rs}`: pure and available without
  `net`. Tested from committed real S3 chunk listings, parsed with the same
  `ListObjectsV2` XML types as the live client.
- E.3 `RetryPolicy`: pure (returns delays, never sleeps). `ChunkIterator`:
  `net`. The request planning and rollover logic behind it stay pure so the
  `async` stream shares them.
- E.4 `examples/live_decode.rs`: `net`.

## Verification

```
cargo test -p recast-radar-data
cargo test -p recast-radar-data --no-default-features
cargo check -p recast-radar-data --no-default-features --target wasm32-unknown-unknown
```
