//! The facade's default feature set runs a decode -> filter -> correct ->
//! map pipeline on a real volume using only `recast_radar_tools` paths.
//!
//! Input: testdata `odim-espdg-20260707-1927-pvol-dbzh-vradh` (committed), an
//! AEMET Perdiguera (Spain) C-band Doppler PVOL from 2026-07-07 with two
//! sweeps of DBZH + VRADH. Provenance is in `testdata/other/manifest.toml`.

use std::error::Error;

use recast_radar_tools::core::{ElevationCut, MomentGrid, MomentType, RadarVolume};
use recast_radar_tools::{correct, filters, io, map, odim};

type TestResult = Result<(), Box<dyn Error>>;

const ESPDG: &str = "odim-espdg-20260707-1927-pvol-dbzh-vradh";

fn espdg_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(recast_radar_testdata::bytes(ESPDG)?)
}

fn decode() -> Result<RadarVolume, Box<dyn Error>> {
    Ok(io::decode_supported_volume_bytes(&espdg_bytes()?)?)
}

fn moment<'a>(cut: &'a ElevationCut, moment: &MomentType) -> Result<&'a MomentGrid, String> {
    cut.moments
        .get(moment)
        .ok_or_else(|| format!("cut at {} deg has no {moment:?}", cut.elevation_deg))
}

fn values(grid: &MomentGrid) -> Vec<f32> {
    let rows = grid.radial_indices.len();
    let gates = grid.gate_range.gate_count;
    let mut out = Vec::with_capacity(rows * gates);
    for row in 0..rows {
        for gate in 0..gates {
            out.push(grid.scaled_value(row, gate).unwrap_or(f32::NAN));
        }
    }
    out
}

fn finite_max(grid: &MomentGrid) -> f32 {
    values(grid)
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(f32::NEG_INFINITY, f32::max)
}

fn assert_same_geometry(a: &MomentGrid, b: &MomentGrid, what: &str) {
    assert_eq!(a.radial_indices, b.radial_indices, "{what}: radial rows");
    assert_eq!(a.gate_range, b.gate_range, "{what}: gate layout");
}

#[test]
fn io_module_routes_to_the_odim_module_decoder() -> TestResult {
    let routed = decode()?;
    let direct = odim::decode_odim_h5_volume(&espdg_bytes()?)?;
    assert_eq!(routed.site.id, "ESPDG");
    assert_eq!(direct.site.id, routed.site.id);
    assert_eq!(routed.cuts.len(), 2);
    assert_eq!(direct.cuts.len(), routed.cuts.len());
    Ok(())
}

#[test]
fn filters_smoothing_keeps_coverage_and_range_on_real_reflectivity() -> TestResult {
    let volume = decode()?;
    let cut = volume.cuts.first().ok_or("no cuts")?;
    let dbzh = moment(cut, &MomentType::Reflectivity)?;
    let smoothed = filters::smooth_moment_grid(dbzh);
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
    let volume = decode()?;
    for (index, cut) in volume.cuts.iter().enumerate() {
        let vradh = moment(cut, &MomentType::Velocity)?;
        let nyquist = cut
            .radials
            .first()
            .and_then(|radial| radial.nyquist_velocity_mps)
            .ok_or("ESPDG radials carry a Nyquist velocity")?;
        let dealiased = correct::dealias_velocity_grid(cut, vradh);
        assert_same_geometry(vradh, &dealiased, "dealiased VRADH");
        assert_eq!(dealiased.moment, MomentType::Velocity);

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
                "cut {index} gate {gate}: {r} -> {d} is not a 2*Nyquist unfold"
            );
        }
        assert!(compared > 0, "cut {index}: no velocity gates compared");
    }
    Ok(())
}

#[test]
fn map_composite_is_at_least_the_lowest_sweep_maximum() -> TestResult {
    let volume = decode()?;
    let cut = volume.cuts.first().ok_or("no cuts")?;
    let base = moment(cut, &MomentType::Reflectivity)?;
    let composite = map::composite_reflectivity_grid(&volume).ok_or("no composite")?;
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
