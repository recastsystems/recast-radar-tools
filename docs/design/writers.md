# Writers: CfRadial 1, CfRadial 2 / FM301, ODIM_H5 and HDF5

Status: implemented on the `hdf5-netcdf` stream (gap G5; G14 for the writers).
This note records what each writer produces, what it refuses, how every
output is checked, and the reader limitations the checks ran into.

| Writer | Entry point | Output |
|---|---|---|
| HDF5 | `recast_radar_hdf5::write::Writer` | HDF5 1.8 file format: groups, attributes, contiguous / compact / chunked (shuffle, deflate) datasets, references, variable-length strings |
| netCDF-4 | `recast_radar_hdf5::write::netcdf4::NcWriter` | the netCDF-4 data model stored as netCDF-C stores it (dimension scales, `DIMENSION_LIST`, `_Netcdf4Dimid`, `_NCProperties`) |
| CfRadial 1.4 | `recast_radar_io_cfradial::write_cfradial1` | classic netCDF, 64-bit offset (CDF-2) |
| CfRadial 2 / FM301 | `recast_radar_io_cfradial::write_cfradial2` | netCDF-4 (the FM301 view, WMO FM301-2022 flavor) |
| ODIM_H5 PVOL | `recast_radar_io_odim::write_odim_h5_volume` | HDF5; `ODIM_H5/V2_3`, or the source's own version for a volume read from ODIM_H5 |

Every writer takes a `Volume` from any decoder, returns the file as bytes,
and refuses what its format cannot hold with a typed error
(`CfWriteError::Unrepresentable` / `TooLarge`,
`OdimWriteError::Unrepresentable` / `TooLarge`). The writers contain no unsafe code and
no `unwrap`/`expect`/`panic!`; the `writers` fuzz target exercises them. All
output is deterministic (no timestamps are written).

## HDF5 and netCDF-4

`recast-radar-hdf5` writes the HDF5 1.8 format that every HDF5 library since
1.8.0 reads: a version 2 superblock, version 2 object headers with lookup3
checksums, new-style groups with compact links (creation order tracked),
compact attributes (version 3 messages), version 3 layouts (contiguous,
compact, chunked with a version 1 B-tree index), a version 2 filter pipeline
(shuffle, deflate), and global heap collections for variable-length strings
and reference lists. Limits the writer enforces (`WriteError::TooLarge`): an
attribute message over 64 KiB, a chunk over 4 GiB, more than 65,535 links or
attributes on one object. Variable-length data in chunked datasets is
`Unsupported`.

The netCDF-4 layer (`write::netcdf4`) stores dimensions as HDF5 dimension
scales exactly as netCDF-C does: the coordinate variable's dataset when there
is one, else an empty dataset named `This is a netCDF dimension but not a
netCDF variable.`; `REFERENCE_LIST` and `DIMENSION_LIST` object references;
`_Netcdf4Dimid`; `char` attributes as scalar fixed-length strings, `string`
data as variable-length strings; `_FillValue` doubling as the dataset fill
value. Dimensions are fixed-size.

