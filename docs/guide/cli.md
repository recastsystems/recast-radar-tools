# The `recast-radar` command

`recast-radar` is the command-line front end of recast-radar-tools (crate
[`recast-radar-cli`](../../crates/recast-radar-cli)). It inspects, checks,
renders, downloads, converts and serves weather radar files, and publishes
NEXRAD Level II volumes in a GR2Analyst polling directory (see
[Convert and publish](#convert-and-publish)).

- [Install](#install)
- [Input formats](#input-formats)
- [Commands](#commands): [info](#info), [dump](#dump), [render](#render),
  [validate](#validate), [bench](#bench), [fetch](#fetch),
  [convert and publish](#convert-and-publish), [serve](#serve)
- [Polling directories](#polling-directories)
- [Exit status](#exit-status)
- [For developers](#for-developers)

## Install

From a checkout:

```sh
cargo install --path crates/recast-radar-cli          # installs recast-radar into ~/.cargo/bin
cargo build --release -p recast-radar-cli             # or build target/release/recast-radar
```

Minimum Rust version 1.94. The `net` feature (on by default) enables `fetch`
and pulls in the HTTPS client (reqwest with rustls, whose `ring` compiles C).
Without it the command makes no network requests and compiles no C:

```sh
cargo build --release -p recast-radar-cli --no-default-features
```

The [CLI binaries workflow](../../.github/workflows/cli-binaries.yml) is set
to build Linux, Windows and macOS (Apple silicon and Intel) binaries on pushes
to `main` and on manual dispatch, and to keep them as workflow artifacts of
the (private) repository. It has not run on GitHub yet: until a push or a
dispatch runs it, there are no such artifacts, and the macOS builds have not
run anywhere (the README's CI section lists what was checked locally).

`recast-radar --help` lists the commands; `recast-radar <command> --help`
lists a command's options.

## Input formats

The format of a file is detected from its contents, never from its name:

| Format | Notes |
|---|---|
| NEXRAD Level II | Archive II (`AR2V`, `ARCHIVE2`), uncompressed, whole-file gzip or bzip2, bzip2 LDM records; Message 31 and Message 1. The format is read from the bytes, not the file name: a `.gz` name on plain Archive II reads as Archive II |
| NEXRAD Level II records | Real-time chunks: a start chunk (metadata only) and intermediate chunks (no volume header) are described message by message |
| NEXRAD and TDWR Level III | Products with or without the WMO/NOAAPort header, bzip2 and zlib wrapped; General Status and text messages |
| ODIM_H5 | Polar volumes and scans (PVOL, SCAN) |
| CfRadial 1 | Classic netCDF (CDF-1, CDF-2) and netCDF-4 |
| CfRadial 2 / FM301 | netCDF-4, one group per sweep |
| DORADE | Sweep files, and mobile-radar ZIP archives of sweep files and `.msg31` members (several volumes) |
| JMA radar GRIB2 | NICT `Z__C_RJTD_*_RDR_JMAGPV_*` tars; one tar holds every station of the network: `--station ID` picks one (JMA id such as `ITOK`, or station number), `--all-stations` reads them all, the default is the first |

gzip wrappers and single-record ZIP responses (NCI THREDDS) are removed first.
Files larger than 1 GiB are refused. Not read: ODIM Cartesian products
(`IMAGE`, `COMP`).

## Commands

### info

A summary of each file: format and container, site and location, times, scan
strategy, and a table of sweeps.

```console
$ recast-radar info KTLX20240315_000217.trim.V06
KTLX20240315_000217.trim.V06  741 kB  NEXRAD Level II
  site      KTLX  35.3334 N  -97.2778 E  389 m
  time      2024-03-15T00:02:17Z  (rays 2024-03-15T00:02:17.182Z to 2024-03-15T00:02:50.506Z)
  source    NEXRAD Level II, AR2V0006.626, bzip2-blocks
  scan      VCP 212, VCP-212
  level2    RDA build 22.0, metadata messages 2 3 5 15 18
  sweeps    2 sweeps, 960 rays
  sweep  angle  mode                    rays gates  gate_m range_km nyq_m/s  fields
      0   0.48  azimuth_surveillance     480  1832     250    460.0     8.3  DBZH ZDR PHIDP RHOHV CCORH
      1   0.48  azimuth_surveillance     480  1192     250    300.0    23.8  DBZH VRADH WRADH
```

`info` takes several files; a file that does not decode is reported and the
others are still summarized (exit status 1 at the end). `--merge` merges the
files into one volume first, as `convert --merge` would, and summarizes that:
the check for a scan that a feed splits into files (an EUMETNET ORD moment
per file, a DWD sweep per file):

```console
$ recast-radar info --merge 'bejab@...@DBZH.h5' 'bejab@...@VRAD.h5'
2 files merged  462 kB  ODIM_H5
  site      BEJAB (Jabbeke)  51.1917 N  3.0642 E  50 m
  ...
  sweeps    9 sweeps, 3240 rays
  sweep  angle  mode                    rays gates  gate_m range_km nyq_m/s  fields
      0   0.50  azimuth_surveillance     360   300     500    150.0    53.3  DBZH VRAD
```

`--json` prints the same report as JSON (one object, or an array for several
files). Level III files add a `level3` object (product code, mnemonic, WMO
heading, times, elevation, block counts), real-time chunks a `records` object
(messages by type; radials, azimuths, times, moments and radial statuses per
cut).

### dump

Every decoded value of one file: the volume's global attributes, location,
scan strategy, radar parameters and calibration, provenance, every sweep's
metadata (mode, angles, range coordinate, per-ray variables), and every
field's attributes, storage type, packing (`scale_factor`, `add_offset`,
`_FillValue`, `_Undetect`, range-folded code) and gate statistics (valid,
missing, undetect and range-folded counts, minimum, maximum, mean).

```sh
recast-radar dump FILE                       # indented text
recast-radar dump --json FILE                # the same as JSON
recast-radar dump --sweep 1 --field VRADH FILE
recast-radar dump --json --data --sweep 0 --field DBZH FILE   # plus every gate value
recast-radar dump --rays FILE                # every ray's time, azimuth, elevation, per-ray variables
recast-radar dump --fm301 FILE               # the FM301 (CfRadial 2) group tree, ncdump -h style
recast-radar dump --fm301 --flavor wmo --json FILE
```

- Arrays longer than 16 values are summarized as `{count, missing, first,
  last, min, max}` unless `--rays` is given.
- `--data` adds each field's values in physical units, one array per ray, with
  `null` (JSON) or `-` (text) for gates without a value. The rows are written
  as they are produced, so a whole volume can be dumped without holding it
  as JSON in memory; `--sweep` and `--field` keep the output small.
- NEXRAD Level II files add `format_metadata.nexrad`: the metadata messages
  (2, 3, 5, 13, 15, 18, 32), each sweep's Message 31 constant blocks, the
  volume header time and the problems met while decoding them.
- Level III files add `level3`: the message header, product description
  (including its raw halfwords) and WMO/AWIPS text header; `symbology.layers`
  and `graphic.pages` with every display packet as `{code, kind, packet}`
  (storm ids and positions, past and forecast tracks, hail, mesocyclone and
  TVS symbols, text, vectors, contours, generic product components); and
  `tabular` with the text pages. The bins of data packets (radial, raster,
  digital precipitation and generic radial data) are replaced by
  `{"elided_values": N}` unless `--data` is given, like the gate data of the
  volume. General Status and text messages are given the same way.
- The decoder types these values come from have no serde support; they are
  converted from their derived `Debug` form: structs and struct variants
  become objects whose `"@type"` key holds the type or variant name (so a
  Level III generic component's `Text` and `Undecoded` variants stay apart;
  no field can be called `@type`), `None` becomes `null` and `Some(v)`
  becomes `v`, a tuple variant or tuple struct becomes `{"Name": value}` (the
  Level III packet kinds, `{"RdaBuild": 2200}`), a unit variant becomes its
  name as a string, times become ISO 8601 strings, and `NaN` or infinite
  numbers become `null`.
- `--fm301` prints the group tree the FM301 view builds: groups, dimensions,
  variables with dtype and attributes, group attributes. `--flavor xradar`
  (default) follows xradar 0.12 (`open_*_datatree` names, azimuth-sorted
  rays); `--flavor wmo` follows the FM301-2022 text. For Level II the root
  carries the xradar NEXRAD attributes from the metadata messages.
- JSON object keys are sorted alphabetically.

### render

One field of one sweep to a PNG image (RGBA, transparent background, the
radar at the centre).

```sh
recast-radar render FILE -o dbz.png                      # first sweep with reflectivity
recast-radar render FILE --sweep 3 --field ZDR -o zdr.png
recast-radar render FILE --field velocity --dealias -o vel.png
recast-radar render FILE --all-sweeps --field DBZH -o ./frames/
recast-radar render FILE --palette BR.pal --size 2048 -o dbz.png
```

- `--field` takes a field name (case-insensitive) or a quantity: `reflectivity`,
  `velocity`, `width`, `zdr`, `rhohv`, `phidp`, `kdp`. Without it: the sweep's
  reflectivity, else its first field.
- `--dealias` unfolds radial velocity with the region-based dealiaser first
  and draws the result (`VRADDH`).
- `--palette` draws with a GR2Analyst `.pal` color table.
- `--size` (64 to 8192, default 1024) and `--range-fraction` (default 94: the
  far edge of the last gate is 94% of the way from the centre to the edge).
- Level III raster products (composite reflectivity and the like) are drawn
  cell by cell, north up.
- Every other sweep is drawn as a plan view (azimuth around the centre,
  range outwards). For an RHI sweep that is not a cross section; a note on
  standard error says so.

### validate

Decodes files and checks what they decode to. For each file: `OK`, `WARN`
with warnings, or `FAIL` with errors, then a summary line.

```console
$ recast-radar validate KTLX20240315_000217.trim.V06 KLIX20050829_130035.trim.V06 config.cfg
OK    KTLX20240315_000217.trim.V06  (NEXRAD Level II: KTLX 2024-03-15T00:02:17Z 2 sweep(s))
WARN  KLIX20050829_130035.trim.V06  (NEXRAD Level II: KLIX 2005-08-29T13:00:12Z 2 sweep(s))
      warning: has no location
      warning: Level II metadata: metadata record: invalid message at offset 279692: message type 5: ...
FAIL  config.cfg
      error: does not decode: not a recognised radar file (...)
3 file(s): 1 ok, 1 with warnings, 1 failed
```

Errors: the file does not decode; the volume breaks a data-model invariant
(`Volume::seal`: ray arrays of different lengths, fields longer than the
range, duplicate names, ...); the FM301 view cannot be built; a location off
the Earth; a sweep without rays; rays without an azimuth; a non-positive gate
spacing; a field whose row count differs from the rays.

Warnings: no radar name or location, location 0 N 0 E, an implausible time,
incomplete sweeps, azimuths outside 0 to 360, elevations missing or outside
-10 to 180 degrees, rays without a time, explicit ranges that do not
increase, sweeps without fields, no valid gate anywhere, values that decode
to non-finite numbers or outside a plausible range for the quantity, and
Level II metadata messages that did not decode.

Directories are expanded (`-r` descends into subdirectories); in a
directory, `dir.list`, `*.cfg`, `*.txt`, `*.json`, `*.md`, `*.html`, `*.png`,
`*.pal` and hidden files are skipped. `--json` prints the report as JSON;
`--strict` makes warnings fail too. The exit status is 1 when a file fails.

### bench

Decode timing. Each file is read into memory once, decoded `--warmup` times
(default 1), then timed over `--iterations` decodes (default 5).

```console
$ recast-radar bench --threads 1 -n 5 KTLX20240315_000217.trim.V06 bejab.pvol.hdf
file                                           size    min ms    median      mean      MB/s  rays
KTLX20240315_000217.trim.V06                 741 kB     31.97     35.77     36.67      20.7  960
bejab.pvol.hdf                               640 kB      5.22      5.85      6.20     109.5  3960
5 iteration(s) per file, 1 decoder thread(s); MB/s is input bytes over the median
```

`--threads 1` limits the decoders' worker pool to one thread; pinning the
process to one core is up to the operating system (`taskset -c 2`,
`start /affinity 4`). `--metadata` also decodes the NEXRAD metadata messages,
as `info` does. `--json` prints the timings as JSON. The cross-library
benchmark with output checksums is the separate `recast-radar-bench` harness.

### fetch

Downloads. Every file is written through a temporary file and renamed, so an
interrupted download never leaves a truncated file under the final name, and
a file already present with the listed size is not downloaded again
(international frames have no listed size: a non-empty file already present
under the part's name is kept, see below). `-o DIR` sets the output
directory (default: the current one); `--list` lists instead of
downloading.

```sh
recast-radar fetch level2 KTLX                                   # newest volume
recast-radar fetch level2 KTLX -n 3                              # three newest
recast-radar fetch level2 KTLX --date 2024-03-15 --list          # the day's volumes
recast-radar fetch level2 KTLX --date 2024-03-15 --time 00:02 -n 2   # the 2 nearest to 00:02Z
recast-radar fetch chunks KMKX                                   # newest real-time volume, assembled
recast-radar fetch chunks KMKX --keep-chunks                     # ... plus each chunk
recast-radar fetch level3 TLX N0B --date 2024-03-15 --time 00:02 # KTLX works too
recast-radar fetch intl                                          # the international providers
recast-radar fetch intl smhi --list-sites                        # a provider's sites (--online: ask its catalog)
recast-radar fetch intl smhi hemse -n 3                          # newest frames of a site
recast-radar fetch intl ord bejab --date 2026-09-24 --list       # a day of archived frames
recast-radar fetch intl ord bejab --date 2026-09-24 --time 21:39:06   # the archived frame nearest 21:39:06Z
recast-radar fetch polling https://mesonet-nexrad.agron.iastate.edu/level2/raw/   # a polling server's sites
recast-radar fetch polling https://mesonet-nexrad.agron.iastate.edu/level2/raw/ KTLX --list
recast-radar fetch polling https://mesonet-nexrad.agron.iastate.edu/level2/raw/ KTLX -n 1
recast-radar fetch sites                                         # NEXRAD sites (embedded table)
```

| Source | Where |
|---|---|
| `level2` | `unidata-nexrad-level2` bucket on AWS; files keep their archive names (`KTLX20240315_000217_V06`) |
| `chunks` | `unidata-nexrad-level2-chunks`; the chunks of the newest volume are downloaded (4 at a time) and concatenated into one Archive II file, `<SITE><YYYYMMDD>_<HHMMSS>_V06`, or `..._V06.part` while the volume is still in progress |
| `level3` | `unidata-nexrad-level3`; objects `<SSS>_<PPP>_<YYYY>_<MM>_<DD>_<hh>_<mm>_<ss>` |
| `intl` | The `recast-radar-data` international providers (SMHI, NCI Australia, DMI, GeoSphere, FMI, SHMU, DWD, CHMI, ARPA Piemonte and Lombardia, JMA, KAIA, ANM Romania, EUMETNET ORD). A frame of several parts (DWD sweeps, SHMU and ANM moments, ORD moment files) is downloaded part by part; decode it with `convert --merge` or `info --merge`. A JMA tar holds every station: read the one you asked for with `--station` |
| `polling` | A GR2Analyst polling server: `<URL>/config.cfg` for the sites, `<URL>/<SITE>/dir.list` for the volumes; `-n` newest volumes into `<DIR>/<SITE>/` |

`fetch intl` with `--date` reads a provider's archive: the providers listed
with `archive yes` (SMHI, whose dated catalog keeps about the last day; NCI
Australia, about three days behind real time; EUMETNET ORD). `--date` alone
takes the first `-n` frames of that UTC day (`--list` lists all of them);
with `--time`, the `-n` frames nearest that time, searched an hour either
side (ten minutes per frame for larger `-n`). A frame's time is read from
its identity (`bejab_20260924T2140_...`), and each listed frame shows it.

Each part is saved under its upstream file name when that name carries the
scan time, and otherwise under the frame identity (KAIA's file URLs end in a
file number), so a name always stands for one upstream file and a second
run keeps what the first downloaded (`have ...`).

Be a polite client of other people's servers. `fetch polling` waits
`--interval-ms` (default 1000, at least 250) between requests and downloads
one volume unless told otherwise. Listed files that are not volumes (one
still being written, such as `.tmp` or `.part`, or a state or text file) are
listed by `--list` and never downloaded. A polling server rewrites the newest
volume while it grows, so `dir.list` can list a smaller size than the file
has; the size is then noted, not treated as an error.

### convert and publish

```sh
recast-radar convert FILE --to level2 -o out.ar2v              # also cfradial1, odim, fm301
recast-radar convert FILE --to level2 --level2-compression none --gzip -o out.ar2v.gz
recast-radar convert a.h5 b.h5 c.h5 --merge --to level2 -o merged.ar2v   # parts of one scan
recast-radar convert tar --station ITOK --to level2 -o ITOK.ar2v
recast-radar convert FILE --to level2 --chunks -o chunks/     # real-time chunks: chunks/SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-S|I|E
recast-radar publish FILE... --dir ./polling [--site XXXX] [--keep 30] [--merge]
```

`convert` decodes the input (merging several with `--merge`, choosing one
volume of a multi-volume input with `--volume N`) and hands it to the writer
of the `--to` format. `--site` replaces the radar identifier (Level II: the
4-character ICAO of the volume header and Message 31). `--gzip` wraps the
output in gzip. The output is written through a temporary file; an existing
file is replaced only with `--force`. `--chunks` writes NEXRAD real-time
chunks instead of one file, laid out as the `unidata-nexrad-level2-chunks`
bucket (`-o DIR` then names a directory): the `S` chunk holds the volume
header and metadata record, each `I` chunk and the final `E` chunk one bzip2
LDM record of radials, and concatenated they are the Archive II file. It
takes neither `--gzip` nor `--level2-compression none`, and needs a Level II
writer with chunk output (`VolumeWriter::write_chunks`).

`publish` writes each input volume as a Level II file into a polling
directory (see below), keeps the newest `--keep` in each site's `dir.list`,
and adds the site to `config.cfg` and `grlevel2.cfg` unless
`--no-site-config`. A file is named `SITE_YYYYMMDDHHMMSS.ar2v` after the
site and the volume time, and every file and `dir.list` is written to a
temporary name and renamed into place, so a polling client never reads a
partial file.

The writers are `recast-radar-io-nexrad`'s Level II writer (Archive II
following ICD 2620010 and ICD 2620002, [docs/level2/writer.md](../level2/writer.md)),
`recast-radar-io-cfradial`'s CfRadial 1 and FM301 writers and
`recast-radar-io-odim`'s ODIM_H5 writer ([docs/design/writers.md](../design/writers.md)).
A writer that cannot represent the volume (a Level II volume without a site
position, an ODIM_H5 RHI, a Level III level table in CfRadial) refuses with a
message and exit status 1, and leaves no output. The Python package's
`write`, `convert` and `publish` ([Python guide](python.md#writers-and-the-polling-publisher))
use the same registry (`recast_radar_cli::backend::Backends::builtin`).

### serve

A small HTTP/1.1 file server for a directory, such as a polling directory:

```sh
recast-radar serve ./polling                        # http://127.0.0.1:8080/
recast-radar serve ./polling --bind 0.0.0.0:8080    # reachable from other machines
recast-radar serve ./polling --bind 127.0.0.1:0     # any free port; the banner names it
```

GET and HEAD, one request per connection, directory listings in the style of
nginx's autoindex (directories first, then files, sorted without regard to
case), `Content-Length`, `Last-Modified`, `Cache-Control: no-cache`. Paths
stay inside the directory: `..`, backslashes, drive prefixes and names
starting with a dot are refused, and a path that leads outside through a link
is refused. `--max-connections` (default 32) bounds the connections served
at once. A request head (request line and headers, at most 16 KiB) must
arrive within 20 seconds, with no more than 10 seconds between reads; a
slower client is answered `408 Request Timeout` and its connection closed,
so trickling clients cannot hold the connection slots. It is meant for a
local network or a machine behind a reverse proxy; it has no TLS and no
authentication. Each request is logged to standard error.

## Polling directories

GR2Analyst polls a directory over HTTP:

```none
polling/
  config.cfg          Site: KTLX        one line per site
  grlevel2.cfg        (the same lines)
  KTLX/
    dir.list          <size> KTLX_20240315000217.ar2v      "<size> <file name>", oldest first, CRLF
    KTLX_20240315000217.ar2v
    ...
```

The Iowa Environmental Mesonet (<https://mesonet-nexrad.agron.iastate.edu/level2/raw/>)
serves the NEXRAD network and several research radars this way, and North
Dakota's State Water Commission (<https://level2.swc.nd.gov/raw/>) its own
radars. `fetch polling` reads such a server, `publish` writes such a
directory, and `serve` serves it.

## Exit status

| Status | Meaning |
|---|---|
| 0 | Success |
| 1 | The command failed: a file did not decode, `validate` found errors (or warnings with `--strict`), a download failed |
| 2 | Invalid arguments (including an out-of-range `--sweep`, a missing field, an existing output without `--force`) |
| 3 | Not available in this build: a writer, the publisher, or `fetch` without the `net` feature |

## For developers

The command is a library as well: `recast_radar_cli::run(cli, &backends, out)`
runs a parsed command, and `recast_radar_cli::main_with_args` is the whole
program. The writers and the publisher plug into
`recast_radar_cli::backend`:

- `VolumeWriter`: `format()`, and `write(input, options, out)` encoding
  `input.volume` (and, for byte-identical Level II round trips,
  `input.metadata`, the source's NEXRAD metadata messages) into `out`.
- `PollingPublisher`: `publish(input, request)` writing one Level II volume
  into the polling directory `request.root`, updating `dir.list` (keeping
  `request.keep`), and the site configuration when
  `request.update_site_config`.
- `Backends::builtin()` registers the writers of `recast_radar_cli::writers`
  (`Level2Writer`, `CfRadial1Writer`, `OdimWriter`, `Fm301Writer`) and the
  `PollingDirectoryPublisher`; `Backends::stubs()` holds the stubs
  `UnavailableWriter` and `UnavailablePublisher` (exit status 3), for a
  build that wants only some formats (`.with_writer(Box::new(...))`).

`recast_radar_cli::open::open_path` is the format detection and decoding
every command uses, and `recast_radar_cli::serve::serve` the HTTP server.
