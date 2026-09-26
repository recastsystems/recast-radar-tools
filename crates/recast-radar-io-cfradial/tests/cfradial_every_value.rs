//! Every variable and attribute of real CfRadial 1 and 2 files reaches the
//! FM301 model, in a typed slot or verbatim (G7).
//!
//! The reference is netCDF4-python (the netCDF-C library, independent of
//! this crate's classic reader and of `recast-radar-hdf5`):
//! `testdata/golden/netcdf4/<id>.json`, written by `tools/netcdf4_golden.py`,
//! lists every group with its attributes and every variable with its
//! dimensions, attributes and values (a SHA-256 of the stored little-endian
//! bytes). For each file this test places every item in the decoded
//! [`Volume`]:
//!
//! - a field variable (`(time, range)`; a CfRadial 2 sweep's `(ray, range)`)
//!   must be a field whose stored values hash equal to netCDF-C's, and each
//!   of its attributes must be in a field slot (names, units, packing, fill)
//!   or in `Field::attrs.other` with an equal value and type;
//! - any other variable must be an extra variable (root or sweep, under its
//!   name or `<group>.<name>`) with every attribute, or a variable a typed
//!   slot holds, whose attributes are all in `Volume::variable_attrs`;
//! - a global attribute must be in its typed slot or `attrs.other`; a sweep
//!   group's in `Sweep::other` (its `monitoring` group's as
//!   `monitoring.<name>`), a metadata group's in `attrs.other` as
//!   `<group>.<name>`.
//!
//! Nothing is skipped: an item the test cannot place fails it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldData, FloatWidth, LinearTransform, Scalar,
    Volume,
};
use serde_json::Value;

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("netcdf4")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn decode(id: &str) -> Option<Volume> {
    let bytes = match recast_radar_testdata::bytes(id) {
        Ok(bytes) => bytes,
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            return None;
        }
        Err(err) => panic!("{err}"),
    };
    Some(
        recast_radar_io_cfradial::read_cfradial_volume(&bytes).unwrap_or_else(|err| {
            panic!("{id}: {err}");
        }),
    )
}

/// SHA-256 of field values in their stored width, little-endian (the
/// golden's encoding), in the order `fields` gives the rows.
fn fields_sha(fields: &[&Field]) -> String {
    let mut bytes = Vec::new();
    for field in fields {
        match &field.data {
            FieldData::U8 { values, .. } => bytes.extend_from_slice(values),
            FieldData::I8 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
            FieldData::U16 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
            FieldData::I16 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
            FieldData::I32 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
            FieldData::F32 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
            FieldData::F64 { values, .. } => {
                values.iter().for_each(|v| bytes.extend(v.to_le_bytes()))
            }
        }
    }
    recast_radar_testdata::sha256_hex(&bytes)
}

/// A golden attribute value as numbers (NaN spelled as text).
fn numbers(value: &Value) -> Vec<f64> {
    let list = match value {
        Value::Array(list) => list.clone(),
        other => vec![other.clone()],
    };
    list.iter()
        .map(|v| match v {
            Value::String(s) if s == "NaN" => f64::NAN,
            Value::String(s) if s == "Infinity" => f64::INFINITY,
            Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
            other => other.as_f64().unwrap_or(f64::NAN),
        })
        .collect()
}

fn same_number(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// Our attribute value equals the golden one, value and type.
fn same_attr(golden: &Value, ours: &AttrValue) -> bool {
    let kind = golden["kind"].as_str().unwrap_or_default();
    let value = &golden["value"];
    if kind == "text" {
        return match (value, ours) {
            (Value::String(text), AttrValue::Text(have)) => **have == **text,
            (Value::Array(list), AttrValue::Array(ArrayBuf::Text(have))) => {
                list.len() == have.len()
                    && list
                        .iter()
                        .zip(have)
                        .all(|(want, have)| want.as_str() == Some(&**have))
            }
            _ => false,
        };
    }
    let want = numbers(value);
    match ours {
        AttrValue::Scalar(scalar) => {
            scalar.dtype() == kind && want.len() == 1 && same_number(scalar.as_f64(), want[0])
        }
        AttrValue::Array(array) => {
            array.dtype() == kind
                && array.len() == want.len()
                && (0..array.len()).all(|i| {
                    array
                        .get_f64(i)
                        .is_some_and(|have| same_number(have, want[i]))
                })
        }
        _ => false,
    }
}

fn find<'a>(attrs: &'a [(Box<str>, AttrValue)], name: &str) -> Option<&'a AttrValue> {
    attrs
        .iter()
        .find(|(have, _)| &**have == name)
        .map(|(_, value)| value)
}

