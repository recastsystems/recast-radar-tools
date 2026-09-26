# Level II writer

`recast_radar_io_nexrad::write` turns any FM301 [`Volume`] (decoded from NEXRAD Level II, ODIM_H5,
CfRadial, DORADE, JMA GRIB2 or built by hand) into a NEXRAD Archive II file: the `AR2V0006` volume
header, the metadata record, and one Message 31 radial per ray. The same records can be cut into
real-time chunks and published into a GR2Analyst polling directory that follows the GRLevelX
polling conventions.

This page is the reference for what the writer produces, how it maps a volume onto Message 31,
what it refuses, how it keeps to the ICDs, and how the output was checked. Table numbers refer to
ICD 2620002 (RDA/RPG) and ICD 2620010 (Archive II). The writer follows the ICDs and NWS practice
for every value it writes ("ICD compliance" below).

## Use

```rust
use recast_radar_io_nexrad::write::{self, WriteOptions};

let volume = recast_radar_io::read_supported_volume_bytes(&std::fs::read("202609242130_fianj_PVOL.h5")?)?;
let mut options = WriteOptions::default();      // LDM bzip2 records, 120 radials each
options.icao = Some("FANJ".to_owned());         // else derived from the instrument name
options.vcp = Some(11);                         // else volume.scan.vcp_pattern, else 0
let (bytes, summary) =
    write::write_volume_with_source(&volume, write::SourceMetadata::default(), &options)?;
// summary: what was written, left out, coded how, and in which ray order
```

The writer is the crate's `write` feature (off by default, so that decoding alone does not build
the bzip2 encoder, "Compression seam" below):
`recast-radar-io-nexrad = { ..., features = ["write"] }`. The crate's own writer tests need it too
(`cargo test -p recast-radar-io-nexrad --features write`); a workspace test run enables it through
`recast-radar-io`'s dev-dependency.

| Function | Output |
|---|---|
| `write_volume(volume, options)` | the file as bytes |
| `write_volume_to(volume, options, sink)` | the file streamed into an `io::Write` (below), and a `WriteSummary` |
| `write_volume_with_source(volume, source, options)` | bytes and `WriteSummary`, reusing Level II context from a Level II source (`SourceMetadata`) |
| `write_volume_with_source_to(volume, source, options, sink)` | the same streamed into an `io::Write` |
| `rewrite_level2(bytes, options)` | decode Level II bytes with their metadata, metadata record and data messages and write them again (below) |
| `data_messages(bytes)` | the non-radial messages of a Level II file's data records (mid-volume RDA status updates), with their places among the radials, for `SourceMetadata::data_messages` |
| `realtime::write_realtime_chunks[_with_source]` | the same records as `S`, `I` and `E` chunk files |
| `realtime::ChunkWriter` | the chunks while the volume's sweeps arrive: the start chunk with the first sweep, each sweep's records as it is pushed, the end chunk on `finish` |
| `polling::PollingDirectory::publish_volume` / `publish_bytes` | a file in a GR2Analyst polling directory, with `dir.list` and the site lists |

`WriteOptions` (start from `Default` and set fields; the struct is `#[non_exhaustive]`):

| Field | Default | Meaning |
|---|---|---|
| `compression` | `Bzip2LdmRecords` | `None`: records stored as they are; `Bzip2LdmRecords`: LDM records |
| `gzip` | `false` | wrap the whole file in gzip |
| `icao` | `None` | site identifier, 1 to 4 of `[A-Za-z0-9_]`, padded with `_` |
| `vcp` | `None` | VCP number for the VOL blocks and Messages 2 and 5 |
| `quantization` | `Precise` | value coding policy (below) |
| `radials_per_record` | 120 | radials per record, 1 to 65535 |
| `record_layout` | `Continuous` | `Continuous`: records run on across cuts; `WithinCuts`: each cut's last record holds the rest of its radials (below) |
| `volume_number` | `None` | the header's `.NNN`, 1 to 999; a Level II source's own, else 1 |
| `max_range_error_m` | `None` | largest gate position error accepted, metres at the last gate; `None` is half a gate |
| `drop_negative_range_gates` | `false` | leave out gates centred before the radar instead of refusing |
| `field_map` | empty | explicit field name to moment assignments, applied first |
| `nyquist_velocity_mps` | `None` | the radar's Nyquist velocity, written in the RAD block of every radial whose source has none (absent, not finite or not positive); `None` writes 0 there, which readers take as unknown. 0.01 to 327.67 m/s |
| `unambiguous_range_m` | `None` | the same for the radar's unambiguous range; 0.1 to 3276.7 km |

`WriteSummary` reports the site, volume time, sweeps, radials and records written, one
`MomentReport` per written field (moment, source field, word size, scale, offset, whether the
coding is exact, the largest value error, absent rays, dropped gates), the fields
left out and why, the sweeps left out, the source ray of every radial of each sweep not written
as its rays in storage order (`written_rays`), the largest range error, and notes (for example a
carried-over Message 5 that was replaced, a sweep left out, or the sweeps of a foreign volume
whose VEL radials carry no Nyquist velocity).

**Memory.** `write_volume_to` streams: everything a refusal depends on is decided first (a refused
volume writes nothing), then the volume header, the metadata record and the radial records go to
the sink a batch at a time, one record per rayon thread, compressed in parallel. Besides the
volume, the writer holds its plan (the codings and per-ray values) and one batch of records,
compressed and not, never the whole file; with gzip the wrapper compresses as the bytes go
through. A sink, compressor or allocation failure after the first byte leaves a partial file in
the sink. `write_volume` and `write_volume_with_source` stream into a buffer they return (the file
plus one batch). The real-time chunk functions compress every record at once, as they return all
chunks.

## Output

### File

| Part | Content |
|---|---|
| Volume header (24 bytes) | `AR2V0006` (a Level II source keeps its own `AR2V00nn`, 0002 or later), `.NNN`, the date (day 1 = 1970-01-01, 32 bits) and milliseconds past midnight of the volume, the 4-character site |
| Metadata record | 134 fixed 2432-byte frames (below) |
| Radial records | `radials_per_record` Message 31 radials each (`record_layout` below); a Level II source's mid-volume non-radial messages (`SourceMetadata::data_messages`) among them where the source had them |

With `Compression::None` the records follow each other as they are. With `Bzip2LdmRecords` each
record is an LDM record: a 4-byte big-endian control word holding the length of the bzip2 stream
that follows, negated for the last record, as NOAA's files have it. Real-time chunks and the
compressed Archive II file are the same bytes (the start chunk is the header and the metadata
record; each later chunk is one radial record).

