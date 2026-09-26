//! CfRadial 1 `n_gates_vary` storage and per-sweep gate geometry, on files
//! LROSE Radx wrote from real data (three sweeps of the FMI Anjalankoski
//! PVOL, of 500 m and 250 m gates):
//!
//! - a classic file with `range(time, range)` (Radx keeps each sweep's
//!   geometry);
//! - a netCDF-4 file with one `range(range)` (Radx remapped every sweep to
//!   the finest geometry).
//!
//! Goldens: `testdata/golden/cfradial1-ragged/<id>.json`, written by
//! `tools/cfradial_ragged_golden.py` from netCDF4-python's view of the
//! stored variables laid out as CfRadial 1.4 defines (and, for the file
//! with a one-dimensional range, checked there against Py-ART and xradar):
//! per sweep the rays, gates and geometry, per field the raw codes (SHA-256,
//! each ray padded after its `ray_n_gates` with `_FillValue`), scale, offset
//! and fill.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::{FieldData, RangeCoord, Volume};
use recast_radar_io_cfradial::read_cfradial_volume;
use serde_json::Value;

const PER_RAY_GEOMETRY: &str = "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry";
const FINEST_GEOMETRY: &str = "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4";

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("cfradial1-ragged")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn decode(id: &str) -> Volume {
    let bytes = recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{id}: {err}"));
    read_cfradial_volume(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"))
}

fn check(id: &str) {
    let golden = golden(id);
    let volume = decode(id);
    let sweeps = golden["sweeps"].as_array().unwrap();
    assert_eq!(volume.sweeps.len(), sweeps.len(), "{id}: sweeps");
    for (index, (sweep, expected)) in volume.sweeps.iter().zip(sweeps).enumerate() {
        let context = format!("{id} sweep {index}");
        assert_eq!(
            sweep.nrays() as u64,
            expected["rays"].as_u64().unwrap(),
            "{context}: rays"
        );
        let ngates = expected["ngates"].as_u64().unwrap() as u32;
        match &sweep.range {
            RangeCoord::Uniform {
                first_center_m,
                spacing_m,
                ngates: have,
            } => {
                assert_eq!(*have, ngates, "{context}: gates");
                assert!(
                    (first_center_m - expected["first_center_m"].as_f64().unwrap()).abs() < 1e-3,
                    "{context}: first centre {first_center_m}"
                );
                assert!(
                    (spacing_m - expected["spacing_m"].as_f64().unwrap()).abs() < 1e-3,
                    "{context}: spacing {spacing_m}"
                );
            }
            other => panic!("{context}: range {other:?}"),
        }
        for (name, field_golden) in expected["fields"].as_object().unwrap() {
            let field = sweep
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("{context}: no {name}"));
            assert_eq!(field.ngates, ngates, "{context} {name}: gates");
            // The stored codes (little-endian, as the golden hashes them),
            // the fill, the packing and the count of codes that are not the
            // fill, in the stored integer type.
            let (dtype, bytes, fill, scale, offset, not_fill) = match &field.data {
                FieldData::I8 { values, coding } => (
                    "int8",
                    values.iter().map(|code| *code as u8).collect::<Vec<u8>>(),
                    coding.fill_value.map(i64::from),
                    coding.transform.scale_factor(),
                    coding.transform.add_offset(),
                    values
                        .iter()
                        .filter(|code| Some(**code) != coding.fill_value)
                        .count(),
                ),
                FieldData::I16 { values, coding } => (
                    "int16",
                    values.iter().flat_map(|code| code.to_le_bytes()).collect(),
                    coding.fill_value.map(i64::from),
                    coding.transform.scale_factor(),
                    coding.transform.add_offset(),
                    values
                        .iter()
                        .filter(|code| Some(**code) != coding.fill_value)
                        .count(),
                ),
                other => panic!("{context} {name}: storage {}", other.dtype()),
            };
            assert_eq!(dtype, field_golden["dtype"].as_str().unwrap(), "{context} {name}: dtype");
            assert_eq!(fill, field_golden["fill"].as_i64(), "{context} {name}: fill");
            assert_eq!(
                f64::from(scale.unwrap() as f32),
                field_golden["scale_factor"].as_f64().unwrap(),
                "{context} {name}: scale"
            );
            assert_eq!(
                f64::from(offset.unwrap() as f32),
                field_golden["add_offset"].as_f64().unwrap(),
                "{context} {name}: offset"
            );
            assert_eq!(
                recast_radar_testdata::sha256_hex(&bytes),
                field_golden["codes_sha256"].as_str().unwrap(),
                "{context} {name}: codes"
            );
            assert_eq!(
                not_fill as u64,
                field_golden["not_fill"].as_u64().unwrap(),
                "{context} {name}: codes other than the fill"
            );
        }
    }
}

/// `range(time, range)` rows and `n_points` storage: each sweep keeps its
/// own geometry (500 m, 500 m, 250 m gates) and gate count.
#[test]
fn radx_per_ray_geometry_classic_file() {
    check(PER_RAY_GEOMETRY);
    let volume = decode(PER_RAY_GEOMETRY);
    let spacings: Vec<f64> = volume
        .sweeps
        .iter()
        .map(|sweep| match sweep.range {
            RangeCoord::Uniform { spacing_m, .. } => spacing_m,
            RangeCoord::Explicit { .. } => f64::NAN,
        })
        .collect();
    assert_eq!(spacings, [500.0, 500.0, 250.0]);
}

/// One `range(range)` and `n_points` storage in a netCDF-4 file: sweeps of
/// 1000, 1000 and 748 gates.
#[test]
fn radx_finest_geometry_netcdf4_file() {
    check(FINEST_GEOMETRY);
    let volume = decode(FINEST_GEOMETRY);
    let gates: Vec<usize> = volume
        .sweeps
        .iter()
        .map(|sweep| sweep.range.ngates())
        .collect();
    assert_eq!(gates, [1000, 1000, 748]);
}
