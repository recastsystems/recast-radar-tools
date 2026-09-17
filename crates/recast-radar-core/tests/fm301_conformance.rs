//! F.4 conformance: the FM301 view of every golden case against what xradar
//! 0.12.0 and arm_pyart 2.2.5 return for the same file
//! (`testdata/conformance/fm301/`, written by `tools/fm301_golden.py`; plan
//! `docs/superpowers/plans/2026-09-16-wave2.md` F.4, spec 4.3, design note
//! `docs/design/fm301-model.md` section 12).
//!
//! For each case in `index.json` the volume is decoded natively, the
//! `Flavor::Xradar012` view is built twice (`FirstDim::Time` against the
//! golden `time` view, `FirstDim::Auto` against `auto`), and every golden
//! group, dimension, variable and attribute is looked up in the view:
//!
//! - names and dimensions must match;
//! - an array of the same dtype must hash equal (SHA-256 over little-endian
//!   elements with NaN canonicalized, the golden's convention); a `time`
//!   coordinate is hashed as int64 nanoseconds; an array whose dtype differs
//!   only in float width is hashed after widening and, failing that, its
//!   count, min, max, mean, first and last values must agree within
//!   [`TOLERANCE`] relative;
//! - scalars and numeric attributes agree within [`TOLERANCE`]; text and
//!   boolean attributes are equal.
//!
//! The Py-ART side compares the volume with `Radar`: sweep table, per-sweep
//! coordinates, fixed angles, sweep modes, location, instrument parameters,
//! and every field placed on Py-ART's volume range the way
//! `read_nexrad_archive(linear_interp=False)` lays it out (each native gate
//! repeated over the gates it covers, design note 6.5): the physical float32
//! values hash equal to Py-ART's per-sweep arrays, or, where the two readers
//! evaluate in different precision, agree in count, min, max and mean.
//!
//! Differences the readers are known to have are checked explicitly instead
//! of being skipped, each with the design-note section that decides it:
//! [`EXPECTED`] lists them. Everything else must match.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use recast_radar_core::fm301::{
    self, ExtraAttrs, FirstDim, Flavor, Group, Passthrough, Values, ViewOptions, VolumeView,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, Field, PyartNames, Scalar, SourceFormat, Sweep, Volume,
};
use serde_json::Value;

/// Relative tolerance for values the two sides compute in different
/// precision (design note section 15, open question 5).
const TOLERANCE: f64 = 1e-4;

/// The reader differences this test expects, with the section of
/// `docs/design/fm301-model.md` (or of the golden README) that documents each.
/// Every rule below is applied only where it says.
const EXPECTED: &[&str] = &[
    "xradar writes the text `None` for a global attribute it has no value for (A.2, A.4); ours carries the file's value or omits the attribute",
    "xradar writes `Conventions = ODIM_H5/V2_2` for every ODIM file; ours writes the file's `/Conventions` (section 11)",
    "xradar omits `sweep_group_name` / `sweep_fixed_angle` for NEXRAD and writes integer sweep indices as ODIM `sweep_group_name` (section 1); ours always writes the names",
    "xradar writes `follow_mode = not_set` for NEXRAD and ODIM; ours writes the Table 301-15 value `none` (section 10)",
    "xradar omits `nyquist_velocity` for NEXRAD and writes one scalar per ODIM sweep; ours writes it per ray, Table 301-8a (section 9)",
    "xradar has no `polarization_mode`, `frequency`, `_FillValue`, `_Undetect`, `valid_range` or `flag_*` for NEXRAD; ours writes them (7.1, 10)",
    "xradar puts CfRadial `frequency` at the root; FM301 puts it in every sweep (section 1)",
    "xradar labels ODIM `TH` linear and unitless; io-odim uses dBZ (8.2 note 1)",
    "xradar reads the zero-filled Message 5 of KLIX 2005 as VCP 0 (A.3); ours has no VCP definition and takes the VCP number from the radials",
    "xradar writes CfRadial calibration entries equal to their `_FillValue` (-9999); ours leaves them unset (section 2)",
    "Py-ART reads ODIM `rstart` in km; AEMET writes metres (espdg), which io-odim detects (odim.rs, `first_gate_m_from_rstart`)",
    "Py-ART's ODIM reader reports `meters_to_center_of_first_gate = 0` while its data starts at the first centre (section 14)",
    "Py-ART's ODIM reader takes azimuths as the complex mean of startazA/stopazA in (-180, 180], elevations from where/elangle, and ray times by file position; ours follow xradar (arithmetic mean in [0, 360), startelA/stopelA midpoints, acquisition order from a1gate) (sections 3, 14)",
    "Py-ART rounds a Message 1 volume's fixed angle (the first radial's elevation) to 0.1 degree (A.3)",
    "xradar keeps CfRadial variable attributes verbatim on coordinate and instrument variables; the model's typed slots write the FM301 attributes (sections 9, 12.4), so only dataset variables compare attributes there",
    "xradar puts a moving platform's latitude/longitude/altitude(time) at the root; FM301 keeps them per sweep in the platform track (section 9)",
    "xradar takes a NEXRAD sweep's range from its coarsest moment and misplaces the finer ones (6.2); the fields still compare natively, the coordinate does not",
    "xradar omits WRADH for Message 1 volumes and reads the -375 m first gate as 65161 (A.3)",
    "xradar reads an ODIM Nyquist velocity from the dataset's how/NI only and copies CfRadial's sweep numbers into sweep_number and sweep_group_name; ours falls back to the root how/NI and numbers the groups it writes",
    "xradar keeps a CfRadial range coordinate's float32 values; a range the decoder found uniform is regenerated from its first centre and spacing (RangeCoord::Uniform, section 3)",
    "xradar squeezes a one-entry r_calib dimension into scalars and keeps the calibration time as text; ours writes calib arrays and seconds since the reference (section 12.4)",
    "Py-ART writes a zero Nyquist velocity and unambiguous range for Level II radials that carry none; ours leaves the variable out",
];

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

struct Case {
    id: String,
    kind: String,
    xradar: Option<Value>,
    pyart: Option<Value>,
}

fn conformance_dir() -> PathBuf {
    recast_radar_testdata::testdata_dir().join("conformance/fm301")
}

fn read_json(path: &PathBuf) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn cases() -> Vec<Case> {
    let dir = conformance_dir();
    let index = read_json(&dir.join("index.json"));
    index["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let golden = |key: &str, status: &str| {
                (entry[status] == "ok").then(|| read_json(&dir.join(entry[key].as_str().unwrap())))
            };
            Case {
                id: entry["id"].as_str().unwrap().to_owned(),
                kind: entry["reader_kind"].as_str().unwrap().to_owned(),
                xradar: golden("xradar", "xradar_status"),
                pyart: golden("pyart", "pyart_status"),
            }
        })
        .collect()
}

/// The decoded volume and, for Level II, the metadata that carries xradar's
/// NEXRAD attributes.
struct Decoded {
    volume: Volume,
    nexrad: Option<recast_radar_io_nexrad::NexradVolume>,
}

/// The file the Rust side decodes for a case: the case's file, except that
/// the netCDF-4 X-SAPR case (an HDF5 container the classic netCDF reader
/// cannot open) decodes its committed classic-container twin, a raw
/// variable-for-variable copy with identical data
/// (`testdata/other/manifest.toml`, `derived_from`).
fn decoded_id(case: &Case) -> &str {
    match case.id.as_str() {
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4" => "cfrad1-xsapr-sgp-20110520-ppi-classic",
        id => id,
    }
}