fn text(value: &Value) -> Option<&str> {
    value["value"].as_str()
}

/// A global attribute in its typed slot.
fn global_slot(volume: &Volume, attr: &Value) -> Option<bool> {
    let name = attr["name"].as_str().unwrap();
    let raw = text(attr);
    let trimmed = raw
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let attrs = &volume.attrs;
    let bool_of = |text: &str| match text.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    };
    Some(match name {
        "Conventions" => volume.provenance.source_conventions.as_deref() == raw,
        "version" => volume.provenance.source_version.as_deref() == raw,
        "title" => attrs.title == trimmed,
        "institution" => attrs.institution == trimmed,
        "references" => attrs.references == trimmed,
        "source" => attrs.source == trimmed,
        "history" => attrs.history == trimmed,
        "comment" => attrs.comment == trimmed,
        "instrument_name" => trimmed.is_none_or(|t| attrs.instrument_name == t),
        "site_name" => attrs.site_name.as_deref() == raw,
        "scan_name" => volume.scan.name == trimmed,
        "scan_id" => {
            let want = match &attr["value"] {
                Value::String(text) => text.trim().to_owned(),
                other => format!("{}", numbers(other)[0]),
            };
            volume.scan.id.map(|id| id.to_string()) == Some(want.clone())
                || volume
                    .scan
                    .definition
                    .as_ref()
                    .and_then(|d| d.scan_id_text.clone())
                    == Some(want)
        }
        "platform_is_mobile" => raw.and_then(bool_of) == Some(attrs.platform_is_mobile),
        "ray_times_increase" => raw.and_then(bool_of) == attrs.ray_times_increase,
        "simulated" => raw.and_then(bool_of) == Some(attrs.simulated),
        "time_coverage_start" | "time_coverage_end" => {
            let coverage = volume.time_coverage.expect("time coverage");
            let instant = if name.ends_with("start") {
                coverage.start
            } else {
                coverage.end
            };
            raw.is_some_and(|raw| {
                raw.trim_end_matches('Z').replace(' ', "T")
                    == instant.format("%Y-%m-%dT%H:%M:%S").to_string()
            })
        }
        _ => return None,
    })
}

/// Field attributes a field slot holds.
fn field_slot(field: &Field, attr: &Value) -> Option<bool> {
    let name = attr["name"].as_str().unwrap();
    let want = || numbers(&attr["value"]).first().copied();
    let transform = match &field.data {
        FieldData::U8 { coding, .. } => Some(coding.transform),
        FieldData::I8 { coding, .. } => Some(coding.transform),
        FieldData::U16 { coding, .. } => Some(coding.transform),
        FieldData::I16 { coding, .. } => Some(coding.transform),
        FieldData::I32 { coding, .. } => Some(coding.transform),
        FieldData::F32 { coding, .. } => coding.transform,
        FieldData::F64 { coding, .. } => coding.transform,
    };
    let fill = match &field.data {
        FieldData::U8 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::I8 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::U16 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::I16 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::I32 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::F32 { coding, .. } => coding.fill_value.map(f64::from),
        FieldData::F64 { coding, .. } => coding.fill_value,
    };
    let width = |kind: &str| match kind {
        "float32" => Some(FloatWidth::F32),
        "float64" => Some(FloatWidth::F64),
        _ => None,
    };
    let kind = attr["kind"].as_str().unwrap_or_default();
    Some(match name {
        "standard_name" => field.attrs.standard_name.as_deref() == text(attr),
        "long_name" => field.attrs.long_name.as_deref() == text(attr),
        "units" => field.attrs.units.as_deref() == text(attr),
        "sampling_ratio" => {
            field.attrs.sampling_ratio.map(f64::from) == want().map(|v| f64::from(v as f32))
        }
        "scale_factor" | "add_offset" => match transform {
            Some(LinearTransform::CfScaleOffset {
                scale_factor,
                add_offset,
                attr_width,
            }) => {
                let have = if name == "scale_factor" {
                    scale_factor
                } else {
                    add_offset
                };
                want().is_some_and(|want| same_number(have, want))
                    && (width(kind) == Some(attr_width)
                        || (name == "add_offset" && width(kind).is_some()))
            }
            // A float field with the identity packing holds physical
            // values: scale 1 and offset 0 say nothing more.
            None => want() == Some(if name == "scale_factor" { 1.0 } else { 0.0 }),
            _ => false,
        },
        "_FillValue" | "missing_value" => want().is_some_and(|want| {
            fill.is_some_and(|fill| {
                same_number(fill, want) || same_number(fill, f64::from(want as f32))
            })
        }),
        _ => return None,
    })
}

