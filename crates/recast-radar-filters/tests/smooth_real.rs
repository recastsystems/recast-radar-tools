//! Polar binomial smoothing on real reflectivity sweeps.
//!
//! Expected values: `testdata/golden/filters/smooth.json`, written by
//! `tools/filters_map_golden.py smooth` from MetPy 1.7.1 reflectivity with a
//! numpy reference of the documented kernel (NaN-aware [1 2 1] x [1 2 1] over
//! azimuth x range, azimuth wrapping, range clamped, coverage never grown).
//!
//! The KDVN and KTLX 2013 fixtures are trimmed sectors (120 and 480 of 720
//! radials), so their first and last rows would wrap onto each other; those
//! two rows are not compared. The KTLX 1999 sweep is a whole circle and every
//! row is compared, including the wrap.

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{array, as_f64, as_opt_f64, as_usize, assert_close, cell, golden, row_stats};
use recast_radar_core::{Field, Quantity};
use recast_radar_filters::smooth_field;
use serde_json::Value;

fn case<'a>(golden: &'a Value, id: &str) -> &'a Value {
    array(&golden["cases"])
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("no golden case for {id}"))
}

fn reflectivity(id: &str, case: &Value) -> Option<Field> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let mut volume = common::level2(&path);
    let mut sweep = volume.sweeps.swap_remove(as_usize(&case["sweep"]));
    let index = sweep
        .fields
        .iter()
        .position(|field| field.quantity == Quantity::Reflectivity)
        .expect("REF");
    let grid = sweep.fields.swap_remove(index);
    assert_eq!(grid.shape().0, as_usize(&case["rows"]));
    assert_eq!(grid.shape().1, as_usize(&case["gates"]));
    Some(grid)
}

fn checked_rows(case: &Value) -> std::ops::RangeInclusive<usize> {
    let rows = array(&case["checked_rows"]);
    as_usize(&rows[0])..=as_usize(&rows[1])
}

/// Every compared row: the smoothed row's gate count and value sum equal the
/// reference, and the golden sample gates match to 1e-4 dBZ.
fn assert_matches_reference(case: &Value, smoothed: &Field) {
    let stats = row_stats(smoothed);
    let row_valid = array(&case["row_valid"]);
    let row_sum = array(&case["row_sum"]);
    for row in checked_rows(case) {
        assert_eq!(stats[row].0, as_usize(&row_valid[row]), "row {row} gates");
        assert_close(
            stats[row].1,
            as_f64(&row_sum[row]),
            1e-3,
            &format!("row {row} sum"),
        );
    }
    for sample in array(&case["samples"]) {
        let sample = array(sample);
        let (row, gate) = (as_usize(&sample[0]), as_usize(&sample[1]));
        let expected = as_opt_f64(&sample[2]).expect("sampled gates are valid");
        let actual = cell(smoothed, row, gate).expect("smoothed gate");
        assert_close(
            f64::from(actual),
            expected,
            1e-4,
            &format!("row {row} gate {gate}"),
        );
    }
}

/// A gate whose valid 3x3 neighbours all carry its own value is unchanged
/// (the kernel is normalized over the valid neighbours). Real sweeps rarely
/// have a constant full neighbourhood (KTLX 2013: 1 gate), so the check also
/// covers gates at echo edges whose remaining neighbours are equal.
#[test]
fn uniform_field_is_unchanged() {
    let golden = golden("filters/smooth.json");
    let mut checked = 0;
    for id in [
        "l2-kdvn-20200810-180401-trim",
        "l2-ktlx-20130520-201643-trim",
        "l2-ktlx-19990504-002218-trim",
    ] {
        let case = case(&golden, id);
        let Some(grid) = reflectivity(id, case) else {
            return;
        };
        let smoothed = smooth_field(&grid);
        for list in ["constant_neighbourhood", "edge_unchanged"] {
            let expected = &case[list];
            let samples = array(&expected["samples"]);
            assert_eq!(
                samples.len(),
                as_usize(&expected["count"]).min(200),
                "{id} {list}"
            );
            for sample in samples {
                let sample = array(sample);
                let (row, gate) = (as_usize(&sample[0]), as_usize(&sample[1]));
                let value = as_f64(&sample[2]);
                assert_eq!(
                    cell(&grid, row, gate).map(f64::from),
                    Some(value),
                    "{id} input"
                );
                let actual = cell(&smoothed, row, gate).expect("smoothed gate");
                assert_close(
                    f64::from(actual),
                    value,
                    1e-5,
                    &format!("{id} {list} row {row} gate {gate}"),
                );
                checked += 1;
            }
        }
        assert_matches_reference(case, &smoothed);
    }
    assert!(checked >= 400, "only {checked} uniform-neighbourhood gates");
}

