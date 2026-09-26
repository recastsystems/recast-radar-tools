# The data model

Every decoder produces a [`Volume`](../../crates/recast-radar-core/src/model/volume.rs)
from the `model` module (the `recast-radar-core` crate). The model follows
WMO FM301 (CfRadial 2), the layout xradar and Py-ART also read, so its names
are the FM301 names: a volume holds sweeps, a sweep holds rays and fields,
and a field holds one variable's values on those rays.

| Type | FM301 | Holds |
|---|---|---|
| `Volume` | root group | global attributes (`attrs`), `time_reference`, `location`, `scan` (strategy, VCP), `radar_parameters`, `radar_calibration`, `provenance` (source format, version, compression), `sweeps` |
| `Sweep` | `sweep_<n>` group | `sweep_mode`, `fixed_angle_deg`, `rays` (`time_s`, `azimuth_deg`, `elevation_deg`), the `range` coordinate, per-ray variables (`ray_vars`: Nyquist velocity, PRT, pulse width, ...), `fields` |
| `Field` | a data variable | `name`, `quantity`, `polarization`, `attrs` (units, CF and FM301 attributes), `nrays` x `ngates` values in the source's encoding, the gate mapping onto the sweep's range |

## Physical values

A field keeps the values in the source's own encoding: 8- or 16-bit codes
with a scale and offset (NEXRAD, ODIM, most CfRadial files), or floats. The
decoders never expand a whole volume to floats; you decide what to convert.

