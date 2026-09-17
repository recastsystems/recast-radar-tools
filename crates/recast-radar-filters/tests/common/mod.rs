//! Shared helpers for the real-data tests: corpus decoding and golden values.
//!
//! Golden files live under `testdata/golden/filters/` and are written by
//! `tools/filters_map_golden.py` from MetPy, Py-ART, netCDF4 and a standalone
//! DORADE walker (never from this workspace's readers).

#![allow(dead_code)]

use std::path::Path;

use recast_radar_core::{Field, Volume};
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
///
/// The goldens take a tilt's elevation from its first radial (MetPy
/// `Level2File`), while the decoder reports the VCP cut angle as
/// `fixed_angle_deg` (design note 5.2) and the column products use it: each
/// sweep's fixed angle is set to its first ray's elevation so products and
/// references use the same tilt elevations.
pub fn level2(path: &Path) -> Volume {
    let mut volume = recast_radar_io_nexrad::read_volume_from_path(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    for sweep in &mut volume.sweeps {
        if let Some(elevation) = sweep.rays.elevation_deg.first() {
            sweep.fixed_angle_deg = *elevation;
        }
    }
    volume
}

/// Decode a real DORADE sweep file.
pub fn dorade(path: &Path) -> Volume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_dorade::dorade::read_dorade_sweep_volume(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode the station of a real single-station JMA GRIB2 tar.
pub fn jma(path: &Path) -> Volume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_jma::read_jma_tar_first_station(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real classic-netCDF CfRadial 1 file.
pub fn cfradial(path: &Path) -> Volume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_cfradial::cfradial::read_cfradial1_volume(&bytes)
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

pub fn array(value: &Value) -> &Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
}

/// Finite value of a field gate, `None` for no data (missing, range folded,
/// NaN, or a ray without a row).
pub fn cell(field: &Field, row: usize, gate: usize) -> Option<f32> {
    field.value(row, gate).filter(|value| value.is_finite())
}

/// Per-row count of finite gates and their sum (f64).
pub fn row_stats(field: &Field) -> Vec<(usize, f64)> {
    let (rows, gates) = field.shape();
    (0..rows)
        .map(|row| {
            (0..gates)
                .filter_map(|gate| cell(field, row, gate))
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