/// Variables whose values the decoders put in typed slots (their
/// attributes go to `Volume::variable_attrs`).
const SLOTTED: &[&str] = &[
    // coordinates and the sweep table
    "time",
    "range",
    "azimuth",
    "elevation",
    "sweep_number",
    "fixed_angle",
    "sweep_fixed_angle",
    "sweep_group_name",
    "sweep_start_ray_index",
    "sweep_end_ray_index",
    "sweep_mode",
    "follow_mode",
    "prt_mode",
    "polarization_mode",
    "target_scan_rate",
    "rays_are_indexed",
    "ray_angle_res",
    "rays_angle_resolution",
    "qc_procedures",
    // per-ray instrument variables
    "nyquist_velocity",
    "unambiguous_range",
    "prt",
    "prt_ratio",
    "n_samples",
    "pulse_count",
    "pulse_width",
    "scan_rate",
    "antenna_transition",
    "r_calib_index",
    "independent_samples",
    "measured_transmit_power_h",
    "measured_transmit_power_v",
    "radar_measured_transmit_power_h",
    "radar_measured_transmit_power_v",
    // location and platform
    "latitude",
    "longitude",
    "altitude",
    "altitude_agl",
    "heading",
    "roll",
    "pitch",
    "drift",
    "rotation",
    "tilt",
    // root scalars
    "volume_number",
    "platform_type",
    "instrument_type",
    "primary_axis",
    "status_str",
    "time_coverage_start",
    "time_coverage_end",
    // radar parameters
    "frequency",
    "radar_antenna_gain_h",
    "radar_antenna_gain_v",
    "radar_beam_width_h",
    "radar_beam_width_v",
    "radar_beam_width_h_deg",
    "radar_beam_width_v_deg",
    "radar_rx_bandwidth",
    "radar_receiver_bandwidth",
];

/// Where a golden group's items live.
#[derive(Clone, Debug, PartialEq)]
enum Level {
    Root,
    Sweep(usize),
    Monitoring(usize),
    /// A metadata group (`radar_parameters`, ...), under its FM301 name.
    Meta(String, String),
    /// Any other group, kept verbatim as `<prefix>.<name>`.
    Other(String),
}

fn level(path: &str, sweeps: &[String]) -> Level {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => Level::Root,
        [sweep] if sweeps.iter().any(|s| s == sweep) => {
            Level::Sweep(sweeps.iter().position(|s| s == sweep).unwrap())
        }
        [sweep, "monitoring"] if sweeps.iter().any(|s| s == sweep) => {
            Level::Monitoring(sweeps.iter().position(|s| s == sweep).unwrap())
        }
        [meta @ ("radar_parameters" | "radar_calibration")] => {
            Level::Meta((*meta).to_owned(), (*meta).to_owned())
        }
        [meta @ ("georeferencing_correction" | "georeference_correction")] => {
            Level::Meta("georeferencing_correction".to_owned(), (*meta).to_owned())
        }
        other => Level::Other(other.join(".")),
    }
}