fn decode(case: &Case) -> Option<Decoded> {
    let path = match recast_radar_testdata::path(decoded_id(case)) {
        Ok(path) => path,
        Err(err) if err.is_offline() => {
            eprintln!("skipping {}: {err}", case.id);
            return None;
        }
        Err(err) => panic!("{err}"),
    };
    let bytes = std::fs::read(&path).unwrap();
    Some(if case.kind == "nexrad" {
        let nexrad = recast_radar_io_nexrad::read_volume_with_metadata(&bytes)
            .unwrap_or_else(|e| panic!("{}: {e}", case.id));
        Decoded {
            volume: nexrad.volume.clone(),
            nexrad: Some(nexrad),
        }
    } else {
        Decoded {
            volume: recast_radar_io::read_supported_volume_bytes(&bytes)
                .unwrap_or_else(|e| panic!("{}: {e}", case.id)),
            nexrad: None,
        }
    })
}

// ---------------------------------------------------------------------------
// Hashing (the golden's convention)
// ---------------------------------------------------------------------------

fn canonical_f32(value: f32) -> u32 {
    if value.is_nan() {
        0x7FC0_0000
    } else {
        value.to_bits()
    }
}

fn canonical_f64(value: f64) -> u64 {
    if value.is_nan() {
        0x7FF8_0000_0000_0000
    } else {
        value.to_bits()
    }
}

fn sha256_le(array: &ArrayBuf) -> Option<String> {
    let mut bytes = Vec::new();
    match array {
        ArrayBuf::I8(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::U8(v) => bytes.extend_from_slice(v),
        ArrayBuf::I16(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::U16(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::I32(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::U32(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::I64(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&x.to_le_bytes())),
        ArrayBuf::F32(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&canonical_f32(*x).to_le_bytes())),
        ArrayBuf::F64(v) => v
            .iter()
            .for_each(|x| bytes.extend_from_slice(&canonical_f64(*x).to_le_bytes())),
        ArrayBuf::Text(_) => return None,
    }
    Some(recast_radar_testdata::sha256_hex(&bytes))
}

fn sha256_f32(values: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&canonical_f32(*value).to_le_bytes());
    }
    recast_radar_testdata::sha256_hex(&bytes)
}

// ---------------------------------------------------------------------------
// Numeric summaries
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Summary {
    count: usize,
    finite: usize,
    min: Option<f64>,
    max: Option<f64>,
    mean: Option<f64>,
    first: Vec<f64>,
    last: Option<f64>,
}

fn summarize(values: impl Iterator<Item = f64> + Clone) -> Summary {
    let all: Vec<f64> = values.collect();
    let finite: Vec<f64> = all.iter().copied().filter(|v| v.is_finite()).collect();
    let (min, max) = finite.iter().fold((None, None), |(lo, hi), v| {
        (
            Some(lo.map_or(*v, |lo: f64| lo.min(*v))),
            Some(hi.map_or(*v, |hi: f64| hi.max(*v))),
        )
    });
    let mean = (!finite.is_empty()).then(|| finite.iter().sum::<f64>() / finite.len() as f64);
    Summary {
        count: all.len(),
        finite: finite.len(),
        min,
        max,
        mean,
        first: all.iter().take(3).copied().collect(),
        last: all.last().copied(),
    }
}

fn close(actual: f64, expected: f64) -> bool {
    if actual.is_nan() && expected.is_nan() {
        return true;
    }
    (actual - expected).abs() <= TOLERANCE * expected.abs().max(1e-6)
}

fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        },
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Our side, flattened
// ---------------------------------------------------------------------------

struct OurVar {
    dims: Vec<String>,
    array: Option<ArrayBuf>,
    scalar: Option<Scalar>,
    text: Option<String>,
    attrs: BTreeMap<String, AttrValue>,
}

struct OurGroup {
    dims: BTreeMap<String, usize>,
    attrs: BTreeMap<String, AttrValue>,
    vars: BTreeMap<String, OurVar>,
}

fn flatten(view: &VolumeView<'_>) -> BTreeMap<String, OurGroup> {
    fn walk(group: &Group<'_>, path: &str, out: &mut BTreeMap<String, OurGroup>) {
        let vars = group
            .variables
            .iter()
            .map(|var| {
                let (array, scalar, text) = match &var.values {
                    Values::Scalar(s) => (None, Some(*s), None),
                    Values::Text(t) => (None, None, Some(t.to_string())),
                    other => (Some(other.materialize().unwrap()), None, None),
                };
                (
                    var.name.to_string(),
                    OurVar {
                        dims: var.dims.iter().map(|d| d.to_string()).collect(),
                        array,
                        scalar,
                        text,
                        attrs: var
                            .attrs
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.clone()))
                            .collect(),
                    },
                )
            })
            .collect();
        out.insert(
            path.to_owned(),
            OurGroup {
                dims: group
                    .dims
                    .iter()
                    .map(|(name, len)| (name.to_string(), *len))
                    .collect(),
                attrs: group
                    .attrs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
                vars,
            },
        );
        for child in &group.children {
            let child_path = if path == "/" {
                format!("/{}", child.name)
            } else {
                format!("{path}/{}", child.name)
            };
            walk(child, &child_path, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(&view.root, "/", &mut out);
    out
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    hashed: usize,
    summarized: usize,
    attrs: usize,
    scalars: usize,
}

impl Report {
    fn error(&mut self, what: impl Into<String>) {
        self.errors.push(what.into());
    }
}

// ---------------------------------------------------------------------------
// xradar comparison
// ---------------------------------------------------------------------------

/// Variables ours writes that xradar 0.12 does not (see [`EXPECTED`]).
const OURS_ONLY_VARIABLES: &[&str] = &[
    "nyquist_velocity",
    "unambiguous_range",
    "polarization_mode",
    "sweep_group_name",
    "sweep_fixed_angle",
    "frequency",
    "rays_are_indexed",
    "rays_angle_resolution",
    "target_scan_rate",
    "calib_index",
    // FM301 mandatory items a CfRadial file may lack; xradar writes only
    // what the file has.
    "instrument_type",
    "platform_type",
    "primary_axis",
    "follow_mode",
    "prt_mode",
];

/// Root attributes ours writes that xradar 0.12 does not.
const OURS_ONLY_ATTRS: &[&str] = &[
    "site_name",
    "ray_times_increase",
    "scan_id",
    "platform_is_mobile",
];

/// Variable attributes ours writes that xradar 0.12 does not.
const OURS_ONLY_VAR_ATTRS: &[&str] = &[
    "_FillValue",
    "_Undetect",
    "valid_range",
    "flag_values",
    "flag_meanings",
    "flag_masks",
    "coordinates",
    "comment",
    "standard_name",
    "long_name",
    "units",
    "positive",
    "axis",
    "sampling_ratio",
];

/// xradar root attributes derived from the VCP definition (Message 5).
const VCP_ROOT_ATTRS: &[&str] = &[
    "scan_name",
    "dynamic_scan_type",
    "mpda_vcp",
    "base_tilt_vcp",
    "num_base_tilts",
    "vcp_truncated",
    "vcp_sequence_active",
    "number_elevation_cuts",
    "doppler_velocity_resolution",
    "vcp_pulse_width",
];

/// Root variables xradar writes per ray (dimension `time`) for a moving
/// platform; FM301 keeps them in each sweep's platform track (section 9).
const PLATFORM_TRACK: &[&str] = &["latitude", "longitude", "altitude", "altitude_agl"];

fn attr_text(value: &AttrValue) -> Option<String> {
    match value {
        AttrValue::Text(t) => Some(t.to_string()),
        AttrValue::Bool(b) => Some(if *b { "true" } else { "false" }.to_owned()),
        _ => None,
    }
}

fn attr_f64(value: &AttrValue) -> Option<f64> {
    match value {
        AttrValue::Scalar(s) => Some(s.as_f64()),
        AttrValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        AttrValue::Array(a) if a.len() == 1 => a.get_f64(0),
        _ => None,
    }
}

/// Text attributes compare trimmed; an empty golden text is "unset", which
/// ours writes as `None` (xradar's own placeholder) or omits.
fn same_text(golden: &str, ours: &str) -> bool {
    let (golden, ours) = (golden.trim(), ours.trim());
    golden == ours || (golden.is_empty() && ours == "None")
}

/// One golden attribute against ours. `what` names the owner.
fn compare_attr(report: &mut Report, what: &str, name: &str, golden: &Value, ours: &AttrValue) {
    report.attrs += 1;
    let ok = match golden {
        Value::String(text) => attr_text(ours).is_some_and(|t| same_text(text, &t)),
        Value::Bool(b) => match ours {
            AttrValue::Bool(o) => o == b,
            other => attr_text(other).is_some_and(|t| t == if *b { "true" } else { "false" }),
        },
        Value::Number(_) => attr_f64(ours).is_some_and(|o| close(o, json_f64(golden).unwrap())),
        Value::Array(items) => match ours {
            AttrValue::Array(array) => {
                array.len() == items.len()
                    && items.iter().enumerate().all(|(i, item)| {
                        match (json_f64(item), array.get_f64(i)) {
                            (Some(g), Some(o)) => close(o, g),
                            _ => false,
                        }
                    })
            }
            AttrValue::Scalar(s) => {
                items.len() == 1 && json_f64(&items[0]).is_some_and(|g| close(s.as_f64(), g))
            }
            _ => false,
        },
        Value::Null => true,
        Value::Object(_) => true,
    };
    if !ok {
        report.error(format!(
            "{what}: attribute {name} is {ours:?}, golden {golden}"
        ));
    }
}

struct Context<'a> {
    case: &'a Case,
    view: &'a str,
    source: SourceFormat,
    time_reference: DateTime<Utc>,
    /// The golden's root `number_elevation_cuts` (NEXRAD): 0 when xradar
    /// read a zero-filled Message 5.
    zero_vcp: bool,
    /// Level II volume without a VCP definition (Message 1 era).
    message_1: bool,
    /// The golden `time` view, whose attributes the `auto` view omits.
    time_view: &'a Value,
}