Both layers are checked by reading their output with this crate's own reader
(unit tests in `write/tests.rs`) and with independent readers: the examples
`h5_write_demo` and `nc4_write_demo` write one file each that uses every
structure the writers emit, and `tools/hdf5_writer_check.py hdf5 <file>` /
`netcdf4 <file>` checks every attribute, dataset, layout, filter, reference,
dimension scale and value of them with h5py (the HDF Group's C library),
netCDF-C (through netCDF4-python) and xarray's `netcdf4` and `h5netcdf`
engines. The volume writers built on them are checked by LROSE Radx (the
HDF5 C++ API, for ODIM) and the readers in `tools/writer_check.py`
(Verification, below).

## CfRadial 1 (classic netCDF)

Layout (CfRadial 1.4): dimensions `time` (every ray of every sweep),
`range`, `n_points` when rays hold different numbers of gates, `sweep`,
string lengths, and `frequency`, `r_calib` and the dimensions of kept
variables when needed; global attributes; the root, sweep, calibration,
coordinate and per-ray instrument variables; `ray_n_gates`,
`ray_start_index`, `ray_start_range` and `ray_gate_spacing` when the layout
needs them; the fields `(time, range)` or `(n_points)`; the variables and
attributes a volume keeps verbatim.

- **Sweep order.** Sweeps are stored in the order of their first ray time:
  xradar's CfRadial 1 reader sorts every ray of a file by time before cutting
  sweeps out by index, so any other order misreads. A moved sweep keeps its
  original index as `sweep_number`. Rays keep their order within a sweep,
  except in `n_points` storage (below).
- **Gate geometry** (`Cfradial1Options::range_layout`, `RangeLayout`).
  CfRadial 1 states one gate geometry per ray. Real volumes change it from
  sweep to sweep: NEXRAD Message 1 has 1 km surveillance sweeps beside
  250 m Doppler sweeps (one grid), and FMI's volumes interleave 500 m and
  250 m sweeps; read as LROSE Radx reads them (every first gate centred at
  0 m), the 250 m gates straddle the 500 m grid's edges (no grid holds
  both). Layouts:

  | Layout | File | xradar | Py-ART | LROSE Radx | this crate |
  |---|---|---|---|---|---|
  | `Common` | one `range(range)` at the finest spacing from the earliest gate edge; coarser sweeps repeated, later-starting ones padded; `Unrepresentable` when the sweeps are on no common grid | reads | reads | reads | reads |
  | `PerSweep` | `range(sweep, range)` (CfRadial 1.4 section 4.4) with `meters_to_center_of_first_gate` / `meters_between_gates` per sweep, and `ray_start_range` / `ray_gate_spacing` per ray | reads | refuses (`ValueError`) | refuses (`Range has incorrect dimensions`) | reads |
  | `PerRay` | `range(range)` of the longest sweep; each ray's geometry in `ray_start_range` / `ray_gate_spacing`; `Unrepresentable` when a float cannot state a sweep's first centre (above -9999 m, the fill) or spacing (positive) | misplaces the other sweeps' gates | misplaces them | reads | reads |
  | `Auto` (default) | `Common` when the sweeps are on one grid, else `PerSweep` | | | | |

  No layout of a volume whose geometry varies is read by every reader. LROSE
  Radx itself writes `range(time, range)` for such a volume, which neither
  Py-ART nor xradar reads (the committed Radx fixtures below). `Auto` falls
  back to `PerSweep` because a reader that cannot follow it refuses the
  file, where `PerRay` is read silently wrong by Py-ART and xradar.
  The price of `Common`, which `Auto` prefers because every reader reads
  it: a coarser sweep comes back on the finer grid, each value repeated
  (NEXRAD Message 1's 1 km surveillance sweep reads back as 250 m gates);
  `PerSweep` keeps its geometry exactly (test
  `cfradial1_writer_repeats_or_keeps_coarser_sweeps`). Within one sweep, a
  field on a coarser part of the range (Message 1 reflectivity in a Doppler
  sweep) is always repeated: one geometry per ray is all CfRadial 1 has.
  Explicit gate centres must be the same in every sweep.
- **Gates per ray.** When the sweeps' rows differ in length, the fields are
  stored over `n_points` (`n_gates_vary = "true"`, section 2.3.1): each ray
  holds its own sweep's gates, located by `ray_start_index` and
  `ray_n_gates`, instead of every ray padded to the longest sweep. xradar,
  Py-ART and Radx read it (checked). Each sweep's rays are then stored in
  time order (`write::time_order`): xradar lays `n_points` rows out by ray
  time and misplaces the rays of a sweep stored otherwise (below). A volume
  whose sweeps run out of time order (ODIM rows stored from north)
  therefore reads back with its rays in time order.
  Otherwise the fields are `(time, range)`.
- **Reading it back.** The CfRadial 1 reader reads all of these: `n_points`
  storage (each sweep's rows as long as its longest ray, shorter rays
  padded with the field's fill), `range(sweep, range)`, Radx's
  `range(time, range)` (a sweep whose rays disagree is
  `CfRadialError::PerRayGeometry`) and `ray_start_range` /
  `ray_gate_spacing` where they state a sweep geometry other than
  `range(range)`. Tested on two files LROSE Radx wrote from three sweeps of
  FMI's Anjalankoski PVOL (`io-cfradial/tests/cfradial_ragged_real.rs`, goldens by
  `tools/cfradial_ragged_golden.py` from netCDF4-python, and for the file
  with a one-dimensional `range` checked there against Py-ART and xradar).
- **Limits.** The `range` dimension has at least two gates (readers need
  two centres to state the spacing): a one-gate volume gets a second of
  fill, and `n_points` storage keeps each ray to its own gate. Rows over
  16,384 gates, or fields beyond the 1 GiB decode budget, are `TooLarge`
  before anything is allocated: that is what this crate's readers accept,
  so every file written reads back.
- **Size.** The classic format has no compression, and `u8` codes widen to
  `short` by default (see Fields), so CfRadial 1 is by far the largest
  output: KILX 2026-04-18 writes 287 MB (415 MB before `n_points` storage;
  FM301 26 MB, ODIM 27 MB), KTLX 2013-05-20 133 MB (67 MB with
  `unsigned_attribute`; FM301 8.5 MB). A compressed netCDF-4 flavour of
  CfRadial 1 is not written (Not done).
- **Fields.** A field keeps its storage type and raw codes when every sweep
  has the same coding, with `scale_factor`/`add_offset` in the width the
  source wrote them, `_FillValue`, `_Undetect`, `valid_range`, and
  `flag_values`/`flag_meanings` carrying the range-folded code. The classic
  format has no unsigned types: `u8` and `u16` are widened to `short` and
  `int` (codes unchanged) by default, because LROSE Radx, the CfRadial
  reference reader, ignores `_Unsigned` and reads the codes 128 off;
  `Cfradial1Options::unsigned_attribute` stores them as `byte`/`short` with
  `_Unsigned = "true"` for readers that apply it (netCDF4-python, xarray,
  xradar, Py-ART; checked). A field whose coding differs between sweeps
  (ODIM gains per dataset) is written as `float` physical values; its
  undetect and range-folded gates become missing.
- **What CF readers decode.** Undetect and range-folded gates keep their
  raw codes, stated by `_FillValue`, `_Undetect`, `valid_range` and
  `flag_values`/`flag_meanings`; readers apply different subsets of these.
  Every CF reader masks the `_FillValue` code, which for NEXRAD fields is
  also the undetect code (raw 0). netCDF4-python (with its default
  masking) and Py-ART also mask codes outside `valid_range`, so the NEXRAD
  range-folded code (raw 1) reads as missing. xarray applies only
  `_FillValue` and the packing, so it, and xradar through it, decode the
  range-folded code as the value of code 1 (-32.5 dBZ for reflectivity,
  -64 m/s for 0.5 m/s velocity). No CF reader applies `_Undetect`: an ODIM
  `undetect` code, which is not the `_FillValue`, decodes as the value of
  that code in all of them. A user of these readers masks the gates with
  the attributes (`flag_values` for range folding, `_Undetect` for
  undetect). Checked on the files `write_all` writes for
  `l2-ktlx-20240315-000217-trim` and `odim-bejab-20190606-0000-pvol`
  (2026-09-25; xarray `open_datatree` on the FM301 file and
  `open_dataset` on the CfRadial 1 file, netCDF4-python, Py-ART
  `read_cfradial`): xarray decoded 342 range-folded reflectivity gates
  of the KTLX volume as numbers, netCDF4-python and Py-ART none; all four
  decoded the 1,137,710 undetect gates of the BEJAB reflectivity as
  numbers. The same holds for FM301 output, which states the codes the same
  way.
- **Metadata.** A CfRadial 1 volume is written back with its own global and
  variable attributes and kept variables, so a CfRadial 1 file read and
  written again reads back as the same volume (test
  `cfradial1_files_read_back_unchanged`). Other formats' volume attributes
  become global attributes; their sweep attributes become variables: text
  `(sweep, string_length)`, numbers `double (sweep)`, arrays with one value
  per ray `(time)` (ODIM's per-ray `how` arrays), other arrays
  `(sweep, <name>_len)`.

## CfRadial 2 / FM301 (netCDF-4)

`write_cfradial2` serialises the FM301 view (`fm301::volume_view`, flavor
`Wmo2022`, rays in time order) through `NcWriter`: every group, dimension,
variable and attribute the view states. Fields keep their storage type and raw
codes with the packing attributes in the packed type; they are chunked by
rays (up to 4 MiB a chunk), byte-shuffled and deflated (level 4 by default,
`Cfradial2Options::deflate`). By default the view keeps everything the
volume holds (`Passthrough::All`); `with_passthrough(false)` writes FM301
names only. Attributes netCDF-4 reserves (`_NCProperties`, `CLASS`,
`DIMENSION_LIST`, ...) are not copied from a source. An integer field with
absent rays or padding gates and no fill code gets a code none of its gates
uses as `_FillValue` (as in CfRadial 1). A sweep of rays without
gates is written with an empty `range` (the CfRadial 2 reader reads it
back); a sweep over 16,384 gates per ray or fields beyond the 1 GiB decode
budget is `TooLarge`, before the view materialises anything.

## ODIM_H5 PVOL

`write_odim_h5_volume` writes `/what`, `/where`, `/how` and one `datasetN`
per sweep with `what`/`where`/`how` and one `dataM` per field (plane chunked
and deflated, `CLASS = IMAGE`), a `legend` for flag fields and `qualityK`
groups for quality fields. Module documentation of `write` has the details;
the choices that matter:

- **Version.** Volumes of other formats are written as `ODIM_H5/V2_3`, whose
  `where/rstart` is in km (xradar, wradlib and Py-ART agree on it). v2.4
  states `rstart` in metres, which Py-ART reads as km and Radx as a km gate
  centre. A volume read from ODIM_H5 keeps its version and units.
- **Plane numbers.** A quantity has the same `dataM` in every dataset,
  numbered in order of first appearance in the volume, so a dataset without
  it skips that number (FMI's own volumes do the same). Py-ART's
  `read_odim_h5` takes the plane names of `dataset1` and reads the same
  `dataM` in every dataset: numbering each dataset's planes from 1, as the
  writer first did, made it read one quantity's values under another's name
  (below). `OdimWriteOptions::every_quantity` gives every dataset a plane
  for every quantity of the volume, all `nodata` where the sweep lacks it,
  for readers that need the same planes everywhere: Py-ART then reads every
  quantity, and LROSE Radx, which opens `data1` to `dataN` in every dataset,
  reads the file at all. It is off by default because such a plane reads
  back as a field the sweep did not have, every gate missing.
- **Codes.** Planes keep storage type and raw codes; NEXRAD
  `(raw - offset) / scale` becomes `gain = 1/scale`, `offset = -offset/scale`.
  ODIM has one `nodata` and one `undetect` per plane: a plane whose fill code
  is its undetect code (NEXRAD raw 0) gets the range-folded code (raw 1) as
  `nodata`, else a code no gate uses; range-folded gates, absent rows and
  padding are written as `nodata`.
- **Rays.** Rows are written in azimuth order starting at north, except for a
  volume read from ODIM_H5 (already in ODIM order). For other formats
  `startazA`/`stopazA` are each centre azimuth minus and plus the largest
  power of two not above half the ray spacing, so any reader's mean is the
  centre exactly, with `elangles` and `startazT`/`stopazT` (NaN for a ray
  without a time: the other rays keep their sub-second times, where leaving
  the arrays out would have readers spread every ray over whole-second start
  and end times). A non-finite azimuth is written as it is. A volume read
  from ODIM_H5 gets none derived: the reader keeps the arrays its ray
  coordinates came from (`Sweep::other`), and they are written back.
- **Every attribute in its group.** The reader stores a kept attribute under
  its bare name only when that name places it back in its group (an ODIM_H5
  `what`/`where` table attribute of that group, or a `how` attribute outside
  the tables; `io-odim/src/tables.rs`), else as `<group>.<name>` (FMI's
  plane `what/type`, RMI's quality `what/NAME`). The writer uses the same
  tables, so every attribute returns to its group. What the reader lifts into
  typed slots is written from the slot: a dataset `how` constant shared by
  every dataset goes to the root `how`, `rpm` becomes `antspeed`,
  `beamwidth` becomes `beamwH`/`beamwV`, at the model's float32 precision; an
  enumerated plane (h5py `bool`) becomes its integer codes with a `legend`.
  A root `how` group is always written (Radx recognises ODIM by it).
- **Refused:** a volume without sweeps; RHI sweeps (a PVOL holds PPIs);
  sweeps without rays, gates or fields; explicit gate centres
  (`Unrepresentable`); a sweep over 16,384 gates per ray or planes beyond
  the 1 GiB decode budget (`TooLarge`, checked before anything is
  allocated).

## Verification

In the workspace (every run of `cargo test`):

- `recast-radar-io/tests/write_real.rs`: each writer on 19 real volumes of
  every source format (Level II message 31 and 1, among them KTLX 1999,
  whose ARCHIVE2 header has a NUL ICAO and so no name; LROSE Radx's
  CfRadial 1 of an FMI volume with 500 m and 250 m sweeps, CfRadial 1 classic and
  netCDF-4, CfRadial 2 from Radx and xradar, DORADE, ODIM from five
  producers) plus two RHIs; the file read back through the router matches
  the source gate by gate (values, missing, undetect; ray angles to 1e-4
  degrees, times to 1 microsecond); CfRadial 1 files written again read back
  as the same volume; both unsigned storages; every gate geometry layout
  (the FMI volume) and the Message 1 trade-off (KLIX 2005); rays in time order in
  every `n_points` output; ODIM plane numbering with and without
  `every_quantity`; `instrument_name` written for a volume without a name
  (LROSE Radx refuses a CfRadial 1 file without it); the typed refusals.
  The gate comparison (`tests/common/compare.rs`, `compare_volumes` with
  each writer's expectations) is shared with `examples/write_readback.rs`
  and the `writers` fuzz target: azimuths compare modulo 360 degrees, gate centres
  within float32 precision, grids that agree gate for gate pair by index,
  CfRadial fields by the netCDF name the writers give them (`/` and control
  characters become `_`), ODIM fields by name, else by position.
- `every_writer_output_reads_back_unchanged_when_written_again` (same
  file): for each writer and each of the 21 volumes, the volume read from the
  writer's file, written and read again, is the same volume (NaN equal to
  NaN, ray times to a microsecond). Getting there fixed the CfRadial 2
  reader for FM301-2022 files: `radar_parameters` names without the
  `radar_` prefix, the sweep groups' `frequency` coordinate (it had fallen
  back to a `wavelength` attribute and a rounded 5625 MHz), `calib_index`
  per ray and in `radar_calibration`, and every Table 301-11 monitoring
  variable; the view writes a kept `source_version` instead of the reader's
  own; the CfRadial 1 writer keeps a CfRadial 1 volume's NaN location
  variables. KTLX 1999 found one more: the ODIM writer built `what/source`
  from the identity for an ODIM volume whose file had none, so the reader's
  placeholder name `ODIM` came back as `NOD:ODIM`; an ODIM volume now keeps
  its own `what/source`, empty when it had none.
- `recast-radar-io-odim/tests/write_roundtrip.rs`: the eleven ODIM corpus
  files (RMI twice, met.no, AEMET, Met Eireann, DMI twice, SMHI, FMI, DWD, ARPA
  Lombardia) read, written and read again give the same volume, deflated and
  not.

Many files: `cargo run --release -p recast-radar-io --example write_readback
-- <files or directories>` writes each volume with every writer and reads
each file back through the router, comparing it with the source gate by
gate with the writer tests' `compare_volumes` (sweeps, rays, angles, times,
gate geometry, and every gate's value, missing, undetect and range-folded
state). The ODIM reader reads `where/rstart` to the millimetre: rounded to
the metre, a half-metre start moves every gate centre by 0.5 m.

Independent readers: `tools/writer_check.py` reads what
`cargo run --release -p recast-radar-io --example write_all -- <dir> <ids or files>`
writes and compares every gate with the source volume: the 21 volumes
above; the full KTLX 2013-05-20 Level II; the KILX 2026 and KMTX 2024
trims; ODIM files of DWD (Boostedt), ARPA Lombardia (Desio), Met Eireann
(Shannon), met.no, RMI (Wideumont, Jabbeke 2026 DBZH and VRAD) and the
superblock v3 container of DMI Romo; and the full S-Pol CfRadial 1 and 2;
`cfradial1.nc`,
`cfradial1-unsigned.nc`, `fm301.nc`, `odim.h5`, and `cfradial1-perray.nc`
(`RangeLayout::PerRay`) and `odim-every.h5` (`every_quantity`) where they
differ from the default output:

| File | Readers |
|---|---|
| `odim.h5` | h5py (planes decoded by hand), wradlib `read_opera_hdf5`, xradar `open_odim_datatree`, Py-ART `aux_io.read_odim_h5`, LROSE Radx |
| `odim-every.h5` | h5py, Py-ART, LROSE Radx |
| `cfradial1.nc`, `cfradial1-perray.nc` | netCDF4-python (laid out by hand from `n_points`, `range(sweep, range)` or the per-ray geometry), xradar `open_cfradial1_datatree`, Py-ART `read_cfradial`, LROSE Radx |
| `cfradial1-unsigned.nc` | netCDF4-python, xradar, Py-ART |
| `fm301.nc` | xarray `open_datatree` (netcdf4 and h5netcdf engines), xradar `open_cfradial2_datatree`, netCDF4-python, h5py |

LROSE: `RadxConvert -cf_classic -preserve_sweeps -const_ngates` in the
`nexbench` container reads each file with Radx's ODIM_H5 or CfRadial reader
and writes CfRadial 1, which netCDF4-python compares. Radx re-packs integer
fields with float32 `scale_factor`/`add_offset`; values are compared within
that precision. Each check runs with Python's cyclic garbage collector off
and a collection between checks: xarray's file managers close files from
finalizers that take its non-reentrant HDF5 lock, and a collection inside a
locked section deadlocked long runs in `open_datatree`.

A reader that fails on a written file whose source is a file it also reads
(`write_all` copies an ODIM_H5 or netCDF source next to the outputs) is run
on the source too: the same error there, file names aside, is that
reader's behaviour on the data, and the check reports a limitation ("fails
the same way on the source file"). For Radx, which converts CfRadial 1 and
2 alike to CfRadial 1, its reading of a CfRadial source is compared with its
reading of the written CfRadial 1 file.

Result on 2026-09-25 (the hdf5-netcdf stream's run, one process, on these
volumes and on 16 converted Level II volumes that are no longer in the
corpus): **no check failed**; every check that did not agree gate for gate
was one of the reader limitations below. The run has not been repeated on
the corpus as it now stands, nor after the ODIM writer began naming itself
in `how/software` (below), apart from a rerun of the ODIM checks on KTLX
2024, KLIX 2005 and KTLX 1999 Level II, X-SAPR CfRadial 1 and NOXP DORADE,
which gave the same results.

### Reader limitations found

Each is recognised by the check, from the file or by the same failure on
the source file, and reported with its reason; none is a property of the
written file.

| Reader | Limitation | Evidence |
|---|---|---|
| LROSE Radx | No FM301 reader. `Cf2RadxFile::isCfRadial2` requires `Conventions` containing `Cf/Radial` and a `version` containing `2`; FM301 files (ours and xradar's `to_cfradial2`) have neither, so Radx falls back to `LeoCf2RadxFile` (Leosphere lidar), which throws on a `radar_parameters` group without all five CfRadial 2.0 `radar_*` variables (its `readDoubleVar(name, val, false)` passes `false` as the missing value, leaving `required` true; FM301-2022 names drop the prefix), and overruns its stack on any numeric array sweep attribute (`att.getValues(&intValue)` into one `int`; gdb backtrace in `_readSweepsMetaAsInFile`). | 19 FM301 files; lrose-core `Cf2RadxFile_read.cc`, `LeoCf2RadxFile.cc` |
| LROSE Radx | ODIM: takes `where/rstart` as the first gate centre, always in km (ODIM_H5 defines the start of the first bin, in metres from v2.4); ignores `nodata`/`undetect` (it masks only the lowest code of an integer plane); gives every ray azimuth 0 and the 1970 epoch in a dataset without `how` (the source RMI file reads the same way); needs a root `how` group to recognise ODIM. | `OdimHdf5RadxFile.cc`; the check compares on the ODIM_H5 geometry and accepts a missing gate read as the decoded `nodata` code |
| LROSE Radx | CfRadial 1: takes the range from `meters_between_gates`; the X-SAPR test file (Py-ART's example data) states 60 m against a 960 m range variable, which the writer keeps verbatim for a CfRadial 1 source. Gates are compared by position there. | `cfrad1-xsapr-*` |
| Py-ART | `read_odim_h5` needs one `rscale` for every dataset (NEXRAD Message 1 volumes have 1 km and 250 m datasets). | KLIX 2005 |
| Py-ART | `read_odim_h5` needs one `rstart` for every dataset (`range start changes between sweeps`): a volume whose first 500 m and 250 m gates are both centred at 0 m has datasets that start at -0.25 and -0.125 km. | the FMI volume of the Radx fixtures |
| Py-ART | `read_odim_h5` takes the plane names of `dataset1` and reads the same `dataM` in every dataset. A quantity `dataset1` lacks is not read (a NEXRAD Doppler cut's `VRADH`, `WRADH`); where a dataset lacks one of `dataset1`'s, it reads all missing. With planes numbered per dataset it read one quantity's values as another's: before the numbering fix, Py-ART on our ODIM of the full KTLX 2013-05-20 volume against Py-ART on the Level II gave 888,139 ZDR, 910,084 RHOHV and 890,614 PHIDP gates wrong (found in review; the check then compared only the fields each source sweep has, on trimmed volumes). Now 0 gates differ for DBZH, ZDR, PHIDP and RHOHV over the whole volume, and with `every_quantity` for VRADH too (`WRADH` see below). The check reads every Py-ART field in every sweep and requires all-missing where the dataset has no such plane. | KTLX 2013, 2024 |
| LROSE Radx | ODIM: opens `data1` to `dataN` in every dataset and gives up on a gap (`Cannot open data grop`), as on FMI's own volumes; `every_quantity` output has no gaps. | split-cut NEXRAD volumes |
| Py-ART, LROSE Radx | CfRadial 1 `range(sweep, range)` (`RangeLayout::PerSweep`, the default for a volume on no common grid): Py-ART raises `ValueError`, Radx `Range has incorrect dimensions`. Radx reads `RangeLayout::PerRay`. | the FMI volume of the Radx fixtures |
| Py-ART, xradar | CfRadial 1 `ray_start_range` / `ray_gate_spacing` are ignored: every ray's gates are taken from `range(range)`, so `RangeLayout::PerRay` sweeps of another geometry are misplaced (the check reads the others and reports these). Neither reads Radx's `range(time, range)`. | the Radx fixtures |
| xradar | `open_cfradial1_datatree` sorts every ray by time before cutting sweeps out by index: sweeps whose ray times overlap (AEMET's two datasets) interleave. | ESPDG |
| xradar | `open_cfradial1_datatree` (0.12) lays `n_points` rows out by stacking and unstacking over (time, range), which puts them in time order while the sweep's coordinates keep file order: every ray of a sweep stored out of time order gets another ray's gates, silently (a sweep stored in azimuth order from north). The writer therefore stores each sweep's rays in time order in `n_points` storage (`write::time_order`); found by the check on the RMI Jabbeke PVOL. | BEJAB 2019 |
| Py-ART | `read_odim_h5` maps `WRAD` but not `WRADH` (ODIM_H5 2.2 and later), so it never reads spectrum width from our files or any current producer's. | every ODIM output with `WRADH` |
| xradar | `open_odim_datatree` reads a quality group's or a data plane's `legend` dataset as a variable on the ray dimension and fails; it fails the same way on the FMI and ARPA Lombardia source files. | FIANJ, ITDES (Desio `CLASS`) |
| LROSE Radx | CfRadial 1: reads the int16 raw code -32767 as missing, although it is neither `_FillValue` (-32768) nor outside a valid range (the variable has none); S-Pol's VR stores that code (-26.8 m/s) at 9,255 gates. Radx reads the source CfRadial 1 file the same way (its CfRadial 2 twin it cannot read: it falls back to the lidar reader). The check recognises the gates from the raw codes of the file Radx read and counts them in a note. | S-Pol CfRadial 1 and 2 |
| LROSE Radx | ODIM: reads Met Eireann's VRADH (uint8, gain 8/127, offset -8.063, NI 7.9785) differently from its gain and offset: raw 110 reads -1.1195 m/s instead of -1.1339, 111,860 gates off in the first sweep. It reads the source file identically (the check ran Radx on both); the cause is not established. | IESHA |
| Py-ART | `read_odim_h5` requires a root `Conventions` attribute (`KeyError`). ARPA Lombardia's Desio PVOL has none, and the ODIM writer keeps an ODIM volume's own `Conventions`, or its lack of one: Py-ART fails the same way on the source file. Adding the attribute would be a choice between faithful copying and a spec-complete file (left to the owner). | ITDES |

## Fuzzing

The `writers` fuzz target (`fuzz/src/lib.rs`) decodes its input through the
format router and writes the volume with all three volume writers; each file
a writer returns must read back through the router with the same rays per
sweep and, gate by gate, the same data as the source (the writer tests'
`compare_volumes`; beyond about a million gate checks per output it
compares the gates of every n-th ray, and ray angles and times of all), or
the harness panics. CfRadial 1 is written in the default, per-sweep and
per-ray layouts and ODIM with and without `every_quantity` (whose extra
all-missing planes are checked for rays only). Seeds: twelve real volumes
(`fuzz-tools seeds`), among them LROSE Radx's ragged CfRadial 1 of three
FMI sweeps of 500 m and 250 m gates. The first rounds (below) checked rays per sweep
only; the value comparison came after review.

Runs on 2026-09-25 (nexbench, libFuzzer fork mode, 15 minutes a target)
found three ways a writer wrote a file its own reader refused, all from
mutated Level II: a sweep of rays without gates (the CfRadial 2 reader
refused it; it now reads such a sweep), one-gate sweeps (the CfRadial 1
writer's one-gate `range`; `range` now always has two gates, the second
fill, and `n_points` storage keeps each ray to its own), and moment
geometry giving a sweep a 13-million-gate range (the FM301 and ODIM writers
materialised it; both now refuse more than 16,384 gates per ray and fields
beyond the decode budget with `TooLarge`, as the CfRadial 1 writer did).
The first two are committed regression inputs (`testdata/fuzz/manifest.toml`,
`recast-radar-io/tests/fuzz_regressions.rs`). On the fixed code a further
15-minute round found no crash: `writers` 68,878 inputs, `cfradial`
1,072,808, `io_router` 1,376,435 (and `hdf5` 258,286, `odim` 525,531 in
the round before). The other timeouts (one in `hdf5`, two in `writers`;
the third `writers` timeout was the 75-million-gate range) replay in under
half a second: they came from the load of the shared machine.

After review (2026-09-25; nexbench, fork mode, the `hdf5`, `odim` and
`cfradial` harnesses also reading without HDF5 metadata checksums, and
`writers` comparing every gate), three rounds of 20, 15 and 10 minutes over
`writers`, `hdf5`, `odim`, `cfradial` (and `io_router` in the first):

- `writers` saved 20 crash artifacts (18, 1 and 1 by round) from two
  defects: the CfRadial reader wrapping a -8.6e-41 degree azimuth to 360
  instead of 0 (four artifacts; fixed, with a unit test) and the ODIM
  `rstart` below (one). The rest were the comparison's own:
  azimuths that readers wrap into [0, 360), gate centres 4,000 to 8,700 km
  out that float32 rounds by a metre, and ranges with repeated or NaN
  centres; replaying the seeds found one more, ODIM planes that the
  per-volume numbering reorders (KVNX 2011). The comparison handles each
  (`tests/common/compare.rs`).
- ODIM `rstart`: a Level II radial whose first gate lies 27.6 km out was
  written in km (`ODIM_H5/V2_3`, as every other reader expects), and the
  ODIM reader took a pre-v2.4 `rstart` over 20 as metres (its heuristic for
  producers that wrote metres), so the sweep read back 27 km short. The
  writer now names itself in root `how/software` and `how/sw_version`, and
  the reader takes that writer's `rstart` as km at any distance (regression
  input `fuzz-writers-l2-odim-rstart-beyond-20-km`, 312 bytes).
- `hdf5` found a u64 overflow in `chunk_locations` (a v2 B-tree chunk
  record's scaled offset times the chunk dimension; regression input
  `fuzz-hdf5-chunk-offset-overflow`), and the stable checksum-free
  byte-flip test in `recast-radar-hdf5` found a panic on chunk dimensions
  of another rank than the dataspace; both are errors now.
- `odim`, `cfradial` and `io_router`: no crash. The three timeouts (one
  each in `io_router`, `hdf5` and `writers`) replay in 8 ms to 1.2 s; the
  `writers` one was the unchanged BEWID seed, whose full-gate comparison
  took 0.6 s, now bounded by the ray step.

The last round, on the fixed code (10 minutes a target): `writers` 5,601
inputs and one crash (the `rstart` one above, fixed after it), `hdf5`
61,437, `odim` 386,463, `cfradial` 207,921, none with a crash.

The review after that round drove the `writers` harness with a stable
mutator (panics caught, overflow checks on) over 9,500 inputs: 14 panics
from two defects, both fixed.

- An ODIM `startazA` of 1.6e185 (a corrupt FRALE scan): the reader cast
  the start/stop mean to single precision before wrapping it, which gave an
  infinite azimuth, and the CfRadial reader's wrap turned that into NaN.
  The ODIM and CfRadial readers now wrap an azimuth in double precision
  before narrowing it (a finite angle always names a direction), and they
  and the ODIM writer keep a non-finite azimuth as it is.
- One DORADE ray without a time (`fuzz-writers-dorade-ray-without-time`):
  the ODIM writer left `startazT`/`stopazT` out of the whole dataset, so
  every other ray read back at the whole-second start. The arrays now hold
  NaN for that ray only.

`fuzz-tools mutate` (fuzz/README.md) does the same on any platform, on one
core. Three runs over the thirteen `writers` seeds and more real files
(four DORADE cuts, the ODIM seeds of `bejab`, `deboo`, `itdes`, `norst`,
`seang` and the h5latest container, three CfRadial files, and six FRALE
scans from the feed corpus that are not in the manifest) found five more
defects:

- A Level II moment named U+0001 (`fuzz-writers-l2-empty-field-name`): the
  CfRadial writers name its variable `_`, a valid netCDF name, and the
  comparison looked the field up by its own name. The CfRadial comparisons
  now pair fields by the sanitized name.
- 1.4e-100 m gates in an ODIM sweep
  (`fuzz-writers-odim-gate-spacing-below-float`): the CfRadial 1 per-ray
  layout wrote a float `ray_gate_spacing` of 0, which readers take as fill,
  so the sweep read back on another sweep's gates. `RangeLayout::PerRay`
  refuses a sweep whose start or spacing a float cannot state
  (`Unrepresentable`); the other layouts write it.
- An integer field without a fill code and with an absent ray
  (`fuzz-writers-dorade-absent-rows-without-fill`): the FM301 view states
  `_FillValue` only from the coding, so the CfRadial 2 file had none and
  the absent ray read back as values. The CfRadial 2 writer now gives such
  a field a code none of its gates uses, as the CfRadial 1 writer did.
- A ray time of about -1.8e308 s
  (`fuzz-writers-cfradial1-ray-time-near-float-max`): the ODIM reader's
  `(startazT + stopazT) / 2` overflowed to -inf. Its means are
  `a / 2 + b / 2` now, equal wherever the sum is finite.
- A FRALE `stopazA` whose datatype the HDF5 reader cannot interpret: the
  ODIM reader keeps it as bytes, which the writer writes back as a `u8`
  array of eight values per ray. The source read its azimuths from
  `startazA` alone and the output fell back to storage-order centres. A
  `stopazA` of another length than the rays is now ignored (each ray stops
  where the next starts). No regression input: the scan is not in the
  manifest.

| Run (`fuzz-tools mutate writers`) | Inputs | Panics |
|---|---|---|
| rng seed 1, 27 files, before the fixes above | 12,000 | 2 (the field name, the gate spacing) |
| rng seed 2, 30 files, after those two | 30,000 | 4 of 3 kinds (the absent ray, the time, the `stopazA`) |
| rng seed 3, 30 files, after every fix | 30,000 | none |
| rng seeds 1 and 2 again, after every fix (the same inputs) | 42,000 | none |

The review's three reproducers and every input of these runs that
panicked replay clean (`fuzz-tools replay writers`).

## Not done

- A compressed (netCDF-4) flavour of CfRadial 1: the classic output is
  the largest of the three by far (Size, above).
- Unsigned 32/64-bit and signed 64-bit field storage: the model has none, so
  such planes are float64 already on read.
- The FM301 writer has no Radx-compatible dialect (see the limitation above);
  a `Cf/Radial` convention string and `radar_*` names would be a separate
  flavor, not FM301.
- ODIM: attributes of the root, dataset and plane groups themselves (none in
  the corpus but `Conventions`) are written back under `how`; an enumerated
  plane's HDF5 enum type is not kept (its meanings are, as a legend).
- ODIM: a volume read from an ODIM file without the mandatory root
  `Conventions` (ARPA Lombardia's Desio PVOL) is written without it, as its
  source was, and Py-ART cannot open either. Writing `ODIM_H5/V2_x` there
  would make the output more readable than its source; it is left to the
  owner (faithful copy or spec-complete file).
- ODIM: an attribute whose HDF5 datatype the reader cannot interpret (an
  opaque type, a corrupt datatype message) is kept as its bytes, and the
  writer writes it back as a `u8` array: readable as numbers where the
  source's was not. The model has no opaque attribute type.
- Special codes in CF output for readers other than Py-ART: see "What CF
  readers decode" above; whether FM301 output should state them another way
  is open.
