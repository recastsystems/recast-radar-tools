//! Shared helpers for the real-data tests: corpus decoding and golden values.
//!
//! Golden files live under `testdata/golden/retrieve/` and are written by
//! `tools/retrieve_golden.py` from MetPy, Py-ART, the NHC and SPC storm
//! databases and numpy reference implementations (never from this
//! workspace's readers or algorithms).

#![allow(dead_code)]

use std::path::Path;

use recast_radar_core::{ElevationCut, MomentGrid, MomentStorage, MomentType, RadarVolume};
use serde_json::Value;

/// Parsed golden file `testdata/golden/<relative>`.
pub fn golden(relative: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real Level II file with the NEXRAD reader.
pub fn level2(path: &Path) -> RadarVolume {
    recast_radar_io_nexrad::decode_volume_from_path(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real DORADE sweep file.
pub fn dorade(path: &Path) -> RadarVolume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_dorade::dorade::decode_dorade_sweep_volume(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode the station of a real single-station JMA GRIB2 tar.
pub fn jma(path: &Path) -> RadarVolume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_jma::decode_jma_tar_first_station(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real classic-netCDF CfRadial 1 file.
pub fn cfradial(path: &Path) -> RadarVolume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real ODIM_H5 polar volume.
pub fn odim(path: &Path) -> RadarVolume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_odim::odim::decode_odim_h5_volume(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn as_usize(value: &Value) -> usize {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("expected an unsigned integer, got {value}")) as usize
}

pub fn as_i64(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("expected an integer, got {value}"))
}

pub fn as_f64(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number, got {value}"))
}

/// A golden float that may be `null` (no data).
pub fn as_opt_f64(value: &Value) -> Option<f64> {
    if value.is_null() {
        None
    } else {
        Some(as_f64(value))
    }
}

pub fn as_str(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("expected a string, got {value}"))
}

pub fn array(value: &Value) -> &Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
}

/// The golden case (element of `cases`) whose fields match every `(key, value)` pair.
pub fn find_case<'a>(cases: &'a Value, keys: &[(&str, &str)]) -> &'a Value {
    array(cases)
        .iter()
        .find(|case| {
            keys.iter().all(|(key, want)| {
                case[key].as_str() == Some(want)
                    || case[key].as_u64().map(|v| v.to_string()).as_deref() == Some(want)
            })
        })
        .unwrap_or_else(|| panic!("no golden case matching {keys:?}"))
}

/// Finite value of a grid cell, `None` for no data (missing, range folded or NaN).
pub fn cell(grid: &MomentGrid, row: usize, gate: usize) -> Option<f32> {
    grid.scaled_value(row, gate)
        .filter(|value| value.is_finite())
}

/// Per-row count of finite cells and their sum (f64).
pub fn row_stats(grid: &MomentGrid) -> Vec<(usize, f64)> {
    (0..grid.radial_count())
        .map(|row| {
            (0..grid.gate_range.gate_count)
                .filter_map(|gate| cell(grid, row, gate))
                .fold((0usize, 0.0f64), |(count, sum), value| {
                    (count + 1, sum + f64::from(value))
                })
        })
        .collect()
}

pub fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

/// Compare a computed grid with a golden `grid_summary` (per-row finite counts and
/// sums, sampled cells): counts must match exactly, sums and cells within `tolerance`.
pub fn assert_grid_matches(grid: &MomentGrid, summary: &Value, tolerance: f64, what: &str) {
    assert_eq!(
        grid.radial_count(),
        as_usize(&summary["rows"]),
        "{what}: rows"
    );
    assert_eq!(
        grid.gate_range.gate_count,
        as_usize(&summary["gates"]),
        "{what}: gates"
    );
    let stats = row_stats(grid);
    let valid: usize = stats.iter().map(|(count, _)| count).sum();
    assert_eq!(valid, as_usize(&summary["valid"]), "{what}: finite cells");
    let row_valid = array(&summary["row_valid"]);
    let row_sum = array(&summary["row_sum"]);
    for (row, (count, sum)) in stats.iter().enumerate() {
        assert_eq!(
            *count,
            as_usize(&row_valid[row]),
            "{what}: row {row} finite count"
        );
        // Row sums accumulate up to a few thousand cells: scale the tolerance by count.
        let sum_tolerance = tolerance * (*count as f64).max(1.0);
        assert_close(
            *sum,
            as_f64(&row_sum[row]),
            sum_tolerance,
            &format!("{what}: row {row} sum"),
        );
    }
    for entry in array(&summary["cells"]) {
        let entry = array(entry);
        let (row, gate) = (as_usize(&entry[0]), as_usize(&entry[1]));
        let expected = as_opt_f64(&entry[2]);
        let actual = cell(grid, row, gate).map(f64::from);
        match (actual, expected) {
            (Some(a), Some(e)) => {
                assert_close(a, e, tolerance, &format!("{what}: cell ({row}, {gate})"))
            }
            (None, None) => {}
            _ => panic!("{what}: cell ({row}, {gate}) is {actual:?}, golden {expected:?}"),
        }
    }
}

/// Grid of `moment` on `cut`.
pub fn moment<'a>(cut: &'a ElevationCut, moment: &MomentType) -> &'a MomentGrid {
    cut.moments
        .get(moment)
        .unwrap_or_else(|| panic!("cut at {} deg has no {moment}", cut.elevation_deg))
}

/// Index of the lowest cut carrying `moment`.
pub fn lowest_cut_with(volume: &RadarVolume, moment: &MomentType) -> usize {
    volume
        .cuts
        .iter()
        .enumerate()
        .filter(|(_, cut)| cut.moments.contains_key(moment))
        .min_by(|a, b| a.1.elevation_deg.total_cmp(&b.1.elevation_deg))
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("no cut carries {moment}"))
}

/// Overwrite every finite value of a real grid with `f(value)`, keeping no-data cells.
/// Used by the edge-case tests to mutate real decoded data in place.
pub fn map_values(grid: &mut MomentGrid, f: impl Fn(f32) -> f32) {
    map_cells(grid, |_, _, value| f(value));
}

/// Like [`map_values`], with the cell's row and gate passed to `f`.
pub fn map_cells(grid: &mut MomentGrid, f: impl Fn(usize, usize, f32) -> f32) {
    let gates = grid.gate_range.gate_count;
    let values: Vec<f32> = (0..grid.radial_count())
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .map(|(row, gate)| cell(grid, row, gate).map_or(f32::NAN, |value| f(row, gate, value)))
        .collect();
    grid.scale = 1.0;
    grid.offset = 0.0;
    grid.nodata = None;
    grid.range_folded = None;
    grid.storage = MomentStorage::F32(values);
}

/// Two grids with the same geometry and the same value in every cell (no-data cells
/// match each other; `MomentGrid: PartialEq` would fail on NaN storage).
pub fn assert_same_grid(actual: &MomentGrid, expected: &MomentGrid, what: &str) {
    assert_eq!(actual.moment, expected.moment, "{what}: moment");
    assert_eq!(actual.gate_range, expected.gate_range, "{what}: gate range");
    assert_eq!(
        actual.radial_indices, expected.radial_indices,
        "{what}: radial indices"
    );
    for row in 0..expected.radial_count() {
        for gate in 0..expected.gate_range.gate_count {
            assert_eq!(
                cell(actual, row, gate),
                cell(expected, row, gate),
                "{what}: cell ({row}, {gate})"
            );
        }
    }
}
