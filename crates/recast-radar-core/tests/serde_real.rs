//! The `serde` feature on real volumes: a decoded volume serializes to JSON,
//! and the deserialized copy equals the decoded volume.
//!
//! Equality is `PartialEq` on the whole `Volume` (every attribute, sweep,
//! ray variable and packed value). There is no fallback for NaN: JSON has no
//! NaN (serde_json writes it as `null` and cannot read that back into a
//! float), and NaN != NaN, so a volume holding a NaN float fails this test.
//! The volumes below hold none; one that does needs a binary format and a
//! NaN-aware comparison. serde_json's `float_roundtrip` feature makes its
//! f64 parsing exact.
//!
//! Deserialization checks what `Volume::seal` checks, holds each range
//! to `MAX_GATES_PER_RADIAL` gates, and checks that each extra variable's
//! shape describes its values. The rejection tests
//! alter one value of a real volume's JSON each and expect an error, not a
//! panic or an allocation the document does not hold.
//!
//! Needs the `serde` feature: `cargo test -p recast-radar-core --features serde`.

mod common;

use recast_radar_core::{Field, Sweep, Volume};
use serde_json::Value;

fn round_trip(id: &str, volume: &Volume) {
    let json = serde_json::to_string(volume).unwrap_or_else(|error| panic!("{id}: {error}"));
    let back: Volume =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("{id}: read back: {error}"));
    assert!(back == *volume, "{id}: the deserialized volume differs");
}

fn path(id: &str) -> std::path::PathBuf {
    recast_radar_testdata::path(id).unwrap_or_else(|error| panic!("{id}: {error}"))
}

#[test]
fn level2_volume_round_trips() {
    let id = "l2-ktlx-20240315-000217-trim";
    round_trip(id, &common::level2(&path(id)));
}

#[test]
fn odim_volume_round_trips() {
    let id = "odim-espdg-20260707-1927-pvol-dbzh-vradh";
    round_trip(id, &common::odim(&path(id)));
}

#[test]
fn cfradial_volume_round_trips() {
    let id = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01";
    round_trip(id, &common::cfradial(&path(id)));
}

#[test]
fn jma_volume_round_trips() {
    let id = "jma-n5-20191012-090000";
    round_trip(id, &common::jma(&path(id)));
}

const LEVEL2: &str = "l2-ktlx-20240315-000217-trim";

/// The KTLX volume as a JSON value.
fn level2_json() -> Value {
    let volume = common::level2(&path(LEVEL2));
    serde_json::to_value(&volume).unwrap_or_else(|error| panic!("{LEVEL2}: {error}"))
}

/// The first field of sweep 0 of a volume's JSON.
fn first_field(json: &mut Value) -> &mut Value {
    &mut json["sweeps"][0]["fields"][0]
}

/// The error of reading `json` back as a `T`, which must fail.
fn rejection<T: serde::de::DeserializeOwned + std::fmt::Debug>(json: Value) -> String {
    match serde_json::from_value::<T>(json) {
        Ok(value) => panic!("accepted: {value:?}"),
        Err(error) => error.to_string(),
    }
}

/// A field whose shape claims u32::MAX × u32::MAX gates over the values it
/// holds. Accepted, `Field::to_physical` panicked reserving the capacity.
#[test]
fn a_field_must_hold_nrays_times_ngates_values() {
    let mut json = level2_json();
    let field = first_field(&mut json);
    field["nrays"] = Value::from(u32::MAX);
    field["ngates"] = Value::from(u32::MAX);
    let alone = field.clone();

    let error = rejection::<Volume>(json);
    assert!(error.contains("values, expected"), "{error}");
    let error = rejection::<Field>(alone);
    assert!(error.contains("values, expected"), "{error}");
}

