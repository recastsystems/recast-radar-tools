//! Every attribute and every data plane of real ODIM_H5 files reaches the
//! FM301 model, in a typed slot or verbatim (G7).
//!
//! The reference is h5py (the HDF Group's C library, independent of the
//! Rust HDF5 reader): `testdata/golden/hdf5/<id>.json`, written by
//! `tools/hdf5_golden.py`, lists every object of the file with every
//! attribute (datatype, value; long arrays as a SHA-256 of the stored
//! little-endian bytes) and every dataset's values (the same hash). For each
//! file this test walks that list and finds each item in the decoded
//! [`Volume`]:
//!
//! - an attribute a typed slot holds must have the slot's value (the table
//!   in [`slot_check`]: location, fixed angle, range, codings, ray
//!   coordinates, radar parameters, calibration, Nyquist, scan rate, ...);
//! - any other attribute must be in the passthrough of its level (root
//!   `attrs.other`, sweep `other`, field `attrs.other`) under its name,
//!   `<group>.<name>`, or `<subgroup>.<name>` for a nested group, with an
//!   equal value (stored width restored before hashing);
//! - every `data` plane (of a `dataM` group or a quality group) must be a
//!   field whose stored values hash equal to h5py's;
//! - every `legend` dataset must be the field's `flag_values` and
//!   `flag_meanings`.
//!
//! Nothing is skipped: an item the test cannot place fails it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use recast_radar_core::fm301::{self, Passthrough, ViewOptions};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, Field, FieldData, LinearTransform, RangeCoord, Sweep, Volume,
};
use recast_radar_io_odim::read_odim_h5_volume;
use serde_json::Value;

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("hdf5")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn sha256(bytes: &[u8]) -> String {
    recast_radar_testdata::sha256_hex(bytes)
}

/// Where an HDF5 object's items land in the model.
#[derive(Clone, Debug, PartialEq)]
enum Place {
    Root,
    Sweep(usize),
    Field(usize, String),
}

/// An HDF5 path split into the model place and the key prefix its
/// attributes get (`radar_system.` for `/how/radar_system`), and the ODIM
/// group the object is (`what`, `where`, `how`, `data`, `legend`, or `` for
/// the level's own group and unknown groups).
#[derive(Debug)]
struct Location {
    place: Place,
    group: String,
    prefix: String,
}

/// The decoded field name of each `dataM` and quality group of each
/// dataset: the plane's `what/quantity` (`<quantity>_<dataM>` for a second
/// plane of a quantity), `<plane>_qualityK` for a plane's quality group and
/// `qualityK` for a dataset's.
fn field_names(golden: &Value, datasets: &[String]) -> BTreeMap<String, String> {
    let objects = golden["objects"].as_object().unwrap();
    let mut names = BTreeMap::new();
    for dataset in datasets {
        let mut planes: Vec<(u32, String)> = links(golden, dataset)
            .into_iter()
            .filter_map(|name| numbered(&name, "data").map(|n| (n, name)))
            .collect();
        planes.sort();
        let mut taken: Vec<String> = Vec::new();
        for (_, plane) in planes {
            let path = format!("{dataset}/{plane}");
            let quantity = objects
                .get(&format!("{path}/what"))
                .and_then(|what| text_attr(what, "quantity"))
                .unwrap_or_else(|| plane.to_uppercase());
            let name = if taken.contains(&quantity) {
                format!("{quantity}_{plane}")
            } else {
                quantity
            };
            taken.push(name.clone());
            for quality in links(golden, &path) {
                if numbered(&quality, "quality").is_some() {
                    names.insert(format!("{path}/{quality}"), format!("{name}_{quality}"));
                }
            }
            names.insert(path, name);
        }
        for quality in links(golden, dataset) {
            if numbered(&quality, "quality").is_some() {
                names.insert(format!("{dataset}/{quality}"), quality);
            }
        }
    }
    names
}

fn numbered(name: &str, prefix: &str) -> Option<u32> {
    name.strip_prefix(prefix)?.parse().ok()
}

