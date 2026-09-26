# Reading radar files

Every decoder returns the same type, [`model::Volume`](data-model.md): a
radar volume in the WMO FM301 (CfRadial 2) layout. The easiest entry point is
the router in the `io` module, which reads a byte buffer of any supported
format.

## Any format

<!-- example: crates/recast-radar-tools/examples/read_any.rs -->
```rust
//! Read a radar file of any supported format and list its sweeps and fields.
//!
//! cargo run --release -p recast-radar-tools --example read_any -- <radar-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::io;
use recast_radar_tools::model::{Field, RangeCoord};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <radar-file>")?);
    let bytes = std::fs::read(&path)?;

    // NEXRAD Level II, ODIM_H5, CfRadial 1, DORADE or a JMA GRIB2 tar, also
    // inside gzip or a single-file ZIP. The router sniffs the format.
    let volume = io::read_supported_volume_bytes(&bytes)?;

    println!(
        "{} ({:?}), {}",
        volume.attrs.instrument_name, volume.provenance.source_format, volume.time_reference
    );
    let location = volume.location;
    if let (Some(lat), Some(lon)) = (location.latitude_deg, location.longitude_deg) {
        let altitude = location.altitude_m.unwrap_or(f64::NAN);
        println!("latitude {lat:.4}, longitude {lon:.4}, altitude {altitude:.0} m");
    }

    for (index, sweep) in volume.sweeps.iter().enumerate() {
        println!(
            "sweep {index}: {} at {:.2} deg, {} rays, {}",
            sweep.sweep_mode.as_str(),
            sweep.fixed_angle_deg,
            sweep.nrays(),
            range_text(&sweep.range)
        );
        for field in &sweep.fields {
            println!("  {}", field_text(field));
        }
    }
    Ok(())
}

/// Gate count, first gate centre and spacing of a range coordinate.
fn range_text(range: &RangeCoord) -> String {
    let first_km = range.center_m(0).unwrap_or(f64::NAN) / 1000.0;
    match range.spacing_m() {
        Some(spacing) => format!(
            "{} gates from {first_km:.3} km every {spacing:.1} m",
            range.ngates()
        ),
        None => format!("{} gates from {first_km:.3} km", range.ngates()),
    }
}

/// Name, quantity, units and storage type of a field.
fn field_text(field: &Field) -> String {
    // Units the source stated, else the FM301 units of a known name.
    let units = field
        .attrs
        .units
        .as_deref()
        .or_else(|| field.name.info().map(|info| info.units))
        .unwrap_or("");
    let (rays, gates) = field.shape();
    format!(
        "{:<8} {:?} [{units}], {rays} x {gates} {}",
        field.name.as_str(),
        field.quantity,
        field.data.dtype()
    )
}
```

On the committed ODIM_H5 fixture
`testdata/files/other/odim/espdg.pvol.20260707.dbzh_vradh.h5` (AEMET
Perdiguera, Spain):

<!-- output: read_any repo:testdata/files/other/odim/espdg.pvol.20260707.dbzh_vradh.h5 -->
```text
ESPDG (OdimH5), 2026-07-07 19:27:49 UTC
latitude 41.7340, longitude -0.5459, altitude 835 m
sweep 0: azimuth_surveillance at 1.50 deg, 360 rays, 299 gates from 0.450 km every 500.0 m
  VRADH    RadialVelocity [m s-1], 360 x 299 float64
  DBZH     Reflectivity [dBZ], 360 x 299 float64
sweep 1: azimuth_surveillance at 0.50 deg, 360 rays, 299 gates from 0.450 km every 500.0 m
  VRADH    RadialVelocity [m s-1], 360 x 299 float64
  DBZH     Reflectivity [dBZ], 360 x 299 float64
```

and on the NEXRAD file `KTLX20240315_000217_V06` (first lines):

<!-- output-head: read_any testdata:l2-ktlx-20240315-000217 -->
```text
KTLX (NexradLevel2), 2024-03-15 00:02:17 UTC
latitude 35.3334, longitude -97.2778, altitude 389 m
sweep 0: azimuth_surveillance at 0.48 deg, 720 rays, 1832 gates from 2.125 km every 250.0 m
  DBZH     Reflectivity [dBZ], 720 x 1832 uint8
  ZDR      DifferentialReflectivity [dB], 720 x 1192 uint16
  PHIDP    DifferentialPhase [degree], 720 x 1192 uint16
  RHOHV    CorrelationCoefficient [1], 720 x 1192 uint8
  CCORH    ClutterCorrection [dB], 720 x 1832 uint8
```

The storage types are the source's own: NEXRAD moments stay 8- and 16-bit
codes, and a field can have fewer gates than its sweep's range coordinate
(the dual-polarization moments above end at 1192 gates). See
[the data model](data-model.md) for how to turn codes into physical values.

`io::read_supported_volume_bytes` recognizes, in this order: DORADE
sweepfiles, HDF5 (read as ODIM_H5), classic netCDF (read as CfRadial 1), JMA
GRIB2 tar archives, and otherwise NEXRAD Level II. It first removes a gzip
wrapper or a single-member ZIP record. `io::sniff_supported_volume_format`
tells which decoder a buffer goes to without decoding it.
`io::read_supported_volume_with_metadata` also returns the format's metadata
(below).

