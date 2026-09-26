//! JSON descriptions of decoded volumes, shared by `info`, `dump` and
//! `validate`.
//!
//! Metadata structs go through their serde derives, so a value the model
//! gains is printed without a change here. Attribute lists (`other`,
//! `extra`) become JSON objects, and long per-ray arrays are summarized
//! unless the caller asks for them in full.

use chrono::{DateTime, SecondsFormat, Utc};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldData, Gate, Scalar, Sweep, Volume,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Arrays longer than this are summarized unless shown in full.
const INLINE_ARRAY_LIMIT: usize = 16;

/// RFC 3339 UTC time, with milliseconds when the time has a fraction.
pub(crate) fn time_text(time: &DateTime<Utc>) -> String {
    let format = if time.timestamp_subsec_nanos() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Millis
    };
    time.to_rfc3339_opts(format, true)
}

pub(crate) fn scalar_value(scalar: Scalar) -> Value {
    match scalar {
        Scalar::I8(v) => json!(v),
        Scalar::U8(v) => json!(v),
        Scalar::I16(v) => json!(v),
        Scalar::U16(v) => json!(v),
        Scalar::I32(v) => json!(v),
        Scalar::U32(v) => json!(v),
        Scalar::I64(v) => json!(v),
        Scalar::U64(v) => json!(v),
        Scalar::F32(v) => float_value(f64::from(v)),
        Scalar::F64(v) => float_value(v),
    }
}

fn float_value(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

pub(crate) fn array_value(array: &ArrayBuf) -> Value {
    fn floats<T: Copy + Into<f64>>(values: &[T]) -> Value {
        Value::Array(values.iter().map(|v| float_value((*v).into())).collect())
    }
    match array {
        ArrayBuf::I8(v) => json!(v),
        ArrayBuf::U8(v) => json!(v),
        ArrayBuf::I16(v) => json!(v),
        ArrayBuf::U16(v) => json!(v),
        ArrayBuf::I32(v) => json!(v),
        ArrayBuf::U32(v) => json!(v),
        ArrayBuf::I64(v) => json!(v),
        ArrayBuf::F32(v) => floats(v),
        ArrayBuf::F64(v) => floats(v),
        ArrayBuf::Text(v) => json!(v),
    }
}

pub(crate) fn attr_value(value: &AttrValue) -> Value {
    match value {
        AttrValue::Text(text) => Value::String(text.to_string()),
        AttrValue::Bool(flag) => Value::Bool(*flag),
        AttrValue::Scalar(scalar) => scalar_value(*scalar),
        AttrValue::Array(array) => array_value(array),
    }
}

/// `[(name, value)]` as a JSON object. A repeated name keeps every value, as
/// an array.
pub(crate) fn attrs_object(pairs: &[(Box<str>, AttrValue)]) -> Value {
    let mut map = Map::new();
    let mut repeated = std::collections::HashSet::new();
    for (name, value) in pairs {
        let value = attr_value(value);
        match map.get_mut(name.as_ref()) {
            None => {
                map.insert(name.to_string(), value);
            }
            Some(Value::Array(values)) if repeated.contains(name.as_ref()) => values.push(value),
            Some(existing) => {
                let previous = existing.take();
                *existing = json!([previous, value]);
                repeated.insert(name.to_string());
            }
        }
    }
    Value::Object(map)
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|err| json!({ "error": err.to_string() }))
}

/// Remove nulls from an object (absent optional values).
fn drop_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, drop_nulls(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Replace the attribute-list member `key` of an object with an object.
fn replace_pairs(value: &mut Value, key: &str, pairs: &[(Box<str>, AttrValue)]) {
    if let Value::Object(map) = value {
        if pairs.is_empty() {
            map.remove(key);
        } else {
            map.insert(key.to_owned(), attrs_object(pairs));
        }
    }
}

/// Numeric arrays longer than [`INLINE_ARRAY_LIMIT`] become
/// `{count, first, last, min, max}` unless `full`.
pub(crate) fn summarize_arrays(value: Value, full: bool) -> Value {
    if full {
        return value;
    }
    match value {
        Value::Array(items) if items.len() > INLINE_ARRAY_LIMIT => {
            let numbers: Vec<Option<f64>> = items.iter().map(Value::as_f64).collect();
            if items.iter().all(|v| v.is_number() || v.is_null()) {
                array_summary(&numbers)
            } else {
                Value::Array(
                    items
                        .into_iter()
                        .map(|v| summarize_arrays(v, full))
                        .collect(),
                )
            }
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| summarize_arrays(v, full))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, summarize_arrays(v, full)))
                .collect(),
        ),
        other => other,
    }
}

