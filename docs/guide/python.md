# The `recast_radar` Python package

`recast_radar` opens weather radar files with the Rust decoders of
recast-radar-tools and hands them to Python the way the Python radar
ecosystem expects: an `xarray.DataTree` laid out like xradar's (WMO FM301,
CfRadial 2), or a `pyart.core.Radar`. It also exposes the writers and the
data fetchers. The package is the crate
[`crates/recast-radar-py`](../../crates/recast-radar-py): a PyO3 extension
module (`recast_radar._native`) and the Python code around it
(`crates/recast-radar-py/python/recast_radar`).

- [Install](#install)
- [Quick start](#quick-start)
- [Input formats](#input-formats)
- [`open`: DataTrees](#open-datatrees)
- [`Volume`: volumes kept in Rust](#volume-volumes-kept-in-rust)
- [`to_pyart`: Py-ART radars](#to_pyart-py-art-radars)
- [Writers and the polling publisher](#writers-and-the-polling-publisher)
- [`recast_radar.fetch`: downloads](#recast_radarfetch-downloads)
- [The xarray backend](#the-xarray-backend)
- [Errors](#errors)
- [Speed](#speed)
- [Tests](#tests)
- [How it works](#how-it-works)

## Install

The package is not on PyPI. Build a wheel from a checkout with
[maturin](https://www.maturin.rs/) (1.9.4 or later) and a Rust toolchain
(1.94 or later):

```sh
cd crates/recast-radar-py
maturin build --release              # wheel in target/wheels (or --out DIR)
pip install <the wheel>
maturin develop --release            # or build and install into the active virtualenv
```

The [Python wheels workflow](../../.github/workflows/python-wheels.yml) is set
to build abi3 wheels for Linux (x86_64, manylinux2014), Windows (x64) and
macOS (Apple silicon and Intel) on pushes to `main` and on manual dispatch,
and to keep them as workflow artifacts of the private repository. One wheel
per platform serves CPython 3.10 and later. It has not run on GitHub yet:
until a push or a dispatch runs it, there are no such artifacts, and the
macOS wheels have not been built anywhere (the README's CI section lists what
was checked locally). The wheels are never uploaded anywhere: there is no
publish step, and the package metadata carries the `Private :: Do Not
Upload` classifier, which PyPI refuses.

Requirements: `numpy` and `xarray` 2024.10 or later (the release with
`DataTree`). `to_pyart` needs `arm_pyart`. The `net` Cargo feature (on by
default) builds `recast_radar.fetch` in; it links the HTTPS client (reqwest
with rustls, whose `ring` compiles C). `maturin build --no-default-features`
leaves it out.

## Quick start

```python
import recast_radar

tree = recast_radar.open("KTLX20240315_000217_V06")      # xarray.DataTree
tree["sweep_0"]["DBZH"]                                  # dBZ, NaN where no echo
tree["sweep_0"]["DBZH"].sel(azimuth=slice(90, 100)).load()

radar = recast_radar.to_pyart("KTLX20240315_000217_V06") # pyart.core.Radar

volume = recast_radar.read("KTLX20240315_000217_V06")    # kept in Rust
volume.vcp, volume.nsweeps, volume.field_names
volume.to_datatree(first_dim="time")

from recast_radar import fetch
paths = fetch.level2("KTLX", count=1, dest="data")       # newest volume from AWS
```

## Input formats

`open`, `read`, `read_all` and `to_pyart` take a path (`str` or
`os.PathLike`) or the file's bytes (`bytes`, `bytearray`, `memoryview`). The
format is detected from the contents, as the `recast-radar` command does
([CLI guide, Input formats](cli.md#input-formats)): NEXRAD Level II (every
Archive II variant, whatever the file name says), NEXRAD and
TDWR Level III products with a data array (a one-sweep volume), ODIM_H5
polar volumes, CfRadial 1 (classic netCDF or netCDF-4) and CfRadial 2 /
FM301 (netCDF-4), DORADE sweep files and mobile-radar ZIP archives (paths
only), and JMA radar GRIB2 tars. gzip and single-file ZIP wrappers are
removed first.

Not read: ODIM Cartesian products, and Level III products without a data
array (graphic, tabular and text products raise `DecodeError`; `dump` reads
them).

## `open`: DataTrees

```python
recast_radar.open(source, *, first_dim="auto", decode=True, decode_times=True,
                  mask_range_folded=True, range_folded_variable=False,
                  packed_attrs="encoding", flavor="xradar", passthrough="flavor",
                  station=None, volume=0) -> xarray.DataTree
```

`recast_radar.open_datatree` is the same function.

The tree follows FM301 in xradar 0.12's spelling, so it replaces
`xradar.io.open_*_datatree()` output:

- `/`: global attributes, `latitude`, `longitude` and `altitude`
  (coordinates), `sweep_group_name`, `sweep_fixed_angle`, `time_coverage_*`,
  `volume_number`, `platform_type`, `instrument_type`.
- `/radar_parameters`, `/radar_calibration`, `/georeferencing_correction`
  when the source has them.
- `/sweep_<n>`: coordinates `azimuth`, `elevation`, `time`, `range` (and
  `frequency`), per-ray variables (`nyquist_velocity`, `prt`, ...), the sweep
  variables (`sweep_mode`, `sweep_fixed_angle`, ...) and one `(ray, range)`
  variable per field (`DBZH`, `VRADH`, `ZDR`, ...), with the source's
  attributes (for NEXRAD, xradar's VCP and RDA status attributes on the root
  and each sweep).

| Option | Default | Effect |
|---|---|---|
| `first_dim` | `"auto"` | `"auto"`: rays sorted by azimuth (by elevation for RHI sweeps) under dimension `azimuth`/`elevation`, xradar's default. `"time"`: rays in acquisition order under dimension `time` |
| `decode` | `True` | CF decoding of packed fields (`scale_factor`, `add_offset`, `_FillValue`), as `xr.decode_cf(mask_and_scale=True)`. `False` keeps packed integers and all attributes |
| `decode_times` | `True` | `time` as `datetime64[ns]`; `False` keeps seconds since `time_reference` |
| `mask_range_folded` | `True` | with `decode`, range-folded gates (NEXRAD raw 1) are NaN, as Py-ART masks them |
| `range_folded_variable` | `False` | adds `<FIELD>_flags` (`uint8`, 1 where range folded, `flag_meanings = "range_folded"`) beside each field with a range-folded code, linked through the field's `ancillary_variables` |
| `packed_attrs` | `"encoding"` | after decoding, `_Undetect`, `valid_range`, `valid_min`, `valid_max` and `flag_*` of scaled fields (numbers in packed units) move into `.encoding`; `"attrs"` leaves them in `.attrs`, as xradar does |
| `flavor` | `"xradar"` | `"wmo"`: the FM301-2022 names (`fixed_angle`, UDUNITS units); needs `first_dim="time"` |
| `passthrough` | `"flavor"` | `"all"` adds the source attributes xradar drops (CfRadial global attributes, ODIM `how` attributes, DORADE VOLD text) |
| `station` | `None` | JMA tars: the station (JMA id or number); default the first |
| `volume` | `0` | inputs with several volumes (mobile-radar ZIP archives): which one |

Values are lazy: the tree is built at once, and field values are decoded
when read (`.values`, `.load()`, plotting). Fields are always packed in the
tree's encoding the way the source stores them: `uint8`/`uint16` NEXRAD
codes with the ICD scale and offset, ODIM `gain`/`offset`, CfRadial
`scale_factor`/`add_offset`, or floats with their `_FillValue`.

Where this tree differs from xradar 0.12, on purpose (design note
[`docs/design/fm301-model.md`](../design/fm301-model.md), sections 6, 7, 8
and 14; the Rust conformance test lists every item):

- NEXRAD fields carry `_FillValue = 0`, `_Undetect`, `valid_range` and
  `flag_values`/`flag_meanings` for range folding, so decoded fields are NaN
  where there is no echo; xradar's decoded NEXRAD fields read -33 dBZ there.
- Each NEXRAD sweep has one range at its finest gate spacing: 1 km
  reflectivity on a 250 m range is repeated four times (Py-ART's
  `linear_interp=False` values); dual-pol moments shorter than the sweep are
  padded with `_FillValue`. xradar takes the coarser moment's range in mixed
  sweeps and misplaces the finer moments.
- ODIM `TH` has units `dBZ` (xradar: "unitless").
- FM301 items xradar leaves out are present: `sweep_group_name` and
  `sweep_fixed_angle` for every source, `polarization_mode`, per-ray
  `nyquist_velocity` and `unambiguous_range`, `radar_parameters`
  variables, `frequency`.
- To save a tree with xarray (`tree.to_netcdf(path)`), open it with
  `flavor="wmo", first_dim="time"`. The xradar flavor carries xradar's
  Python `bool` attributes (`avset_enabled`, `mpda_vcp`, ...), which netCDF
  cannot hold, so its `to_netcdf` fails, as it does for xradar's own trees.
  xarray drops `.encoding` keys it does not know, so a decoded tree written
  this way loses the attributes `packed_attrs="encoding"` moved there;
  `decode=False` or `packed_attrs="attrs"` keeps them.
- A view warning (`RuntimeWarning`, "ray times are not increasing") is raised
  with `first_dim="time"` when the source has no per-ray times (ODIM files
  with equal start and end times, JMA, some CfRadial and DORADE files), as
  xradar warns for such ODIM files.

## `Volume`: volumes kept in Rust

```python
volume = recast_radar.read(source, *, station=None, volume=0)
volumes = recast_radar.read_all(source, *, station=None, all_stations=False)
merged = recast_radar.merge([part1, part2, ...])
cycles = recast_radar.split_scan_cycles(volume)
```

`read` decodes one volume and keeps it in Rust; `read_all` returns every
volume of an input (each member of a mobile-radar archive, every station of a
JMA tar with `all_stations=True`). `merge` joins the parts of one scan
(per-quantity ODIM files, DWD sweep files): the first part is the base, later
parts add fields to sweeps at the same angle, ray geometry and collection
time (first rays at most 60 s apart), and add the other sweeps as sweeps of
their own. `split_scan_cycles` returns one volume per scan cycle, its sweeps
in the order they were collected: a Level II file holds one volume scan,
and the Level II writer refuses a volume of more than one (a cut collected
again, as in JMA's 10-minute tars, a sweep that begins minutes after the
others ended, or a Level II radial that begins a volume again).

Properties: `source_format` (`"nexrad_level2"`, `"odim_h5"`, `"cfradial1"`,
...), `format_name`, `label`, `instrument_name`, `time_reference`,
`time_coverage`, `latitude`, `longitude`, `altitude`, `scan_name`, `vcp`,
`nsweeps` (also `len(volume)`), `sweeps` (one dictionary per sweep:
`fixed_angle`, `sweep_mode`, `nrays`, `ngates`, `range_start`,
`gate_spacing`, `fields`), `field_names`, `has_level2_metadata`, and
`format_metadata`: for NEXRAD Level II `{"nexrad": {...}}` with the metadata
messages (RDA status, performance data, VCP, adaptation data, clutter maps,
PRF data), each sweep's Message 31 constant blocks and the decode problems;
`None` for other formats.

Methods: `to_datatree(**options)` (the options of `open`), `to_pyart(...)`,
`write(path, format, ...)`, `to_bytes(format, ...)`, `publish(root, ...)`.
Each conversion works on a copy of the field buffers, so a volume can be
converted and written any number of times; `open` is the zero-copy path.

## `dump`: every decoded value

```python
report = recast_radar.dump(source, *, data=False, rays=False, station=None, all_stations=False)
```

A dict with what `recast-radar dump --json` prints (layout in
`docs/guide/cli.md`, `dump`): the volume, sweep and field metadata, the NEXRAD
Level II metadata messages (`report["volumes"][i]["format_metadata"]`), and
for Level III the message header, product description, text header, every
symbology and graphic packet and the tabular pages (`report["level3"]`).
It reads what `open` refuses because it holds no radar volume: Level III
graphic and tabular products (storm tracking, mesocyclone, hail, TVS: storm
ids, positions and forecast tracks are the `StormIds`, `ScitPast` and
`ScitForecast` symbol packets) and Level II real-time chunks after the first
(`report["records"]`). `data=True` adds every gate value and the bins of
Level III data packets; `rays=True` every ray's values.

```python
nst = recast_radar.dump("KDVN_SDUS33_NSTDVN_202008101804")
for layer in nst["level3"]["symbology"]["layers"]:
    for packet in layer:
        for storm in packet["packet"].get("StormIds", []):
            print(storm["id"], storm["i"] / 4, storm["j"] / 4)   # km east, north
```

## `to_pyart`: Py-ART radars

```python
radar = recast_radar.to_pyart(source, *, field_names="config", station=None, volume=0)
```

`source` is a path, bytes, a `Volume` or a DataTree (from `open`, or from
xradar). The radar has Py-ART's layout: rays of all sweeps in file order, one
range for the volume (from the smallest first gate centre, at the smallest
spacing, to the farthest gate), and masked `float32` fields (CfRadial fields
keep the type netCDF decodes them to, as `pyart.io.read_cfradial` does).
Fields are masked at `_FillValue`, `_Undetect`, range-folded gates and NaN.
A sweep with coarser gates is repeated onto the finer volume range: each
volume gate takes the sweep gate whose extent holds its centre (the lower
one on a tie), which is what `pyart.io.read_nexrad_archive(...,
linear_interp=False)` gives for NEXRAD's 4:1 and 2:1 ratios, including its
2:1 rule that leaves the volume gate past the last coarse gate's centre
masked. LROSE Radx's CfRadial 1 of FMI Anjalankoski (500 m and 250 m
sweeps, both from 0 m) is in the test suite for this.

`field_names`: `"config"` (Py-ART's defaults, which its algorithms look for:
`reflectivity`, `velocity`, `differential_reflectivity`, ...), `"reader"`
(the names Py-ART's reader for the format gives, for example
`reflectivity_horizontal` for ODIM and the file's names for CfRadial),
`"fm301"` (unchanged) or a dict. `recast_radar.pyart_field_name(name, mode,
source_format)` gives one mapping.

On the files of the test suite the fields are bit-identical to Py-ART's own
readers, masks included (`pyart.io.read_nexrad_archive(linear_interp=False)`
for Level II, `pyart.aux_io.read_odim_h5`, `pyart.io.read_cfradial`). Where
Py-ART's readers report other metadata, this package keeps its own:
per-ray ODIM times and elevations (Py-ART: whole seconds and the nominal
elevation), NaN location for Message 1 volumes (Py-ART: 0), the first ray's
elevation as a Message 1 sweep's fixed angle (Py-ART: the VCP's), sweep
numbers from 0 (Py-ART keeps a CfRadial file's), and espdg-style ODIM files
that write `where/rstart` in metres (Py-ART reads kilometres). NEXRAD times
count from the volume header time as Py-ART's do, even when the header is
later than the first ray.

Level III products match `pyart.io.read_nexrad_level3` field for field, masks
included, with these differences: gate and ray centres here, gate starts and
start angles there; the sweep time here, the volume start time for every ray
there; NaN (or the product description's elevation) for the fixed angle of
products without one, where Py-ART writes 0; DHR 1.0 dBZ lower than Py-ART's
(MetPy's `Level3File.map_data` agrees with this package); DPR's raw 0 kept as
0 mm/h (Py-ART masks it); and N0S named `SRM` (Py-ART: `velocity`).

## Writers and the polling publisher

```python
recast_radar.writers()               # {"level2": True, "cfradial1": True, "odim": True, "fm301": True}
volume.write("KTLX.ar2v", "level2", compression="bzip2", gzip=False, site=None, overwrite=False)
data = volume.to_bytes("cfradial1")
recast_radar.convert("in.h5", "out.ar2v", "level2")
volume.publish("polling", site="KTLX", keep=30)        # GR2Analyst polling directory
recast_radar.publisher_available()
recast_radar.write_chunks(volume, "chunks")          # NEXRAD real-time chunks: chunks/SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-S|I|E
```

Formats: `"level2"` (NEXRAD Archive II, bzip2 LDM records or `compression=
"none"`), `"cfradial1"`, `"odim"` (ODIM_H5 PVOL), `"fm301"` (CfRadial 2 in
netCDF-4). `gzip=True` wraps any format; `site` replaces the radar id (the
4-character ICAO for Level II). Files are written through a temporary file
and renamed into place.

Every write function (`write`, `to_bytes`, `convert`, `publish`,
`write_chunks`) also takes these keywords:

| Keyword | Effect |
|---|---|
| `sweeps=[0, 2, 5]` | keep only these sweeps (0-based), in this order; Level II holds at most 32 |
| `sweeps_in_time_order=True` | put the sweeps in the order their first rays were collected |
| `position=(lat, lon, height_m)` | the site position to write (a Message 1 volume has none) |
| `quantization="precise"` | Level II value coding: `"precise"` (never coarser than the source), `"compatible"` (NEXRAD's word sizes, which xradar 0.12 reads), `"standard"` (NOAA's codings where they hold every value) |
| `nyquist_velocity=`, `unambiguous_range=` | the radar's own values (m/s, m) for Level II radials whose source has none (JMA) |
| `drop_negative_range_gates=True` | leave out gates centred before the radar (Message 1 Doppler gates from -375 m) |
| `strict=True` | refuse, writing nothing, a write that would leave out a field or sweep |

What a writer leaves out (a field Level II has no moment for, a second
reflectivity field) and its notes (a coding coarser than the source, radials
reordered, a missing Nyquist velocity) come as `recast_radar.WriteWarning`s;
`publish` also returns them under `"left_out"` and `"notes"`. For example,
JMA's 10-minute tars hold two 5-minute cycles, which the Level II writer
refuses as one volume; each cycle, from its top sweep down, or one cycle's
sweeps:

```python
itok = recast_radar.read("Z__C_RJTD_20260924210000_RDR_JMAGPV_N5_grib2.RS47937.tar", station="ITOK")
for number, cycle in enumerate(recast_radar.split_scan_cycles(itok), start=1):
    cycle.write(f"ITOK_{number}.ar2v", "level2")
itok.write("ITOK.ar2v", "level2",
           sweeps=[0, 2, 3, 6, 7, 10, 11, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32],
           sweeps_in_time_order=True)
klix = recast_radar.read("KLIX20050829_130035.V06")            # Message 1: no position
ref = recast_radar.read("KLIX20210829_180425_V06")
klix.write("KLIX.ar2v", "level2", position=(ref.latitude, ref.longitude, ref.altitude),
           drop_negative_range_gates=True)
```

`write_chunks(volume, dest=None, *, site=None, overwrite=False)` writes a
volume as NEXRAD Level II real-time chunks, laid out as the
`unidata-nexrad-level2-chunks` bucket: the `S` chunk holds the volume header
and metadata record, each `I` chunk and the final `E` chunk one bzip2 LDM
record of radials, and concatenated they are one Archive II file. Without
`dest` it returns `[{"key", "kind", "number", "data"}]`; with `dest` it
writes each chunk to `dest/key` and returns the paths. It is the Python side
of `recast-radar convert --chunks` and needs a Level II writer that
implements `VolumeWriter::write_chunks`.

These go through the same backend registry as the `recast-radar convert`
and `publish` commands (`recast_radar_cli::backend::Backends::builtin`),
which registers every writer and the polling-directory publisher. A writer
that cannot represent a volume raises `UnrepresentableError`, whose message
names the keyword that helps where there is one, and leaves no file;
`UnavailableError` (a `NotImplementedError`) is for a build without a
writer or the publisher. `pytests/test_write.py` reads written Level II and
CfRadial 1 files with Py-ART and compares them with Py-ART's and h5py's
reading of the source.

## `recast_radar.fetch`: downloads

```python
from recast_radar import fetch

fetch.level2_files("KTLX", "2024-03-15")          # AWS listing: key, name, size, time, url
fetch.level2("KTLX", datetime(2024, 3, 15, 0, 5), dest="data", count=2)
data, info = fetch.realtime("KTLX")                # newest real-time volume, assembled
fetch.level3_files("TLX", "N0B")                   # Level III, newest first by default
fetch.level3("TLX", "N0B", dest="data")
fetch.intl_providers()                             # dmi, fmi, smhi, dwd, ord, jma, ...
site = fetch.intl_sites("dmi")[0]["id"]            # "06036" (Sindal)
volumes = fetch.intl("dmi", site)                  # decoded (split frames merged)
fetch.intl("dmi", site, dest="data")               # or saved
fetch.intl_frames("ord", "bejab", date="2026-09-24")                 # a day of archived frames
fetch.intl("ord", "bejab", dest="data", when=datetime(2026, 9, 24, 21, 39, 6))   # the archived frame nearest a time
iem = "https://mesonet-nexrad.agron.iastate.edu/level2/raw/"
fetch.polling_sites(iem)                           # the server's config.cfg
fetch.polling_files("KTLX", iem)                   # dir.list
fetch.polling("KTLX", dest="data", server=iem)     # newest volume
fetch.nexrad_sites()
fetch.download(url) / fetch.download(url, dest)
```

Times are UTC (naive datetimes are taken as UTC). Files are written through a
temporary file and renamed into place; a file already there with the listed
size is not downloaded again. Downloads release the GIL, and `realtime`
fetches up to four chunks at once.

International frames: `intl_frames` and `intl` take the newest `count` frames
by default. Providers with an archive (`intl_providers()` entries with
`"archive": True`: SMHI, whose dated catalog keeps about the last day; NCI
Australia, about three days behind real time; EUMETNET ORD) also take
`date=` (every frame of that UTC day; `intl` keeps the first `count`) or
`when=` (the `count` frames nearest that time, searched an hour either side,
ten minutes per frame for larger counts); other providers raise
`ValueError` before any request. Each frame is `{"identity", "time",
"merge", "urls", "names"}`, `time` being the scan time read from the
identity. A part is saved under its upstream file name when that name
carries the scan time, else under the frame identity, so a name always
stands for one upstream file and a non-empty file already there under it is
kept, as `recast-radar fetch intl` does.

GR2Analyst polling servers are often run by volunteers. `polling_sites`,
`polling_files` and `polling` take the server's base URL and share one pace
per server: each request waits until `interval` seconds (default 1, at least
0.25) have passed since the process's last request to that server, so calls
one after another stay polite. `polling` fetches the newest `count` volumes
(default 1, at most 100), skipping listed files that are not volumes (one
still being written, or a state or text file), like `recast-radar fetch
polling`.

## The xarray backend

The package registers the xarray engine `recast_radar`:

```python
xr.open_datatree("KTLX20240315_000217_V06", engine="recast_radar")
xr.open_dataset("KTLX20240315_000217_V06", engine="recast_radar", group="sweep_0")
```

`open_dataset` returns one group with the root's coordinates (`latitude`,
`longitude`, `altitude`); `mask_and_scale`, `decode_times`, `drop_variables`
and the options of `open` are passed through. The engine is never chosen
automatically.

## Errors

| Exception | Base | Raised when |
|---|---|---|
| `recast_radar.DecodeError` | `ValueError` | the input is not a radar file the package reads, or is damaged; a Level III product has no data array; a Level II chunk holds no volume |
| `recast_radar.UnavailableError` | `NotImplementedError` | this build has no writer for the format, no publisher, or no network support |
| `recast_radar.UnrepresentableError` | `ValueError` | the output format cannot hold something in the volume |
| `recast_radar.FetchError` | `OSError` | a download or listing failed |
| `FileNotFoundError`, `OSError` | | reading or writing a file failed |

## Speed

`crates/recast-radar-py/bench/read_bench.py` times every reader decoding a
whole file, every field value included, in one process (median of the runs
after one warm-up run). Run it from the repository root with recast_radar,
xradar and arm_pyart importable:

```sh
python crates/recast-radar-py/bench/read_bench.py [--threads N] [--repeat 5] [--json] [ID ...]
```

Results on 2026-09-24: Windows 11, AMD Zen 5 with 32 hardware threads,
Python 3.13.7, numpy 2.5.3, xarray 2026.7.0, xradar 0.12.0, arm_pyart 2.2.5,
recast_radar 0.1.0 (wheel built by `maturin build --release`, the workspace
release profile with fat LTO), median of 5 runs after one warm-up run. The
machine was shared with other agents' builds, so a number can move by some
tens of percent between runs: treat differences under about a third as
noise. (In the first table the 16.1 MB KDVN volume decodes faster than the
10.8 MB KTLX one: that is the noise.)

Rust decoders with their default threads:

| file | MB | recast_radar | recast_radar packed | recast_radar to_pyart | xradar | xradar packed | pyart |
|---|---:|---:|---:|---:|---:|---:|---:|
| `l2-ktlx-20240315-000217` | 10.8 | 1404 ms | 259 ms | 1458 ms | 8496 ms | 8098 ms | 6283 ms |
| `l2-kdvn-20200810-180401` | 16.1 | 870 ms | 222 ms | 1016 ms | 8224 ms | 7645 ms | 4312 ms |
| `l2-klix-20050829-130035` | 4.8 | 276 ms | 170 ms | 485 ms | 1401 ms | 1583 ms | 1338 ms |
| `odim-dkrom-20260820-1130-pvol` | 1.7 | 185 ms | 68 ms | 226 ms | 713 ms | 520 ms | 483 ms |
| `odim-iesha-20260305-0115-pvol` | 1.7 | 82 ms | 44 ms | 111 ms | 374 ms | 315 ms | 104 ms |
| `odim-bejab-20190606-0000-pvol` | 0.6 | 55 ms | 37 ms | 40 ms | 199 ms | 184 ms | 61 ms |
| `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | 1.7 | 32 ms | 16 ms | 22 ms | 67 ms | 44 ms | 33 ms |
| `cfrad1-dow8-20211011-223602-rhi-trim3-classic` | 0.9 | 12 ms | 11 ms | 8 ms | 49 ms | 43 ms | 32 ms |

Rust decoders on one thread (`--threads 1`; xradar and Py-ART always run on
one):

| file | MB | recast_radar | recast_radar packed | recast_radar to_pyart | xradar | xradar packed | pyart |
|---|---:|---:|---:|---:|---:|---:|---:|
| `l2-ktlx-20240315-000217` | 10.8 | 1977 ms | 891 ms | 1585 ms | 6990 ms | 6247 ms | 3109 ms |
| `l2-kdvn-20200810-180401` | 16.1 | 1294 ms | 838 ms | 1707 ms | 9434 ms | 7469 ms | 3970 ms |
| `l2-klix-20050829-130035` | 4.8 | 236 ms | 154 ms | 311 ms | 1287 ms | 1300 ms | 1233 ms |
| `odim-dkrom-20260820-1130-pvol` | 1.7 | 165 ms | 51 ms | 165 ms | 603 ms | 460 ms | 364 ms |
| `odim-iesha-20260305-0115-pvol` | 1.7 | 64 ms | 41 ms | 67 ms | 275 ms | 264 ms | 105 ms |
| `odim-bejab-20190606-0000-pvol` | 0.6 | 44 ms | 30 ms | 43 ms | 217 ms | 188 ms | 55 ms |
| `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | 1.7 | 21 ms | 16 ms | 20 ms | 73 ms | 59 ms | 33 ms |
| `cfrad1-dow8-20211011-223602-rhi-trim3-classic` | 0.9 | 14 ms | 10 ms | 10 ms | 50 ms | 36 ms | 30 ms |

In these tables, on one thread, `open(...).load()` is 3.5 to 7.3 times faster
than xradar's reader on every file, and `to_pyart` is 1.3 to 4 times faster
than Py-ART's own reader. With the default threads, one file comes out the
other way: `to_pyart` takes 111 ms on `odim-iesha` against Py-ART's 104 ms,
within the noise (67 ms against 105 ms on one thread).

The small files do not hold those ratios from run to run. A later
single-thread run of the four smallest files (same machine, a thin-LTO
wheel rather than the release profile, arm_pyart 2.3.0, which the reference
virtualenv now has) measured `open(...).load()` at 90, 53, 26 and 17 ms
against xradar's 318, 229, 59 and 42 ms (`odim-iesha`, `odim-bejab`,
`cfrad1-irene`, `cfrad1-dow8`: 2.3 to 4.3 times), and `to_pyart` at 83, 56,
22 and 14 ms against Py-ART's 125, 52, 36 and 27 ms (0.9 to 1.9 times). On
files this small a few tens of milliseconds decide the ratio, and that is
the size of the machine's noise.

`recast_radar packed` is the zero-copy path (the decoders' buffers, rays
reordered where `first_dim="auto"` asks for it). The decoded columns are
mostly NumPy and xarray turning every gate into float64 (the type xradar's
NEXRAD encoding asks for, design note 12.3); `to_pyart` builds Py-ART's
masked float32 arrays in NumPy. Both are candidates for a Rust fast path.

## Tests

`crates/recast-radar-py/pytests` runs on real files only, resolved through
the testdata manifests (committed fixtures, or the SHA-256-checked download
cache of `recast-radar-testdata`):

```sh
cd crates/recast-radar-py
maturin develop --release
pip install pytest "xradar==0.12.0" "arm_pyart==2.2.5" h5netcdf netCDF4 "metpy==1.7.1"
pip install tomli                         # Python 3.10 only (xradar 0.12 and arm_pyart 2.2.5 need 3.11)
python -m pytest                          # all; -m "not slow" skips the full-volume cases
RECAST_RADAR_NETWORK_TESTS=1 python -m pytest -m network   # a few live requests
```

- `test_xradar.py`: groups, dimensions, coordinates, packed field values
  (both `first_dim` choices), decoded values and field attributes against
  xradar 0.12 on Level II, ODIM and CfRadial files, including the four
  full-size FM301 conformance volumes (`slow`). Five trimmed Level II
  fixtures keep only part of each sweep. xradar drops sweeps like that as
  incomplete, so those cases are skipped; the full volumes they were cut
  from are compared instead (`slow`: KTLX 2024 and KDVN among the
  conformance volumes, plus KILX 2026, KTLX 2013 and PGUA 2023). One
  documented xradar difference is allowed there: in PGUA's sweep 9 xradar
  0.12 leaves out radials 120 and 240, the last radial of two LDM records
  in an elevation whose records also carry RDA status messages between
  radials; Py-ART and MetPy read all 360, as this package does, and the
  other 358 are compared.
- `test_pyart.py`: `to_pyart` against Py-ART 2.2.5's readers, field values
  and masks bit for bit, plus the sweep table, angles, times and location,
  for Level II, ODIM, CfRadial (with the mixed 500 m / 250 m FMI
  Anjalankoski volume) and twelve Level III products (`read_nexrad_level3`, with the
  differences listed above); DHR levels against MetPy.
- `test_api.py`: inputs, options, zero-copy buffers, lazy mapped fields, every
  input format, `Volume`, `merge`, the xarray engine, `dump` (the storm-id
  packets of a Level III NST against MetPy's) and `format_metadata`.
- `test_write.py`: every writer, the real-time chunk writer and the
  publisher are linked, and their output reads back (the polling directory
  in the GRLevelX layout).
- `test_fetch.py`: argument checks, the polling pace and the volume-name
  filter offline; with `RECAST_RADAR_NETWORK_TESTS=1`, one AWS listing and
  download, two small requests to the Iowa Environmental Mesonet's polling
  server, and SMHI archive lookups by date and by time with one volume
  downloaded (and kept on the second call).

`RECAST_RADAR_TESTDATA` and `RECAST_RADAR_TESTDATA_OFFLINE` work as for the
Rust tests. The suite reads the manifests with `tomllib`, or with the `tomli`
backport on Python 3.10, where xradar 0.12 and arm_pyart 2.2.5 do not
install (they need 3.11): `test_xradar.py` and `test_pyart.py` skip there and
the rest runs against xarray 2025.6, the last release for 3.10. The pins (xradar 0.12.0, arm_pyart 2.2.5, MetPy
1.7.1) are the versions the FM301 goldens and the documented differences
(`testdata/conformance/fm301/index.json`, design note section 14) were made
with. The suite also passes with arm_pyart 2.3.0.

## How it works

The Rust side (`crates/recast-radar-py/src`) decodes through the same code as
the `recast-radar` command (`recast_radar_cli::open`), with the GIL
released, then builds the FM301 view of the volume
(`recast_radar_core::fm301::volume_view`, with the NEXRAD metadata
attributes for Level II) and detaches it with `VolumeView::layout`. It then
consumes the volume: each field's buffer (`Vec<u8>`, `Vec<u16>`, ...) goes to
`numpy::PyArray::from_vec`, which takes ownership without copying, and is
reshaped to `[rays, native gates]`. From then on NumPy owns the decoder's
memory (design note [12.2](../design/fm301-model.md#122-binding-strategy-ownership-moves-to-numpy)).

The Python side (`_tree.py`) builds the DataTree. A field whose buffer already
is the FM301 variable (rays in storage order, gates equal to the sweep's
range) is the moved array itself. Otherwise the variable is a lazy xarray
backend array (`FieldArray`) over the moved buffer that applies the ray
permutation, gate padding and repetition when values are read, as xradar's
backend arrays do. Under `first_dim="auto"` NEXRAD, CfRadial and JMA fields
are lazy permuted views; under `"time"`, ODIM fields with per-ray times are.
CF decoding is xarray's own (`xr.decode_cf`), so it is lazy as well.

`to_pyart` reads the same tree with `first_dim="time"`, packed, and puts the
rays back in storage order before laying the volume out for Py-ART
(`_pyart.py`).

The crate keeps the workspace's `forbid(unsafe_code)`: PyO3 0.29's macros
expand to code the lint accepts, and no `unsafe` is written by hand.
`numpy::PyArray::borrow_from_array`, the one way to lend Rust-owned memory to
NumPy, is `unsafe` and is not used.

## Processing and native rendering

See `docs/guide/frontend-processing.md` for the shared Python/CLI product API, rendering,
and the CLI bundled in the Python wheel.
