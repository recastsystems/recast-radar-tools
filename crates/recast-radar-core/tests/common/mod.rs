//! Shared helpers for the real-data model tests: corpus decoding and golden
//! values.
//!
//! Golden values live in `testdata/golden/core/model.json`, written by
//! `tools/core_golden.py` from Py-ART, MetPy, h5py, netCDF4 and a GRIB2
//! section walker (never from this workspace's readers), plus a reference
//! implementation of the documented `merge_volumes` rules over that
//! independent metadata.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};
use recast_radar_core::{Field, FieldData, FieldName, Quantity, Sweep, Volume};
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
pub fn level2(path: &Path) -> Volume {
    recast_radar_io_nexrad::read_volume_from_path(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Decode real Level II bytes (a real-time chunk prefixed with its start
/// chunk, for example).
pub fn level2_bytes(bytes: &[u8]) -> Volume {
    recast_radar_io_nexrad::read_volume_from_bytes(bytes)
        .unwrap_or_else(|error| panic!("level2 bytes: {error}"))
}

/// Decode a real ODIM_H5 polar volume.
pub fn odim(path: &Path) -> Volume {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    recast_radar_io_odim::read_odim_h5_volume(&bytes)
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

/// A copy of `volume` keeping only the fields of the listed quantities in
/// every sweep: the per-product part a split feed would have delivered.
pub fn part_with_only(volume: &Volume, keep: &[Quantity]) -> Volume {
    let mut part = volume.clone();
    for sweep in &mut part.sweeps {
        sweep.fields.retain(|field| keep.contains(&field.quantity));
    }
    part
}

/// The field `name` of `sweep`.
pub fn field<'a>(sweep: &'a Sweep, name: &FieldName) -> &'a Field {
    sweep
        .field(name)
        .unwrap_or_else(|| panic!("sweep {} has no {name}", sweep.sweep_number))
}

/// Field names of a sweep, as text.
pub fn field_names(sweep: &Sweep) -> BTreeSet<String> {
    sweep.fields.iter().map(|f| f.name.to_string()).collect()
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

/// FM301 name of a Level II data block as MetPy lists it (`REF`, `PHI`, ...).
pub fn nexrad_field(block: &str) -> FieldName {
    FieldName::from_nexrad_block(block.as_bytes())
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

/// Raw codes of a u8/u16 field row.
fn raw_row(field: &Field, row: usize) -> Vec<u64> {
    let gates = field.ngates as usize;
    match &field.data {
        FieldData::U8 { values, .. } => values[row * gates..(row + 1) * gates]
            .iter()
            .map(|&v| u64::from(v))
            .collect(),
        FieldData::U16 { values, .. } => values[row * gates..(row + 1) * gates]
            .iter()
            .map(|&v| u64::from(v))
            .collect(),
        other => panic!("{} storage has no unsigned raw codes", other.dtype()),
    }
}

/// Per-row sum of raw codes (u8 or u16 storage) of a field.
pub fn raw_row_sums(field: &Field) -> Vec<u64> {
    (0..field.nrays as usize)
        .map(|row| raw_row(field, row).iter().sum())
        .collect()
}

/// Raw code at a cell of u8/u16 storage.
pub fn raw_code(field: &Field, row: usize, gate: usize) -> u16 {
    let index = row * field.ngates as usize + gate;
    match &field.data {
        FieldData::U8 { values, .. } => u16::from(values[index]),
        FieldData::U16 { values, .. } => values[index],
        other => panic!("{} storage has no unsigned raw codes", other.dtype()),
    }
}

/// Bitwise equality of two value buffers and codings (float storage compares
/// NaN gates equal, unlike `PartialEq`).
pub fn data_identical(a: &FieldData, b: &FieldData) -> bool {
    match (a, b) {
        (
            FieldData::F32 {
                values: x,
                coding: p,
            },
            FieldData::F32 {
                values: y,
                coding: q,
            },
        ) => {
            format!("{p:?}") == format!("{q:?}")
                && x.len() == y.len()
                && x.iter().zip(y).all(|(u, v)| u.to_bits() == v.to_bits())
        }
        (
            FieldData::F64 {
                values: x,
                coding: p,
            },
            FieldData::F64 {
                values: y,
                coding: q,
            },
        ) => {
            format!("{p:?}") == format!("{q:?}")
                && x.len() == y.len()
                && x.iter().zip(y).all(|(u, v)| u.to_bits() == v.to_bits())
        }
        (x, y) => x == y,
    }
}

/// Bitwise equality of two fields (float storage compares NaN gates equal,
/// unlike `PartialEq`).
pub fn fields_identical(a: &Field, b: &Field) -> bool {
    a.name == b.name
        && a.quantity == b.quantity
        && a.polarization == b.polarization
        && a.attrs == b.attrs
        && a.nrays == b.nrays
        && a.ngates == b.ngates
        && a.gates == b.gates
        && a.absent_rows == b.absent_rows
        && data_identical(&a.data, &b.data)
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