/// KDVN 2020-08-10 derecho: the smoothed sweep has exactly the native
/// coverage (MetPy's valid gates per row), empty gates stay empty, and values
/// match the reference.
#[test]
fn steps_soften_and_coverage_does_not_grow() {
    let golden = golden("filters/smooth.json");
    let id = "l2-kdvn-20200810-180401-trim";
    let case = case(&golden, id);
    let Some(grid) = reflectivity(id, case) else {
        return;
    };
    let smoothed = smooth_field(&grid);
    assert_eq!(smoothed.shape(), grid.shape());
    assert_eq!(smoothed.gates, grid.gates);
    assert_eq!(smoothed.absent_rows, grid.absent_rows);

    let native_row_valid = array(&case["native_row_valid"]);
    let stats = row_stats(&smoothed);
    for (row, (count, _)) in stats.iter().enumerate() {
        assert_eq!(
            *count,
            as_usize(&native_row_valid[row]),
            "row {row} coverage"
        );
    }
    let total: usize = stats.iter().map(|(count, _)| count).sum();
    assert_eq!(total, as_usize(&case["native_valid_total"]));
    for row in 0..grid.shape().0 {
        for gate in 0..grid.shape().1 {
            assert_eq!(
                cell(&grid, row, gate).is_some(),
                cell(&smoothed, row, gate).is_some(),
                "row {row} gate {gate}"
            );
        }
    }

    // The steepest reflectivity steps (53.5 dB across a 3x3 neighbourhood at
    // the bow echo's edge) soften: strictly between the neighbourhood's
    // extremes, at the reference value.
    for step in array(&case["steepest"]) {
        let (row, gate) = (as_usize(&step["row"]), as_usize(&step["gate"]));
        let actual = f64::from(cell(&smoothed, row, gate).expect("smoothed gate"));
        assert!(actual > as_f64(&step["min"]) && actual < as_f64(&step["max"]));
        assert_close(
            actual,
            as_f64(&step["smoothed"]),
            1e-4,
            &format!("row {row} gate {gate}"),
        );
    }
    assert_matches_reference(case, &smoothed);
}

/// KTLX 2013-05-20 Moore supercell and the 1999 Bridge Creek-Moore supercell
/// (whole circle, azimuth wrap included): across the steepest gradients the
/// smoothed value lies strictly between the neighbourhood's extremes and
/// equals the reference; every row matches the reference.
#[test]
fn interior_step_blends() {
    let golden = golden("filters/smooth.json");
    for id in [
        "l2-ktlx-20130520-201643-trim",
        "l2-ktlx-19990504-002218-trim",
    ] {
        let case = case(&golden, id);
        let Some(grid) = reflectivity(id, case) else {
            return;
        };
        let smoothed = smooth_field(&grid);
        let steepest = array(&case["steepest"]);
        assert_eq!(steepest.len(), 50);
        for step in steepest {
            let (row, gate) = (as_usize(&step["row"]), as_usize(&step["gate"]));
            assert!(as_f64(&step["max"]) - as_f64(&step["min"]) > 40.0);
            let actual = f64::from(cell(&smoothed, row, gate).expect("smoothed gate"));
            assert!(
                actual > as_f64(&step["min"]) && actual < as_f64(&step["max"]),
                "{id} row {row} gate {gate}: {actual} outside the neighbourhood"
            );
            assert_close(
                actual,
                as_f64(&step["smoothed"]),
                1e-4,
                &format!("{id} row {row} gate {gate}"),
            );
        }
        assert_matches_reference(case, &smoothed);
    }
}