#[test]
fn absent_rows_must_ascend_below_nrays() {
    let mut json = level2_json();
    let field = first_field(&mut json);
    let nrays = field["nrays"].clone();
    field["absent_rows"] = Value::from(vec![nrays]);
    let error = rejection::<Volume>(json.clone());
    assert!(error.contains("absent rows"), "{error}");

    let field = first_field(&mut json);
    field["absent_rows"] = Value::from(vec![2u32, 1]);
    let error = rejection::<Volume>(json);
    assert!(error.contains("absent rows"), "{error}");
}

/// A field with no rows and u32::MAX gates is a consistent field, but its
/// sweep has rays: `Sweep::seal` would append u32::MAX absent gates for each
/// ray. Deserialization refuses it before sealing.
#[test]
fn every_field_needs_a_row_for_every_ray() {
    let mut json = level2_json();
    let field = first_field(&mut json);
    field["nrays"] = Value::from(0u32);
    field["ngates"] = Value::from(u32::MAX);
    let Some(data) = field["data"].as_object_mut() else {
        panic!("field data is not an object: {}", field["data"]);
    };
    for storage in data.values_mut() {
        storage["values"] = Value::Array(Vec::new());
    }
    let sweep = json["sweeps"][0].clone();

    let error = rejection::<Volume>(json);
    assert!(error.contains("rows has 0 entries"), "{error}");
    let error = rejection::<Sweep>(sweep);
    assert!(error.contains("rows has 0 entries"), "{error}");
}

/// A per-ray array one short: `Sweep::seal` refuses it.
#[test]
fn per_ray_arrays_need_one_entry_per_ray() {
    let mut json = level2_json();
    let Some(times) = json["sweeps"][0]["rays"]["time_s"].as_array_mut() else {
        panic!("rays.time_s is not an array");
    };
    times.pop();
    let error = rejection::<Volume>(json);
    assert!(error.contains("sweep time has"), "{error}");
}

/// A uniform range is three numbers, so its gate count costs a document
/// nothing: 4,000,000,000 gates on the KTLX sweep made the FM301 view build a
/// `range` array of that length and map DBZH onto `480 × 4e9` cells.
/// Deserialization holds the range to `MAX_GATES_PER_RADIAL`.
#[test]
fn the_range_is_held_to_the_gates_per_radial_limit() {
    let limit = recast_radar_core::bounded_read::MAX_GATES_PER_RADIAL;
    for ngates in [4_000_000_000u64, limit as u64 + 1] {
        let mut json = level2_json();
        let range = &mut json["sweeps"][0]["range"]["Uniform"]["ngates"];
        assert!(range.is_u64(), "range: {}", json["sweeps"][0]["range"]);
        *range = Value::from(ngates);
        let sweep = json["sweeps"][0].clone();

        let error = rejection::<Volume>(json);
        assert!(
            error.contains(&format!("range spans {ngates} range gates (limit {limit})")),
            "{error}"
        );
        let error = rejection::<Sweep>(sweep);
        assert!(error.contains("range spans"), "{error}");
    }

    // At the limit the document is read, and the range is what it says.
    let mut json = level2_json();
    json["sweeps"][0]["range"]["Uniform"]["ngates"] = Value::from(limit as u64);
    let volume: Volume = serde_json::from_value(json).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(volume.sweeps[0].range.ngates(), limit);
}

/// A field's gate mapping places it on the range, and `Sweep::seal` grows a
/// uniform range to cover every field, so a field starting at range gate 4e9
/// would claim the same range as above. Deserialization refuses a field whose
/// extent passes the limit before sealing.
#[test]
fn a_field_extent_is_held_to_the_gates_per_radial_limit() {
    let limit = recast_radar_core::bounded_read::MAX_GATES_PER_RADIAL;
    let mut json = level2_json();
    let field = first_field(&mut json);
    let name = field["name"].clone();
    let ngates = field["ngates"]
        .as_u64()
        .unwrap_or_else(|| panic!("ngates: {}", field["ngates"]));
    field["gates"]["start"] = Value::from(4_000_000_000u32);
    let error = rejection::<Volume>(json.clone());
    assert!(
        error.contains(&format!(
            "spans {} range gates (limit {limit})",
            4_000_000_000 + ngates
        )),
        "{error} ({name})"
    );

    // A stride that stays well inside u32 but passes the limit.
    let stride = limit as u64 / ngates + 1;
    let field = first_field(&mut json);
    field["gates"]["start"] = Value::from(0u32);
    field["gates"]["stride"] = Value::from(stride);
    let error = rejection::<Volume>(json);
    assert!(
        error.contains(&format!("spans {} range gates", stride * ngates)),
        "{error}"
    );
}