**Record layout.** Every record of the corpus's NOAA LDM files (2011 to 2026) and every committed
real-time chunk holds 120 radials of one cut, but their cuts are all 360 or 720 radials, so they
cannot show whether NOAA's records end with a cut or run on. The two differ only for other cuts
(JMA's 512 radials, pre-2008 NEXRAD, many foreign radars):

- `Continuous` (the default): 120 radials a record whatever the cuts, only the last record
  holding fewer.
- `WithinCuts`: each cut starts a record and its last record holds the rest, so a real-time
  chunk holds radials of one cut and a cut's last chunk goes out as soon as the cut is pushed.

Neither makes xradar 0.12 read a compressed file whose cuts are not multiples of 120 radials:
it addresses message n of the data records as message (n - 134) mod 120 of LDM record
(n - 134) / 120 + 1, and finds a message inside a record by walking from the one it read last,
so it reads a sweep right only when the sweep starts a record and every record before it is full.
Under `Continuous` it lists every sweep but fails (IndexError) on the data of a sweep that starts
inside a record; under `WithinCuts` its header pass stops after the first cut. It reads
uncompressed files by file position, so `Compression::None` reads in full whatever the cuts
(checked on JMA's 512-radial cuts, KVWX 2008 and KLIX 2005).

`gzip = true` wraps the whole file in one gzip member. With `Compression::None` this is the
layout of NOAA's archive files from 1991 to 2015 (`KTLX20130520_201643_V06.gz` is gzip over
uncompressed records). With `Bzip2LdmRecords` it is gzip over LDM
records, which this decoder, Py-ART and MetPy read, but RSL, LROSE Radx and xradar 0.12 read only
unwrapped.

Every message has a 12-byte zero CTM header and a Table II message header: size in halfwords,
RDA channel 8 (Open RDA, single channel), type, a sequence number (1 to 3 for the metadata
messages, from 4 on for the radials, wrapping at 0x7FFF), the radial's or volume's date and time,
and the segment fields. A radial of a volume decoded from Level II keeps the channel byte and the
message generation date and time its source recorded (the model's `nexrad_message_channels`,
`nexrad_message_date` and `nexrad_message_milliseconds`).

### Metadata record

The writer synthesises what real files carry in their first LDM record, at the frames NOAA's files
use (frames counted from 1):

| Frames | Message | Content |
|---|---|---|
| 127 to 130 | 18, RDA adaptation data (Table XV), 9468 bytes in 4 segments (2400, 2400, 2400, 2268) | zero except the site name (ICAO), latitude and longitude (degrees, minutes, seconds, hemisphere), transmitter frequency (MHz, from `radar_parameters.frequency_hz`), antenna gain and horizontal beam width |
| 133 | 5, volume coverage pattern (Table XI) | the VCP number, the pulse width (long, 4, when the radar's pulse is longer than 3 us, between NEXRAD's 1.57 us short and 4.57 us long pulses, else short, 2: from the rays' pulse widths, else `radar_parameters` or the first calibration, else short, the pulse of NEXRAD's precipitation patterns), one cut per written sweep: its fixed angle, waveform (1, contiguous surveillance, for a REF-only cut; 2, contiguous Doppler with ambiguity resolution, for a Doppler-only cut; 3, contiguous Doppler without ambiguity resolution, the waveform of NEXRAD's upper cuts that carry every moment in one scan, for both), the super-resolution bit of 0.5-degree cuts, the azimuth rate (the sweep's target scan rate, else measured from the ray times), and each moment's SNR threshold; velocity resolution 0.5 m/s, or 1 m/s when VEL is written 8-bit at scale 1 |
| 134 | 2, RDA status (Table IV, 60 halfwords) | operate, on line, remote control, reflectivity, velocity and width enabled, the VCP, build 20.00, operational mode, super resolution enabled when any cut is 0.5 degree |
| the rest | empty frames | |

The other frames of real files hold the clutter filter map (Message 15), the bypass map (13) and
the performance data (3). The writer has no such data for a foreign volume and leaves those frames
empty.

A Level II source can pass its own metadata record (`SourceMetadata::metadata_record`), which is
then written byte for byte, padded with empty frames to 134 when shorter, except:

- a record that holds radials (files before 2005, some converted feeds) is ignored and the
  synthesised record written, with a note;
- readers index Message 5's cuts by the written elevation numbers (1 to n in sweep order, below):
  when the written sweeps are not the source's cuts 1 to n in order (a volume that leaves out or
  reorders sweeps), Message 5 is written with the source cut of each written sweep, in the
  written order (the header, pattern and every cut's bytes as the source has them), with a note;
  a Message 5 that does not decode, or has no cut for a written sweep's source elevation number,
  is replaced by the synthesised one, with a note;
- a Message 18 shorter than Table XV's 9468 bytes, which MetPy and Py-ART cannot unpack, is
  replaced by the synthesised one when it spans four frames, else the whole record is
  synthesised, with a note;
- the record agrees with the options: when the written VCP (`options.vcp`) differs from the one
  Message 5 or Message 2 names, their pattern numbers are set to it (Message 2 keeps its sign,
  negative for a locally selected pattern), and when the written site (`options.icao`) differs
  from Message 18's site name, the site name is set to it; each change is noted.

### Message 31 radial

One message per written ray, cut by cut and within a cut in the order the rays were collected
(below), each sized to its content (no padding after the last block). A foreign volume's cuts
are written in the order their sweeps were collected (each sweep's earliest ray; a sweep without
ray times keeps its place after the one stored before it), as Level II holds radials: a scan
collected from the top down (BEJAB, JMA) is written with its top cut first, and a note lists the
order when it differs from the storage order (`WriteSummary::written_sweeps` gives the source
sweep of every cut). A Level II source keeps the order it is given, its own or the one a caller
chose for its cuts.

| Block | Content |
|---|---|
| Data Header Block (72 bytes, 10 pointer slots) | site (a Level II source radial's own identifier, blank ones included, unless `options.icao` is set; else the written site), ray time (ms), date, radial number (the source's, else ray + 1), azimuth, compression 0, radial length, azimuth resolution (below), radial status (below), elevation number, cut sector (the source's, else 1), elevation, spot blanking (the source's, else 0) and azimuth indexing (the source's; else the sweep's recorded indexing angle, `nexrad_azimuth_indexing_angle_deg`, or its ray angle resolution when `rays_are_indexed`; else 0), block count and pointers |
| VOL (52 bytes, version 3.0) | latitude and longitude (`volume.location`), site height (the altitude rounded to the metre, feedhorn 0), calibration constant (`radar_calibration[0].base_1km_hc_dbz`), transmitter powers 0, system ZDR (`zdr_correction_db`), initial system PHIDP (`system_phidp_deg`), the VCP, processing status 0, ZDR bias estimate 0 |
| ELV (12 bytes) | atmospheric attenuation 0, calibration constant |
| RAD (28 bytes) | the ray's unambiguous range (`ray_vars.unambiguous_range_m`, 0.1 km) and Nyquist velocity (`ray_vars.nyquist_velocity_mps`, 0.01 m/s), each `options.unambiguous_range_m` and `options.nyquist_velocity_mps` where the ray has none, else 0; noise levels (`noise_hc_dbm`, `noise_vc_dbm`), calibration constants |
| Moments | REF, VEL, SW, ZDR, PHI, RHO, CFP in that order, each with its gate count, first gate range and spacing (metres), TOVER, SNR threshold and control flags (a Level II source's own, else 0), word size, scale, offset and codes |

A moment block is left out of a radial whose ray the source field does not provide (an absent
row). A ray that no written moment provides is left out altogether, because every radial must
carry a moment: Py-ART (`scan_info` takes a cut's moments from its first radial, then
`_find_range_params` indexes the first one) and xradar (the sweep's `sweep_data` comes from its
first radial) fail on a cut whose first radial has none. The rays left out are those missing from
`summary.written_rays`.

For a Level II source with `SourceMetadata::metadata`, every radial's own VOL, ELV and RAD blocks
and Data Header items come back (the metadata reader keeps them per radial in
`SweepElevationData::radials`), with the location and VCP of the volume written. A sweep's blocks
are found by its source elevation number (`Sweep::elevation_number`), so they stay with their
sweep when the volume leaves out or reorders the source's sweeps.

**Radial status** (Table III-C): 3 for the first written radial of the volume, 0 for the first
radial of a later cut (5 for the last cut), 2 for the last radial of a cut, 4 for the last radial
of the volume, 1 otherwise. The radial number of a foreign radial is its place in the written
cut, from 1. For a Level II source the radial's own code is kept where it plays the same
part: a cut start recorded as 0 or 5 (the RDA writes 0 for a last cut it did not plan as last),
and codes outside the table in mid-cut (KVWX 2008 has 8).

**Azimuth resolution** (byte 20): a Level II source radial's own code, as the metadata reader keeps
it (`RadialConstants::azimuth_resolution_code`); else 1 (0.5 degree) when the sweep's
`rays_angle_resolution_deg`, or else its median azimuth step, is at most 0.65 degree, and 2
(1 degree) otherwise. NEXRAD's 720- and 360-radial cuts fall on either side, and so do foreign
cuts of 600 or more radials (0.60 degree steps and finer) and of 512 or fewer (0.70 degree and
coarser). Message 5's super-resolution bit and Message 2 follow the first written radial's code.

## Mapping a volume onto Message 31

### Sweeps and cuts

Every sweep with rays becomes one elevation cut, in volume order; its elevation number is its
position (1 to 32). Readers need the numbers to run from 1 without gaps: Py-ART groups radials
by elevation numbers 1 to the largest (a missing number becomes an empty sweep) and MetPy adds an
empty sweep for each missing number. A Level II volume that leaves out or reorders its source's
sweeps is therefore numbered again (source cuts 1, 3, 4 become 1, 2, 3), with a note listing the
changed numbers, and a carried-over Message 5 is listed again to match (above). Sweeps without
rays are left out (`summary.skipped_sweeps`), and so are sweeps on whose rays no written moment
has data (no field maps to a moment, every mapped field was left out, or every row of the written
fields is absent), with a note: Level II cannot hold a cut without moments, and Py-ART and xradar
fail on one (Py-ART 2.3.0 with an IndexError, xradar 0.12 with a KeyError `sweep_data`). The
numbers of the cuts written after such a sweep move up by one. Split cuts and
SAILS or MESO-SAILS cuts need nothing special: NEXRAD numbers each of them as its own cut, and
Message 5 lists one cut per sweep with its own fixed angle and waveform, as NOAA's VCP 212 and
215 volumes do. A volume merged from several files (ODIM scans, JMA reflectivity and velocity
products) is written in the order of its sweeps.

Sweep modes: azimuth surveillance, sector, manual PPI and vertical pointing are written. RHI and
the other non-PPI modes are refused (`WriteError::UnsupportedSweepMode`).

Rays are written with their own azimuths and elevations (Message 31 stores them as f32, so they
come back bit for bit) and times (to the millisecond, from 1970 to 2149), in the order they were
collected, as Level II has them (the first radial opens the cut, and NOAA's files run forward in
time). A Level II source keeps its order. A sweep stored from another azimuth than
the one it started at is written from its earliest ray: ODIM stores rays from north
(`where/a1gate` names the first one radiated), so BEJAB 2019's lowest cut is stored from 0.5
degrees but was collected from 212.5 degrees, and its times step back once, by 19.9 s, at ray 212.
The rule: when the ray times (to the millisecond) run forward but for one step back, and the last
ray is no later than the first, the sweep is turned round to start at that step; any other order
is kept, because a clock can step back while the antenna runs on (NOXP's DORADE times step back a
second every 29 rays or so, with the azimuths still advancing). `summary.written_rays` gives the
source ray of every radial of each sweep turned round (or thinned, above). This differs from the
FM301 view's `time` dimension, which sorts every ray by time because a CF coordinate must be
monotonic; a Level II cut follows the antenna. JMA's rays share one time per sweep, so they are
written from the grid's start azimuth.

### Fields to moments

In each sweep, a field is assigned to a moment by, in order:

1. `options.field_map` (for example DORADE's `DB_ZDR` to ZDR);
2. its name: `DBZH` or `DBZ` to REF (`TH`, `DBTH` as a fallback), `VRADH` or `VRAD` to VEL
   (`VRADDH` as a fallback), `WRADH` or `WRAD` to SW, `ZDR` (`UZDR`) to ZDR, `PHIDP` (`UPHIDP`) to
   PHI, `RHOHV` (`URHOHV`) to RHO, `CCORH` to CFP;
3. its quantity with a fitting polarization (horizontal or unspecified for REF, VEL, SW and CFP;
   dual or unspecified for ZDR, PHI and RHO); total power only for REF, last.

One field per moment and sweep: the best-ranked wins, and the others are reported in
`summary.skipped_fields` with the field that won. Fields with no Message 31 moment (KDP, SQI,
temperature, and so on) are reported there too. A sweep where no field maps is left out (above);
a volume where none is left is refused (`WriteError::NoMoments`).

### Gate geometry

Each moment keeps its field's own native geometry (`Field::native_geometry`): the centre of the
first gate and the spacing, rounded to whole metres (Message 31 stores both in metres). The
rounding may move the last gate by at most `max_range_error_m` (default half a gate spacing),
else the field is refused (`WriteError::Geometry`). A first gate outside 0 to 32767 m, a spacing
outside 1 to 32767 m, a non-uniform range, or more than 16384 gates is refused as well. Message 1
volumes place their Doppler gates from -375 m: with `drop_negative_range_gates` the gates centred
before the radar are left out (reported per moment in `dropped_gates`), else the field is
refused.

### Quantisation

Gates are unsigned 8- or 16-bit codes, `value = (code - offset) / scale`, code 0 below threshold
(missing, undetected and non-finite values) and code 1 range folded (`Gate::RangeFolded`). Fields
that are already NEXRAD codes (decoded from Level II) are copied code for code under every policy.
Every other field shares one coding with the same moment's fields in the other sweeps, because
Py-ART decodes every sweep of a moment with the first sweep's scale and offset.

| Policy | Coding |
|---|---|
| `Precise` (default) | no value coded more coarsely than its source stores it. The ICD's typical coding when every value lies on it; else an exact coding of the values' own evenly spaced grid: its step is the storage step of integer sources, else estimated from the gaps between float values, else the coarsest of 1, 0.5, 0.25, 0.1, 0.05, 0.01, ..., 0.0001 that holds every value (JMA's uneven level tables are hundredths); 8-bit words when the grid has at most 254 levels, else 16-bit. Only float data on no such grid (more than 65534 levels) is coded with the finest 16-bit coding that covers it, with the error reported. |
| `Compatible` | the same choices within the word sizes NEXRAD files use: REF, VEL, SW, RHO and CFP 8-bit, ZDR and PHI 8-bit or 16-bit with codes up to 2047 and 1023. What does not fit is coded with the finest such coding that covers every value, coarser than a 16-bit or float source. |
| `Standard` | the ICD's typical codings as NOAA's current files carry them (KTLX 2024, KILX 2026): REF 8-bit at scale 2 and offset 66, VEL and SW 8-bit at 2 and 129, ZDR 16-bit at 32 and 418, PHI 16-bit at 2.8361 and 2, RHO 8-bit at 300 and -60.5, CFP 8-bit at 1 and 8, rounding to the nearest code. It never clips: a moment with any value outside its typical coding's range (DMI's RHOHV from 0, below the RHO coding's 0.208 floor) is coded as `Compatible` codes it instead. |

`MomentReport::max_abs_error` is the largest difference between a source value and the value a
reader decodes; `exact` is set when that is float rounding of the coding.

No policy clips or drops a value: a fixed coding is used only where it holds every value of the
moment, and a volume with a value its coding cannot hold is refused
(`WriteError::ValueOutsideCoding`), nothing written. A coding chosen from the values always holds
them; the refusal is met under the real-time `ChunkWriter`, whose codings the planned volume fixes
before the data arrives ("Real-time chunks" below).

**Which policy.** `Precise` is the default because a converter should not lose precision it was
given. Where every value fits 8 bits exactly it writes what `Compatible` writes; this is the case
for most ODIM feeds (8-bit data at one gain and offset per moment). The two differ for 16-bit and float sources, and for 8-bit sources with 255 or 256 levels
or with different gains in different sweeps. Sweep 0 of the committed fixtures (step = 1 / scale):

| Source | `Compatible` | `Precise` |
|---|---|---|
| ODIM BEJAB, DKROM, IESHA, NORST, ESPDG (8-bit) | 8-bit, exact | the same |
| CfRadial X-SAPR REF (float, 0.01 dB) | 8-bit, step 0.351 dB, error 0.175 dB | 16-bit, step 0.01, exact |
| CfRadial Irene VEL (8-bit, 255 levels of 0.378 m/s) | 8-bit, step 0.379 m/s, error 0.19 m/s | 16-bit, exact |
| DORADE COW2 REF, VEL, ZDR, RHO (16-bit, 0.01 and 0.0001) | 8-bit steps 0.203 dB, 0.625 m/s, 16-bit 0.023 dB, 8-bit 0.0038 | 16-bit, exact |
| DORADE NOXP VEL, SW, PHI (16-bit, 0.01) | 8-bit steps 0.060, 0.018; 16-bit 0.176 | 16-bit, exact |
| JMA REF, VEL (float level tables) | 8-bit steps 0.20 dB, 0.53 m/s | 16-bit, exact |

The cost is xradar 0.12 (the latest release): its Level II backend keeps only the low 8 bits of
every 16-bit moment but ZDR and PHI, and the low 11 and 10 bits of those
(`NexradLevel2ArrayWrapper._getitem` masks the words with `0xFF`, `0x7FF` and `0x3FF`), so it
misreads the 16-bit moments `Precise` writes for 16-bit and float sources (on COW2 by up to 51
dBZ, 156 m/s and 41 dB). The independent-reader check below confirms that its values are exactly
the masked codes, and that Py-ART, MetPy, RSL, LROSE Radx, the `nexrad` crate and this decoder read
the true values. GR2Analyst was not run, so how it reads a 16-bit REF, VEL or SW, or a scale and
offset NOAA does not write (the exact-grid codings of `Precise` and `Compatible`, such as BEJAB's
VEL at scale 2.3827), has not been checked. Choose `Compatible` for files that must read in xradar
0.12, and `Standard` for NOAA's current codings wherever they hold the values. The precision each
gives up is in `max_abs_error`. Which one is the default is an owner decision (Open items).

### Site, VCP, time, location

- **Site**: `options.icao`; else the instrument name when it is 4 characters of `[A-Za-z0-9_]`;
  else an ODIM node of two country letters and three radar letters becomes the first letter plus
  the radar letters (`DKROM` to `DROM`, `FIANJ` to `FANJ`); else the first four letters or
  digits, upper case. Padded with `_`. Py-ART takes every site whose identifier starts with `T`
  for a TDWR and looks its location up in its own table, failing with a `KeyError` for any other:
  JMA's Takayasu becomes `TAKA` and Py-ART 2.3.0 cannot open that file. Set `options.icao` to
  another identifier where Py-ART must read the file.
- **VCP**: `options.vcp`, else `volume.scan.vcp_pattern`, else 0 (no pattern).
- **Volume time**: a Level II source's volume header time, else the earliest written radial's:
  the start of the scan, which the polling directory's file names carry
  (`BJAB20190606_000022_V06.ar2v` for BEJAB's scan from 25 degrees at 00:00:22 down to
  0.3 degrees at 00:04:19). The real-time start chunk has the first pushed part's earliest
  radial.
- **Location**: latitude and longitude from `volume.location` into the VOL block and Message 18,
  altitude into the VOL block's site height. A volume without a finite latitude, longitude and
  height is refused (`MissingLocation`): the writer never writes a made-up position such as 0, 0.
  Message 1 volumes carry none; set `volume.location` to the radar's position before writing
  them.

## Refusals

Everything is decided before the first byte is produced (`plan.rs`), so an error means nothing
was written, except `WriteError::Io` from the sink. Level II cannot hold, and the writer refuses:

| Error | When |
|---|---|
| `EmptyVolume` | no sweep has rays |
| `NoMoments` | no ray of any sweep has data of a moment (no field maps to one, or every mapped field was left out or has no rows) |
| `UnsupportedSweepMode` | an RHI or other non-PPI sweep |
| `TooManySweeps` | more than 32 sweeps to write (elevation numbers are 1 to 32); a `ChunkWriter` push past the planned cuts |
| `UnplannedMoment` | a `ChunkWriter` push with a moment the planned volume lacks (below) |
| `InvalidSiteId` | a site that is not 1 to 4 of `[A-Za-z0-9_]`, or no letters to derive one from |
| `Geometry` | gates outside 0 to 32767 m, spacing outside 1 to 32767 m or not whole metres within the allowed error, non-uniform gates, more than 16384 gates, gates before the radar |
| `Ray` | a non-finite azimuth or elevation, or a time outside 1970 to 2149 |
| `Inconsistent` | a per-ray array (times, elevations, Nyquist velocities, unambiguous ranges) or a written field's rows, values, absent rows or gate stride that do not match the sweep's ray count: a volume built or changed without `Sweep::seal`, which checks them (the writer does not index past them) |
| `RadialTooLarge` | a radial over the 65535-byte radial length of the Data Header Block |
| `InvalidOption` | `radials_per_record`, `volume_number`, `max_range_error_m`, `nyquist_velocity_mps` or `unambiguous_range_m` out of range |
| `MissingLocation` | no finite site latitude, longitude or height in `volume.location` |
| `MetadataRecord` | a carried-over metadata record that does not frame |
| `DataMessage` | a carried-over data message that is not a non-radial message in whole 2432-byte frames |
| `LimitExceeded`, `Compression`, `Io` | resource limits, compressor and sink failures |

## Compression seam

Every bzip2 stream and the gzip wrapper come from `src/write/compress.rs` and nowhere else. The
bzip2 streams come from `recast-radar-bzip2`'s encoder at level 9 (the 900k block size of NOAA's
records), compressed in parallel on rayon through an `EncoderPool` that lives for one file (or
one real-time chunk writer), so each worker thread's encoder and its work buffers (about 20 MB)
are allocated once per volume. The encoder writes libbzip2 1.0.8's stream byte for byte except in
one field of a block that repeats a shorter string; the unit test `records_are_libbzip2_streams`
compares a real metadata record's stream with the `bzip2` crate's (`libbz2-rs-sys`), which is a
dev-dependency only. The `write` feature turns on `recast-radar-bzip2`'s `rayon` feature, so that
decoding alone does not build the encoder. Gzip uses `flate2` with the zlib-rs backend, as a
writer the file streams through.

## Re-encoding a Level II file

`rewrite_level2` decodes the file with its metadata (`read_volume_with_metadata`), its metadata
record (`messages::metadata_record`) and its data messages (`data_messages`), and writes them with
`write_volume_with_source`. What comes back: the metadata record (with the option changes above),
the non-radial messages of the data records at their places among the radials (the KIWA
2026-09-17 archive's three mid-volume Message 2 updates, for example), every radial's constant
blocks and Data Header items (its radar identifier, blank in KVWX 2008's radials, and azimuth
resolution code included), every radial's message channel byte and generation time, the volume
header time and every gate code. What changes: the grouping
of records (`radials_per_record` radials, by `record_layout`) and their compression, the radials'
message sequence numbers (numbered from 4), the radial statuses that place a radial in the written
volume (volume start and end, cut starts and ends), and what the options set: the header's volume
number, and with `icao` the site in the header, in Message 18 and in every radial.
`writer_roundtrip.rs` checks each of these on every Level II file of the manifests.

## Real-time chunks

`realtime::write_realtime_chunks` returns the chunks of the NEXRAD real-time bucket
(`unidata-nexrad-level2-chunks`): chunk 1 (`S`) is the volume header and the metadata record,
chunks 2 to N-1 (`I`) and the last (`E`) one radial record each: at most 120 radials, running on
across cuts or all of one cut by `record_layout` (the committed KIWA chunks, whose cuts are 360
and 720 radials, hold 120 radials of one cut, as both layouts write them). `ChunkedVolume::chunk_key` names them
`SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-K` (for example `KIWA/307/20260917-003629-001-S`), and
`concatenated()` gives back the Archive II file. Chunks are always LDM records; other compression
options are refused.

`realtime::ChunkWriter` sends the chunks while the volume is still being collected. It is built
from a planned volume (the previous volume of the radar, or any volume with the planned sweeps):
its sweeps are the cuts the start chunk's Message 5 lists, and each moment's coding is chosen from
its fields, because every sweep of a moment must share one coding and the start chunk goes out
before the data. `push(part)` takes the next sweeps (for example one decoded ODIM scan) and returns
the chunks they complete: the start chunk with the first push, whose volume header time is that
part's earliest radial, then one chunk per complete record. The pushed sweeps must stay one scan
cycle: a sweep that collects a cut again, begins after a pause of minutes or begins a Level II
volume again is refused (`MixedScanCycles`), with nothing sent. The last record so far is held back
until the next push or `finish`, because the volume's last record must end it (radial status 4,
negated control word); under `Continuous` the next push first fills it to 120 radials, so the tail
of a cut that is not a multiple of 120 goes out with the next cut's first radials, while under
`WithinCuts` a cut's records are complete when it is pushed. `finish` returns the end chunk.
Pushing the planned volume's sweeps one by one gives exactly the chunks `write_realtime_chunks`
gives for it, under either layout; a volume can end early; a push past the planned cuts is refused
(`TooManySweeps`), and so is a push with a value outside its moment's planned coding
(`ValueOutsideCoding`, nothing sent): the writer never clips a value, so plan with a volume whose
values span the radar's. A push with a moment the planned volume lacks is refused
(`UnplannedMoment`), nothing sent: its coding would be chosen from that push's sweeps alone and
differ from sweep to sweep, which Py-ART decodes with the first sweep's scale and offset. Plan with
a volume that has every moment the radar sends (a Doppler volume, not a reflectivity-only scan).

## Polling directory

`polling::PollingDirectory` publishes files the way GRLevelX polling clients (GR2Analyst's
polling mode) read a polling directory: the directory a client's polling URL names holds
`grlevel2.cfg`, and each site's directory its listing. The GRLevelX manual names only that file;
the layout and the defaults below follow the GRLevelX-style servers captured in the corpus
(`testdata/feeds/manifest.toml`): the Iowa Environmental Mesonet's `config.cfg` (`ListFile:
dir.list`, then 220 `Site:` lines in the order the sites were added), the North Dakota State
Water Commission's `dir.list` of KXWA (1,161 LF lines such as `31040
KXWA20260921_105810_V06.ar2v`) and the Laredo feed's `grlevel2.cfg` (`Site: LARE`):

```text
<root>/config.cfg         "ListFile: dir.list", then "Site: XXXX" lines, one per site
<root>/grlevel2.cfg       "Site: XXXX" lines
<root>/<SITE>/dir.list    "<size> <filename>" lines, oldest first
<root>/<SITE>/<SITE>YYYYMMDD_HHMMSS_V06.ar2v[.gz]
```

Every line ends in LF. A publish appends its site to each site list that lacks it and keeps the
rest of the file as it is, so a list kept by hand keeps its lines and order; a new `config.cfg`
starts with `ListFile: dir.list`.

A file name is a name format and a suffix. The format (`with_name_format`) has `{site}` for the
site and chrono's strftime specifiers for the volume time; the default, `{site}%Y%m%d_%H%M%S_V06`,
gives the NWS archive's `SITEYYYYMMDD_HHMMSS_V06` (the site padded with `_` to 4). The suffix is
`.ar2v`, or `.ar2v.gz`
when the bytes are gzip, unless `with_suffix` fixes one (MetPy picks gzip by the `.gz` suffix of a
path, so only gzip bytes should be named `.gz`). `publish_named` publishes under a name the caller
chooses. Every name must be a plain file name that every platform can hold, because a polling
directory may be written or served on Windows: a name with a path separator, a control character
or any of `<>:"|?*` (a `:` would write an NTFS alternate data stream), a name ending in a dot or a
space, a Windows device name with or without an extension (`CON`, `PRN`, `AUX`, `NUL`, `COM0` to
`COM9`, `LPT0` to `LPT9`, `nul.ar2v`), and `dir.list` or its temporary `dir.list.tmp` in any case,
are refused (`InvalidFileName`); so is a site that is a device name (`InvalidSite`), and a name
format that has an unknown `%` specifier or renders such a name (`%H:%M`; `InvalidNameFormat`).
`dir.list` and the retention limit order files by name, so a format should write the time from
year to second. A lower-case site in the names and in the files comes from a lower-case
`WriteOptions::icao`; the site derived from an ODIM node is upper case (`DKROM` to `DROM`).

The listed size is the stored file's, in bytes. That departs from the one captured listing:
the North Dakota server's sizes are not bytes (it lists `KXWA20260924_214316_V06.ar2v` as 40288
while the pinned file is 20,616,906 bytes, about its size in 512-byte blocks), and the GRLevelX
manual does not say which unit clients expect; which one to write is an open decision (Open
items). Each file and `dir.list` is written to a
temporary name and renamed into place, so a polling client never reads a partial file or listing.
At most `DEFAULT_MAX_FILES` (30, as `recast-radar publish` and the Python package keep: two and a
half hours of 5-minute volumes) are kept per site unless `with_max_files` sets another limit;
older ones are deleted and unlisted. Each publish lists its site in `config.cfg` and `grlevel2.cfg` unless
`with_site_lists(false)` leaves them as they are. One publisher per root is assumed.

## ICD compliance

The writer follows ICD 2620010 (Archive II) and ICD 2620002 (RDA/RPG) and the practice of NOAA's
own Level II files, and writes none of the departures from them that converted Level II files
are known to carry:

| Item | What the writer writes | What it never writes |
|---|---|---|
| Volume header date | the 32-bit day number (day 1 = 1970-01-01) in bytes 12 to 15 | a 16-bit day in the upper halfword followed by two zero bytes |
| Message 5 size | the size in halfwords including the 8-halfword message header, as for every message | a size that leaves the message header out |
| Metadata record | the 134 fixed frames of NOAA's files, Messages 18, 5 and 2 at their frames (a carried-over record padded to 134) | a record of only a few frames |
| Records | LDM records of `radials_per_record` whole radials (120 by default), one bzip2 stream each, the last control word negated | fixed-size pieces of one frame stream, with messages split across records |
| Uncompressed radials | Message 31 radials back to back, each sized to its content | radials padded into, or running over, fixed 2432-byte frames |
| Message 18 | the full Table XV body (9468 bytes in 4 segments) on the Open RDA channel, with the site name, position, frequency, antenna gain and beam width | an all-zero body on the legacy channel |
| First gate | the range to the centre of the first gate (Table XVII-B), where Py-ART, MetPy and xradar place it; for JMA the grid's range start (the first bin's inner bound, WMO template 3.120 octets 35-38) plus half a gate: 250 m for 500 m gates | the range to the start of the first bin |
| Site position | the volume's latitude, longitude and height; a volume without one is refused (`MissingLocation`) | a zero position |
| Nyquist velocity, unambiguous range | the ray's own, else the radar's value the caller gives (`nyquist_velocity_mps`, `unambiguous_range_m`), else 0, which readers take as unknown, with a note | a constant placeholder |
| Codings | per policy, never clipping or dropping a source value ("Quantisation") | a fixed coding that clips or drops values outside it |
| Moments of a cut | the fields of one sweep in its cut, and the sweeps of one scan cycle in the volume: a volume of more than one cycle is refused (`MixedScanCycles`, below), and `merge_volumes` pairs sweeps only when they were collected together | moments of different scan cycles in one cut or volume |
| Cut order and volume time | cuts in the order their sweeps were collected; the header at the scan's first radial | cuts out of collection order, a header at the last-collected sweep |
| Pulse width | Message 5's pulse width from the radar's own pulse width ("Metadata record") | one pulse width code for every radar |

A Level II file holds one volume scan. Before writing, the writer takes the sweeps it would write
in the order they were collected and refuses the volume (`WriteError::MixedScanCycles`, nothing
written) when they are more than one scan cycle
(`recast_radar_core::model::scan_cycles`):

- a sweep collects a cut the cycle already collected: the same sweep mode, a fixed angle within
  0.05 degrees, the same gates and the same field names. A long-range surveillance cut and a
  Doppler cut at one angle have other gates or moments and stay in one cycle; JMA's 10-minute
  tars collect every cut of their first 5-minute cycle again 281 s (Takayasu 2019) or 284 s
  (Okinawa 2026) later;
- a sweep begins more than 240 s after every sweep of the cycle ended. Operational scans run their
  cuts back to back; the longest pause within a cycle in the corpus is 129 s (Takayasu's velocity
  file, which leaves out the reflectivity-only cuts collected in between, its rays carrying only
  each sweep's observation start). Hurum's 2026-06-12 14:46 velocity file carries a 90 degree
  sweep collected at 14:38:53, in the scan before, 442 s before its other sweeps began;
- for a Level II source, a radial begins a volume scan again (radial status 3). A Level II
  volume coverage pattern collects cuts again within one volume by design (SAILS and MESO-SAILS
  rescans of the lowest cut, MRLE), so the repeated-cut rule does not apply to it: KOAX 2014 and
  KEWX 2016 (SAILS) and KDVN 2020 (MESO-SAILS 2) are one cycle.

Every committed volume of the corpus but the JMA tars and that Hurum file is one cycle
(`recast-radar-io/tests/scan_cycles_corpus.rs`). `split_scan_cycles` gives one volume per
cycle, its sweeps in collection order and its time reference at its first ray, to write one at a
time: `recast-radar convert --split-scan-cycles` (cycle N written to the output name with `_N`
before its extension), `recast-radar publish --split-scan-cycles`, `recast_radar.split_scan_cycles`
in Python, or one cycle's sweeps chosen with `--sweeps` / `sweeps=`. `merge_volumes` pairs sweeps
of parts only when their first rays are at most 60 s apart, so a part's sweep of another cycle is
kept as a sweep of its own, never merged into this cycle's cut; the merged volume then shows it
as another cycle.

## Verification

### Tests (`cargo test`)

All on real files from the corpus (`docs/testdata/corpus.md`); downloaded files are skipped when
they are not cached and the network is off.

| Test | What it checks |
|---|---|
| `recast-radar-io-nexrad/tests/writer_roundtrip.rs` | every Level II file of the manifests (WSR-88D and TDWR, 1991 to 2026): decode with metadata, write, decode again, for uncompressed records and LDM records, and for files under 1.5 MB both again with gzip. Message 31 volumes come back with every gate code, ray and sweep value and the volume-level model identical; with the source's metadata record carried over, the NEXRAD metadata too (Messages 2, 3, 5, 13, 15, 18, 32 and every radial's VOL, ELV and RAD blocks and Data Header items, radar identifier and azimuth resolution code included), the radial statuses that place a radial in the written volume aside; and the data records' non-radial messages (mid-volume Message 2 updates) come back at their places among the radials. KVWX 2008's blank radial identifiers come back blank, and become the site `options.icao` names. Message 1 volumes, which carry no site position, are refused (`MissingLocation`) and then, with the position of a Message 31 file of the same radar, keep every written gate (the ones before the radar dropped). `Standard` quantisation still copies NEXRAD codes. A volume that leaves out or reorders KTLX 2024's sweeps keeps each sweep's Message 5 cut, fixed angle and constant blocks; VCP and site overrides reach Messages 2, 5 and 18 of the carried-over record, each noted. |
| `recast-radar-core/tests/real_cycles.rs`, `recast-radar-io/tests/scan_cycles_corpus.rs` | `scan_cycles` against a reference implementation of its rules over the sweep times and geometry h5py and a GRIB2 section walker read (`tools/core_golden.py`): JMA Takayasu 2019 and Okinawa 2026 reflectivity and velocity tars two cycles each, the second beginning where the first cut is collected again; Hurum's velocity file two, its 90 degree sweep of the scan before alone; the other ODIM files one; SAILS and MESO-SAILS Level II volumes one; `split_scan_cycles` giving volumes that seal, in collection order, their time reference at their first ray; every committed volume of the corpus one cycle but those |
| `recast-radar-io/tests/level2_writer_intl.rs` | real ODIM_H5 (Belgium, Denmark, Ireland, Norway, Spain), CfRadial 1 (ARM X-SAPR, the SMART-R2 in Irene), DORADE (COW2, NOXP) and JMA GRIB2 (Takayasu, reflectivity and velocity, and both merged, each refused whole as two scan cycles, `MixedScanCycles`, and written one cycle at a time; Hurum's velocity file likewise) volumes written and decoded again under the default policy and `Compatible`: the cuts in the order their sweeps were collected (`written_sweeps`), the volume time the earliest radial's, every ray once, in the order `written_rays` reports (ODIM's and X-SAPR's sweeps written from their earliest ray, their times then running forward; NOXP's kept as stored), with azimuths and elevations bit for bit, times to the millisecond, gate geometry to the metre, fixed angles within half an angle code, every value within the reported quantisation step, location and frequency; under the default, no value coarser than its source stores it; JMA's 512-radial cuts in 120-radial records across cuts by default and in records of one cut under `WithinCuts`, with `ChunkWriter`'s chunks equal to the whole volume's under both; `ChunkWriter` planned from the first 5-minute cycle of JMA Okinawa's 2026 tar refusing, under `Compatible`, the second cycle's 0.2-degree sweep, whose 3 gates above 47.2 dBZ its planned REF coding cannot hold (`ValueOutsideCoding`, nothing sent), while the whole-file writer holds them and `Precise`'s plan takes the whole cycle; `Standard` giving DKROM NOAA's codings where they hold its values and `Compatible`'s where not (RHOHV), no value clipped; a JMA velocity cycle's radials without a Nyquist velocity noted, and filled by the options (out-of-range values refused); RHI volumes refused |
| `recast-radar-io-nexrad/tests/writer_layout.rs` | the bytes: header, LDM control words, the 134-frame metadata record with Messages 18, 5 and 2 where real files have them, every radial's blocks; real-time chunks concatenating to the file, and `ChunkWriter` (the start chunk with the first sweep, the held-back last record, a volume ended early, pushes past the planned cuts refused, and a Doppler cut pushed after a plan of KTLX's surveillance cut refused as `UnplannedMoment` with nothing sent); `write_volume_to` streaming KDVN 2020 in records of 10 radials from a two-thread pool (many batches) to the bytes of the whole-file writer under every compression, the real-time chunks' for LDM records, only the last control word negated, a gzip wrapper holding exactly the unwrapped file, and a sink that fails part way returning its error; sweeps without fields or with every row absent left out, rays without data left out, every radial carrying a moment, a volume with no data refused; the polling directory (names and name formats, `publish_named`, `dir.list`, site lists, retention, and the names, formats and sites refused, Windows' included); a volume with a copy of its first cut, which begins a volume scan again, refused (`MixedScanCycles`), and a `ChunkWriter` pushed the same sweep twice likewise; every refusal leaving the sink empty, per-ray arrays and field rows that do not match the ray count included |

### Independent readers (`tools/level2_writer_check.py`)

```text
cargo run --release -p recast-radar-io --example level2_writer_check -- [--quantization compatible] OUT
cargo build --release --manifest-path tools/level2_writer_nexrad_crate/Cargo.toml \
    --config 'patch.crates-io.nexrad.path="<nexrad checkout>/nexrad"'
RADXCONVERT=/usr/local/lrose/bin/RadxConvert \
    sh tools/level2_writer_external/run.sh EXT OUT/*.ar2v* <NEXRAD and DORADE sources>   # in nexbench
python tools/level2_writer_check.py OUT/manifest.json --nexrad-crate <exe> --external EXT
```

The example writes Level II files from real volumes (NEXRAD re-encoded with their metadata,
metadata record and data messages; ODIM_H5, CfRadial 1, DORADE and JMA converted) in
the variants `bzip2`, `none`, `gzip` (gzip over uncompressed records) and `bzip2-gzip`, and BEJAB
2019 once more with sweep 1's fields removed (`no-sweep-1-fields`: the sweep is left out), under
the default policy or the one `--quantization` names; the manifest carries each summary,
`written_rays` included. The script reads each file with Py-ART (`read_nexrad_archive`), MetPy
(`Level2File`), xradar (`open_nexradlevel2_datatree`), the Rust `nexrad` crate, RSL
(`RSL_wsr88d_to_radar`) and LROSE (`RadxConvert`), and checks that every reader opens it with the
sweeps and radials written, that the readers agree gate for gate, and that the values equal the
source's as an independent reader of the source reads them (the same reader on a NEXRAD source,
h5py on ODIM, netCDF4 on CfRadial, LROSE RadxConvert's CfRadial of a DORADE source, the
standard-library GRIB2 walker of `tools/golden_io_formats.py` on a JMA tar; the source's rays taken
in the written order) within the reported quantisation error. Only the merged JMA volume, which has
no single source file, is checked by the readers' agreement alone.