**netCDF-4 files are not read yet.** A netCDF-4 file is an HDF5 container,
so the router sends it to the ODIM_H5 decoder, which cannot read it. That
covers most CfRadial 1 files written today and every CfRadial 2 / FM301
file. The ODIM_H5 decoder reads HDF5 superblock versions 0 and 1 only, and
the public netCDF-4 CfRadial files checked so far use version 2. On the netCDF-4 copy of the
XSAPR fixture (`testdata/files/other/cfradial/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc`)
the router returns `IoError::Odim` with `InvalidMessage` at offset 8:
"HDF5 superblock version 2 (1.10+ 'latest' layout) is unsupported", followed
by a hint to convert the file. Convert such a file to classic netCDF first
(for example `nccopy -k classic in.nc out.nc` from the netCDF utilities) and
read the copy: the classic copy of the same fixture,
`cfrad.xsapr_sgp_ppi_20110520.classic.nc`, reads as CfRadial 1. An ODIM_H5
file written with the HDF5 "latest" layout fails the same way. A native
netCDF-4 and modern HDF5 reader is planned.

## Format-specific decoders

Call a format's module directly to skip the sniffing, to read a file by path,
or to use options the router does not have:

| Format | Module (feature) | Entry points |
|---|---|---|
| NEXRAD Level II | `nexrad` | `read_volume_from_path`, `read_volume_from_bytes`, `read_volume_with_metadata`, `read_gzip_volume_from_reader`; previews of a partial download |
| NEXRAD and TDWR Level III | `level3` | `decode_product`, `decode_message`, `read_level3_volume` |
| ODIM_H5 (HDF5 superblock 0 or 1) | `odim` | `read_odim_h5_volume`; Cartesian products: `decode_odim_h5_cartesian_max` |
| CfRadial 1 (classic netCDF only; not netCDF-4) | `cfradial` | `read_cfradial1_volume` |
| DORADE | `dorade` | `read_dorade_sweep_volume`, `read_dorade_volume_from_slices`, `read_dorade_volume_from_paths`; mobile-radar archives: `read_mobile_archive_from_path`, `read_mobile_dir_from_path` |
| JMA radar GRIB2 tar | `jma` | `read_jma_tar_volumes` (every station, or one by `site_filter`), `read_jma_tar_first_station` |

A JMA tar holds one GRIB2 member per radar of the national network. The
router decodes the first station only; call `jma::read_jma_tar_volumes` with a
station (`Some("ITOK")`, or its number, `Some("RS47937")`) or with `None` for
all of them.

A DORADE sweepfile holds one sweep. `dorade::read_dorade_volume_from_paths`
assembles a volume from several sweepfiles, and `io::read_mobile_archive_from_path`
reads a whole mobile-radar deployment archive (DORADE and Level II members).

## Level III products

Level III products are not radar volumes: a product can be a radial image, a
raster, a table, text, a wind profile or a set of graphic symbols.
`level3::decode_product(&bytes)` returns a `Level3Product` with the WMO
heading, the message and product description headers and every symbology,
graphic and tabular block. `level3::read_level3_volume(&bytes)` converts a
radial or raster product into a one-sweep `Volume`. The router does not read
Level III.

## Format metadata

A Level II file carries metadata messages that have no place in FM301: the
volume coverage pattern (Message 5), adaptation data (Message 18), clutter
filter maps (Messages 13 and 15), RDA status (Message 2) and the constant
blocks of every Message 31 radial. `nexrad::read_volume_with_metadata`
returns them beside the volume (`NexradVolume { volume, metadata }`).
Values that do fit the model (the VCP number, Nyquist velocities, calibration
constants) are in the volume itself. For the other formats, attributes the
model has no slot for are kept verbatim in the `other` lists (`attrs.other`,
`Sweep::other`, `FieldAttrs::other`) and in `extra_vars`.

## Errors and limits

Every decoder returns a typed error (`NexradError`, `OdimError`,
`CfRadialError`, `DoradeError`, `JmaError`, `Level3Error`; the router wraps
them in `IoError`) and never panics on malformed input. Every decoder also
bounds what a file can make it allocate: expanded sizes, gates per ray, rays,
sweeps and total decoded bytes. A file over a limit fails with an error
rather than exhausting memory: `LimitExceeded` for the Level II, ODIM_H5,
CfRadial, DORADE and JMA decoders; for Level III, `DecompressedTooLarge`
(decompressed data) or `InvalidPacket` (a radial or raster packet over its
cell or grid limit). The shared limits are in
`model::bounded_read`, and each decoder crate documents its own in a
`# Limits` section. See [Conventions](conventions.md#errors).

## Without a file system

The byte entry points (`read_supported_volume_bytes`, `read_volume_from_bytes`,
...) need no file system and work on `wasm32-unknown-unknown`. The path entry
points return an I/O error there.
