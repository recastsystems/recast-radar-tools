//! Shared helpers for the real-data model tests: corpus decoding and golden
//! values.
//!
//! Golden values live in `testdata/golden/core/model.json`, written by
//! `tools/core_golden.py` from Py-ART, MetPy, h5py, netCDF4 and a GRIB2
//! section walker (never from this workspace's readers), plus a reference
//! implementation of the documented `merge_radar_volumes` rules over that
//! independent metadata.

#![allow(dead_code)]

use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};
use recast_radar_core::{MomentGrid, MomentType, RadarVolume};
use serde_json::Value;

/// Parsed golden file `testdata/golden/core/model.json`.
pub fn golden() -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("core")
        .join("model.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode a real Level II file with the NEXRAD reader.
pub fn level2(path: &Path) -> RadarVolume {
    recast_radar_io_nexrad::decode_volume_from_path(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode real Level II bytes (a real-time chunk prefixed with its start
/// chunk, for example).
pub fn level2_bytes(bytes: &[u8]) -> RadarVolume {
    recast_radar_io_nexrad::decode_volume_from_bytes(bytes)
        .unwrap_or_else(|error| panic!("level2 bytes: {error}"))
}

/// Decode a real ODIM_H5 polar volume.
pub fn odim(path: &Path) -> RadarVolume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_odim::decode_odim_h5_volume(&bytes)
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

/// The KIWA start chunk followed by one intermediate chunk: a real archive
/// prefix holding the metadata record and 120 radials of one sweep.
pub fn kiwa_chunk_bytes(start: &[u8], chunk_path: &Path) -> Vec<u8> {
    let mut bytes = start.to_vec();
    bytes.extend_from_slice(
        &std::fs::read(chunk_path)
            .unwrap_or_else(|error| panic!("{}: {error}", chunk_path.display())),
    );
    bytes
}

/// A copy of `volume` keeping only the listed moments in every cut: the
/// per-product part a split feed would have delivered.
pub fn part_with_only(volume: &RadarVolume, keep: &[MomentType]) -> RadarVolume {
    let mut part = volume.clone();
    for cut in &mut part.cuts {
        cut.moments.retain(|moment, _| keep.contains(moment));
    }
    part
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

pub fn as_str(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("expected a string, got {value}"))
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

pub fn moment(name: &str) -> MomentType {
    MomentType::from_nexrad_name(name)
}

/// Golden timestamps: `2026-09-17T00:36:29.397Z`, `2019-10-12T09:00:00Z` or
/// ODIM `20260612T145005Z`.
pub fn time(text: &str) -> DateTime<Utc> {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.fZ")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%SZ"))
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%SZ"))
        .unwrap_or_else(|error| panic!("golden time {text}: {error}"));
    DateTime::from_naive_utc_and_offset(naive, Utc)
}

/// Per-row sum of raw codes (u8 or u16 storage) of a grid.
pub fn raw_row_sums(grid: &MomentGrid) -> Vec<u64> {
    let gates = grid.gate_range.gate_count;
    (0..grid.radial_count())
        .map(|row| match &grid.storage {
            recast_radar_core::MomentStorage::U8(values) => values[row * gates..(row + 1) * gates]
                .iter()
                .map(|&v| u64::from(v))
                .sum(),
            recast_radar_core::MomentStorage::U16(values) => values[row * gates..(row + 1) * gates]
                .iter()
                .map(|&v| u64::from(v))
                .sum(),
            recast_radar_core::MomentStorage::F32(_) => panic!("float storage has no raw codes"),
        })
        .collect()
}

/// Raw code at a cell of u8/u16 storage.
pub fn raw_code(grid: &MomentGrid, row: usize, gate: usize) -> u16 {
    let index = row * grid.gate_range.gate_count + gate;
    match &grid.storage {
        recast_radar_core::MomentStorage::U8(values) => u16::from(values[index]),
        recast_radar_core::MomentStorage::U16(values) => values[index],
        recast_radar_core::MomentStorage::F32(_) => panic!("float storage has no raw codes"),
    }
}

/// Bitwise equality of two grids (float storage compares NaN gates equal,
/// unlike `PartialEq`).
pub fn grids_identical(a: &MomentGrid, b: &MomentGrid) -> bool {
    use recast_radar_core::MomentStorage;
    a.moment == b.moment
        && a.gate_range == b.gate_range
        && a.scale.to_bits() == b.scale.to_bits()
        && a.offset.to_bits() == b.offset.to_bits()
        && a.nodata == b.nodata
        && a.range_folded == b.range_folded
        && a.radial_indices == b.radial_indices
        && match (&a.storage, &b.storage) {
            (MomentStorage::U8(x), MomentStorage::U8(y)) => x == y,
            (MomentStorage::U16(x), MomentStorage::U16(y)) => x == y,
            (MomentStorage::F32(x), MomentStorage::F32(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits())
            }
            _ => false,
        }
}

/// Physical value of an ODIM u8 cell from h5py's raw code: `None` for the
/// nodata and undetect sentinels, else `gain * raw + offset`.
pub fn odim_physical(quantity: &Value, raw: u64) -> Option<f64> {
    let nodata = as_f64(&quantity["nodata"]) as u64;
    let undetect = as_f64(&quantity["undetect"]) as u64;
    if raw == nodata || raw == undetect {
        None
    } else {
        Some(as_f64(&quantity["gain"]) * raw as f64 + as_f64(&quantity["offset"]))
    }
}

pub fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}