**Result** (2026-09-25, the writer at `8a9c80a` and the script at that commit; Py-ART 2.3.0,
MetPy 1.7.1, xradar 0.12.0, h5py 3.16.0, netCDF4 1.7.4, the `nexrad` crate 1.0.0-rc.4 at
`1591b64`, RSL 1.50 and LROSE RadxConvert of the 2025-08 release in nexbench): every file written
from the sources the example lists (Level II, ODIM_H5, JMA with the merged volume, CfRadial 1 and
DORADE), once under the default policy (`Precise`) and once under `Compatible`: **0 failures** in
both. That run predates the integration changes (the site-position refusal, the pulse width, the
non-clipping `Standard`) and included three Level II sources that are no longer in the corpus.

**Rerun on the integration branch** (2026-09-26, the writer at `aa162f5`: the site-position
refusal, the pulse width, the non-clipping `Standard`, the refusal of values outside a coding and
the `recast-radar-bzip2` encoder; Py-ART 2.3.0, MetPy 1.7.1, xradar 0.12.0, h5py 3.16.0, netCDF4
1.7.4, the `nexrad` crate 1.0.0-rc.4 at `1591b64`, RSL 1.50 and LROSE RadxConvert in nexbench):
32 files under each policy, `Precise` and `Compatible` (KTLX 2024, KILX 2026 and KIWA 2026 whole
volumes, the KTLX, KDVN, TSTL and KLIX 2005 trims, KVWX 2008, five ODIM_H5 volumes, two CfRadial
1, two DORADE and the JMA N5, N6 and merged volumes, in the variants the example writes), read by
all six readers: **0 failures** in both runs, with 189 and 114 notes, all of the kinds listed
below. The KLIX 2005 Message 1 trim, which carries no site position, is given the position of the
KLIX 2021 Message 31 trim (as `recast-radar convert --position-from` gives it) and written with
its gates before the radar dropped. Every source but the merged JMA volume is compared with an
independent reading of it: DORADE COW2 and NOXP with RadxConvert's (the decoder keeps COW2's 3
rays flagged in transition, as RadxConvert does), JMA N5 and N6 with the GRIB2 walker. KVWX 2008's
re-encoding, whose radials keep their blank radar identifier, reads in every reader as its source
does. RSL reads every file but the gzip-wrapped LDM records (it crashes on BEJAB's and reads the
unwrapped file) and the DORADE sources; RadxConvert every file but the gzip-wrapped ones and the
KVWX 2008 source. The notes are reader limitations; where a limitation is the reader's handling of
what the writer wrote, the script checks that it is exactly that, and fails otherwise:

- xradar 0.12 keeps only the low 8 bits of 16-bit moments other than ZDR and PHI (and 11 and 10
  bits of those): for the 16-bit moments `Precise` writes for float and 16-bit sources (X-SAPR
  REF, Irene VEL, COW2, NOXP, JMA REF; 75 moments of one sweep each, the only difference between
  the two runs) its values must equal the true codes (MetPy's) masked that way, and do. On COW2 that puts
  REF, VEL, ZDR and RHO up to 51.2 dBZ, 156.2 m/s, 40.96 dB and 0.95 off the true values under
  `Precise`; under `Compatible` it reads them right.
- xradar 0.12's record addressing (Record layout, above): of the compressed files whose cuts are
  not multiples of 120 radials (JMA N5, N6 and merged, KVWX 2008, KLIX 2005) it lists every sweep but fails on the data of a sweep that starts inside a record
  (IndexError); the script then requires an uncompressed file of the same source that xradar reads
  in full, and each has one.
  Where a record holds mid-volume Message 2 frames as well (KIWA 2026, the TSTL trim, as NOAA's
  files do) it loses as many radials, on the source as on the written file. It reads no gzip file.
- Py-ART cannot open two kinds of written file. A site identifier starting with `T` (JMA's `TAKA`)
  makes it look the site up in its TDWR table and fail with a `KeyError`; the script enters the
  site into that table from the file's VOL block first, as a user must (or choose another
  `options.icao`). And the KLIX 2005 trim written with `drop_negative_range_gates` (REF on 1000 m
  gates from 0 m, VEL and SW on 250 m gates from 125 m once the gates before the radar are
  dropped) fails with `ValueError: Gate spacing is neither 1/4 or 1/2`, compressed or not: Py-ART
  puts every moment on one range from the smallest first gate and spacing (0 m, 250 m) and can
  only widen a moment's gates 2 or 4 times from that first gate. It reads the source, whose
  Message 1 radials it places itself. The other readers read both files; xradar reads the
  uncompressed one in full.
- Py-ART puts every moment of a volume on one range, and xradar every moment of a sweep:
  moments on other gates are resampled and not compared gate for gate.
- RSL looks the site up in its own table and reads unknown sites as KTLX (its location then
  replaces the file's); stores values in 16 bits with fixed ranges (gates outside them are not
  compared); has no range-folded code for PHI and RHO; reads an `AR2V0001` source as Message 1
  radials; and reads gzip over LDM records only unwrapped.
- RadxConvert remaps every moment to one range geometry per ray; reads no gzip file here; and
  takes scan rate, pulse width and sample counts from Messages 5 and 18.
- The `nexrad` crate reads only one to three radials per sweep of the TSTL trim, so that source
  is not compared with it (it reads the written file); it fails on KVWX 2008 (no Message 5) and on
  its re-encoding alike.
- MetPy cannot read some sources (the KLIX 2005 trim, KVWX 2008); it reads every written file.

### Fuzzing

Two targets cover the writer (`fuzz/README.md`):

- `level2-writer` decodes the input as Level II with its metadata, writes it (without the source
  metadata as LDM records; with its metadata, metadata record and data messages uncompressed,
  gzip-wrapped, or as real-time chunks, by input length), and decodes the output again: the
  written volume must decode with the sweeps and radials reported, the source's rays in their
  order (a ray left out only when no written moment has data on it), every radial's time and
  angles, and every written moment's codes, gates and absent rows as the source had them. Seeds:
  11 real Level II files; a Message 1 volume is given KTLX's position in the modes that set the
  site. Every moment here is a NEXRAD moment, whose codes are copied.
- `level2-writer-router` decodes the input with the format router (ODIM_H5, CfRadial, DORADE,
  JMA, Level II) and writes it under the `Precise`, `Compatible` or `Standard` policy
  (plain, or dropping gates before the radar, accepting any range rounding, supplying a Nyquist
  velocity and unambiguous range where the source has none and going through the real-time
  chunker), by input length, so the quantiser, the gate geometry of foreign ranges, the site
  derivation, the field mapping and the ray order are fuzzed. The output must decode with the
  sweeps and radials reported, each radial the source ray `written_rays` names (every ray at most
  once, a ray left out only when no written moment has data on it) with its time (to the
  millisecond) and angles (bit for bit), and every moment's gate count, first gate, spacing,
  absent rays and values (each within the reported `max_abs_error`, sentinels as sentinels).
  Seeds: 9 real ODIM, CfRadial, DORADE and JMA files, an RHI among them.

`fuzz-tools smoke <target> <n>` runs `n` seeded mutations of every seed through a target on
stable Rust, repeatably and on Windows. At `63ef7e2` (2026-09-25; ray order, rays and sweeps
without data, record layouts and the chunk writer's pinned codings included): `level2-writer`
13,000 mutants (6,196 written and decoded again), `level2-writer-router` 9,000 (2,667), no panic.
Again at `8a9c80a` (the Nyquist velocity and unambiguous range options, the streamed writer and
the radials' own identifiers and resolution codes): the same counts, no panic. No
AddressSanitizer campaign has run on `8a9c80a` or on the integration changes; the campaigns below
ran on the code before it.

Campaigns in nexbench (cargo-fuzz 0.13.2, nightly, AddressSanitizer, one worker per target):

| Target and code | Runs | Result |
|---|---|---|
| `level2-writer`, the working tree between `f646a66` and `91790be` (an earlier, wider form of the range refinement) | 69,405 in 20 min | no finding |
| `level2-writer`, `b6d7e80` | 12,317 | a REF moment with a NaN scale and offset was written below threshold instead of copied; fixed in `9d0b76a` (NEXRAD codes are copied whatever their scale and offset), kept as `fuzz-level2-writer-nexrad-moment-nan-scale` with a regression test in `tests/fuzz_regressions.rs` |
| `level2-writer`, `9d0b76a` | 106,011 in 30 min | no finding (peak RSS 676 MB, 920 new corpus units) |
| `level2-writer-router`, `c1e4cdf` (before records within cuts and the chunk writer) | 19,325 in 20 min | no finding (peak RSS 663 MB, 1,633 new units) |
| `level2-writer`, `c7d5fa8` (the final writer code) | 96,996 in 30 min | no finding (peak RSS 599 MB, 2,628 new units) |
| `level2-writer-router`, `c7d5fa8` | 20,435 in 14 min | stopped by a libFuzzer clock glitch ("working on the last Unit for -1 seconds", a timeout of 2^64 - 1 s); the saved input runs in 6.6 s under AddressSanitizer, as its unmutated seed (6.3 s), and in 164 ms in the stable replay: no finding |
| `level2-writer-router`, `c7d5fa8`, fork mode | 26,841 in 17 min | no crash, timeout or OOM; stopped by a restart of the container |
| `level2-writer-router`, `c7d5fa8`, fork mode, from the grown corpus | 33,363 in 20 min | no crash, timeout or OOM (corpus 1,459 units, 14,754 coverage points) |
| `level2-writer`, `33ab2d0` (ray order, rays and sweeps without data, record layouts), fork mode | 212,982 in 30 min | 7 inputs panic in the harness, not the writer: it compared ray times relative to each volume's own reference, which the decoder takes from the first radial it reads (to the second), so it moves when the writer leaves out a ray or sweep without data. Fixed in `63ef7e2` (times compared in milliseconds since 1970, as the router harness does); `writer_layout.rs` checks the times of a sweep whose first ray is left out |
| `level2-writer-router`, `33ab2d0`, fork mode | 67,438 in 30 min | no crash, timeout or OOM |
| `level2-writer`, `63ef7e2`, fork mode, from the grown corpus | 160,957 in 30 min | the 7 inputs above run without a panic; no crash, timeout or OOM (corpus 1,812 units) |
| `level2-writer-router`, `63ef7e2`, fork mode, from the grown corpus | 21,219 in 30 min | no crash, timeout or OOM (corpus 1,579 units, 14,578 coverage points) |

The foreign inputs are slow under AddressSanitizer (JMA's float planes take seconds per input
against milliseconds without it), so the router target runs at 16 to 50 inputs a second.


## Open items

- The facade (`recast-radar-tools`) exposes the writer through its `nexrad-write` feature; the
  crate's own feature is `write`.
- The Level II decoder keeps each radial's azimuth resolution code in
  `RadialConstants::azimuth_resolution_code` (the metadata), which the writer writes back; it does
  not set `Sweep::rays_angle_resolution_deg` from it, which would change the FM301 view of every
  Level II volume.
- The writer has no clutter maps or performance data for a foreign volume (Messages 13, 15, 3).
- GR2Analyst itself was not run on the output (it is a Windows GUI application, not installed
  here); what it takes is inferred from the ICDs, NOAA's files and the six independent readers.
- The CfRadial 1 decoder stores the 0-based sweep index in `Sweep::elevation_number`, which the
  model documents as the 1-based ICD elevation number. The writer uses the numbers only for
  Level II sources, so it is not affected.
- xradar 0.12 cannot read a compressed file whose cuts are not multiples of 120 radials under any
  record layout (Record layout, above); `Compression::None` files read in full. Py-ART cannot
  open a file whose site starts with `T` unless the site is in its table, nor the KLIX 2005
  Message 1 volume once its gates before the radar are dropped (Independent readers, above),
  nor a file whose moments are on gates other than 1, 2 or 4 times the smallest spacing (from
  the smallest first gate): Hurum's scan, whose 90 degree sweep has 30 m gates beside the other
  sweeps' 250 m, fails with "Gate spacing is neither 1/4 or 1/2". The writer notes such a file
  and names the sweeps. These are the readers' limits, not the files'.
- JMA (decoder, not writer): every ray of a sweep carries the sweep's observation start
  (template 4.51022 octets 51-52), not a time of its own; the decoder does not read the PRFs as
  a Nyquist velocity, and reads the reflectivity table's level 1 (0.0) as 0 dBZ. The range start
  is the first bin's inner bound (WMO template 3.120 octets 35-38, which JMA's template follows
  octet for octet; ecCodes names them `offsetFromOriginToInnerBound`), so the first gate is
  centred half a spacing beyond it. `merge_volumes` keeps the N6 sweeps whose first ray is
  rotated from their N5 sweep's (the two lowest Doppler tilts) as sweeps of their own.
- The polling directory lists sizes in bytes, which the captured North Dakota listing does not
  (Polling directory, above); GR2Analyst was not run to see which it expects.

**Decisions for the owner.** Each is a default where the ICD leaves a choice. The writer does
what the list says; each other choice is one option away.

1. **Quantisation default.** `Precise` (the default) never codes a value more coarsely than its
   source: it writes 16-bit REF, VEL and SW for 16-bit and float sources (JMA's REF, CfRadial and
   DORADE fields), and exact-grid scales and offsets for 8-bit sources off NOAA's grid (BEJAB's
   VEL at 2.3827). xradar 0.12 misreads those 16-bit moments, and whether GR2Analyst reads them
   has not been checked. `Compatible` keeps NEXRAD's word sizes (xradar reads it right; the
   precision lost is reported). `Standard` writes NOAA's current codings wherever they hold the
   values.