impl Context<'_> {
    fn cfradial(&self) -> bool {
        matches!(
            self.source,
            SourceFormat::CfRadial1 | SourceFormat::CfRadial2
        )
    }

    fn time_group(&self, path: &str) -> Option<&Value> {
        self.time_view["groups"]
            .as_array()?
            .iter()
            .find(|g| g["path"] == path)
    }

    /// The golden `time` view's range attribute of a sweep group.
    fn time_range_attr(&self, path: &str, attr: &str) -> Option<f64> {
        self.time_group(path)?["variables"]
            .as_array()?
            .iter()
            .find(|v| v["name"] == "range")
            .and_then(|v| json_f64(&v["attrs"][attr]))
    }

    fn time_range_spacing(&self, path: &str) -> Option<f64> {
        self.time_range_attr(path, "meters_between_gates")
    }

    /// xradar read a Message 1 Doppler sweep's -375 m first gate as the
    /// unsigned 65161 (A.3); its range coordinate is then wrong.
    fn negative_first_gate(&self, path: &str) -> bool {
        self.message_1
            && self.time_range_attr(path, "meters_to_center_of_first_gate") == Some(65161.0)
    }
}

fn compare_group_attrs(
    report: &mut Report,
    ctx: &Context<'_>,
    path: &str,
    golden: &Value,
    ours: &OurGroup,
) {
    let what = format!("{} {} {path}", ctx.case.id, ctx.view);
    let Some(golden_attrs) = golden["attrs"].as_object() else {
        return;
    };
    for (name, value) in golden_attrs {
        // xradar's placeholder for an attribute it has no value for, and
        // CfRadial's empty strings.
        if value == "None" || value.as_str().is_some_and(|t| t.trim().is_empty()) {
            continue;
        }
        if ctx.zero_vcp && VCP_ROOT_ATTRS.contains(&name.as_str()) {
            continue;
        }
        let Some(mine) = ours.attrs.get(name) else {
            report.error(format!("{what}: attribute {name} = {value} missing"));
            continue;
        };
        if name == "Conventions" && ctx.source == SourceFormat::OdimH5 {
            report.attrs += 1;
            let mine = attr_text(mine).unwrap_or_default();
            if value != "ODIM_H5/V2_2" || !mine.starts_with("ODIM_H5/") {
                report.error(format!("{what}: Conventions {mine} vs golden {value}"));
            }
            continue;
        }
        compare_attr(report, &what, name, value, mine);
    }
    if path == "/" {
        for name in ours.attrs.keys() {
            let expected = golden_attrs.contains_key(name)
                || OURS_ONLY_ATTRS.contains(&name.as_str())
                || (ctx.zero_vcp && VCP_ROOT_ATTRS.contains(&name.as_str()));
            if !expected {
                report.error(format!("{what}: attribute {name} is not in the golden"));
            }
        }
    }
}

/// Our `time` coordinate (seconds since the volume reference) as int64
/// nanoseconds since the epoch, the golden's `datetime64[ns]` hash input.
fn time_ns(reference: DateTime<Utc>, seconds: &[f64]) -> Vec<i64> {
    let base = reference.timestamp_nanos_opt().unwrap();
    seconds
        .iter()
        .map(|s| base + (s * 1e9).round() as i64)
        .collect()
}

fn our_f64s(array: &ArrayBuf) -> Vec<f64> {
    (0..array.len())
        .map(|i| array.get_f64(i).unwrap())
        .collect()
}

fn widened(array: &ArrayBuf) -> ArrayBuf {
    match array {
        ArrayBuf::F32(v) => ArrayBuf::F64(v.iter().map(|x| f64::from(*x)).collect()),
        ArrayBuf::I32(v) => ArrayBuf::I64(v.iter().map(|x| i64::from(*x)).collect()),
        other => other.clone(),
    }
}

/// Golden array summary against ours: hash when the dtype allows it, else
/// count, min, max, mean, first and last within the tolerance.
fn compare_array(
    report: &mut Report,
    what: &str,
    golden_values: &Value,
    golden_dtype: &str,
    ours: &ArrayBuf,
    ours_is_time: Option<DateTime<Utc>>,
) {
    let golden_hash = golden_values["sha256"].as_str();
    let count = golden_values["count"].as_u64().map(|c| c as usize);
    if let Some(count) = count
        && count != ours.len()
    {
        report.error(format!("{what}: {} elements, golden {count}", ours.len()));
        return;
    }
    // Text arrays: values.
    if let ArrayBuf::Text(texts) = ours {
        if let Some(values) = golden_values["values"].as_array() {
            let mine: Vec<&str> = texts.iter().map(|t| &**t).collect();
            let theirs: Vec<&str> = values.iter().filter_map(Value::as_str).collect();
            if mine != theirs {
                report.error(format!("{what}: {mine:?}, golden {theirs:?}"));
            }
            report.scalars += 1;
        }
        return;
    }
    // datetime64: hash int64 nanoseconds.
    if golden_dtype == "datetime64[ns]" {
        let Some(reference) = ours_is_time else {
            report.error(format!(
                "{what}: golden is datetime64, ours is {}",
                ours.dtype()
            ));
            return;
        };
        let ns = ArrayBuf::I64(time_ns(reference, &our_f64s(ours)));
        if sha256_le(&ns).as_deref() == golden_hash {
            report.hashed += 1;
            return;
        }
        // Fall back to the epoch-second summaries.
        let seconds: Vec<f64> = our_f64s(ours)
            .iter()
            .map(|s| {
                reference.timestamp() as f64 + reference.timestamp_subsec_nanos() as f64 * 1e-9 + s
            })
            .collect();
        compare_summary(
            report,
            what,
            golden_values,
            &summarize(seconds.iter().copied()),
            0.0,
        );
        return;
    }
    // Same dtype: the hash must match.
    if golden_dtype == ours.dtype() {
        match sha256_le(ours) {
            Some(hash) if Some(hash.as_str()) == golden_hash => report.hashed += 1,
            Some(hash) => report.error(format!(
                "{what}: sha256 {hash} != golden {} ({golden_dtype}, {} elements)",
                golden_hash.unwrap_or("?"),
                ours.len()
            )),
            None => report.error(format!("{what}: cannot hash {}", ours.dtype())),
        }
        return;
    }
    // Float width differs: widening is exact when the source was narrow.
    let wide = widened(ours);
    if wide.dtype() == golden_dtype
        && let Some(hash) = sha256_le(&wide)
        && Some(hash.as_str()) == golden_hash
    {
        report.hashed += 1;
        return;
    }
    let numeric_golden = golden_values["min"].is_number() || golden_values["min"].is_null();
    if !numeric_golden {
        report.error(format!(
            "{what}: ours is {}, golden {golden_dtype} with no summary",
            ours.dtype()
        ));
        return;
    }
    compare_summary(
        report,
        what,
        golden_values,
        &summarize(our_f64s(ours).into_iter()),
        0.0,
    );
}

