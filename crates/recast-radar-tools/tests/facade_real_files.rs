//! The facade's default feature set runs a decode -> filter -> correct ->
//! map pipeline on a real volume using only `recast_radar_tools` paths.
//!
//! Input: testdata `odim-espdg-20260707-1927-pvol-dbzh-vradh` (committed), an
//! AEMET Perdiguera (Spain) C-band Doppler PVOL from 2026-07-07 with two
//! sweeps of DBZH + VRADH. Provenance is in `testdata/other/manifest.toml`.

use std::error::Error;

use recast_radar_tools::core::{Field, Quantity, Sweep, Volume};
use recast_radar_tools::{correct, filters, io, map, odim};

type TestResult = Result<(), Box<dyn Error>>;

const ESPDG: &str = "odim-espdg-20260707-1927-pvol-dbzh-vradh";

fn espdg_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(recast_radar_testdata::bytes(ESPDG)?)
}

fn decode() -> Result<Volume, Box<dyn Error>> {
    Ok(io::read_supported_volume_bytes(&espdg_bytes()?)?)
}

fn field(sweep: &Sweep, quantity: Quantity) -> Result<&Field, String> {
    sweep
        .find(quantity)
        .ok_or_else(|| format!("sweep at {} deg has no {quantity:?}", sweep.fixed_angle_deg))
}

/// The lowest sweep (the file stores 1.5 deg before 0.5 deg).
fn lowest(volume: &Volume) -> Result<&Sweep, String> {
    volume
        .sweeps
        .iter()
        .min_by(|a, b| a.fixed_angle_deg.total_cmp(&b.fixed_angle_deg))
        .ok_or_else(|| "no sweeps".to_owned())
}

fn values(field: &Field) -> Vec<f32> {
    field.to_physical()
}

fn finite_max(field: &Field) -> f32 {
    values(field)
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(f32::NEG_INFINITY, f32::max)
}

fn assert_same_geometry(a: &Field, b: &Field, what: &str) {
    assert_eq!(a.shape(), b.shape(), "{what}: shape");
    assert_eq!(a.absent_rows, b.absent_rows, "{what}: absent rows");
    assert_eq!(a.gates, b.gates, "{what}: gate mapping");
}

#[test]
fn io_module_routes_to_the_odim_module_decoder() -> TestResult {
    let routed = decode()?;
    let direct = odim::read_odim_h5_volume(&espdg_bytes()?)?;
    assert_eq!(routed.attrs.instrument_name, "ESPDG");
    assert_eq!(direct.attrs.instrument_name, routed.attrs.instrument_name);
    assert_eq!(routed.sweeps.len(), 2);
    assert_eq!(direct.sweeps.len(), routed.sweeps.len());
    Ok(())
}

#[test]
fn filters_smoothing_keeps_coverage_and_range_on_real_reflectivity() -> TestResult {
    let volume = decode()?;
    let sweep = lowest(&volume)?;
    let dbzh = field(sweep, Quantity::Reflectivity)?;
    let smoothed = filters::smooth_field(dbzh);
    assert_same_geometry(dbzh, &smoothed, "smoothed DBZH");

    let raw = values(dbzh);
    let smooth = values(&smoothed);
    let (lo, hi) = raw
        .iter()
        .filter(|v| v.is_finite())
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    assert!(lo <= hi, "real DBZH plane has valid gates");
    for (i, (r, s)) in raw.iter().zip(&smooth).enumerate() {
        assert_eq!(r.is_finite(), s.is_finite(), "coverage changed at {i}");
        if s.is_finite() {
            assert!(
                (lo - 1e-3..=hi + 1e-3).contains(s),
                "smoothed {s} outside raw range [{lo}, {hi}] at {i}"
            );
        }
    }
    Ok(())
}

#[test]
fn correct_dealiasing_only_adds_nyquist_intervals_on_real_velocity() -> TestResult {
    let mut volume = decode()?;
    // AEMET's exporter copies the reflectivity sentinels onto the velocity
    // plane; the opt-in recovery masks its no-echo 0 m/s fill.
    odim::recover_copied_whatgroup_velocity_nodata(&mut volume);
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        let vradh = field(sweep, Quantity::RadialVelocity)?;
        let nyquist = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|values| values.first().copied())
            .ok_or("ESPDG rays carry a Nyquist velocity")?;
        let dealiased = correct::dealias_velocity(sweep, vradh);
        assert_same_geometry(vradh, &dealiased, "dealiased VRADH");
        assert_eq!(dealiased.quantity, Quantity::DealiasedRadialVelocity);

        let raw = values(vradh);
        let unfolded = values(&dealiased);
        let mut compared = 0usize;
        for (gate, (r, d)) in raw.iter().zip(&unfolded).enumerate() {
            if !(r.is_finite() && d.is_finite()) {
                continue;
            }
            compared += 1;
            let intervals = (d - r) / (2.0 * nyquist);
            assert!(
                (intervals - intervals.round()).abs() < 1e-3,
                "sweep {index} gate {gate}: {r} -> {d} is not a 2*Nyquist unfold"
            );
        }
        assert!(compared > 0, "sweep {index}: no velocity gates compared");
    }
    Ok(())
}

#[test]
fn map_composite_is_at_least_the_lowest_sweep_maximum() -> TestResult {
    let volume = decode()?;
    let sweep = lowest(&volume)?;
    let base = field(sweep, Quantity::Reflectivity)?;
    let composite = map::composite_reflectivity(&volume).ok_or("no composite")?;
    assert_same_geometry(base, &composite, "composite");

    let base_max = finite_max(base);
    let composite_max = finite_max(&composite);
    assert!(base_max.is_finite(), "lowest sweep has valid reflectivity");
    assert!(
        composite_max >= base_max,
        "column max {composite_max} below lowest-sweep max {base_max}"
    );
    Ok(())
}