2. **VCP.** 0 (no pattern) when neither `options.vcp` nor the volume names one.
3. **Record layout.** `Continuous` (the default) runs records across cuts that are not multiples
   of 120 radials; `WithinCuts` ends each record with its cut, as NOAA's chunks happen to (their
   cuts are multiples of 120), and sends a cut's last chunk as soon as the cut is pushed. Neither
   changes what xradar 0.12 reads.
4. **Ray and cut order.** A foreign sweep stored from another azimuth than the one it started
   at (ODIM) is written from its earliest ray, keeping the source's times, so each cut runs
   forward in time from the ray collected first, and a foreign volume's cuts are written in the
   order their sweeps were collected, as Level II does. A scan collected from the top down is
   then written with its highest cut first; Py-ART, MetPy and xradar read that order, and
   whether GR2Analyst lists such cuts as it lists NOAA's (always bottom-up, SAILS aside) has not
   been checked.
5. **JMA's meaning** (decoder, not writer; Open items): reflectivity level 1, per-ray times
   spread between the observation start and end offsets, and the PRFs. Each changes the decoded
   volume, its FM301 view and every JMA golden. (The range start is settled: the first bin's
   inner bound.)
6. **Scan cycles.** A volume of more than one scan cycle is refused rather than split
   silently; the command and the Python package split only when asked (`--split-scan-cycles`,
   `split_scan_cycles`). The pause that separates cycles is 240 s, and a Level II source's
   repeated cuts are never a new cycle (SAILS).

[`Volume`]: ../../crates/recast-radar-core/src/model/volume.rs
