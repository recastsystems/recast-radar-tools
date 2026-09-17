//! Max-value swaths over real consecutive sweeps, and base-tilt selection on
//! real split-cut volumes.
//!
//! Expected values: `testdata/golden/track/swath.json`, written by
//! `tools/track_golden.py` (section `swath`): the per-gate maximum / signed
//! extreme of two consecutive NOXP sector sweeps (DORADE walker) mapped onto the
//! reference sweep by the documented 0.1-degree nearest-azimuth rule, and the
//! per-sweep moment lists and first-ray elevations of two trimmed Level II
//! volumes from MetPy.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    array, as_f64, as_i64, as_str, as_usize, assert_grid_matches, assert_samples, cell, dorade,
    golden, level2, noxp_sweeps, sweep_bytes,
};
use recast_radar_core::{Field, FieldName, Quantity, Volume};
use recast_radar_testdata::require_file;
use recast_radar_track::{SwathAggregation, base_tilt_sweep, value_swath};

/// The two consecutive NOXP 0.5 deg sector sweeps of 2009-05-25 20:35:29 and
/// 20:36:59Z named by the golden (171 rays each, 1002 x 150 m gates).
fn noxp_frames(expected: &serde_json::Value, archive: &std::path::Path) -> Vec<Volume> {
    let sweeps = noxp_sweeps(archive);
    array(&expected["members"])
        .iter()
        .map(|member| {
            let name = as_str(member);
            dorade(name, sweep_bytes(&sweeps, name))
        })
        .collect()
}

/// The FM301 field name the decoder gives a MetPy Level II moment.
fn moment(name: &str) -> FieldName {
    match name {
        "REF" => FieldName::Dbzh,
        "VEL" => FieldName::Vradh,
        "SW" => FieldName::Wradh,
        "ZDR" => FieldName::Zdr,
        "RHO" => FieldName::Rhohv,
        "PHI" => FieldName::Phidp,
        other => panic!("unknown golden moment {other}"),
    }
}

/// The name of the field of `quantity` on a frame's only sweep (the NOXP
/// sweeps keep their DORADE names: `DZ`, `VR`).
fn field_name(frame: &Volume, quantity: Quantity) -> FieldName {
    frame.sweeps[0]
        .find(quantity)
        .expect("field of the quantity")
        .name
        .clone()
}

/// The swath's only field.
fn swath_field(swath: &Volume) -> &Field {
    assert_eq!(swath.sweeps.len(), 1);
    assert_eq!(swath.sweeps[0].fields.len(), 1);
    &swath.sweeps[0].fields[0]
}

