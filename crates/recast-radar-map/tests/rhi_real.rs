//! Native RHI panels on real RHI sweeps, and the RHI heuristics on real PPIs.
//!
//! Inputs: the FARM DOW8 CfRadial RHI (open-radar-data, 3 of 8 fields in a
//! classic netCDF container), the FARM DOW6 DORADE RHI from the Marshall Fire
//! (first 41 rays, 6 of them antenna transition), and trimmed NEXRAD Level II
//! PPI sweeps. Expected values: `testdata/golden/map/rhi.json`, written by
//! `tools/filters_map_golden.py rhi` from netCDF4 1.7.4, a standalone DORADE
//! block walker, MetPy 1.7.1 and Py-ART 2.2.5, with a 4/3-Earth panel
//! reference (every pixel inverse-mapped to slant range and elevation, nearest
//! gate, nearest beam within 1 degree).

mod common;

use common::{array, as_f64, as_i64, as_usize, assert_close, golden};
use recast_radar_core::{ElevationCut, MomentType, RadarVolume, ScanMode};
use recast_radar_map::{
    CrossSection, cut_looks_like_rhi, rhi_coverage_range_m, rhi_coverage_top_m,
    rhi_fixed_azimuth_deg, rhi_section,
};
use serde_json::Value;

/// Decode an RHI fixture and check the decoded sweep against the golden
/// geometry read from the file (ray count, gate geometry, elevations).
fn rhi_volume(key: &str) -> Option<(Value, RadarVolume)> {
    let golden = golden("map/rhi.json")[key].clone();
    let id = golden["id"].as_str().expect("id").to_owned();
    let path = match recast_radar_testdata::path(&id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let volume = if key == "dow8" {
        common::cfradial(&path)
    } else {
        common::dorade(&path)
    };
    assert_eq!(volume.cuts.len(), 1, "{id}");
    let cut = &volume.cuts[0];
    assert_eq!(cut.radials.len(), as_usize(&golden["rays"]), "{id} rays");
    let grid = cut.moments.get(&MomentType::Reflectivity).expect("REF");
    let gates = &grid.gate_range;
    assert_eq!(
        i64::from(gates.first_gate_m),
        as_i64(&golden["first_gate_m"])
    );
    assert_eq!(
        i64::from(gates.gate_spacing_m),
        as_i64(&golden["gate_spacing_m"])
    );
    assert_eq!(gates.gate_count, as_usize(&golden["gate_count"]));
    let (min, max) = cut
        .radials
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), radial| {
            (lo.min(radial.elevation_deg), hi.max(radial.elevation_deg))
        });
    assert_close(
        f64::from(min),
        as_f64(&golden["elevation_min_deg"]),
        1e-5,
        "min elevation",
    );
    assert_close(
        f64::from(max),
        as_f64(&golden["elevation_max_deg"]),
        1e-5,
        "max elevation",
    );
    Some((golden, volume))
}

fn panel(cut: &ElevationCut, golden: &Value) -> CrossSection {
    let grid = cut.moments.get(&MomentType::Reflectivity).expect("REF");
    rhi_section(
        cut,
        grid,
        as_usize(&golden["width"]),
        as_usize(&golden["height"]),
        as_f64(&golden["top_m"]) as f32,
        as_f64(&golden["max_range_m"]) as f32,
    )
    .expect("panel")
}

/// Per panel row: filled pixel count and value sum equal the reference; the
/// total count of empty pixels is exactly the reference's pixels without data,
/// beyond the gates, or without a beam within 1 degree.
fn assert_panel_rows(section: &CrossSection, golden: &Value, what: &str) {
    let row_valid = array(&golden["row_valid"]);
    let row_sum = array(&golden["row_sum"]);
    for y in 0..section.height {
        let row = &section.values[y * section.width..(y + 1) * section.width];
        let (count, sum) = row
            .iter()
            .filter(|value| value.is_finite())
            .fold((0usize, 0.0f64), |(count, sum), value| {
                (count + 1, sum + f64::from(*value))
            });
        assert_eq!(count, as_usize(&row_valid[y]), "{what} panel row {y}");
        assert_close(
            sum,
            as_f64(&row_sum[y]),
            1e-2,
            &format!("{what} panel row {y} sum"),
        );
    }
    let empty = section
        .values
        .iter()
        .filter(|value| !value.is_finite())
        .count();
    let expected = as_usize(&golden["no_data_pixels"])
        + as_usize(&golden["beyond_gates_pixels"])
        + as_usize(&golden["no_beam_pixels"]);
    assert_eq!(empty, expected, "{what} empty pixels");
}