#[derive(Default, Debug)]
struct Counts {
    fields: usize,
    extra_vars: usize,
    slotted_vars: usize,
    attrs: usize,
}

fn check(id: &str) -> Option<Counts> {
    let volume = decode(id)?;
    let golden = golden(id);
    let groups = golden["groups"].as_array().unwrap();
    // The source name of each sweep group.
    let sweep_names: Vec<String> = volume
        .sweeps
        .iter()
        .enumerate()
        .map(
            |(index, sweep)| match find(&sweep.other, "sweep_group_name") {
                Some(AttrValue::Text(name)) => name.to_string(),
                _ => format!("sweep_{index}"),
            },
        )
        .collect();
    let all_extras: Vec<(&ExtraVariable, Option<usize>)> = volume
        .extra_vars
        .iter()
        .map(|e| (e, None))
        .chain(
            volume
                .sweeps
                .iter()
                .enumerate()
                .flat_map(|(i, s)| s.extra_vars.iter().map(move |e| (e, Some(i)))),
        )
        .collect();
    let mut counts = Counts::default();
    let mut failures: Vec<String> = Vec::new();

    for group in groups {
        let path = group["path"].as_str().unwrap();
        let level = level(path, &sweep_names);
        // Group attributes.
        for attr in group["attributes"].as_array().unwrap() {
            let name = attr["name"].as_str().unwrap();
            counts.attrs += 1;
            let (others, key): (&[(Box<str>, AttrValue)], String) = match &level {
                Level::Root => (&volume.attrs.other, name.to_owned()),
                Level::Sweep(i) => (&volume.sweeps[*i].other, name.to_owned()),
                Level::Monitoring(i) => (&volume.sweeps[*i].other, format!("monitoring.{name}")),
                Level::Meta(_, source) => (&volume.attrs.other, format!("{source}.{name}")),
                Level::Other(prefix) => (&volume.attrs.other, format!("{prefix}.{name}")),
            };
            match find(others, &key) {
                Some(value) if same_attr(attr, value) => {}
                Some(value) => {
                    failures.push(format!("{path}@{name}: kept as {value:?}, file has {attr}"))
                }
                None => {
                    let typed = match &level {
                        Level::Root => global_slot(&volume, attr),
                        Level::Sweep(i) if name == "sweep_mode" => Some(
                            text(attr)
                                .is_some_and(|t| volume.sweeps[*i].sweep_mode.as_str() == t.trim()),
                        ),
                        _ => None,
                    };
                    match typed {
                        Some(true) => {}
                        Some(false) => {
                            failures.push(format!("{path}@{name}: typed slot differs from {attr}"))
                        }
                        None => failures.push(format!("{path}@{name}: not in the model")),
                    }
                }
            }
        }

        // Variables.
        let ray_dim = group["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "time" && v["dims"].as_array().is_some_and(|d| d.len() == 1))
            .and_then(|v| v["dims"][0].as_str())
            .or_else(|| {
                group["variables"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["name"] == "azimuth")
                    .and_then(|v| v["dims"][0].as_str())
            })
            .unwrap_or("time")
            .to_owned();
        for var in group["variables"].as_array().unwrap() {
            let name = var["name"].as_str().unwrap();
            let dims: Vec<&str> = var["dims"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap())
                .collect();
            let attrs = var["attributes"].as_array().unwrap();
            let context = format!("{path}/{name}");
            let is_field = dims == [ray_dim.as_str(), "range"] && var["type"] != "char";
            if is_field && matches!(level, Level::Root | Level::Sweep(_)) {
                let fields: Vec<&Field> = match &level {
                    Level::Sweep(i) => volume.sweeps[*i]
                        .fields
                        .iter()
                        .filter(|f| f.name.as_str() == name)
                        .collect(),
                    _ => volume
                        .sweeps
                        .iter()
                        .filter_map(|s| s.fields.iter().find(|f| f.name.as_str() == name))
                        .collect(),
                };
                if fields.is_empty() {
                    failures.push(format!("{context}: no field"));
                    continue;
                }
                counts.fields += 1;
                if Some(fields_sha(&fields).as_str()) != var["value"]["sha256"].as_str() {
                    failures.push(format!("{context}: field values differ from netCDF-C's"));
                }
                for attr in attrs {
                    counts.attrs += 1;
                    let attr_name = attr["name"].as_str().unwrap();
                    for field in &fields {
                        match find(&field.attrs.other, attr_name) {
                            Some(value) if same_attr(attr, value) => {}
                            Some(value) => failures.push(format!(
                                "{context}@{attr_name}: kept as {value:?}, file has {attr}"
                            )),
                            None => match field_slot(field, attr) {
                                Some(true) => {}
                                Some(false) => failures.push(format!(
                                    "{context}@{attr_name}: field slot differs from {attr}"
                                )),
                                None => failures
                                    .push(format!("{context}@{attr_name}: not in the model")),
                            },
                        }
                    }
                }
                continue;
            }
            // An extra variable, under its name or with its group prefix.
            let candidates: Vec<String> = match &level {
                Level::Root | Level::Sweep(_) => vec![name.to_owned()],
                Level::Monitoring(_) => vec![name.to_owned(), format!("monitoring.{name}")],
                Level::Meta(fm301, source) => vec![
                    name.to_owned(),
                    format!("{source}.{name}"),
                    format!("{fm301}.{name}"),
                ],
                Level::Other(prefix) => vec![format!("{prefix}.{name}")],
            };
            let wanted_sweep = match &level {
                Level::Sweep(i) | Level::Monitoring(i) => Some(*i),
                _ => None,
            };
            let extra = all_extras.iter().find(|(extra, sweep)| {
                candidates.iter().any(|c| *extra.name == **c)
                    && (wanted_sweep.is_none() || *sweep == wanted_sweep)
            });
            if let Some((extra, _)) = extra {
                counts.extra_vars += 1;
                for attr in attrs {
                    counts.attrs += 1;
                    let attr_name = attr["name"].as_str().unwrap();
                    match find(&extra.attrs, attr_name) {
                        Some(value) if same_attr(attr, value) => {}
                        Some(value) => failures.push(format!(
                            "{context}@{attr_name}: kept as {value:?}, file has {attr}"
                        )),
                        None => failures.push(format!("{context}@{attr_name}: not in the model")),
                    }
                }
                continue;
            }
            // A slotted variable: its attributes in `variable_attrs`.
            let group_name = match &level {
                Level::Root => String::new(),
                Level::Sweep(i) => format!("sweep_{i}"),
                Level::Monitoring(i) => format!("sweep_{i}/monitoring"),
                Level::Meta(fm301, _) => fm301.clone(),
                Level::Other(_) => {
                    failures.push(format!("{context}: not in the model"));
                    continue;
                }
            };
            let slotted = SLOTTED.contains(&name)
                || name.starts_with("r_calib_")
                || matches!(&level, Level::Meta(..));
            if !slotted {
                failures.push(format!(
                    "{context}: neither a field, an extra variable nor slotted"
                ));
                continue;
            }
            counts.slotted_vars += 1;
            if attrs.is_empty() {
                continue;
            }
            let Some(entry) = volume
                .variable_attrs
                .iter()
                .find(|entry| *entry.group == *group_name && *entry.name == *name)
            else {
                failures.push(format!(
                    "{context}: attributes not in variable_attrs[{group_name}]"
                ));
                continue;
            };
            for attr in attrs {
                counts.attrs += 1;
                let attr_name = attr["name"].as_str().unwrap();
                match find(&entry.attrs, attr_name) {
                    Some(value) if same_attr(attr, value) => {}
                    Some(value) => failures.push(format!(
                        "{context}@{attr_name}: kept as {value:?}, file has {attr}"
                    )),
                    None => failures.push(format!("{context}@{attr_name}: not in the model")),
                }
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
                if let Some(counts) = check($id) {
                    eprintln!("{}: {counts:?}", $id);
                    assert!(counts.fields > 0);
                }
            }
        )*
    };
}

every_value_tests! {
    xsapr_classic => "cfrad1-xsapr-sgp-20110520-ppi-classic",
    xsapr_netcdf4 => "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    dow8_trim3_classic => "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
    dow8_netcdf4 => "cfrad1-dow8-20211011-223602-rhi",
    irene_classic => "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
    spol_netcdf4 => "cfrad1-spol-20080604-002217-sur",
    spol_cfradial2 => "cfrad2-spol-20080604-002217-sur",
    irene_cfradial2_radx => "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    iesha_cfradial2_radx_int32 => "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    xsapr_cfradial2_xradar => "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    dow8_cfradial2_xradar => "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
}

/// Integer attributes keep their stored type (CF wants a `_FillValue` in
/// its variable's type): IRENE's `antenna_transition:_FillValue` is a byte,
/// `n_samples:_FillValue` an int, both kept with the values the typed slots
/// hold.
#[test]
fn integer_attributes_keep_their_type() {
    let Some(volume) = decode("cfrad1-irene-sr2-20110827-120420-sur-sweeps01") else {
        return;
    };
    let fill = |variable: &str| {
        volume
            .variable_attrs
            .iter()
            .find(|entry| entry.group.is_empty() && &*entry.name == variable)
            .and_then(|entry| find(&entry.attrs, "_FillValue"))
            .cloned()
    };
    assert_eq!(
        fill("antenna_transition"),
        Some(AttrValue::Scalar(Scalar::I8(-128)))
    );
    assert_eq!(
        fill("n_samples"),
        Some(AttrValue::Scalar(Scalar::I32(-9999)))
    );
    assert!(volume.sweeps[0].ray_vars.antenna_transition.is_some());
}

/// The FM301 view writes the source's own attributes of slotted variables
/// with `Passthrough::All` (the view's own value wins where both have one),
/// and not in the xradar flavor: DOW8's `time` keeps Radx's comment,
/// `nyquist_velocity` its `meta_group`, `sweep_mode` its `options`.
#[test]
fn passthrough_all_writes_the_source_variable_attributes() {
    use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, ViewOptions};
    let Some(volume) = decode("cfrad1-dow8-20211011-223602-rhi-trim3-classic") else {
        return;
    };
    let options = |passthrough| ViewOptions {
        flavor: Flavor::Xradar012,
        first_dim: FirstDim::Time,
        passthrough,
    };
    let all = fm301::volume_view(&volume, options(Passthrough::All), None).unwrap();
    let flavor = fm301::volume_view(&volume, options(Passthrough::Flavor), None).unwrap();
    let attr = |view: &fm301::VolumeView<'_>, variable: &str, name: &str| {
        view.group("sweep_0")
            .and_then(|group| group.variable(variable))
            .and_then(|variable| {
                variable
                    .attrs
                    .iter()
                    .find(|(have, _)| have == name)
                    .map(|(_, value)| value.clone())
            })
    };
    assert_eq!(
        attr(&all, "time", "comment"),
        Some(AttrValue::text(
            "times are relative to the volume start_time"
        ))
    );
    assert_eq!(attr(&flavor, "time", "comment"), None);
    assert_eq!(
        attr(&all, "nyquist_velocity", "meta_group"),
        Some(AttrValue::text("instrument_parameters"))
    );
    let options = attr(&all, "sweep_mode", "options");
    assert!(
        options
            .as_ref()
            .and_then(AttrValue::as_text)
            .is_some_and(|text| text.starts_with("sector, coplane, rhi")),
        "{options:?}"
    );
    // The view's own values stay: `units` and `meters_between_gates`.
    assert_eq!(
        attr(&all, "range", "units"),
        attr(&flavor, "range", "units")
    );
    assert_eq!(
        attr(&all, "range", "meters_between_gates"),
        attr(&flavor, "range", "meters_between_gates")
    );
}
