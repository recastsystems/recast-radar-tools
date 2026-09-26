# Processing

The algorithm modules take model types in and give model types out: a
function reads a `Sweep` or a `Volume` and returns a new `Field` (physical
`f32` values, NaN for no data) on the same rays and gates, which you can add
to the sweep with `Sweep::add_field` and then read, render or pass on like
any decoded field.

## Dealiasing velocity

A Doppler radar measures radial velocity only within plus or minus the
Nyquist velocity; faster winds fold into that interval. Dealiasing unfolds
them (feature `correct`).

<!-- example: crates/recast-radar-tools/examples/dealias_velocity.rs -->
```rust
//! Dealias (unfold) the Doppler velocity of every sweep in a Level II file.
//!
//! cargo run --release -p recast-radar-tools --example dealias_velocity -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::model::Quantity;
use recast_radar_tools::{correct, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);
    let mut volume = nexrad::read_volume_from_path(&path)?;

    for sweep in &mut volume.sweeps {
        let Some(raw) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        // Region-based unfolding. The result (VRADDH) has the same rays and
        // gates as the source field.
        let dealiased = correct::dealias_velocity(sweep, raw);

        let mut unfolded = 0;
        let (rows, gates) = raw.shape();
        for row in 0..rows {
            for gate in 0..gates {
                let before = raw.value(row, gate).unwrap_or(f32::NAN);
                let after = dealiased.value(row, gate).unwrap_or(f32::NAN);
                if (after - before).abs() > 1.0 {
                    unfolded += 1;
                }
            }
        }
        let nyquist = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|values| values.first().copied());
        println!(
            "{:>5.2} deg: Nyquist {:.1} m/s, {unfolded} gates unfolded",
            sweep.fixed_angle_deg,
            nyquist.unwrap_or(f32::NAN)
        );

        // Keep the dealiased field beside the raw velocity.
        sweep.add_field(dealiased)?;
    }
    Ok(())
}
```

On `KTLX20240315_000217_V06` (first lines):

<!-- output-head: dealias_velocity testdata:l2-ktlx-20240315-000217 -->
```text
 0.48 deg: Nyquist 23.8 m/s, 1833 gates unfolded
 0.88 deg: Nyquist 23.8 m/s, 2459 gates unfolded
 0.48 deg: Nyquist 23.8 m/s, 1878 gates unfolded
```

`correct` has three engines:

- `dealias_velocity(sweep, field)`: region-based unfolding of one sweep. It
  needs the rays' Nyquist velocity (`sweep.ray_vars.nyquist_velocity_mps`);
  `dealias_skipped_no_nyquist` tells when a sweep has none.
- `dealias_volume` and `dealias_velocity_v4`: a volume engine anchored to an
  environmental wind profile (`EnvironmentalWindProfile`) and optionally to
  the previous volume (`TemporalPrior`), with per-gate confidence.
- `dealias_velocity_pyart_region`: a port of Py-ART's region-based
  dealiaser, for results that match Py-ART.

## Filters and smoothing

Feature `filters`:

- `apply_reflectivity_gate_filter` masks any field of a sweep where the
  sweep's reflectivity at the same range is missing or below a threshold
  (for example velocity in clear air), sampling reflectivity by true range
  when the two fields have different gate spacings.
- `smooth_field` smooths a field on its polar grid without growing its
  coverage.
- `upsample_field` interpolates a coarse sweep onto a finer polar grid for
  display, with guards that keep velocity folds and melting-layer
  correlation minima from blending.

## Column products and composites

Feature `map`. The column products look down through every tilt of the
volume above each gate of the lowest tilt. A tilt is a sweep at one
elevation; RHIs are skipped:

<!-- example: crates/recast-radar-tools/examples/composite.rs -->
```rust
//! Column products of a radar volume: composite reflectivity, echo tops and
//! vertically integrated liquid, with the maximum of each and a PNG of the
//! composite.
//!
//! cargo run --release -p recast-radar-tools --features render \
//!     --example composite -- <radar-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::model::{Field, Sweep};
use recast_radar_tools::render::{self, RasterOptions};
use recast_radar_tools::{io, map};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <radar-file> <out-dir>".into());
    };
    let mut volume = io::read_supported_volume_bytes(&std::fs::read(&input)?)?;

    // Each product is a field on the rays and gates of the lowest tilt with
    // reflectivity, the column base. An RHI is not a tilt: on a volume of
    // RHIs only there is no base and no product.
    let base = map::column_base_sweep(&volume).ok_or("no tilt has reflectivity")?;
    let composite = map::composite_reflectivity(&volume).ok_or("no composite")?;
    let tops = map::echo_top(&volume, map::ECHO_TOP_THRESHOLD_DBZ).ok_or("no echo tops")?;
    let vil = map::vil(&volume).ok_or("no VIL")?;

    // The base is the sweep with the lowest `tilt_elevation_deg`, the
    // elevation the antenna scanned at, which can differ from the cut angle
    // in `fixed_angle_deg`.
    let sweep = &volume.sweeps[base];
    let elevation_deg = volume.tilt_elevation_deg(base).unwrap_or(f32::NAN);
    println!("column base: sweep {base} at {elevation_deg:.2} deg");
    print_maximum(sweep, &composite, "dBZ");
    print_maximum(sweep, &tops, "m");
    print_maximum(sweep, &vil, "kg m-2");

    // Added to the base sweep, the composite draws like any other field.
    let name = composite.name.clone();
    volume.sweeps[base].add_field(composite)?;
    let path = out_dir.join("composite.png");
    render::render_field_png(&volume, base, &name, &path, RasterOptions::default())?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The largest value of a column product and where it is.
fn print_maximum(sweep: &Sweep, field: &Field, units: &str) {
    let (rays, gates) = field.shape();
    let maximum = (0..rays)
        .flat_map(|ray| (0..gates).map(move |gate| (ray, gate)))
        .filter_map(|(ray, gate)| field.value(ray, gate).map(|value| (value, ray, gate)))
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let Some((value, ray, gate)) = maximum else {
        println!("{}: no values", field.name);
        return;
    };
    let range_km = field
        .native_geometry(&sweep.range)
        .map_or(f64::NAN, |(first, spacing)| {
            (first + gate as f64 * spacing) / 1000.0
        });
    println!(
        "{}: maximum {value:.1} {units} at azimuth {:.1} deg, {range_km:.1} km",
        field.name, sweep.rays.azimuth_deg[ray]
    );
}
```

On `KTLX20240315_000217_V06`:

<!-- output: composite testdata:l2-ktlx-20240315-000217 dir:out -->
```text
column base: sweep 4 at 0.41 deg
CREF: maximum 72.5 dBZ at azimuth 135.8 deg, 95.6 km
ET: maximum 19366.5 m at azimuth 97.7 deg, 362.9 km
VIL: maximum 75.3 kg m-2 at azimuth 167.7 deg, 236.1 km
wrote out/composite.png
```

Echo tops far from the radar are high because even the lowest beam is high
there; a column product is only as good as the volume's vertical sampling.

`map` also has hail products (`hail`, `poh`, `mehs`), `vil_density`, vertical
cross sections along a line (`reflectivity_section`, `velocity_section`,
`field_section`), RHI panels, and `box_resample` onto a regular 3-D grid.

## Derived products

Feature `retrieve`:

- `derive_product(sweep, product, config)` computes one product of
  `DerivedSweepProduct`: KDP and filtered differential phase, attenuation
  and corrected reflectivity, rain rates, textures, hail and turbulence
  signatures, a meteorological gate mask. `derive_sweep_in_place` and
  `derive_volume_in_place` add a configured set to every sweep.
- `cappi`, `column_max` and the echo base, depth and height products work
  on the whole volume.
- `azimuthal_shear` and `radial_divergence` (linear least squares
  derivatives of velocity), `detect_rotation_sites` (mesocyclone and
  tornado vortex candidates), `compute_vwp` (a VAD wind profile), and
  `find_center_and_retrieve` (GBVTD tropical cyclone circulation).

## Tracking

Feature `track`: `identify_storm_cells` finds cells in a volume,
`StormTracker` follows them from volume to volume, `value_swath` accumulates
the maximum of a field over time (hail and rotation swaths), and the
`tracks` module builds rotation tracks.