/// Panel pixels hold the value of the beam and gate the 4/3-Earth geometry
/// maps them to: sampled pixels equal the file's value at the reference's ray
/// and gate, and every panel row matches the reference (DOW8 768 x 320 over
/// 60 km x 15 km as the app draws it; DOW6 400 x 200 over 50 km x 26 km).
#[test]
fn rhi_section_samples_the_matching_beam() {
    for key in ["dow8", "dow6"] {
        let Some((golden, volume)) = rhi_volume(key) else {
            return;
        };
        let cut = &volume.cuts[0];
        let grid = cut.moments.get(&MomentType::Reflectivity).expect("REF");
        let spec = &array(&golden["panels"])[0];
        let section = panel(cut, spec);
        assert_eq!(section.values.len(), section.width * section.height);
        let samples = array(&spec["samples"]);
        assert_eq!(samples.len(), 150);
        for sample in samples {
            let sample = array(sample);
            let (x, y) = (as_usize(&sample[0]), as_usize(&sample[1]));
            let (row, gate) = (as_usize(&sample[3]), as_usize(&sample[4]));
            let expected = as_f64(&sample[5]);
            let actual = f64::from(section.values[y * section.width + x]);
            assert_close(actual, expected, 1e-4, &format!("{key} pixel ({x}, {y})"));
            let decoded = grid.scaled_value(row, gate).map(f64::from);
            assert_close(decoded.expect("gate"), expected, 1e-4, "decoded gate");
        }
        assert_panel_rows(&section, spec, key);
    }
}

/// Pixels that no beam reaches within 1 degree stay empty: above DOW8's
/// 70 degree top beam, and above DOW6's 30 degree top beam or below its
/// 13 degree lowest beam.
#[test]
fn rhi_section_is_empty_above_the_top_beam() {
    for key in ["dow8", "dow6"] {
        let Some((golden, volume)) = rhi_volume(key) else {
            return;
        };
        let spec = &array(&golden["panels"])[0];
        let section = panel(&volume.cuts[0], spec);
        assert!(as_usize(&spec["no_beam_pixels"]) > 10_000, "{key}");
        let samples = array(&spec["no_beam_samples"]);
        assert_eq!(samples.len(), 50);
        for pixel in samples {
            let pixel = array(pixel);
            let (y, x) = (as_usize(&pixel[0]), as_usize(&pixel[1]));
            assert!(
                section.values[y * section.width + x].is_nan(),
                "{key} pixel ({x}, {y}) has no beam within 1 degree"
            );
        }
        assert_panel_rows(&section, spec, key);
    }
    // DOW8 spot check from the app's panel: 12 km up at 3 km out needs about
    // 76 degrees of elevation.
    let Some((golden, volume)) = rhi_volume("dow8") else {
        return;
    };
    let spec = &array(&golden["panels"])[0];
    let section = panel(&volume.cuts[0], spec);
    let x = (3_000.0f32 / 60_000.0 * (section.width - 1) as f32).round() as usize;
    let y = ((1.0 - 12_000.0f32 / 15_000.0) * (section.height - 1) as f32).round() as usize;
    assert!(section.values[y * section.width + x].is_nan());
}

/// DOW8 has 950 gates of 125 m (118.75 km of slant range): a 130 km panel is
/// empty past the last gate on every beam.
#[test]
fn rhi_section_is_empty_beyond_gate_coverage() {
    let Some((golden, volume)) = rhi_volume("dow8") else {
        return;
    };
    let spec = &array(&golden["panels"])[1];
    assert_eq!(as_f64(&spec["max_range_m"]), 130_000.0);
    let section = panel(&volume.cuts[0], spec);
    assert!(as_usize(&spec["beyond_gates_pixels"]) > 1000);
    let samples = array(&spec["beyond_gates_samples"]);
    assert_eq!(samples.len(), 50);
    for pixel in samples {
        let pixel = array(pixel);
        let (y, x) = (as_usize(&pixel[0]), as_usize(&pixel[1]));
        assert!(
            section.values[y * section.width + x].is_nan(),
            "pixel ({x}, {y})"
        );
    }
    // The last column is 130 km out, past 118.75 km on every beam.
    for y in 0..section.height {
        assert!(section.values[y * section.width + section.width - 1].is_nan());
    }
    assert_panel_rows(&section, spec, "dow8 130 km");
}