fn array_summary(values: &[Option<f64>]) -> Value {
    let valid: Vec<f64> = values.iter().flatten().copied().collect();
    let min = valid.iter().copied().fold(f64::INFINITY, f64::min);
    let max = valid.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut summary = Map::new();
    summary.insert("count".to_owned(), json!(values.len()));
    summary.insert("missing".to_owned(), json!(values.len() - valid.len()));
    if let (Some(first), Some(last)) = (values.first(), values.last()) {
        summary.insert("first".to_owned(), first.map_or(Value::Null, float_value));
        summary.insert("last".to_owned(), last.map_or(Value::Null, float_value));
    }
    if !valid.is_empty() {
        summary.insert("min".to_owned(), float_value(min));
        summary.insert("max".to_owned(), float_value(max));
    }
    Value::Object(summary)
}

fn extra_variables(vars: &[ExtraVariable], full: bool) -> Value {
    Value::Array(
        vars.iter()
            .map(|var| {
                let mut map = Map::new();
                map.insert("name".to_owned(), json!(var.name.as_ref()));
                map.insert("dims".to_owned(), json!(var.dims));
                map.insert("shape".to_owned(), json!(var.shape));
                map.insert("dtype".to_owned(), json!(var.values.dtype()));
                map.insert(
                    "values".to_owned(),
                    summarize_arrays(array_value(&var.values), full),
                );
                if !var.attrs.is_empty() {
                    map.insert("attrs".to_owned(), attrs_object(&var.attrs));
                }
                Value::Object(map)
            })
            .collect(),
    )
}

/// Everything of a volume except its sweeps.
pub(crate) fn volume_metadata(volume: &Volume, full: bool) -> Value {
    let mut map = Map::new();
    let mut attrs = to_value(&volume.attrs);
    replace_pairs(&mut attrs, "other", &volume.attrs.other);
    map.insert("attrs".to_owned(), drop_nulls(attrs));
    map.insert("volume_number".to_owned(), json!(volume.volume_number));
    map.insert(
        "time_reference".to_owned(),
        json!(time_text(&volume.time_reference)),
    );
    if let Some(coverage) = &volume.time_coverage {
        map.insert(
            "time_coverage".to_owned(),
            json!({ "start": time_text(&coverage.start), "end": time_text(&coverage.end) }),
        );
    }
    map.insert(
        "location".to_owned(),
        drop_nulls(to_value(&volume.location)),
    );
    map.insert(
        "platform_type".to_owned(),
        json!(volume.platform_type.as_str()),
    );
    map.insert(
        "instrument_type".to_owned(),
        json!(volume.instrument_type.as_str()),
    );
    if let Some(axis) = volume.primary_axis {
        map.insert("primary_axis".to_owned(), json!(axis.as_str()));
    }
    if let Some(status) = &volume.status_str {
        map.insert("status_str".to_owned(), json!(status));
    }
    map.insert(
        "scan".to_owned(),
        summarize_arrays(drop_nulls(to_value(&volume.scan)), full),
    );
    map.insert(
        "radar_parameters".to_owned(),
        drop_nulls(to_value(&volume.radar_parameters)),
    );
    if !volume.radar_calibration.is_empty() {
        let calibrations = volume
            .radar_calibration
            .iter()
            .map(|calibration| {
                let mut value = to_value(calibration);
                replace_pairs(&mut value, "extra", &calibration.extra);
                drop_nulls(value)
            })
            .collect();
        map.insert("radar_calibration".to_owned(), Value::Array(calibrations));
    }
    if let Some(correction) = &volume.georeferencing_correction {
        map.insert(
            "georeferencing_correction".to_owned(),
            drop_nulls(to_value(correction)),
        );
    }
    if !volume.extra_vars.is_empty() {
        map.insert(
            "extra_vars".to_owned(),
            extra_variables(&volume.extra_vars, full),
        );
    }
    map.insert(
        "provenance".to_owned(),
        drop_nulls(to_value(&volume.provenance)),
    );
    if let Some(simulation) = &volume.simulation {
        map.insert("simulation".to_owned(), drop_nulls(to_value(simulation)));
    }
    Value::Object(map)
}