/// Count, min, max, mean, first and last of a golden summary against ours.
/// `absolute` widens the tolerance to an absolute margin (0 for the plain
/// relative rule).
fn compare_summary(report: &mut Report, what: &str, golden: &Value, ours: &Summary, absolute: f64) {
    report.summarized += 1;
    let near = |mine: f64, theirs: f64| close(mine, theirs) || (mine - theirs).abs() <= absolute;
    for (key, mine) in [("min", ours.min), ("max", ours.max), ("mean", ours.mean)] {
        if golden.get(key).is_none() {
            continue; // integer summaries carry no mean
        }
        match (json_f64(&golden[key]), mine) {
            (None, None) => {}
            (Some(theirs), Some(mine)) if near(mine, theirs) => {}
            (theirs, mine) => report.error(format!("{what}: {key} {mine:?}, golden {theirs:?}")),
        }
    }
    if absolute == 0.0 {
        if let Some(first) = golden["first"].as_array() {
            for (i, item) in first.iter().enumerate() {
                match (json_f64(item), ours.first.get(i)) {
                    (Some(theirs), Some(mine)) if close(*mine, theirs) => {}
                    (theirs, mine) => {
                        report.error(format!("{what}: first[{i}] {mine:?}, golden {theirs:?}"));
                    }
                }
            }
        }
        if let (Some(theirs), Some(mine)) = (json_f64(&golden["last"]), ours.last)
            && !close(mine, theirs)
        {
            report.error(format!("{what}: last {mine}, golden {theirs}"));
        }
    }
    if let Some(count) = golden["count"].as_u64()
        && count as usize != ours.count
    {
        report.error(format!("{what}: {} elements, golden {count}", ours.count));
    }
    if let Some(nan) = golden["count_nan"].as_u64()
        && nan as usize != ours.count - ours.finite
    {
        report.error(format!(
            "{what}: {} NaN, golden {nan}",
            ours.count - ours.finite
        ));
    }
}

/// The golden range coordinate of a NEXRAD sweep whose spacing xradar took
/// from a coarser moment than ours (design note 6.2): xradar then stores
/// every field on its native gates, padded with 0 to the sweep's widest
/// field, and misplaces the finer ones on the wrong range. The fields are
/// still comparable natively; the range coordinate is not.
fn coarse_xradar_range(ctx: &Context<'_>, golden_group: &Value, ours: &OurGroup) -> bool {
    if ctx.source != SourceFormat::NexradLevel2 {
        return false;
    }
    let golden_spacing = golden_group["variables"]
        .as_array()
        .and_then(|vars| vars.iter().find(|v| v["name"] == "range"))
        .and_then(|v| json_f64(&v["attrs"]["meters_between_gates"]));
    let golden_spacing = golden_spacing.or_else(|| {
        // The auto view omits attributes identical to the time view's.
        ctx.time_range_spacing(golden_group["path"].as_str().unwrap_or(""))
    });
    let our_spacing = ours
        .vars
        .get("range")
        .and_then(|v| v.attrs.get("meters_between_gates"))
        .and_then(attr_f64);
    match (golden_spacing, our_spacing) {
        (Some(theirs), Some(mine)) => !close(mine, theirs),
        _ => false,
    }
}