/// The geometric RHI test accepts the real RHIs (CfRadial sweep_mode "rhi",
/// DORADE RADD scan mode 3) and rejects every NEXRAD PPI sweep; the fixed
/// azimuth is the circular mean of the file's ray azimuths.
#[test]
fn rhi_heuristic_accepts_elevation_sweeps_and_rejects_ppi() {
    for key in ["dow8", "dow6"] {
        let Some((golden, volume)) = rhi_volume(key) else {
            return;
        };
        match key {
            "dow8" => assert_eq!(golden["sweep_mode"], "rhi"),
            _ => assert_eq!(as_i64(&golden["radd_scan_mode"]), 3),
        }
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Rhi), "{key}");
        let cut = &volume.cuts[0];
        assert!(as_f64(&golden["elevation_max_deg"]) - as_f64(&golden["elevation_min_deg"]) > 10.0);
        assert!(as_f64(&golden["azimuth_spread_deg"]) < 3.0);
        assert!(cut_looks_like_rhi(cut), "{key}");
        assert_close(
            f64::from(rhi_fixed_azimuth_deg(cut)),
            as_f64(&golden["circular_mean_azimuth_deg"]),
            1e-3,
            &format!("{key} fixed azimuth"),
        );
    }

    let golden = golden("map/rhi.json");
    let mut rejected = 0;
    for sweep in array(&golden["ppi"]) {
        assert_eq!(sweep["scan_type"], "ppi");
        let id = sweep["id"].as_str().expect("id");
        let path = recast_radar_testdata::require_file!(id);
        let volume = common::level2(&path);
        let cut = &volume.cuts[as_usize(&sweep["sweep"])];
        assert_eq!(cut.radials.len(), as_usize(&sweep["rays"]));
        assert!(!cut_looks_like_rhi(cut), "{id} sweep {}", sweep["sweep"]);
        rejected += 1;
    }
    assert_eq!(rejected, 6);

    // Every radial of the 17-tilt KTLX 2013 volume in one cut: 19 degrees of
    // elevation, but azimuths all around the circle.
    let flat = &golden["flattened_volume"];
    assert!(as_f64(&flat["elevation_max_deg"]) - as_f64(&flat["elevation_min_deg"]) > 10.0);
    assert!(as_f64(&flat["azimuth_spread_deg"]) > 3.0);
    let path = recast_radar_testdata::require_file!(flat["id"].as_str().expect("id"));
    let mut volume = common::level2(&path);
    assert_eq!(volume.cuts.len(), as_usize(&flat["sweeps"]));
    let mut cut = volume.cuts.swap_remove(0);
    for other in &volume.cuts {
        cut.radials.extend(other.radials.iter().cloned());
    }
    assert_eq!(cut.radials.len(), as_usize(&flat["rays"]));
    assert!(!cut_looks_like_rhi(&cut));

    // Fewer than 8 rays is not enough to call an RHI: every 22nd DOW8 ray
    // (7 rays up to 62 degrees) is rejected, every 21st (8 rays) accepted.
    let Some((_, mut dow8)) = rhi_volume("dow8") else {
        return;
    };
    let cut = &mut dow8.cuts[0];
    let all = cut.radials.clone();
    for (step, rays, accepted) in [(21, 8, true), (22, 7, false)] {
        cut.radials = all.iter().step_by(step).cloned().collect();
        assert_eq!(cut.radials.len(), rays);
        let (lo, hi) = cut
            .radials
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), r| {
                (lo.min(r.elevation_deg), hi.max(r.elevation_deg))
            });
        assert!(hi - lo > 10.0);
        assert_eq!(cut_looks_like_rhi(cut), accepted, "{rays} rays");
    }
}

/// Panel extents: the highest beam height and the furthest ground range of
/// the last gate edge, from the file's elevations and gate geometry (DOW8:
/// 70 degree top beam at 118.75 km of slant range; DOW6: 30 degrees at 50 km).
#[test]
fn rhi_coverage_extents_track_the_sweep() {
    for key in ["dow8", "dow6"] {
        let Some((golden, volume)) = rhi_volume(key) else {
            return;
        };
        let cut = &volume.cuts[0];
        let grid = cut.moments.get(&MomentType::Reflectivity).expect("REF");
        assert_close(
            f64::from(rhi_coverage_top_m(cut, grid)),
            as_f64(&golden["coverage_top_m"]),
            0.01,
            &format!("{key} coverage top"),
        );
        assert_close(
            f64::from(rhi_coverage_range_m(cut, grid)),
            as_f64(&golden["coverage_range_m"]),
            0.01,
            &format!("{key} coverage range"),
        );
    }
    let Some((golden, _)) = rhi_volume("dow8") else {
        return;
    };
    // About 112 km of height and just under the 118.75 km slant range on the ground.
    assert!((110_000.0..=119_000.0).contains(&as_f64(&golden["coverage_top_m"])));
    assert!((118_000.0..=119_000.0).contains(&as_f64(&golden["coverage_range_m"])));
}