fn links(golden: &Value, path: &str) -> Vec<String> {
    let key = if path.is_empty() { "/" } else { path };
    golden["objects"][key]["links"]
        .as_array()
        .map(|links| {
            links
                .iter()
                .map(|link| link["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn text_attr(object: &Value, name: &str) -> Option<String> {
    object["attributes"]
        .as_array()?
        .iter()
        .find(|attr| attr["name"] == name)
        .and_then(|attr| attr["value"]["values"][0].as_str())
        .map(str::to_owned)
}

fn locate(path: &str, datasets: &[String], fields: &BTreeMap<String, String>) -> Location {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let (place, rest): (Place, &[&str]) = match parts.as_slice() {
        [] => (Place::Root, &[]),
        [dataset, rest @ ..] if datasets.contains(&format!("/{dataset}")) => {
            let sweep = datasets
                .iter()
                .position(|name| name == &format!("/{dataset}"))
                .unwrap();
            // The deepest data or quality group on the path is the field.
            let mut field = None;
            for depth in (1..=rest.len()).rev() {
                let group = format!("/{dataset}/{}", rest[..depth].join("/"));
                if let Some(name) = fields.get(&group) {
                    field = Some((name.clone(), depth));
                    break;
                }
            }
            match field {
                Some((name, depth)) => (Place::Field(sweep, name), &rest[depth..]),
                None => (Place::Sweep(sweep), rest),
            }
        }
        _ => (Place::Root, &parts[..]),
    };
    let group = rest.first().copied().unwrap_or_default();
    let odim_group = matches!(group, "what" | "where" | "how");
    let nested: &[&str] = if odim_group { &rest[1..] } else { rest };
    let prefix: String = nested.iter().map(|part| format!("{part}.")).collect();
    Location {
        place,
        group: if odim_group || matches!(group, "data" | "legend") {
            group.to_owned()
        } else {
            String::new()
        },
        prefix,
    }
}

/// The golden attribute value as numbers (`None` for text).
fn golden_numbers(value: &Value) -> Option<Vec<f64>> {
    value["values"].as_array().map(|values| {
        values
            .iter()
            .map(|v| match v {
                Value::String(s) if s == "NaN" => f64::NAN,
                Value::String(s) if s == "Infinity" => f64::INFINITY,
                Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
                other => other.as_f64().unwrap_or(f64::NAN),
            })
            .collect()
    })
}

/// SHA-256 of our numbers in the golden's stored width, little-endian (the
/// golden's canonical encoding).
fn stored_sha(numbers: &[f64], ty: &Value) -> String {
    let size = ty["size"].as_u64().unwrap_or(8);
    let signed = ty["signed"].as_bool().unwrap_or(true);
    let class = ty["class"].as_str().unwrap_or_default();
    let mut out = Vec::new();
    for value in numbers {
        match (class, size, signed) {
            ("float", 4, _) => out.extend_from_slice(&(*value as f32).to_le_bytes()),
            ("float", _, _) => out.extend_from_slice(&value.to_le_bytes()),
            (_, 1, true) => out.extend_from_slice(&(*value as i8).to_le_bytes()),
            (_, 1, false) => out.extend_from_slice(&(*value as u8).to_le_bytes()),
            (_, 2, true) => out.extend_from_slice(&(*value as i16).to_le_bytes()),
            (_, 2, false) => out.extend_from_slice(&(*value as u16).to_le_bytes()),
            (_, 4, true) => out.extend_from_slice(&(*value as i32).to_le_bytes()),
            (_, 4, false) => out.extend_from_slice(&(*value as u32).to_le_bytes()),
            (_, _, true) => out.extend_from_slice(&(*value as i64).to_le_bytes()),
            (_, _, false) => out.extend_from_slice(&(*value as u64).to_le_bytes()),
        }
    }
    sha256(&out)
}

fn our_numbers(value: &AttrValue) -> Option<Vec<f64>> {
    match value {
        AttrValue::Scalar(scalar) => Some(vec![scalar.as_f64()]),
        AttrValue::Array(array) if !matches!(array, ArrayBuf::Text(_)) => {
            Some((0..array.len()).filter_map(|i| array.get_f64(i)).collect())
        }
        AttrValue::Bool(value) => Some(vec![f64::from(u8::from(*value))]),
        _ => None,
    }
}

fn our_texts(value: &AttrValue) -> Option<Vec<String>> {
    match value {
        AttrValue::Text(text) => Some(vec![text.to_string()]),
        AttrValue::Array(ArrayBuf::Text(texts)) => {
            Some(texts.iter().map(|text| text.to_string()).collect())
        }
        _ => None,
    }
}

/// True when our passthrough value equals the golden attribute's.
fn same_value(attr: &Value, ours: &AttrValue) -> bool {
    let ty = &attr["type"];
    let value = &attr["value"];
    let len = value["len"].as_u64().unwrap_or(0) as usize;
    match ty["class"].as_str().unwrap_or_default() {
        "string" | "vlen_string" => {
            let Some(texts) = our_texts(ours) else {
                return false;
            };
            if let Some(values) = value["values"].as_array() {
                let golden: Vec<&str> = values.iter().map(|v| v.as_str().unwrap()).collect();
                return texts == golden;
            }
            let mut bytes = Vec::new();
            for text in &texts {
                bytes.extend_from_slice(text.as_bytes());
                bytes.push(0);
            }
            texts.len() == len && Some(sha256(&bytes).as_str()) == value["sha256"].as_str()
        }
        "integer" | "float" => {
            let Some(numbers) = our_numbers(ours) else {
                return false;
            };
            numbers.len() == len
                && Some(stored_sha(&numbers, ty).as_str()) == value["sha256"].as_str()
        }
        "enum" => {
            // By member name (a two-member FALSE/TRUE enum as a bool).
            let members = ty["members"].as_object().unwrap();
            let codes = golden_numbers(value).unwrap_or_default();
            let names: Vec<String> = codes
                .iter()
                .map(|code| {
                    members
                        .iter()
                        .find(|(_, v)| v.as_f64() == Some(*code))
                        .map(|(name, _)| name.clone())
                        .unwrap()
                })
                .collect();
            match ours {
                AttrValue::Bool(flag) => names == [if *flag { "TRUE" } else { "FALSE" }],
                other => our_texts(other).is_some_and(|texts| texts == names),
            }
        }
        _ => false,
    }
}

fn first_number(attr: &Value) -> Option<f64> {
    golden_numbers(&attr["value"]).and_then(|values| values.first().copied())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

/// `f32` slot against an attribute value.
fn close32(slot: Option<f32>, value: f64) -> bool {
    slot.is_some_and(|slot| {
        f64::from(slot) == f64::from(value as f32) || close(f64::from(slot), value)
    })
}

fn field<'a>(volume: &'a Volume, sweep: usize, name: &str) -> &'a Field {
    volume.sweeps[sweep]
        .fields
        .iter()
        .find(|field| field.name.as_str() == name)
        .unwrap_or_else(|| panic!("sweep {sweep}: no field {name}"))
}

fn transform_of(field: &Field) -> Option<(f64, f64)> {
    let transform = match &field.data {
        FieldData::U8 { coding, .. } => Some(coding.transform),
        FieldData::I8 { coding, .. } => Some(coding.transform),
        FieldData::U16 { coding, .. } => Some(coding.transform),
        FieldData::I16 { coding, .. } => Some(coding.transform),
        FieldData::I32 { coding, .. } => Some(coding.transform),
        FieldData::F32 { coding, .. } => coding.transform,
        FieldData::F64 { coding, .. } => coding.transform,
    };
    match transform {
        Some(LinearTransform::CfScaleOffset {
            scale_factor,
            add_offset,
            ..
        }) => Some((scale_factor, add_offset)),
        None => Some((1.0, 0.0)),
        _ => None,
    }
}

/// `(fill, undetect)` codes of a field's coding, as f64.
fn sentinels(field: &Field) -> (Option<f64>, Option<f64>) {
    fn int<T: Copy + Into<f64>>(
        fill: Option<T>,
        undetect: Option<T>,
    ) -> (Option<f64>, Option<f64>) {
        (fill.map(Into::into), undetect.map(Into::into))
    }
    match &field.data {
        FieldData::U8 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::I8 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::U16 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::I16 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::I32 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::F32 { coding, .. } => int(coding.fill_value, coding.undetect),
        FieldData::F64 { coding, .. } => (coding.fill_value, coding.undetect),
    }
}

/// SHA-256 of the stored field values, little-endian (the golden's dataset
/// encoding).
fn field_sha(field: &Field) -> String {
    let mut out = Vec::new();
    match &field.data {
        FieldData::U8 { values, .. } => out.extend_from_slice(values),
        FieldData::I8 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
        FieldData::U16 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
        FieldData::I16 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
        FieldData::I32 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
        FieldData::F32 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
        FieldData::F64 { values, .. } => values.iter().for_each(|v| out.extend(v.to_le_bytes())),
    }
    sha256(&out)
}

/// The leading values of a golden attribute (all of them when inlined, else
/// the recorded head).
fn leading(attr: &Value) -> Vec<f64> {
    let value = &attr["value"];
    let list = if value["values"].is_array() {
        &value["values"]
    } else {
        &value["head"]
    };
    golden_numbers(&serde_json::json!({ "values": list })).unwrap_or_default()
}

/// A sibling attribute of the same group.
fn sibling<'a>(group: &'a Value, name: &str) -> Option<&'a Value> {
    group["attributes"]
        .as_array()?
        .iter()
        .find(|attr| attr["name"] == name)
}

/// A per-ray `how` array the sweep's ray coordinates were built from: one
/// value per ray, and each leading ray's coordinate is the array's (the
/// middle of start and stop, as ODIM_H5 v2.4 Table 8 defines the ray).
fn ray_coordinate(volume: &Volume, sweep: &Sweep, group: &Value, name: &str, attr: &Value) -> bool {
    let len = attr["value"]["len"].as_u64().unwrap_or(0) as usize;
    if len != sweep.nrays() {
        return false;
    }
    let pair = |start: &str, stop: &str| {
        let starts = sibling(group, start).map(leading).unwrap_or_default();
        let stops = sibling(group, stop).map(leading).unwrap_or_default();
        (starts, stops)
    };
    match name {
        "startazA" | "stopazA" => {
            let (starts, stops) = pair("startazA", "stopazA");
            starts
                .iter()
                .zip(&stops)
                .enumerate()
                .all(|(ray, (start, stop))| {
                    let stop = if stop < start { stop + 360.0 } else { *stop };
                    let mut middle = (start + stop) / 2.0;
                    if middle >= 360.0 {
                        middle -= 360.0;
                    }
                    (f64::from(sweep.rays.azimuth_deg[ray]) - middle).abs() < 1e-4
                })
                && !starts.is_empty()
        }
        "startelA" | "stopelA" => {
            let (starts, stops) = pair("startelA", "stopelA");
            starts
                .iter()
                .zip(&stops)
                .enumerate()
                .all(|(ray, (start, stop))| {
                    (f64::from(sweep.rays.elevation_deg[ray]) - (start + stop) / 2.0).abs() < 1e-4
                })
                && !starts.is_empty()
        }
        "elangles" => {
            let angles = leading(attr);
            angles
                .iter()
                .enumerate()
                .all(|(ray, angle)| (f64::from(sweep.rays.elevation_deg[ray]) - angle).abs() < 1e-4)
                && !angles.is_empty()
        }
        "startazT" | "stopazT" => {
            let (starts, stops) = pair("startazT", "stopazT");
            let reference = volume.time_reference.timestamp() as f64;
            starts
                .iter()
                .zip(&stops)
                .enumerate()
                .all(|(ray, (start, stop))| {
                    (sweep.rays.time_s[ray] + reference - (start + stop) / 2.0).abs() < 1e-3
                })
                && !starts.is_empty()
        }
        _ => false,
    }
}

/// A `how` attribute a typed slot of `sweeps` (or of the volume) holds.
fn how_slot(volume: &Volume, sweeps: &[&Sweep], group: &Value, name: &str, attr: &Value) -> bool {
    if matches!(
        name,
        "startazA" | "stopazA" | "startelA" | "stopelA" | "elangles" | "startazT" | "stopazT"
    ) {
        return sweeps.len() == 1 && ray_coordinate(volume, sweeps[0], group, name, attr);
    }
    let Some(value) = first_number(attr) else {
        return false;
    };
    let parameters = &volume.radar_parameters;
    let first = |slot: &Option<Vec<f32>>| slot.as_ref().and_then(|values| values.first().copied());
    let any_sweep = |check: &dyn Fn(&Sweep) -> bool| sweeps.iter().any(|sweep| check(sweep));
    let calibration = |check: &dyn Fn(&recast_radar_core::model::RadarCalibration) -> bool| {
        volume.radar_calibration.iter().any(check)
    };
    match name {
        "beamwH" => close32(parameters.beam_width_h_deg, value),
        "beamwV" => close32(parameters.beam_width_v_deg, value),
        "beamwidth" => {
            close32(parameters.beam_width_h_deg, value)
                || close32(parameters.beam_width_v_deg, value)
        }
        "antgainH" => {
            close32(parameters.antenna_gain_h_db, value)
                || calibration(&|entry| close32(entry.antenna_gain_h_db, value))
        }
        "antgainV" => {
            close32(parameters.antenna_gain_v_db, value)
                || calibration(&|entry| close32(entry.antenna_gain_v_db, value))
        }
        "RXbandwidth" => close32(parameters.receiver_bandwidth_hz, value * 1e6),
        "radconstH" => calibration(&|entry| close32(entry.radar_constant_h, value)),
        "radconstV" => calibration(&|entry| close32(entry.radar_constant_v, value)),
        "NI" => any_sweep(&|sweep| close32(first(&sweep.ray_vars.nyquist_velocity_mps), value)),
        "rpm" => any_sweep(&|sweep| close32(sweep.target_scan_rate_deg_per_s, value * 6.0)),
        "antspeed" => any_sweep(&|sweep| close32(sweep.target_scan_rate_deg_per_s, value)),
        "pulsewidth" => {
            any_sweep(&|sweep| close32(first(&sweep.ray_vars.pulse_width_s), value * 1e-6))
        }
        _ => false,
    }
}

// The typed slot check of attribute `name` of `location`, or `None` when
/// the attribute has no slot there.
fn slot_check(
    volume: &Volume,
    location: &Location,
    group: &Value,
    name: &str,
    attr: &Value,
) -> Option<bool> {
    let text = || attr["value"]["values"][0].as_str().map(str::to_owned);
    let number = || first_number(attr);
    let all_sweeps: Vec<&Sweep> = volume.sweeps.iter().collect();
    match (
        &location.place,
        location.group.as_str(),
        location.prefix.as_str(),
        name,
    ) {
        (Place::Root, "", "", "Conventions") => {
            Some(volume.provenance.source_conventions == text())
        }
        (Place::Root, "what", "", "version") => Some(volume.provenance.source_version == text()),
        (Place::Root, "what", "", "source") => Some(volume.attrs.source == text()),
        (Place::Root, "what", "", "date") => {
            Some(Some(volume.time_reference.format("%Y%m%d").to_string()) == text())
        }
        (Place::Root, "what", "", "time") => {
            Some(Some(volume.time_reference.format("%H%M%S").to_string()) == text())
        }
        (Place::Root, "where", "", "lat") => Some(volume.location.latitude_deg == number()),
        (Place::Root, "where", "", "lon") => Some(volume.location.longitude_deg == number()),
        (Place::Root, "where", "", "height") => Some(volume.location.altitude_m == number()),
        (Place::Root, "how", "", _) => Some(how_slot(volume, &all_sweeps, group, name, attr)),
        (Place::Sweep(index), "where", "", _) => {
            let sweep = &volume.sweeps[*index];
            let value = number()?;
            let RangeCoord::Uniform {
                first_center_m,
                spacing_m,
                ngates,
            } = sweep.range
            else {
                return Some(false);
            };
            Some(match name {
                "elangle" => close32(Some(sweep.fixed_angle_deg), value),
                "nbins" => f64::from(ngates) == value,
                "nrays" => sweep.nrays() as f64 == value,
                "rscale" => spacing_m == value,
                // ODIM_H5 v2.4 states metres, earlier versions km.
                "rstart" => {
                    let v24 =
                        volume.provenance.source_conventions.as_deref() == Some("ODIM_H5/V2_4");
                    close(
                        first_center_m - spacing_m / 2.0,
                        value * if v24 { 1.0 } else { 1000.0 },
                    )
                }
                _ => return None,
            })
        }
        (Place::Sweep(index), "how", "", _) => Some(how_slot(
            volume,
            &[&volume.sweeps[*index]],
            group,
            name,
            attr,
        )),
        (Place::Field(sweep, field_name), "what", "", _) => {
            let field = field(volume, *sweep, field_name);
            Some(match name {
                "quantity" => {
                    let quantity = text()?;
                    field.name.as_str() == quantity
                        || field.name.as_str().starts_with(&format!("{quantity}_data"))
                }
                "gain" => transform_of(field).is_some_and(|(gain, _)| Some(gain) == number()),
                "offset" => transform_of(field).is_some_and(|(_, offset)| Some(offset) == number()),
                "nodata" => sentinels(field).0 == number(),
                "undetect" => sentinels(field).1 == number(),
                _ => return None,
            })
        }
        _ => None,
    }
}

fn others<'a>(volume: &'a Volume, place: &Place) -> &'a [(Box<str>, AttrValue)] {
    match place {
        Place::Root => &volume.attrs.other,
        Place::Sweep(index) => &volume.sweeps[*index].other,
        Place::Field(sweep, name) => &field(volume, *sweep, name).attrs.other,
    }
}