<!-- example: crates/recast-radar-tools/examples/physical_values.rs -->
```rust
//! Read physical values from a radar file: the strongest reflectivity of the
//! lowest sweep, where it is, and how the sweep's gates split into values,
//! no-echo, missing and range-folded gates.
//!
//! cargo run --release -p recast-radar-tools --example physical_values -- <radar-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::io;
use recast_radar_tools::model::{Gate, Quantity, beam_ground_range_m, beam_height_above_radar_m};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <radar-file>")?);
    let volume = io::read_supported_volume_bytes(&std::fs::read(&path)?)?;

    // The lowest sweep that has reflectivity. `tilt_elevation_deg` is the
    // elevation the antenna actually scanned at.
    let (index, sweep) = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, sweep)| sweep.find(Quantity::Reflectivity).is_some())
        .min_by(|(a, _), (b, _)| {
            let a = volume.tilt_elevation_deg(*a).unwrap_or(f32::INFINITY);
            let b = volume.tilt_elevation_deg(*b).unwrap_or(f32::INFINITY);
            a.total_cmp(&b)
        })
        .ok_or("no sweep has reflectivity")?;
    let field = sweep
        .find(Quantity::Reflectivity)
        .ok_or("no reflectivity")?;
    let elevation_deg = volume.tilt_elevation_deg(index).unwrap_or(f32::NAN);
    println!(
        "sweep {index} at {elevation_deg:.2} deg: {} ({} x {} gates)",
        field.name, field.nrays, field.ngates
    );

    // A field stores the source's packed values. `gate` resolves one of them
    // to a physical value or a sentinel; `value` keeps only physical values.
    let (mut values, mut undetect, mut missing, mut folded) = (0, 0, 0, 0);
    let mut strongest: Option<(f32, usize, usize)> = None;
    let (rays, gates) = field.shape();
    for ray in 0..rays {
        for gate in 0..gates {
            match field.gate(ray, gate) {
                Some(Gate::Value(dbz)) => {
                    values += 1;
                    if strongest.is_none_or(|(max, _, _)| dbz > max) {
                        strongest = Some((dbz, ray, gate));
                    }
                }
                Some(Gate::Undetect) => undetect += 1,
                Some(Gate::RangeFolded) => folded += 1,
                // `Missing`, and any sentinel class a later version adds (`Gate`
                // is `#[non_exhaustive]`).
                _ => missing += 1,
            }
        }
    }
    println!("{values} values, {undetect} no echo, {missing} missing, {folded} range folded");

    let (dbz, ray, gate) = strongest.ok_or("no reflectivity values")?;
    // Gate centres come from the field's native geometry on the sweep's range
    // coordinate (a field may have coarser gates than the sweep).
    let (first_m, spacing_m) = field
        .native_geometry(&sweep.range)
        .ok_or("no gate geometry")?;
    let slant_m = first_m + gate as f64 * spacing_m;
    let azimuth_deg = sweep.rays.azimuth_deg[ray];
    let ray_elevation_deg = f64::from(sweep.rays.elevation_deg[ray]);
    let ground_km = beam_ground_range_m(slant_m, ray_elevation_deg) / 1000.0;
    let height_m = beam_height_above_radar_m(slant_m, ray_elevation_deg)
        + volume.location.altitude_m.unwrap_or(0.0);
    println!(
        "strongest: {dbz:.1} dBZ at azimuth {azimuth_deg:.1} deg, {ground_km:.1} km, \
         beam centre {height_m:.0} m above sea level"
    );
    if let Some(time) = volume.ray_time(index, ray) {
        println!("ray time {time}");
    }

    // All values at once, NaN for every sentinel.
    let physical = field.to_physical();
    let mean = physical.iter().filter(|v| v.is_finite()).sum::<f32>() / values.max(1) as f32;
    println!("mean of the {values} values: {mean:.1} dBZ");
    Ok(())
}
```

On `KTLX20240315_000217_V06`:

<!-- output: physical_values testdata:l2-ktlx-20240315-000217 -->
```text
sweep 4 at 0.41 deg: DBZH (720 x 1832 gates)
283642 values, 1035398 no echo, 0 missing, 0 range folded
strongest: 70.5 dBZ at azimuth 140.2 deg, 94.4 km, beam centre 1709 m above sea level
ray time 2024-03-15 00:03:52.922 UTC
mean of the 283642 values: 10.4 dBZ
```

The ways to read values, from one gate to a whole field:

- `Field::gate(ray, gate)` returns a `Gate`: `Value(f32)`, `Undetect` (the
  radar looked and saw no echo; NEXRAD code 0, ODIM `undetect`), `Missing`
  (no data; ODIM `nodata`, CF `_FillValue`, NaN, a ray the source did not
  provide) or `RangeFolded` (NEXRAD code 1). `None` means the indices are
  outside the field.
- `Field::value(ray, gate)` is the physical value or `None`.
- `Field::to_physical()` expands the whole field row-major, NaN for every
  sentinel. `Field::lut8()` gives a 256-entry table for 8-bit fields, which
  is how the renderer decodes without expanding.
- `Field::row(ray)` borrows a row in its storage type (`RowRef::U8`, ...),
  and `field.data` is the whole buffer with its coding (`FieldData::U8 {
  values, coding }`, ...) for code that wants the packed values.

## Finding fields

Field names are the FM301 names where FM301 has one (`DBZH`, `VRADH`, `ZDR`,
`RHOHV`, `PHIDP`, `KDP`, ...), and the source's name otherwise (CfRadial
`VEL`, DORADE `DBZHC_F`), as `FieldName::Other`. Every field also has a
`Quantity`, the semantic class whatever the spelling, so code that wants
"the reflectivity" works for every format:

- `sweep.find(Quantity::Reflectivity)` returns the preferred field of that
  quantity (horizontal polarization first).
- `sweep.field(&FieldName::Dbzh)` looks a field up by name;
  `FieldName::parse("VEL")` builds a name from text.
- `field.name.info()` has the static metadata of a known name: FM301
  standard name, long name and units, and the names xradar and Py-ART use.

## Coordinates and geometry

- Rays: `sweep.rays.azimuth_deg[ray]` and `elevation_deg[ray]` in degrees,
  `time_s[ray]` in seconds since `volume.time_reference`
  (`volume.ray_time(sweep, ray)` gives the absolute time). Rays are in the
  source's order: NEXRAD starts wherever the antenna was, ODIM at north.
- Gates: `sweep.range` is the range coordinate, gate centres in metres.
  A field's gates can be coarser than, or start later than, the sweep's range
  (NEXRAD Message 1 reflectivity has 1 km gates beside 250 m Doppler gates);
  `field.native_geometry(&sweep.range)` gives the field's first gate centre
  and spacing, and `field.gates` the mapping.
- Elevation: `sweep.fixed_angle_deg` is the target angle of the scan
  strategy. `volume.tilt_elevation_deg(sweep)` is the angle the antenna
  scanned at, which is what beam heights need.
- Heights: `beam_height_above_radar_m(slant_range_m, elevation_deg)` and
  `beam_ground_range_m` use the 4/3-Earth model. `trace_refracted_beam`
  follows a beam through a measured refractivity profile instead
  (`RefractivityProfile`), and `propagation_regime` classifies a refractivity
  gradient (subrefractive, near standard, superrefractive, ducting).

## The FM301 view

`model::fm301::volume_view(&volume, ViewOptions::XRADAR, None)` presents a
volume as the FM301 group tree: groups `sweep_0`, `sweep_1`, ... with their
dimensions, variables and typed attributes, named as xradar names them
(`ViewOptions::XRADAR`) or as the FM301-2022 text does (`ViewOptions::WMO`).
Variables borrow the field storage; `Variable::values` says whether a
variable is the stored array or needs ray reordering or gate padding
(`Values::Mapped`), and `Values::materialize` builds the FM301 array. This is
the surface for writers and language bindings.

## Merging partial volumes

Some feeds publish a scan in parts: one file per product or per sweep (ODIM
feeds of several European services, split Level II products).
`model::merge_volumes(parts)` assembles them into one volume: sweeps with the
same angle and ray geometry merge their fields, other sweeps are appended,
and the `MergeReport` counts what was merged and skipped. Parts from
different source formats are rejected.

## Serialization

With the `serde` feature, `Volume` and everything in it implement
`Serialize` and `Deserialize`. A decoded volume survives a round trip
through serde_json unchanged (the test `crates/recast-radar-core/tests/serde_real.rs`
checks Level II, ODIM_H5, CfRadial 1 and JMA volumes). Two things to know
about JSON: turn on serde_json's `float_roundtrip` feature for exact f64
values, and JSON has no NaN (serde_json writes it as `null` and cannot read
it back into a float), so a volume with NaN in its float arrays needs a
binary format.

Deserializing a `Field`, `Sweep` or `Volume` checks what `Volume::seal`
checks (below), so a document from elsewhere cannot make a field claim
more gates than it holds: each field holds `nrays × ngates` values, its
absent rows ascend below `nrays`, every field of a sweep has a row for every
ray, every per-ray array has one entry per ray, and `sweeps[i].sweep_number`
is `i`. It also holds each sweep's range coordinate, and the range gates each
field covers on it, to `bounded_read::MAX_GATES_PER_RADIAL` (16,384), the
ceiling every decoder applies to a radial: a uniform range is three numbers,
and without the limit a few bytes could claim billions of gates, which the
FM301 view would allocate as its `range` array and map every field onto.
Each extra variable (`ExtraVariable`, of a sweep or of the volume) must name
one dimension per `shape` entry, and `shape` must multiply out to the number
of values it holds: the view declares the variable's dimensions from
`shape`. A document that fails is a serde error. Serialize a volume you have
built or changed after calling `seal`, as decoders do; the other types
(`Rays`, `FieldData`, the attribute structs) deserialize without checks of
their own and are checked when the sweep or volume that holds them is.

## Building a volume

Decoders build volumes with `Volume::new`, `Sweep::new`, `Sweep::push_ray`,
`Field::new` and the row pushers, then call `Volume::seal`, which checks the
invariants (every per-ray array has one entry per ray, sweep numbers match
their positions, fields fit the range). The same calls build a volume from
data of your own.