/// Real sweeps whose azimuths cross north (KTLX 2013: 123.2 deg through
/// 2.7 deg; KEWX 2016: 274.2 deg through 33.7 deg): the fixed azimuth is the
/// circular mean, far from the arithmetic mean of the raw angles.
#[test]
fn azimuth_circular_mean_handles_north_wrap() {
    let golden = golden("map/rhi.json");
    let mut checked = 0;
    for sweep in array(&golden["ppi"]) {
        let id = sweep["id"].as_str().expect("id");
        if id == "l2-ktlx-20240315-000217-trim" {
            continue;
        }
        let path = recast_radar_testdata::require_file!(id);
        let volume = common::level2(&path);
        let cut = &volume.cuts[as_usize(&sweep["sweep"])];
        let first = f64::from(cut.radials[0].azimuth_deg);
        let last = f64::from(cut.radials[cut.radials.len() - 1].azimuth_deg);
        assert_close(
            first,
            as_f64(&sweep["azimuth_first_deg"]),
            1e-4,
            "first azimuth",
        );
        assert_close(
            last,
            as_f64(&sweep["azimuth_last_deg"]),
            1e-4,
            "last azimuth",
        );
        assert!(last < first, "{id}: the sweep crosses north");
        let mean = f64::from(rhi_fixed_azimuth_deg(cut));
        let expected = as_f64(&sweep["circular_mean_azimuth_deg"]);
        assert_close(
            mean,
            expected,
            1e-3,
            &format!("{id} sweep {}", sweep["sweep"]),
        );
        assert!((as_f64(&sweep["arithmetic_mean_azimuth_deg"]) - expected).abs() > 4.0);
        checked += 1;
    }
    assert_eq!(checked, 4);
}

/// The app's panel path on the DOW8 RHI end to end: declared scan mode, a
/// pixel-exact echo on beam 37 gate 316 through the forward 4/3-Earth model,
/// and a panel that is mostly filled below the top beam.
#[test]
fn real_dow8_rhi_drives_the_rhi_panel_pipeline() {
    let Some((golden, volume)) = rhi_volume("dow8") else {
        return;
    };
    assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Rhi));
    let cut = &volume.cuts[0];
    let grid = cut.moments.get(&MomentType::Reflectivity).expect("REF");
    let spec = &array(&golden["panels"])[0];
    let section = panel(cut, spec);
    let (width, height) = (section.width, section.height);
    let (top_m, max_range_m) = (section.top_m, section.length_m);

    let (beam, gate) = (37usize, 316usize);
    let expected = grid.scaled_value(beam, gate).expect("echo at [37, 316]");
    assert_close(
        f64::from(expected),
        as_f64(&golden["beam37_gate316_dbz"]),
        1e-4,
        "DBZHC[37, 316]",
    );
    let elevation = f64::from(cut.radials[beam].elevation_deg);
    let slant_m = f64::from(grid.gate_range.first_gate_m)
        + f64::from(grid.gate_range.gate_spacing_m) * gate as f64;
    let z = recast_radar_core::beam_height_above_radar_m(slant_m, elevation) as f32;
    let s = recast_radar_core::beam_ground_range_m(slant_m, elevation) as f32;
    assert!(z < top_m && s < max_range_m, "sample outside panel");
    let x = (s / max_range_m * (width - 1) as f32).round() as usize;
    let y = ((1.0 - z / top_m) * (height - 1) as f32).round() as usize;
    let sampled = section.values[y * width + x];
    // 60 km / 768 px = 78 m per pixel against 125 m gates: within 2 gates.
    let candidates: Vec<f32> = (gate - 2..=gate + 2)
        .filter_map(|g| grid.scaled_value(beam, g))
        .collect();
    assert!(
        candidates.contains(&sampled),
        "panel pixel {sampled} not among beam-37 gates {gate}+-2: {candidates:?}"
    );

    let filled = section
        .values
        .iter()
        .filter(|value| value.is_finite())
        .count();
    assert_eq!(filled, as_usize(&spec["data_pixels"]));
    assert!(filled as f32 / section.values.len() as f32 > 0.5);
}