/// Plane values and legends.
fn check_dataset(
    volume: &Volume,
    location: &Location,
    path: &str,
    dataset: &Value,
    failures: &mut Vec<String>,
) -> bool {
    let Place::Field(sweep, name) = &location.place else {
        failures.push(format!("{path}: a dataset outside a data or quality group"));
        return false;
    };
    let field = field(volume, *sweep, name);
    match location.group.as_str() {
        "data" => {
            let expected = dataset["value"]["sha256"].as_str();
            if Some(field_sha(field).as_str()) != expected {
                failures.push(format!("{path}: values of field {name} differ from h5py's"));
            }
            let shape: Vec<u64> = dataset["shape"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap())
                .collect();
            if shape != [u64::from(field.nrays), u64::from(field.ngates)] {
                failures.push(format!(
                    "{path}: shape {shape:?} vs {}x{}",
                    field.nrays, field.ngates
                ));
            }
            if let Some(members) = dataset["type"]["members"].as_object() {
                // An enumerated plane names its codes.
                let mut expected: Vec<(i64, &str)> = members
                    .iter()
                    .map(|(name, value)| (value.as_i64().unwrap(), name.as_str()))
                    .collect();
                expected.sort();
                let mut ours: Vec<(i64, &str)> = field
                    .attrs
                    .flag_values
                    .iter()
                    .copied()
                    .zip(field.attrs.flag_meanings.iter().map(|m| &**m))
                    .collect();
                ours.sort();
                if ours != expected {
                    failures.push(format!(
                        "{path}: enum members {expected:?} vs flags {ours:?}"
                    ));
                }
            }
            true
        }
        "legend" => {
            let value = &dataset["value"]["compound"];
            let codes: Vec<i64> = field.attrs.flag_values.clone();
            let meanings: Vec<String> = field
                .attrs
                .flag_meanings
                .iter()
                .map(|m| m.to_string())
                .collect();
            let ok = if value.get("code").is_some() {
                // FMI: code (integer) and class (string) columns.
                let golden_codes: Vec<i64> = golden_numbers(&value["code"])
                    .unwrap()
                    .iter()
                    .map(|v| *v as i64)
                    .collect();
                let golden_classes: Vec<String> = value["class"]["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().replace(' ', "_"))
                    .collect();
                codes == golden_codes && meanings == golden_classes
            } else {
                // ODIM_H5 v2.4: key (class name) and value (code as text),
                // NUL-padded character arrays.
                let width = |member: &str| {
                    dataset["type"]["members"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|m| m["name"] == member)
                        .map(|m| m["type"]["size"].as_u64().unwrap() as usize)
                        .unwrap()
                };
                let padded = |texts: Vec<String>, width: usize| {
                    let mut bytes = Vec::new();
                    for text in texts {
                        let mut row = text.into_bytes();
                        row.resize(width, 0);
                        bytes.extend(row);
                    }
                    sha256(&bytes)
                };
                let keys = padded(meanings.clone(), width("key"));
                let values = padded(codes.iter().map(i64::to_string).collect(), width("value"));
                Some(keys.as_str()) == value["key"]["sha256"].as_str()
                    && Some(values.as_str()) == value["value"]["sha256"].as_str()
            };
            if !ok {
                failures.push(format!("{path}: legend vs flags {codes:?} {meanings:?}"));
            }
            true
        }
        _ => {
            failures.push(format!("{path}: unexpected dataset"));
            false
        }
    }
}

#[derive(Default, Debug)]
struct Counts {
    slotted: usize,
    passthrough: usize,
    planes: usize,
    legends: usize,
}

/// The counts for `id`, or `None` when the file is not redistributed and
/// not in the testdata cache.
fn check(id: &str) -> Option<Counts> {
    let golden = golden(id);
    let bytes = recast_radar_testdata::bytes_if_available(id)?;
    let volume = read_odim_h5_volume(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"));
    // The FM301 view takes every field (quality fields and their flags
    // included) in each flavor, and with every passthrough item.
    for options in [
        ViewOptions::XRADAR,
        ViewOptions::WMO,
        ViewOptions {
            passthrough: Passthrough::All,
            ..ViewOptions::XRADAR
        },
    ] {
        let view = fm301::volume_view(&volume, options, None)
            .unwrap_or_else(|err| panic!("{id}: view {options:?}: {err}"));
        for (index, sweep) in volume.sweeps.iter().enumerate() {
            let group = view.group(&format!("sweep_{index}")).expect("sweep group");
            for field in &sweep.fields {
                assert!(
                    group.variable(field.name.as_str()).is_some(),
                    "{id}: sweep {index} {} not in the {options:?} view",
                    field.name.as_str()
                );
            }
        }
    }
    let mut datasets: Vec<(u32, String)> = links(&golden, "/")
        .into_iter()
        .filter_map(|name| numbered(&name, "dataset").map(|n| (n, format!("/{name}"))))
        .collect();
    datasets.sort();
    let datasets: Vec<String> = datasets.into_iter().map(|(_, name)| name).collect();
    assert_eq!(
        volume.sweeps.len(),
        datasets.len(),
        "{id}: one sweep per dataset"
    );
    let fields = field_names(&golden, &datasets);
    let decoded_fields: usize = volume.sweeps.iter().map(|sweep| sweep.fields.len()).sum();
    assert_eq!(
        decoded_fields,
        fields.len(),
        "{id}: one field per data and quality group"
    );

    let mut counts = Counts::default();
    let mut failures = Vec::new();
    for (path, object) in golden["objects"].as_object().unwrap() {
        let location = locate(path, &datasets, &fields);
        if object["kind"] == "dataset" {
            let dataset = &object["dataset"];
            if check_dataset(&volume, &location, path, dataset, &mut failures) {
                if location.group == "legend" {
                    counts.legends += 1;
                } else {
                    counts.planes += 1;
                }
            }
        }
        for attr in object["attributes"].as_array().unwrap() {
            let name = attr["name"].as_str().unwrap();
            // `<subgroup>.<name>` below a what/where/how group (or
            // `<group>.<name>` when the level already had the name);
            // `data.<name>`, `legend.<name>` and `<group>.<name>` for the
            // attributes of datasets and unknown groups.
            let key = format!("{}{name}", location.prefix);
            let candidates = [key.clone(), format!("{}.{key}", location.group)];
            let passthrough = others(&volume, &location.place)
                .iter()
                .find(|(have, _)| candidates.iter().any(|c| &**have == c.as_str()));
            match passthrough {
                Some((_, value)) if same_value(attr, value) => counts.passthrough += 1,
                Some((have, value)) => failures.push(format!(
                    "{path}@{name}: passthrough {have} = {value:?} differs from {}",
                    attr["value"]
                )),
                None => match slot_check(&volume, &location, object, name, attr) {
                    Some(true) => counts.slotted += 1,
                    Some(false) => failures.push(format!(
                        "{path}@{name}: typed slot does not hold {}",
                        attr["value"]
                    )),
                    None => failures.push(format!(
                        "{path}@{name}: not in the model ({location:?}, key {key})"
                    )),
                },
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{id}: {} items did not reach the model:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Some(counts)
}

macro_rules! every_value_tests {
    ($($name:ident => $id:literal,)*) => {
        $(
            #[test]
            fn $name() {
                let Some(counts) = check($id) else {
                    return;
                };
                eprintln!("{}: {counts:?}", $id);
                assert!(counts.planes > 0);
            }
        )*
    };
}

every_value_tests! {
    bejab => "odim-bejab-20190606-0000-pvol",
    bewid_bool_quality_planes => "odim-bewid-20130429-0430-pvol-dbzh-scan1",
    norst => "odim-norst-20170421-0908-pvol",
    espdg_plane_how => "odim-espdg-20260707-1927-pvol-dbzh-vradh",
    iesha => "odim-iesha-20260305-0115-pvol",
    dkrom => "odim-dkrom-20260820-1130-pvol",
    dkrom_h5latest => "odim-dkrom-20260820-1130-pvol-h5latest-trim",
    seang_int16_quality_how_subgroups => "odim-seang-20260924-2130-qcvol-dataset1-trim",
    fianj_quality_legends => "odim-fianj-20260924-2130-pvol-dataset1-trim",
    deboo_root_how_subgroups => "odim-deboo-20260924-2130-sweep-th-00",
    itdes_class_legend => "odim-itdes-20260924-2135-pvol-class",
}

/// The int16 planes of the SMHI file stay int16, with their codes.
#[test]
fn seang_int16_planes_keep_their_storage() {
    let bytes =
        recast_radar_testdata::bytes("odim-seang-20260924-2130-qcvol-dataset1-trim").unwrap();
    let volume = read_odim_h5_volume(&bytes).unwrap();
    let dbzh = field(&volume, 0, "DBZH");
    let FieldData::I16 { coding, .. } = &dbzh.data else {
        panic!("DBZH is {:?}", dbzh.data);
    };
    assert_eq!(coding.fill_value, Some(-32768));
    assert_eq!(coding.undetect, Some(-32767));
    // The three dataset quality groups qualify every plane.
    let quality: Vec<&str> = volume.sweeps[0]
        .fields
        .iter()
        .filter(|field| field.attrs.is_quality_field == Some(true))
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(quality, ["quality1", "quality2", "quality3"]);
    let ancillary: Vec<&str> = dbzh
        .attrs
        .ancillary_variables
        .iter()
        .map(|n| n.as_str())
        .collect();
    assert_eq!(ancillary, ["quality1", "quality2", "quality3"]);
    let task = field(&volume, 0, "quality2")
        .attrs
        .other
        .iter()
        .find(|(name, _)| &**name == "task")
        .map(|(_, value)| value.clone());
    assert_eq!(task, Some(AttrValue::text("se.smhi.detector.beamblockage")));
}