/// Range of a sweep in kilometres to the far edge of its last gate.
pub(crate) fn max_range_km(sweep: &Sweep) -> Option<f64> {
    let ngates = sweep.range.ngates();
    let last = sweep.range.center_m(ngates.checked_sub(1)?)?;
    let half = sweep.range.spacing_m().unwrap_or(0.0) / 2.0;
    Some((last + half) / 1000.0)
}

/// The first Nyquist velocity of a sweep, when it has one.
pub(crate) fn nyquist_mps(sweep: &Sweep) -> Option<f32> {
    sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()?
        .iter()
        .copied()
        .find(|value| value.is_finite())
}

/// A sweep's metadata (everything but its fields).
pub(crate) fn sweep_metadata(sweep: &Sweep, index: usize, full: bool) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("index".to_owned(), json!(index));
    map.insert("sweep_number".to_owned(), json!(sweep.sweep_number));
    map.insert("sweep_mode".to_owned(), json!(sweep.sweep_mode.as_str()));
    if let Some(mode) = &sweep.follow_mode {
        map.insert("follow_mode".to_owned(), json!(mode.as_str()));
    }
    if let Some(mode) = &sweep.prt_mode {
        map.insert("prt_mode".to_owned(), json!(mode.as_str()));
    }
    if let Some(mode) = &sweep.polarization_mode {
        map.insert("polarization_mode".to_owned(), json!(mode.as_str()));
    }
    if let Some(sequence) = &sweep.polarization_sequence {
        map.insert("polarization_sequence".to_owned(), json!(sequence));
    }
    map.insert(
        "fixed_angle_deg".to_owned(),
        float_value(f64::from(sweep.fixed_angle_deg)),
    );
    for (key, value) in [
        (
            "target_scan_rate_deg_per_s",
            sweep.target_scan_rate_deg_per_s,
        ),
        ("rays_angle_resolution_deg", sweep.rays_angle_resolution_deg),
    ] {
        if let Some(value) = value {
            map.insert(key.to_owned(), float_value(f64::from(value)));
        }
    }
    if let Some(indexed) = sweep.rays_are_indexed {
        map.insert("rays_are_indexed".to_owned(), json!(indexed));
    }
    if let Some(qc) = &sweep.qc_procedures {
        map.insert("qc_procedures".to_owned(), json!(qc));
    }
    if let Some(number) = sweep.elevation_number {
        map.insert("elevation_number".to_owned(), json!(number));
    }
    map.insert("complete".to_owned(), json!(sweep.complete));
    map.insert("nrays".to_owned(), json!(sweep.nrays()));
    map.insert(
        "rays".to_owned(),
        summarize_arrays(to_value(&sweep.rays), full),
    );
    map.insert(
        "range".to_owned(),
        summarize_arrays(to_value(&sweep.range), full),
    );
    if let Some(km) = max_range_km(sweep) {
        map.insert("max_range_km".to_owned(), float_value(km));
    }
    let ray_vars = drop_nulls(to_value(&sweep.ray_vars));
    if ray_vars.as_object().is_some_and(|m| !m.is_empty()) {
        map.insert("ray_vars".to_owned(), summarize_arrays(ray_vars, full));
    }
    if let Some(monitoring) = &sweep.monitoring {
        map.insert(
            "monitoring".to_owned(),
            summarize_arrays(drop_nulls(to_value(monitoring)), full),
        );
    }
    if let Some(track) = &sweep.platform_track {
        map.insert(
            "platform_track".to_owned(),
            summarize_arrays(drop_nulls(to_value(track)), full),
        );
    }
    if !sweep.extra_vars.is_empty() {
        map.insert(
            "extra_vars".to_owned(),
            extra_variables(&sweep.extra_vars, full),
        );
    }
    if !sweep.other.is_empty() {
        map.insert("other".to_owned(), attrs_object(&sweep.other));
    }
    map
}

/// Gate counts and value range of a field.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FieldStats {
    pub values: u64,
    pub missing: u64,
    pub undetect: u64,
    pub range_folded: u64,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub mean: Option<f64>,
    pub non_finite: u64,
}

pub(crate) fn field_stats(field: &Field) -> FieldStats {
    let (rows, gates) = field.shape();
    let mut stats = FieldStats::default();
    let mut sum = 0.0f64;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for row in 0..rows {
        for gate in 0..gates {
            match field.gate(row, gate) {
                Some(Gate::Value(value)) if value.is_finite() => {
                    stats.values += 1;
                    sum += f64::from(value);
                    min = min.min(value);
                    max = max.max(value);
                }
                Some(Gate::Value(_)) => stats.non_finite += 1,
                Some(Gate::Undetect) => stats.undetect += 1,
                Some(Gate::RangeFolded) => stats.range_folded += 1,
                // Missing, and any gate class added to the model later.
                _ => stats.missing += 1,
            }
        }
    }
    if stats.values > 0 {
        stats.min = Some(min);
        stats.max = Some(max);
        stats.mean = Some(sum / stats.values as f64);
    }
    stats
}