/// Reflectivity swath of the two sweeps: a single-tilt volume on the reference
/// sweep's geometry (the later one; its 171 azimuths verbatim) holding the
/// per-gate maximum of both frames, labelled with the newest frame's site and
/// time.
#[test]
fn max_reflectivity_takes_per_gate_maximum() {
    let golden = golden("swath.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = noxp_frames(expected, &archive);
    let starts = array(&expected["start_unix"]);
    for (frame, start) in frames.iter().zip(starts) {
        assert_eq!(frame.time_reference.timestamp(), as_i64(start));
        assert_eq!(frame.sweeps.len(), 1);
    }
    let refs: Vec<&Volume> = frames.iter().collect();
    let reflectivity = field_name(&frames[0], Quantity::Reflectivity);
    let swath = value_swath(&refs, &reflectivity, SwathAggregation::Max)
        .expect("both frames carry reflectivity");

    assert_eq!(swath.attrs.instrument_name, frames[1].attrs.instrument_name);
    assert_eq!(swath.time_reference, frames[1].time_reference);
    assert_eq!(swath.sweeps.len(), 1);
    let cut = &swath.sweeps[0];
    let rays = array(&expected["rays"]);
    assert_eq!(cut.nrays(), as_usize(&rays[1]));
    let azimuths = array(&expected["reference_azimuths_deg"]);
    assert_eq!(azimuths.len(), cut.nrays());
    for (azimuth, expected) in cut.rays.azimuth_deg.iter().zip(azimuths) {
        // Golden azimuths are the shortest decimal of the file's f32 values.
        assert_eq!(*azimuth, as_f64(expected) as f32);
    }
    let grid = swath_field(&swath);
    // The golden counts the RDAT words (1002); the reader keeps the 1001
    // cells CSFD describes and drops the padding word.
    assert_eq!(grid.shape().1 + 1, as_usize(&expected["gate_count"]));
    let (first_gate_m, spacing_m) = grid.native_geometry(&cut.range).expect("geometry");
    assert_eq!(first_gate_m, as_i64(&expected["first_gate_m"]) as f64);
    assert_eq!(spacing_m, as_i64(&expected["gate_spacing_m"]) as f64);
    assert_eq!(grid.shape().0, cut.nrays());
    let summary = &expected["reflectivity_max"];
    assert_grid_matches(grid, summary, "reflectivity swath");
    // Gates where the earlier frame holds the larger value keep it.
    assert!(as_usize(&summary["first_frame_larger_count"]) > 1000);
    assert_samples(grid, &summary["first_frame_larger"], "earlier frame wins");
    let older = frames[0].sweeps[0]
        .find(Quantity::Reflectivity)
        .expect("reflectivity");
    for sample in array(&summary["first_frame_larger"]) {
        let sample = array(sample);
        let (row, gate) = (as_usize(&sample[0]), as_usize(&sample[1]));
        // Same azimuth pattern in both sector sweeps: the row maps to itself.
        assert_eq!(
            cell(older, row, gate),
            cell(grid, row, gate),
            "row {row} gate {gate}"
        );
    }
}

/// The swath covers the union of both frames' valid gates: gates lit only in
/// the earlier sweep (the storm's trail) and only in the later one both appear,
/// and nothing else does.
#[test]
fn swath_covers_union_of_two_positions() {
    let golden = golden("swath.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = noxp_frames(expected, &archive);
    let refs: Vec<&Volume> = frames.iter().collect();
    let reflectivity = field_name(&frames[0], Quantity::Reflectivity);
    let swath = value_swath(&refs, &reflectivity, SwathAggregation::Max)
        .expect("both frames carry reflectivity");
    let grid = swath_field(&swath);
    let summary = &expected["reflectivity_max"];
    assert!(as_usize(&summary["only_first_count"]) > 1000);
    assert!(as_usize(&summary["only_second_count"]) > 1000);
    assert_samples(
        grid,
        &summary["only_first_frame"],
        "gate lit only in the earlier sweep",
    );
    assert_samples(
        grid,
        &summary["only_second_frame"],
        "gate lit only in the later sweep",
    );
    let (rows, gates) = grid.shape();
    let finite = (0..rows)
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| cell(grid, row, gate).is_some())
        .count();
    assert_eq!(
        finite,
        as_usize(&summary["union_count"]),
        "union of valid gates"
    );
    // Each frame alone covers strictly less than the union.
    for frame in &frames {
        let alone =
            value_swath(&[frame], &reflectivity, SwathAggregation::Max).expect("single frame");
        let alone = swath_field(&alone);
        let (rows, gates) = alone.shape();
        let count = (0..rows)
            .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
            .filter(|&(row, gate)| cell(alone, row, gate).is_some())
            .count();
        assert!(count < finite, "single frame {count} >= union {finite}");
    }
}

/// Velocity swath with the max-magnitude aggregation: the larger |V| wins and
/// keeps its sign, including where the two sweeps disagree in sign.
#[test]
fn max_magnitude_keeps_sign_of_extreme() {
    let golden = golden("swath.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = noxp_frames(expected, &archive);
    let refs: Vec<&Volume> = frames.iter().collect();
    let velocity = field_name(&frames[0], Quantity::RadialVelocity);
    let swath = value_swath(&refs, &velocity, SwathAggregation::MaxMagnitude)
        .expect("both frames carry velocity");
    let grid = swath_field(&swath);
    let summary = &expected["velocity_max_magnitude"];
    assert_grid_matches(grid, summary, "velocity swath");
    assert!(as_usize(&summary["sign_flip_count"]) > 1000);
    assert_samples(grid, &summary["sign_flips"], "sign of the larger magnitude");
    let (rows, gates) = grid.shape();
    let negatives = (0..rows)
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| cell(grid, row, gate).is_some_and(|v| v < 0.0))
        .count();
    assert_eq!(
        negatives,
        as_usize(&summary["negative_count"]),
        "inbound gates kept"
    );
}

/// TSTL 2023-03-31 23:03Z (TDWR, no dual-pol): MetPy lists REF / VEL / SW only,
/// so a differential-reflectivity swath over that frame is `None`, as is its
/// base tilt; the reflectivity swath of the same frame exists.
#[test]
fn empty_when_no_frame_has_the_moment() {
    let golden = golden("swath.json");
    let expected = &golden["tstl"];
    for sweep in array(&expected["sweeps"]) {
        assert!(
            !array(&sweep["moments"]).iter().any(|m| as_str(m) == "ZDR"),
            "golden sweep carries ZDR"
        );
    }
    let path = require_file!(as_str(&expected["entry"]));
    let volume = level2(&path);
    assert_eq!(volume.sweeps.len(), array(&expected["sweeps"]).len());
    assert!(value_swath(&[&volume], &FieldName::Zdr, SwathAggregation::Max).is_none());
    assert!(
        value_swath(
            &[&volume, &volume],
            &FieldName::Rhohv,
            SwathAggregation::MaxMagnitude
        )
        .is_none()
    );
    assert_eq!(base_tilt_sweep(&volume, &FieldName::Zdr), None);
    assert!(value_swath(&[], &FieldName::Dbzh, SwathAggregation::Max).is_none());
    assert!(value_swath(&[&volume], &FieldName::Dbzh, SwathAggregation::Max).is_some());
}

/// Base tilt per moment on real split-cut volumes: the lowest cut (first-ray
/// elevation from MetPy, first on ties) that carries the moment. KTLX 2024 keeps
/// its 0.48 deg Doppler cut below the 0.58 deg surveillance cut, so REF as well
/// as VEL resolve to the Doppler cut while ZDR / RHO / PHI stay on the
/// surveillance cut; TSTL's two 0.26 deg cuts tie and REF picks the first.
#[test]
fn picks_lowest_tilt_carrying_the_moment() {
    let golden = golden("swath.json");
    for key in ["ktlx", "tstl"] {
        let expected = &golden[key];
        let path = require_file!(as_str(&expected["entry"]));
        let volume = level2(&path);
        let sweeps = array(&expected["sweeps"]);
        assert_eq!(volume.sweeps.len(), sweeps.len(), "{key}: sweep count");
        for (cut, sweep) in volume.sweeps.iter().zip(sweeps) {
            assert_eq!(
                cut.rays.elevation_deg[0],
                as_f64(&sweep["first_ray_elevation_deg"]) as f32,
                "{key}: first-ray elevation"
            );
            for name in array(&sweep["moments"]) {
                let name = as_str(name);
                if name == "CFP" {
                    continue;
                }
                assert!(
                    cut.field(&moment(name)).is_some(),
                    "{key}: sweep lacks {name}"
                );
            }
        }
        for (name, index) in expected["base_tilt"].as_object().expect("base_tilt object") {
            let expected_index = index.as_u64().map(|i| i as usize);
            assert_eq!(
                base_tilt_sweep(&volume, &moment(name)),
                expected_index,
                "{key}: base tilt for {name}"
            );
        }
    }
}
