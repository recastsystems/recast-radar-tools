//! Shared helpers for the real-data tests: corpus decoding and golden values.
//!
//! Golden files live under `testdata/golden/track/` and are written by
//! `tools/track_golden.py` from Py-ART, MetPy (Level II and Level III) and a
//! standalone DORADE walker (never from this workspace's readers).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Utc};
use recast_radar_core::{Field, Volume};
use serde_json::Value;

/// Parsed golden file `testdata/golden/track/<name>`.
pub fn golden(name: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("track")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real Level II file with the NEXRAD reader, unmodified.
///
/// `Sweep::fixed_angle_deg` is the VCP cut angle (Message 5, design note
/// 5.2), which the products use as the tilt elevation; the goldens take the
/// same angle from MetPy's `vcp_info`. The cuts of a split cut and the SAILS /
/// MRLE repeats of one angle therefore have equal tilt elevations.
pub fn level2(path: &Path) -> Volume {
    level2_with_time(path).0
}

/// [`level2`] with the Archive II volume header time (MetPy `Level2File.dt`),
/// the scan time the goldens label volumes with.
pub fn level2_with_time(path: &Path) -> (Volume, DateTime<Utc>) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let decoded = recast_radar_io_nexrad::read_volume_with_metadata(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let time = decoded
        .metadata
        .volume_header_time
        .unwrap_or_else(|| panic!("{}: no volume header time", path.display()));
    (decoded.volume, time)
}

/// A real volume with each sweep's fixed angle replaced by its first ray's
/// elevation: the tilt elevations of the legacy model, which took a Level II
/// cut's elevation from its first radial while the antenna was still
/// settling (KDVN 2020-08-10: 0.27 to 0.70 deg on 0.48 deg cuts). The
/// perturbed column geometry is a real-data edge case for the tracker's merge
/// path.
pub fn with_first_ray_elevations(mut volume: Volume) -> Volume {
    for sweep in &mut volume.sweeps {
        if let Some(elevation) = sweep.rays.elevation_deg.first() {
            sweep.fixed_angle_deg = *elevation;
        }
    }
    volume
}

/// Decode one real DORADE sweep file (bytes of an archive member).
pub fn dorade(name: &str, bytes: &[u8]) -> Volume {
    recast_radar_io_dorade::dorade::read_dorade_sweep_volume(bytes)
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// The regular files of a gzip-compressed ustar archive as `(member path, bytes)`,
/// in archive order. Directory entries, symlinks and empty members are skipped.
pub fn tgz_members(path: &Path) -> Vec<(String, Vec<u8>)> {
    let gz = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_end(&mut tar)
        .unwrap_or_else(|error| panic!("{}: inflate: {error}", path.display()));
    let mut members = Vec::new();
    let mut offset = 0usize;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let name = String::from_utf8_lossy(&header[..100])
            .trim_end_matches('\0')
            .to_owned();
        let size_field = String::from_utf8_lossy(&header[124..136]);
        let size = usize::from_str_radix(size_field.trim_matches(|c| c == '\0' || c == ' '), 8)
            .unwrap_or_else(|error| panic!("{name}: tar size field {size_field:?}: {error}"));
        let data = offset + 512;
        if matches!(header[156], b'0' | 0) && size > 0 {
            members.push((name, tar[data..data + size].to_vec()));
        }
        offset = data + size.div_ceil(512) * 512;
    }
    members
}

/// The `swp.*` sweep files of a Zenodo NOXP archive keyed by basename, sorted by
/// basename (which sorts by time). Symlinked duplicates are not regular files
/// and are skipped by [`tgz_members`].
pub fn noxp_sweeps(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut sweeps: Vec<(String, Vec<u8>)> = tgz_members(path)
        .into_iter()
        .filter_map(|(name, bytes)| {
            let base = name.rsplit('/').next().unwrap_or(&name).to_owned();
            base.starts_with("swp.").then_some((base, bytes))
        })
        .collect();
    sweeps.sort_by(|a, b| a.0.cmp(&b.0));
    sweeps.dedup_by(|a, b| a.0 == b.0);
    sweeps
}

/// The member of `sweeps` with the given basename.
pub fn sweep_bytes<'a>(sweeps: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    sweeps
        .iter()
        .find(|(base, _)| base == name)
        .map(|(_, bytes)| bytes.as_slice())
        .unwrap_or_else(|| panic!("archive has no member {name}"))
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

/// Parse a golden `YYYY-MM-DDTHH:MM:SS.mmmZ` timestamp.
pub fn timestamp(value: &Value) -> DateTime<Utc> {
    let text = as_str(value);
    DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .with_timezone(&Utc)
}

/// Finite value of a field gate, `None` for no data (missing, range folded,
/// NaN, outside the field, or a ray without a row).
pub fn cell(grid: &Field, row: usize, gate: usize) -> Option<f32> {
    grid.value(row, gate).filter(|value| value.is_finite())
}

/// Count, sum (f64), maximum and minimum of the finite cells of a grid.
pub struct GridStats {
    pub finite: usize,
    pub sum: f64,
    pub max: f32,
    pub min: f32,
}

pub fn grid_stats(grid: &Field) -> GridStats {
    let mut stats = GridStats {
        finite: 0,
        sum: 0.0,
        max: f32::NEG_INFINITY,
        min: f32::INFINITY,
    };
    let (rows, gates) = grid.shape();
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = cell(grid, row, gate) {
                stats.finite += 1;
                stats.sum += f64::from(value);
                stats.max = stats.max.max(value);
                stats.min = stats.min.min(value);
            }
        }
    }
    stats
}

/// Check a grid against a golden summary (`finite`, `sum`, `max`, `min`, `samples`).
pub fn assert_grid_matches(grid: &Field, summary: &Value, what: &str) {
    let stats = grid_stats(grid);
    assert_eq!(
        stats.finite,
        as_usize(&summary["finite"]),
        "{what}: finite cell count"
    );
    let expected_sum = as_f64(&summary["sum"]);
    assert_close(
        stats.sum,
        expected_sum,
        (expected_sum.abs() * 1e-5).max(0.05),
        &format!("{what}: sum of finite cells"),
    );
    if let Some(max) = as_opt_f64(&summary["max"]) {
        assert_close(f64::from(stats.max), max, 1e-3, &format!("{what}: maximum"));
    }
    if let Some(min) = as_opt_f64(&summary["min"]) {
        assert_close(f64::from(stats.min), min, 1e-3, &format!("{what}: minimum"));
    }
    assert_samples(grid, &summary["samples"], what);
}

/// Every `[row, gate, value]` sample equals the grid cell (null = no data).
pub fn assert_samples(grid: &Field, samples: &Value, what: &str) {
    for sample in array(samples) {
        let sample = array(sample);
        let (row, gate) = (as_usize(&sample[0]), as_usize(&sample[1]));
        let actual = cell(grid, row, gate);
        match as_opt_f64(&sample[2]) {
            Some(expected) => {
                let actual = actual.unwrap_or_else(|| {
                    panic!("{what}: cell ({row}, {gate}) is empty, expected {expected}")
                });
                assert_close(
                    f64::from(actual),
                    expected,
                    (expected.abs() * 1e-5).max(1e-4),
                    &format!("{what}: cell ({row}, {gate})"),
                );
            }
            None => assert!(
                actual.is_none(),
                "{what}: cell ({row}, {gate}) should be empty, got {actual:?}"
            ),
        }
    }
}

pub fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

/// Planar distance in km between two east/north points.
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