fn coding_value(data: &FieldData) -> Value {
    let coding = match data {
        FieldData::U8 { coding, .. } => to_value(coding),
        FieldData::U16 { coding, .. } => to_value(coding),
        FieldData::I8 { coding, .. } => to_value(coding),
        FieldData::I16 { coding, .. } => to_value(coding),
        FieldData::I32 { coding, .. } => to_value(coding),
        FieldData::F32 { coding, .. } => to_value(coding),
        FieldData::F64 { coding, .. } => to_value(coding),
    };
    let mut coding = drop_nulls(coding);
    // A linear transform has CF equivalents; a Level III level table has
    // none (its coding serializes with the table).
    if let (Value::Object(map), Some(transform)) = (&mut coding, data.transform())
        && let (Some(scale), Some(offset)) = (transform.scale_factor(), transform.add_offset())
    {
        map.insert("scale_factor".to_owned(), float_value(scale));
        map.insert("add_offset".to_owned(), float_value(offset));
    }
    coding
}

/// A field's description. `stats` adds gate counts and the value range.
pub(crate) fn field_metadata(
    field: &Field,
    sweep: &Sweep,
    stats: bool,
    full: bool,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("name".to_owned(), json!(field.name.as_str()));
    map.insert(
        "quantity".to_owned(),
        json!(format!("{:?}", field.quantity)),
    );
    map.insert(
        "polarization".to_owned(),
        json!(format!("{:?}", field.polarization)),
    );
    let mut attrs = to_value(&field.attrs);
    replace_pairs(&mut attrs, "other", &field.attrs.other);
    let attrs = drop_nulls(attrs);
    let attrs = match attrs {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !matches!(v, Value::Array(items) if items.is_empty()))
                .collect(),
        ),
        other => other,
    };
    map.insert("attrs".to_owned(), attrs);
    map.insert("dtype".to_owned(), json!(field.data.dtype()));
    map.insert("nrays".to_owned(), json!(field.nrays));
    map.insert("ngates".to_owned(), json!(field.ngates));
    map.insert(
        "gates".to_owned(),
        json!({ "start": field.gates.start, "stride": field.gates.stride }),
    );
    if let Some((first, spacing)) = field.native_geometry(&sweep.range) {
        map.insert(
            "native_range".to_owned(),
            json!({ "first_center_m": first, "spacing_m": spacing }),
        );
    }
    map.insert("coding".to_owned(), coding_value(&field.data));
    map.insert("absent_rows".to_owned(), json!(field.absent_rows.len()));
    if full && !field.absent_rows.is_empty() {
        map.insert("absent_row_indices".to_owned(), json!(field.absent_rows));
    }
    if stats {
        let stats = field_stats(field);
        map.insert(
            "stats".to_owned(),
            json!({
                "values": stats.values,
                "missing": stats.missing,
                "undetect": stats.undetect,
                "range_folded": stats.range_folded,
                "non_finite": stats.non_finite,
                "min": stats.min,
                "max": stats.max,
                "mean": stats.mean,
            }),
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_attribute_names_keep_every_value() {
        let pairs: Vec<(Box<str>, AttrValue)> = vec![
            ("task".into(), AttrValue::text("a")),
            ("task".into(), AttrValue::text("b")),
            ("gain".into(), AttrValue::Scalar(Scalar::F64(0.5))),
        ];
        assert_eq!(
            attrs_object(&pairs),
            json!({ "task": ["a", "b"], "gain": 0.5 })
        );
    }

    #[test]
    fn long_numeric_arrays_are_summarized() {
        let values: Vec<Value> = (0..20).map(|v| json!(v)).collect();
        let summary = summarize_arrays(json!({ "azimuth": values }), false);
        assert_eq!(
            summary,
            json!({ "azimuth": { "count": 20, "missing": 0, "first": 0.0, "last": 19.0, "min": 0.0, "max": 19.0 } })
        );
        let short = json!({ "legs": [1, 2, 3] });
        assert_eq!(summarize_arrays(short.clone(), false), short);
    }
}