const CFRADIAL: &str = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01";

/// The Irene CfRadial volume as a JSON value. Sweep 0 keeps
/// `ray_start_range` (dims `["time"]`, shape `[360]`, 360 float32 values)
/// and the root keeps `grid_mapping` (a scalar) as extra variables.
fn cfradial_json() -> Value {
    let volume = common::cfradial(&path(CFRADIAL));
    serde_json::to_value(&volume).unwrap_or_else(|error| panic!("{CFRADIAL}: {error}"))
}

/// The extra variable `name` of a JSON object's `extra_vars`.
fn extra_var<'a>(json: &'a mut Value, name: &str) -> &'a mut Value {
    let Some(extras) = json["extra_vars"].as_array_mut() else {
        panic!("no extra_vars array");
    };
    let Some(extra) = extras.iter_mut().find(|extra| extra["name"] == name) else {
        panic!("no extra variable {name}");
    };
    extra
}

/// An extra variable's `shape` is what the FM301 view declares its
/// dimensions from and reorders a per-ray variable's rows by. Accepted,
/// shape `[480, u32::MAX]` over four values made the view reserve 8 TB and
/// abort the process, and `[480, u32::MAX, u32::MAX, u32::MAX]` overflowed
/// the row length. Deserialization refuses a shape that does not multiply
/// out to the values, or has a different number of entries than `dims`.
#[test]
fn an_extra_variable_shape_must_describe_its_values() {
    let big = u64::from(u32::MAX);
    let cases = [
        (vec!["time", "big"], vec![360, big]),
        (vec!["time", "a", "b", "c"], vec![360, big, big, big]),
        (vec!["time", "big"], vec![360]),
        (vec!["time"], vec![360, 1]),
    ];
    for (dims, shape) in cases {
        let mut json = cfradial_json();
        let extra = extra_var(&mut json["sweeps"][0], "ray_start_range");
        assert_eq!(extra["shape"], Value::from(vec![360u32]), "{extra}");
        extra["dims"] = Value::from(dims.clone());
        extra["shape"] = Value::from(shape.clone());
        let sweep = json["sweeps"][0].clone();

        let want = format!(
            "extra variable ray_start_range: dims {dims:?} and shape {shape:?} do not describe \
             a value count of 360"
        );
        let error = rejection::<Volume>(json);
        assert!(error.contains(&want), "{error}");
        let error = rejection::<Sweep>(sweep);
        assert!(error.contains(&want), "{error}");
    }

    // The root's extra variables are checked the same way.
    let mut json = cfradial_json();
    let extra = extra_var(&mut json, "grid_mapping");
    assert_eq!(extra["shape"], Value::Array(Vec::new()), "{extra}");
    extra["dims"] = Value::from(vec!["big"]);
    extra["shape"] = Value::from(vec![u32::MAX]);
    let error = rejection::<Volume>(json);
    assert!(
        error.contains(&format!(
            "extra variable grid_mapping: dims [\"big\"] and shape [{}] do not describe a \
             value count of 1",
            u32::MAX
        )),
        "{error}"
    );
}

#[test]
fn sweep_numbers_must_match_positions() {
    let mut json = level2_json();
    json["sweeps"][0]["sweep_number"] = Value::from(7u32);
    let error = rejection::<Volume>(json);
    assert!(error.contains("sweep_number 7"), "{error}");
}