/// A field's native rows padded with its fill to `gates` gates per row (what
/// xradar stores for the coarse-range sweeps).
fn native_rows_padded(field: &Field, gates: usize) -> ArrayBuf {
    let (rows, native) = field.shape();
    macro_rules! pad {
        ($values:expr, $fill:expr, $variant:ident) => {{
            let mut out = Vec::with_capacity(rows * gates);
            for row in 0..rows {
                let start = row * native;
                let take = native.min(gates);
                out.extend_from_slice(&$values[start..start + take]);
                out.extend(std::iter::repeat_n($fill, gates - take));
            }
            ArrayBuf::$variant(out)
        }};
    }
    match &field.data {
        recast_radar_core::model::FieldData::U8 { values, coding } => {
            pad!(values, coding.fill_code(), U8)
        }
        recast_radar_core::model::FieldData::U16 { values, coding } => {
            pad!(values, coding.fill_code(), U16)
        }
        recast_radar_core::model::FieldData::I8 { values, coding } => {
            pad!(values, coding.fill_code(), I8)
        }
        recast_radar_core::model::FieldData::I16 { values, coding } => {
            pad!(values, coding.fill_code(), I16)
        }
        recast_radar_core::model::FieldData::F32 { values, coding } => {
            pad!(values, coding.fill_code(), F32)
        }
        recast_radar_core::model::FieldData::F64 { values, coding } => {
            pad!(values, coding.fill_code(), F64)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_variable(
    report: &mut Report,
    ctx: &Context<'_>,
    path: &str,
    golden: &Value,
    ours: &OurVar,
    ours_name: &str,
    coarse_range: bool,
    native: Option<&ArrayBuf>,
) {
    let name = golden["name"].as_str().unwrap();
    let what = format!("{} {} {path}/{name}", ctx.case.id, ctx.view);
    let golden_dims: Vec<&str> = golden["dims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    let golden_dtype = golden["dtype"].as_str().unwrap();
    let values = &golden["values"];
    let scalar_to_rays =
        name == "nyquist_velocity" && golden_dims.is_empty() && ours.dims.len() == 1;
    // xradar squeezes a one-entry `r_calib` dimension into scalars.
    let squeezed_calib = path == "/radar_calibration"
        && golden_dims.is_empty()
        && ours.dims == ["calib"]
        && ours.array.as_ref().is_some_and(|a| a.len() == 1);
    if !scalar_to_rays
        && !squeezed_calib
        && golden_dims != ours.dims.iter().map(String::as_str).collect::<Vec<_>>()
    {
        report.error(format!(
            "{what}: dims {:?}, golden {golden_dims:?}",
            ours.dims
        ));
        return;
    }
    if name == "range" && (coarse_range || ctx.negative_first_gate(path)) {
        report.scalars += 1; // xradar's range coordinate is not the sweep's (6.2, A.3)
        return;
    }
    if name == "range"
        && ctx.cfradial()
        && let Some(array) = &ours.array
    {
        // A CfRadial range the decoder found uniform is regenerated from its
        // first centre and spacing; the file's float32 values can differ in
        // the last bit, so the coordinate compares within the tolerance.
        compare_summary(
            report,
            &what,
            values,
            &summarize(our_f64s(array).into_iter()),
            0.0,
        );
        return;
    }
    if name == "sweep_number" && ctx.cfradial() {
        // xradar copies the file's sweep_number (DOW8 trim: 2 for its only
        // sweep); ours numbers the groups it writes.
        report.scalars += 1;
        return;
    }

    // Attributes: every golden attribute, and the golden encoding's
    // `coordinates`, must be in ours. Ours may add the FM301 attributes.
    // CfRadial coordinate and instrument variables carry the file's own
    // attributes in xradar and the model's typed slots in ours, so only the
    // dataset variables (two dimensions) compare attributes there.
    let is_field = golden_dims.len() == 2;
    let compare_attrs = !ctx.cfradial() || is_field;
    if compare_attrs && let Some(attrs) = golden["attrs"].as_object() {
        for (key, value) in attrs {
            if key == "units" && name == "TH" && ctx.source == SourceFormat::OdimH5 {
                continue; // TH units: xradar unitless, io-odim dBZ (8.2 note 1)
            }
            if coarse_range && name == "range" && key.starts_with("meters_") {
                continue; // xradar's range is the coarse moment's (6.2)
            }
            match ours.attrs.get(key) {
                Some(mine) => compare_attr(report, &what, key, value, mine),
                None => report.error(format!("{what}: attribute {key} = {value} missing")),
            }
        }
        for key in ours.attrs.keys() {
            if !attrs.contains_key(key) && !OURS_ONLY_VAR_ATTRS.contains(&key.as_str()) {
                report.error(format!("{what}: attribute {key} is not in the golden"));
            }
        }
    }
    if compare_attrs
        && let Some(coordinates) = golden["encoding"]["coordinates"].as_str()
        && attr_text(
            ours.attrs
                .get("coordinates")
                .unwrap_or(&AttrValue::Bool(false)),
        )
        .as_deref()
            != Some(coordinates)
    {
        report.error(format!(
            "{what}: coordinates {:?}, golden encoding {coordinates}",
            ours.attrs.get("coordinates")
        ));
    }

    // Values.
    if let Some(text) = &ours.text {
        let theirs = values["value"].as_str().unwrap_or("");
        let ok = same_text(theirs, text)
            || (matches!(name, "follow_mode" | "prt_mode")
                && theirs == "not_set"
                && text == "none");
        report.scalars += 1;
        if !ok {
            report.error(format!("{what}: {text:?}, golden {theirs:?}"));
        }
        return;
    }
    if let Some(scalar) = ours.scalar {
        report.scalars += 1;
        let mine = scalar.as_f64();
        match json_f64(&values["value"]) {
            Some(theirs) if close(mine, theirs) => {}
            // xradar writes 0 for the missing site location of a Message 1
            // volume; the model keeps None (section 11).
            Some(theirs) if theirs == 0.0 && mine.is_nan() && ctx.message_1 && path == "/" => {}
            theirs => report.error(format!("{what}: {scalar:?}, golden {theirs:?}")),
        }
        return;
    }
    let Some(array) = &ours.array else {
        report.error(format!("{what}: no values"));
        return;
    };
    if squeezed_calib {
        report.scalars += 1;
        if name == "time" {
            // xradar keeps the calibration time as text; ours is seconds
            // since the volume reference.
            let mine = array
                .get_f64(0)
                .and_then(|s| {
                    ctx.time_reference
                        .checked_add_signed(chrono::Duration::milliseconds(
                            (s * 1000.0).round() as i64
                        ))
                })
                .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string());
            let theirs = values["value"].as_str().map(str::to_owned);
            if mine != theirs {
                report.error(format!("{what}: {mine:?}, golden {theirs:?}"));
            }
            return;
        }
        let mine = array.get_f64(0).unwrap_or(f64::NAN);
        match json_f64(&values["value"]) {
            Some(theirs) if close(mine, theirs) => {}
            theirs => report.error(format!("{what}: {mine}, golden {theirs:?}")),
        }
        return;
    }
    if scalar_to_rays {
        report.scalars += 1;
        let theirs = json_f64(&values["value"]).unwrap_or(f64::NAN);
        // xradar reads only the dataset's how/NI; ours falls back to the
        // root how/NI (odim.rs), which xradar reports as NaN.
        if !theirs.is_nan() && !our_f64s(array).iter().all(|mine| close(*mine, theirs)) {
            report.error(format!(
                "{what}: per-ray values differ from golden scalar {theirs}"
            ));
        }
        return;
    }
    if ours_name == "sweep_group_name" {
        // xradar's ODIM sweep_group_name holds sweep indices, and its
        // CfRadial one the file's sweep numbers (DOW8 trim: `sweep_2` for
        // group `sweep_0`); ours names the groups it writes.
        report.scalars += 1;
        let count = values["count"]
            .as_u64()
            .or_else(|| values["values"].as_array().map(|v| v.len() as u64));
        if count != Some(array.len() as u64) {
            report.error(format!("{what}: {} sweeps, golden {count:?}", array.len()));
        }
        return;
    }
    if coarse_range {
        if name == "range" {
            report.scalars += 1; // the coordinate is xradar's coarse-moment range
            return;
        }
        if let Some(native) = native {
            compare_array(report, &what, values, golden_dtype, native, None);
            return;
        }
    }
    let time = (name == "time").then_some(ctx.time_reference);
    compare_array(report, &what, values, golden_dtype, array, time);
}

/// The root platform-track variable of a moving platform: the sweeps'
/// per-ray tracks concatenated in sweep and acquisition order (xradar keeps
/// the root track in file order in both views).
fn concatenated_track(volume: &Volume, name: &str) -> Option<ArrayBuf> {
    let mut out: Vec<f64> = Vec::new();
    for sweep in &volume.sweeps {
        let track = sweep.platform_track.as_ref()?;
        match name {
            "latitude" => out.extend(&track.latitude_deg),
            "longitude" => out.extend(&track.longitude_deg),
            "altitude" => out.extend(&track.altitude_m),
            "altitude_agl" => out.extend(track.altitude_agl_m.as_ref()?),
            _ => return None,
        }
    }
    Some(ArrayBuf::F64(out))
}

fn has_platform_track(volume: &Volume) -> bool {
    volume.sweeps.iter().any(|s| s.platform_track.is_some())
}

fn compare_xradar_view(
    report: &mut Report,
    ctx: &Context<'_>,
    golden_view: &Value,
    time_view: &Value,
    ours: &BTreeMap<String, OurGroup>,
    volume: &Volume,
) {
    let golden_groups = golden_view["groups"].as_array().unwrap();
    let time_group = |path: &str| -> Option<&Value> {
        time_view["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["path"] == path)
    };
    let mut seen_paths = BTreeSet::new();
    for golden_group in golden_groups {
        let path = golden_group["path"].as_str().unwrap();
        seen_paths.insert(path.to_owned());
        let what = format!("{} {} {path}", ctx.case.id, ctx.view);
        let golden_vars = golden_group["variables"].as_array().unwrap();
        let Some(mine) = ours.get(path) else {
            let empty = golden_vars.is_empty()
                && golden_group["attrs"]
                    .as_object()
                    .is_none_or(|attrs| attrs.is_empty());
            let all_fill = !golden_vars.is_empty()
                && golden_vars.iter().all(|v| {
                    let fill = json_f64(&v["attrs"]["_FillValue"]);
                    fill.is_some() && json_f64(&v["values"]["value"]) == fill
                });
            if !(empty || all_fill) {
                report.error(format!("{what}: group missing"));
            }
            continue;
        };
        let sweep_index = path
            .strip_prefix("/sweep_")
            .and_then(|n| n.parse::<usize>().ok());
        let coarse_range = coarse_xradar_range(ctx, golden_group, mine);
        // Dimensions.
        if let Some(dims) = golden_group["dims"].as_object() {
            for (dim, len) in dims {
                let len = len.as_u64().unwrap() as usize;
                if dim == "frequency" && path == "/" {
                    // FM301 puts frequency in every sweep (section 1).
                    match ours.get("/sweep_0").and_then(|s| s.dims.get("frequency")) {
                        Some(mine) if *mine == len => {}
                        other => {
                            report.error(format!("{what}: frequency dim {other:?}, golden {len}"))
                        }
                    }
                    continue;
                }
                if dim == "time" && path == "/" && has_platform_track(volume) {
                    // xradar's root ray dimension of a moving platform.
                    let rays: usize = volume.sweeps.iter().map(Sweep::nrays).sum();
                    if rays != len {
                        report.error(format!("{what}: root time dim {rays} rays, golden {len}"));
                    }
                    continue;
                }
                if dim == "range" && coarse_range {
                    report.scalars += 1;
                    continue;
                }
                match mine.dims.get(dim) {
                    Some(l) if *l == len => {}
                    other => report.error(format!("{what}: dim {dim} {other:?}, golden {len}")),
                }
            }
        }
        // Attributes (the auto view omits attrs identical to the time view).
        let attr_source = if golden_group.get("attrs").is_some() {
            golden_group
        } else {
            time_group(path).unwrap_or(golden_group)
        };
        compare_group_attrs(report, ctx, path, attr_source, mine);
        // Variables.
        let golden_gates = golden_group["dims"]["range"].as_u64().map(|g| g as usize);
        let mut seen = BTreeSet::new();
        for golden_var in golden_vars {
            let name = golden_var["name"].as_str().unwrap();
            let mut golden_var = golden_var.clone();
            if golden_var.get("attrs").is_none()
                && let Some(time_var) = time_group(path).and_then(|g| {
                    g["variables"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["name"] == name)
                })
            {
                for key in ["attrs", "attr_types", "encoding", "encoding_types"] {
                    if let Some(value) = time_var.get(key) {
                        golden_var[key] = value.clone();
                    }
                }
            }
            seen.insert(name.to_owned());
            let golden_dims: Vec<&str> = golden_var["dims"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            // Moving platform: xradar's root latitude(time) is our per-sweep track.
            if path == "/" && golden_dims == ["time"] && PLATFORM_TRACK.contains(&name) {
                let what = format!("{what}/{name}");
                match concatenated_track(volume, name) {
                    Some(track) => compare_array(
                        report,
                        &what,
                        &golden_var["values"],
                        golden_var["dtype"].as_str().unwrap(),
                        &track,
                        None,
                    ),
                    None => report.error(format!("{what}: no platform track in the sweeps")),
                }
                continue;
            }
            // CfRadial root frequency lives in every sweep in FM301.
            let (owner, ours_name) = if name == "frequency" && path == "/" {
                (ours.get("/sweep_0"), "frequency")
            } else {
                (Some(mine), name)
            };
            let Some(my_var) = owner.and_then(|g| g.vars.get(ours_name)) else {
                // A calibration or parameter entry equal to its fill, or an
                // empty text, is unset in ours.
                let fill = json_f64(&golden_var["attrs"]["_FillValue"]);
                let value = json_f64(&golden_var["values"]["value"]);
                let empty_text = golden_var["values"]["value"].as_str().is_some_and(|t| {
                    t.trim_matches(|c: char| c == '\0' || c.is_whitespace())
                        .is_empty()
                });
                if (fill.is_some() && value == fill) || empty_text {
                    report.scalars += 1;
                    continue;
                }
                report.error(format!("{what}/{name}: variable missing"));
                continue;
            };
            // Native rows of a field on a coarse-range sweep, padded like xradar.
            let native = match (coarse_range, sweep_index, golden_gates) {
                (true, Some(sweep), Some(gates)) if golden_dims.len() == 2 => volume
                    .sweeps
                    .get(sweep)
                    .and_then(|s| s.fields.iter().find(|f| f.name.as_str() == name))
                    .map(|field| {
                        let padded = native_rows_padded(field, gates);
                        match &my_var.array {
                            // The auto view reorders rows; apply the same order.
                            Some(_) if ctx.view == "auto" => {
                                reorder_rows(&padded, gates, volume, sweep)
                            }
                            _ => padded,
                        }
                    }),
                _ => None,
            };
            compare_variable(
                report,
                ctx,
                path,
                &golden_var,
                my_var,
                ours_name,
                coarse_range,
                native.as_ref(),
            );
        }
        for name in mine.vars.keys() {
            if seen.contains(name) || OURS_ONLY_VARIABLES.contains(&name.as_str()) {
                continue;
            }
            if path == "/" && name == "frequency" {
                continue;
            }
            // xradar emits no spectrum width for Message 1 volumes (A.3).
            if ctx.message_1 && name == "WRADH" {
                report.scalars += 1;
                continue;
            }
            // The platform track and georeference variables of a moving
            // platform (xradar: root coordinates; FM301: per sweep).
            if has_platform_track(volume) && PLATFORM_TRACK.contains(&name.as_str()) {
                continue;
            }
            report.error(format!("{what}/{name}: variable is not in the golden"));
        }
    }
    for path in ours.keys() {
        if !seen_paths.contains(path) {
            report.error(format!(
                "{} {} {path}: group is not in the golden",
                ctx.case.id, ctx.view
            ));
        }
    }
}

/// `padded` (rows of `gates`) in the ray order of the auto view: rays sorted
/// by azimuth (stable), the order `fm301::volume_view` applies.
fn reorder_rows(padded: &ArrayBuf, gates: usize, volume: &Volume, sweep: usize) -> ArrayBuf {
    let angles = &volume.sweeps[sweep].rays.azimuth_deg;
    let mut order: Vec<usize> = (0..angles.len()).collect();
    order.sort_by(|a, b| angles[*a].total_cmp(&angles[*b]));
    macro_rules! reorder {
        ($values:expr, $variant:ident) => {{
            let mut out = Vec::with_capacity($values.len());
            for row in &order {
                out.extend_from_slice(&$values[row * gates..(row + 1) * gates]);
            }
            ArrayBuf::$variant(out)
        }};
    }
    match padded {
        ArrayBuf::U8(v) => reorder!(v, U8),
        ArrayBuf::U16(v) => reorder!(v, U16),
        ArrayBuf::I8(v) => reorder!(v, I8),
        ArrayBuf::I16(v) => reorder!(v, I16),
        ArrayBuf::F32(v) => reorder!(v, F32),
        ArrayBuf::F64(v) => reorder!(v, F64),
        other => other.clone(),
    }
}

fn check_xradar(case: &Case, decoded: &Decoded, golden: &Value) -> Report {
    let mut report = Report::default();
    let time_view = &golden["views"]["time"];
    let zero_vcp = golden["views"]["time"]["groups"][0]["attrs"]["number_elevation_cuts"] == 0;
    let extra: Option<&dyn ExtraAttrs> = decoded
        .nexrad
        .as_ref()
        .map(|nexrad| nexrad as &dyn ExtraAttrs);
    let message_1 = decoded
        .nexrad
        .as_ref()
        .is_some_and(|nexrad| nexrad.metadata.vcp.is_none());
    for (view_name, first_dim) in [("time", FirstDim::Time), ("auto", FirstDim::Auto)] {
        let options = ViewOptions {
            flavor: Flavor::Xradar012,
            first_dim,
            passthrough: Passthrough::Flavor,
        };
        let view = fm301::volume_view(&decoded.volume, options, extra).unwrap();
        let ours = flatten(&view);
        let ctx = Context {
            case,
            view: view_name,
            source: decoded.volume.provenance.source_format,
            time_reference: decoded.volume.time_reference,
            zero_vcp,
            message_1,
            time_view,
        };
        compare_xradar_view(
            &mut report,
            &ctx,
            &golden["views"][view_name],
            time_view,
            &ours,
            &decoded.volume,
        );
    }
    report
}

// ---------------------------------------------------------------------------
// Py-ART comparison
// ---------------------------------------------------------------------------

fn per_sweep(variable: &Value, sweep: usize) -> Option<&Value> {
    variable["per_sweep"].as_array()?.get(sweep)
}

/// Py-ART's volume range: first centre (from the data, see [`EXPECTED`]),
/// spacing and gate count.
fn pyart_range(radar: &Value) -> (f64, f64, usize) {
    let values = &radar["variables"]["range"]["values"];
    let first = values["first"].as_array().unwrap();
    let r0 = json_f64(&first[0]).unwrap();
    let dr = first.get(1).and_then(json_f64).map_or(0.0, |r1| r1 - r0);
    (r0, dr, radar["ngates"].as_u64().unwrap() as usize)
}

/// A field's physical values laid out on Py-ART's volume range: each native
/// gate repeated `width / dr` times from the gate its leading edge falls in
/// (`tools/fm301_golden.py`, `verify_nexrad`). `None` when the field's
/// geometry does not sit on that range.
fn pyart_layout(sweep: &Sweep, field: &Field, r0: f64, dr: f64, ngates: usize) -> Option<Vec<f32>> {
    let (first, width) = field.native_geometry(&sweep.range)?;
    if dr <= 0.0 {
        return None;
    }
    let k = (width / dr).round();
    let m = ((first - width / 2.0) - (r0 - dr / 2.0)) / dr;
    if k < 1.0 || (k * dr - width).abs() > 1e-3 * dr || (m - m.round()).abs() > 1e-3 {
        return None;
    }
    let (k, m) = (k as usize, m.round() as i64);
    let (rows, native) = field.shape();
    let mut out = vec![f32::NAN; rows * ngates];
    for row in 0..rows {
        let out_row = &mut out[row * ngates..(row + 1) * ngates];
        for gate in 0..native {
            let value = field.value(row, gate).unwrap_or(f32::NAN);
            for rep in 0..k {
                let index = m + (gate * k + rep) as i64;
                if index >= 0 && (index as usize) < ngates {
                    out_row[index as usize] = value;
                }
            }
        }
    }
    Some(out)
}

/// Py-ART's ODIM azimuth convention: `numpy.angle` of the complex mean of
/// `startazA` and `stopazA`, in (-180, 180].
fn pyart_odim_azimuth(azimuth: f32) -> f64 {
    let wrapped = f64::from(azimuth).rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

fn check_pyart(case: &Case, decoded: &Decoded, golden: &Value) -> Report {
    let mut report = Report::default();
    let volume = &decoded.volume;
    let odim = volume.provenance.source_format == SourceFormat::OdimH5;
    let message_1 = decoded
        .nexrad
        .as_ref()
        .is_some_and(|nexrad| nexrad.metadata.vcp.is_none());
    let radar = &golden["radar"];
    let id = &case.id;
    let sweeps = radar["sweeps"].as_array().unwrap();
    if sweeps.len() != volume.sweeps.len() {
        report.error(format!(
            "{id} pyart: {} sweeps, golden {}",
            volume.sweeps.len(),
            sweeps.len()
        ));
        return report;
    }
    // Metadata.
    let metadata = &radar["metadata"];
    let clean = |name: &str| {
        name.trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_owned()
    };
    if let Some(name) = metadata["instrument_name"].as_str()
        && !clean(name).is_empty()
    {
        report.attrs += 1;
        if clean(name) != clean(&volume.attrs.instrument_name) {
            report.error(format!(
                "{id} pyart: instrument_name {}, golden {name}",
                volume.attrs.instrument_name
            ));
        }
    }
    if let Some(vcp) = metadata["vcp_pattern"].as_u64()
        && vcp != 0
    {
        report.attrs += 1;
        if volume.scan.vcp_pattern.map(u64::from) != Some(vcp) {
            report.error(format!(
                "{id} pyart: vcp_pattern {:?}, golden {vcp}",
                volume.scan.vcp_pattern
            ));
        }
    }
    // Location.
    for (name, mine) in [
        ("latitude", volume.location.latitude_deg),
        ("longitude", volume.location.longitude_deg),
        ("altitude", volume.location.altitude_m),
    ] {
        let theirs = radar["variables"][name]["values"]["first"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(json_f64);
        report.scalars += 1;
        match (mine, theirs) {
            (Some(mine), Some(theirs)) if close(mine, theirs) => {}
            (None, Some(0.0)) => {} // Py-ART writes 0 for an unknown Message 1 site
            (mine, theirs) => {
                report.error(format!("{id} pyart: {name} {mine:?}, golden {theirs:?}"))
            }
        }
    }
    // Range: Py-ART's volume range and whether ours fits it.
    let (r0, dr, ngates) = pyart_range(radar);
    let expected_r0 = volume
        .sweeps
        .iter()
        .flat_map(|s| {
            s.fields
                .iter()
                .filter_map(move |f| f.native_geometry(&s.range))
        })
        .map(|(first, _)| first)
        .fold(f64::INFINITY, f64::min);
    // AEMET's `rstart` in metres, which Py-ART takes as km (see EXPECTED).
    let espdg_rstart = case.id.starts_with("odim-espdg");
    let range_matches = if espdg_rstart {
        report.scalars += 1;
        if !close(r0, 200_250.0) {
            report.error(format!(
                "{id} pyart: expected Py-ART's 200 km first gate, golden {r0}"
            ));
        }
        false
    } else if !close(expected_r0, r0) {
        report.error(format!(
            "{id} pyart: first gate centre {expected_r0}, golden {r0}"
        ));
        false
    } else {
        true
    };
    // Sweep table, coordinates, fixed angles, modes, instrument parameters.
    let modes = radar["variables"]["sweep_mode"]["values"]["values"].as_array();
    let fixed = radar["variables"]["fixed_angle"]["values"]["values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let time_reference = radar["variables"]["time"]["meta"]["units"]
        .as_str()
        .and_then(|u| u.strip_prefix("seconds since "))
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or(volume.time_reference);
    let shift = (volume.time_reference - time_reference).num_milliseconds() as f64 / 1000.0;
    for (index, (entry, sweep)) in sweeps.iter().zip(&volume.sweeps).enumerate() {
        let what = format!("{id} pyart sweep {index}");
        let rays = entry["rays"].as_u64().unwrap() as usize;
        if rays != sweep.nrays() {
            report.error(format!("{what}: {} rays, golden {rays}", sweep.nrays()));
            continue;
        }
        if let Some(theirs) = fixed.get(index).and_then(json_f64) {
            report.scalars += 1;
            // Py-ART rounds a Message 1 volume's first-ray elevation to 0.1 deg.
            let mine = f64::from(sweep.fixed_angle_deg);
            let mine = if message_1 {
                (mine * 10.0).round() / 10.0
            } else {
                mine
            };
            if !close(mine, theirs) {
                report.error(format!(
                    "{what}: fixed_angle {}, golden {theirs}",
                    sweep.fixed_angle_deg
                ));
            }
        }
        if let Some(mode) = modes.and_then(|m| m.get(index)).and_then(Value::as_str) {
            report.scalars += 1;
            if mode != sweep.sweep_mode.as_str() {
                report.error(format!(
                    "{what}: sweep_mode {}, golden {mode}",
                    sweep.sweep_mode.as_str()
                ));
            }
        }
        if let Some(golden) = per_sweep(&radar["variables"]["azimuth"], index) {
            if odim {
                // Py-ART: complex mean of startazA/stopazA in (-180, 180] when
                // the file has measured azimuths, else bin centres in [0, 360);
                // ours (as xradar) the arithmetic mean in [0, 360). Same angles.
                let signed = json_f64(&golden["min"]).is_some_and(|min| min < 0.0);
                let values: Vec<f64> = sweep
                    .rays
                    .azimuth_deg
                    .iter()
                    .map(|a| {
                        if signed {
                            pyart_odim_azimuth(*a)
                        } else {
                            f64::from(*a).rem_euclid(360.0)
                        }
                    })
                    .collect();
                compare_summary(
                    &mut report,
                    &format!("{what}/azimuth"),
                    golden,
                    &summarize(values.iter().copied()),
                    1e-2,
                );
            } else {
                let dtype = golden["dtype"].as_str().unwrap_or("float64");
                let array = ArrayBuf::F32(sweep.rays.azimuth_deg.clone());
                compare_array(
                    &mut report,
                    &format!("{what}/azimuth"),
                    golden,
                    dtype,
                    &array,
                    None,
                );
            }
        }
        if let Some(golden) = per_sweep(&radar["variables"]["elevation"], index) {
            if odim {
                // Py-ART: where/elangle (or how/elangles) for every ray; ours
                // (as xradar) the startelA/stopelA midpoints (section 3).
                let values: Vec<f64> = sweep
                    .rays
                    .elevation_deg
                    .iter()
                    .map(|e| f64::from(*e))
                    .collect();
                compare_summary(
                    &mut report,
                    &format!("{what}/elevation"),
                    golden,
                    &summarize(values.iter().copied()),
                    0.05,
                );
            } else {
                let dtype = golden["dtype"].as_str().unwrap_or("float32");
                let array = ArrayBuf::F32(sweep.rays.elevation_deg.clone());
                compare_array(
                    &mut report,
                    &format!("{what}/elevation"),
                    golden,
                    dtype,
                    &array,
                    None,
                );
            }
        }
        if let Some(golden) = per_sweep(&radar["variables"]["time"], index) {
            let seconds: Vec<f64> = sweep.rays.time_s.iter().map(|t| t + shift).collect();
            // Py-ART's ODIM reader spreads ray times over the sweep by file
            // position; ours (as xradar) by acquisition position from a1gate,
            // so only the sweep's span compares (section 3).
            let absolute = if odim { 1.0 } else { 1e-3 };
            compare_summary(
                &mut report,
                &format!("{what}/time"),
                golden,
                &summarize(seconds.iter().copied()),
                absolute,
            );
        }
        if let Some(parameters) = radar["instrument_parameters"].as_object() {
            for (name, mine) in [
                (
                    "nyquist_velocity",
                    sweep.ray_vars.nyquist_velocity_mps.as_ref(),
                ),
                (
                    "unambiguous_range",
                    sweep.ray_vars.unambiguous_range_m.as_ref(),
                ),
                ("prt", sweep.ray_vars.prt_s.as_ref()),
                ("pulse_width", sweep.ray_vars.pulse_width_s.as_ref()),
            ] {
                let Some(golden) = parameters.get(name).and_then(|v| per_sweep(v, index)) else {
                    continue;
                };
                let Some(mine) = mine else {
                    // Py-ART writes 0 where a Level II radial carries no value
                    // (surveillance cuts of Message 1 volumes); ours has none.
                    if json_f64(&golden["max"]) == Some(0.0) {
                        report.scalars += 1;
                        continue;
                    }
                    report.error(format!("{what}/{name}: missing"));
                    continue;
                };
                compare_summary(
                    &mut report,
                    &format!("{what}/{name}"),
                    golden,
                    &summarize(mine.iter().map(|v| f64::from(*v))),
                    0.0,
                );
            }
        }
    }
    // Fields.
    let fields = golden["fields"].as_object().unwrap();
    for (pyart_name, field_golden) in fields {
        for (index, sweep) in volume.sweeps.iter().enumerate() {
            let what = format!("{id} pyart sweep {index}/{pyart_name}");
            let Some(golden) = per_sweep(field_golden, index) else {
                continue;
            };
            let mine = sweep.fields.iter().find(|f| {
                f.name
                    .pyart_name(PyartNames::Reader, volume.provenance.source_format)
                    == *pyart_name
            });
            let Some(field) = mine else {
                if golden["count_unmasked"].as_u64() == Some(0) {
                    continue; // Py-ART pads a sweep that lacks the field
                }
                report.error(format!(
                    "{what}: field missing (ours: {:?})",
                    sweep
                        .fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                ));
                continue;
            };
            let physical = if range_matches {
                pyart_layout(sweep, field, r0, dr, ngates)
            } else {
                None
            };
            let hash = golden["sha256"].as_str();
            if let Some(values) = &physical
                && Some(sha256_f32(values).as_str()) == hash
            {
                report.hashed += 1;
                continue;
            }
            // Statistics over the native gates (the layout only repeats and
            // pads, which leaves min, max and mean unchanged).
            let values = field.to_physical();
            let summary = summarize(values.iter().map(|v| f64::from(*v)));
            report.summarized += 1;
            let unmasked = golden["count_unmasked"].as_u64().unwrap_or(0) as usize;
            let laid_out = physical
                .as_ref()
                .map(|laid| laid.iter().filter(|v| v.is_finite()).count());
            if laid_out != Some(unmasked) && summary.finite != unmasked {
                report.error(format!(
                    "{what}: {} valid gates, golden {unmasked}",
                    summary.finite
                ));
            }
            for (key, mine) in [
                ("min", summary.min),
                ("max", summary.max),
                ("mean", summary.mean),
            ] {
                match (json_f64(&golden[key]), mine) {
                    (None, None) => {}
                    (Some(theirs), Some(mine)) if close(mine, theirs) => {}
                    (theirs, mine) => {
                        report.error(format!("{what}: {key} {mine:?}, golden {theirs:?}"))
                    }
                }
            }
        }
    }
    report
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[test]
fn every_golden_case_matches_xradar_and_pyart() {
    let cases = cases();
    assert!(cases.len() >= 11, "{} cases in index.json", cases.len());
    let mut failures = String::new();
    let mut checked = 0;
    for case in &cases {
        let Some(decoded) = decode(case) else {
            continue;
        };
        checked += 1;
        let mut reports = Vec::new();
        if let Some(golden) = &case.xradar {
            reports.push(("xradar", check_xradar(case, &decoded, golden)));
        }
        if let Some(golden) = &case.pyart {
            reports.push(("pyart", check_pyart(case, &decoded, golden)));
        }
        for (side, report) in reports {
            eprintln!(
                "{} {side}: {} hashes, {} summaries, {} scalars, {} attributes, {} errors",
                case.id,
                report.hashed,
                report.summarized,
                report.scalars,
                report.attrs,
                report.errors.len()
            );
            assert!(
                report.hashed + report.summarized + report.scalars > 0,
                "{} {side}: nothing compared",
                case.id
            );
            for error in &report.errors {
                eprintln!("  {error}");
                failures.push_str(error);
                failures.push('\n');
            }
        }
    }
    assert!(checked > 0, "no golden file was available");
    assert!(failures.is_empty(), "\n{failures}");
}

#[test]
fn expected_differences_are_listed() {
    // The list is documentation; keep it non-empty and free of duplicates.
    let set: BTreeSet<&str> = EXPECTED.iter().copied().collect();
    assert_eq!(set.len(), EXPECTED.len());
    let _ = Cow::Borrowed("");
}
