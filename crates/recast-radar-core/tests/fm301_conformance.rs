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
//!   elements with NaN canonicalized, the golden's convention); an array whose
//!   dtype differs only in float width is hashed after widening and, failing
//!   that, its count, min, max, mean, first and last values must agree within
//!   [`TOLERANCE`] relative;
//! - a `time` coordinate is decoded with the view's own `units` attribute
//!   (`seconds since <reference>`, which must name a whole second) and hashed
//!   as int64 nanoseconds; xradar's datetime64 values carry float noise (up
//!   to tens of microseconds), so a time that does not hash equal must agree
//!   in count, min, max, mean, first and last within [`TIME_TOLERANCE_S`];
//! - a field on a sweep whose xradar range is coarser than ours (6.2) is
//!   taken from the view's materialized variable at its native gates, padded
//!   as xradar pads it;
//! - scalars and numeric attributes agree within [`TOLERANCE`]; text and
//!   boolean attributes are equal; CfRadial coordinate and instrument
//!   variables compare `units` by meaning and `positive` as text.
//!
//! The Py-ART side compares the volume with `Radar`: sweep table, per-sweep
//! coordinates, fixed angles, sweep modes, location, time reference,
//! instrument parameters, and every field read through the FM301 view (the
//! materialized variable, with its ray order, padding and gate repetition)
//! and placed on Py-ART's volume range the way
//! `read_nexrad_archive(linear_interp=False)` lays it out (each range gate of
//! the sweep repeated over the Py-ART gates it covers, design note 6.5): the
//! physical float32 values must hash equal to Py-ART's per-sweep arrays. Only
//! a case whose range Py-ART misreads (listed) compares count, min, max and
//! mean instead.
//!
//! Differences the readers are known to have are listed in [`EXPECTED`], each
//! with a key and the design-note section that decides it. The code that
//! applies a rule records its key: an item left uncompared counts as a known
//! difference (not as a comparison), and an item compared under a rule (a
//! tolerance, a unit or naming convention) counts as a comparison. Every
//! listed difference must have been applied at least once. Everything else
//! must match, and every item of ours must be in the golden or listed.
//!
//! All 11 cases must run: a case whose file is neither committed nor cached
//! and cannot be downloaded fails the test, unless
//! `RECAST_RADAR_TESTDATA_OFFLINE` is set (then the available cases run and
//! the coverage check is skipped).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use recast_radar_core::fm301::{
    self, ExtraAttrs, FirstDim, Flavor, Group, Passthrough, RowOrder, Values, Variable,
    ViewOptions, VolumeView,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, Coding, GateMapping, PyartNames, Scalar, SourceFormat, Sweep, Volume,
};
use serde_json::Value;

/// Relative tolerance for values the two sides compute in different
/// precision (design note section 15, open question 5).
const TOLERANCE: f64 = 1e-4;

/// Absolute tolerance for absolute times (seconds). xradar's `datetime64`
/// values carry float noise up to 28.6 us (iesha); a wrong time reference is
/// off by whole seconds.
const TIME_TOLERANCE_S: f64 = 1e-3;

/// The reader differences this test expects: a key, used by the code that
/// applies the rule, and the difference with the section of
/// `docs/design/fm301-model.md` (or of the golden README) that documents it.
/// Every rule below is applied only where it says.
const EXPECTED: &[(&str, &str)] = &[
    (
        "xradar-none-text",
        "xradar writes the text `None` for a global attribute it has no value for (A.2, A.4), and keeps CfRadial's empty strings; ours carries the file's value or omits the item",
    ),
    (
        "odim-conventions",
        "xradar writes `Conventions = ODIM_H5/V2_2` for every ODIM file; ours writes the file's `/Conventions` (section 11)",
    ),
    (
        "sweep-group-names",
        "xradar omits `sweep_group_name` / `sweep_fixed_angle` for NEXRAD and writes integer sweep indices as ODIM `sweep_group_name` (section 1); ours always writes the names",
    ),
    (
        "follow-mode",
        "xradar writes `follow_mode = not_set` for NEXRAD and ODIM; ours writes the Table 301-15 value `none` (section 10)",
    ),
    (
        "nyquist-per-ray",
        "xradar omits `nyquist_velocity` and `unambiguous_range` for NEXRAD and writes one Nyquist scalar per ODIM sweep; ours writes both per ray, Table 301-8a (section 9)",
    ),
    (
        "fm301-sentinels",
        "xradar has no `polarization_mode` or `frequency` for NEXRAD and ODIM, and no `_FillValue`, `_Undetect`, `valid_range` or `flag_*` for NEXRAD; ours writes them (7.1, 10)",
    ),
    (
        "cfradial-frequency",
        "xradar puts CfRadial `frequency` at the root; FM301 puts it in every sweep (section 1)",
    ),
    (
        "odim-th-units",
        "xradar labels ODIM `TH` linear and unitless; io-odim uses dBZ (8.2 note 1)",
    ),
    (
        "zero-vcp",
        "xradar reads the zero-filled Message 5 of KLIX 2005 as VCP 0 (A.3); ours has no VCP definition and takes the VCP number from the radials",
    ),
    (
        "calibration-fill",
        "xradar writes CfRadial calibration entries equal to their `_FillValue` (-9999); ours leaves them unset (section 2)",
    ),
    (
        "pyart-odim-rstart",
        "Py-ART reads ODIM `rstart` in km; AEMET writes metres (espdg), which io-odim detects (odim.rs, `first_gate_m_from_rstart`)",
    ),
    (
        "pyart-odim-first-gate",
        "Py-ART's ODIM reader reports `meters_to_center_of_first_gate = 0` while its data starts at the first centre (section 14)",
    ),
    (
        "pyart-odim-rays",
        "Py-ART's ODIM reader takes azimuths as the complex mean of startazA/stopazA in (-180, 180], elevations from where/elangle, and ray times by file position; ours follow xradar (arithmetic mean in [0, 360), startelA/stopelA midpoints, acquisition order from a1gate) (sections 3, 14)",
    ),
    (
        "pyart-message-1-fixed-angle",
        "Py-ART rounds a Message 1 volume's fixed angle (the first radial's elevation) to 0.1 degree (A.3)",
    ),
    (
        "cfradial-verbatim-attrs",
        "xradar keeps CfRadial variable attributes verbatim on coordinate and instrument variables; the model's typed slots write the FM301 attributes (sections 9, 12.4), so only dataset variables compare attributes there",
    ),
    (
        "platform-track",
        "xradar puts a moving platform's latitude/longitude/altitude(time) at the root; FM301 keeps them per sweep in the platform track (section 9)",
    ),
    (
        "coarse-range",
        "xradar takes a NEXRAD sweep's range from its coarsest moment and misplaces the finer ones (6.2); the fields still compare natively, the coordinate does not",
    ),
    (
        "message-1-xradar",
        "xradar omits WRADH for Message 1 volumes and reads the -375 m first gate as 65161 (A.3)",
    ),
    (
        "xradar-ni-and-sweep-numbers",
        "xradar reads an ODIM Nyquist velocity from the dataset's how/NI only and copies CfRadial's sweep numbers into sweep_number and sweep_group_name; ours falls back to the root how/NI and numbers the groups it writes (sections 1, 9)",
    ),
    (
        "cfradial-range",
        "xradar keeps a CfRadial range coordinate's float32 values; a range the decoder found uniform is regenerated from its first centre and spacing (RangeCoord::Uniform, section 3)",
    ),
    (
        "calib-squeeze",
        "xradar squeezes a one-entry r_calib dimension into scalars and keeps the calibration time as text; ours writes calib arrays and seconds since the reference (section 12.4)",
    ),
    (
        "pyart-zero-nyquist",
        "Py-ART writes a zero Nyquist velocity and unambiguous range for Level II radials that carry none; ours leaves the variable out (section 9)",
    ),
    (
        "site-parameters",
        "xradar 0.12 writes empty radar_parameters and radar_calibration groups for NEXRAD and ODIM, and no ODIM pulse width or calibration index; ours writes what the source records: the NEXRAD Message 18 frequency, antenna gain and (before Build 18) beam width, and ODIM `how` beam widths, antenna gains, receiver bandwidth, and per-dataset radar constants and pulse width with each ray's calibration index (sections 2, 9)",
    ),
    (
        "empty-groups",
        "xradar writes radar_parameters, radar_calibration and georeferencing_correction groups with no values; ours omits a group it has nothing for (section 1)",
    ),
    (
        "fm301-extras",
        "ours writes FM301 items xradar 0.12 omits: the sweep variables rays_are_indexed, rays_angle_resolution, target_scan_rate, calib_index, instrument_type, platform_type, primary_axis, follow_mode and prt_mode where a source lacks them; the root attributes site_name, ray_times_increase, scan_id and platform_is_mobile; and standard_name, long_name, units, coordinates, comment, positive, axis and sampling_ratio on variables (sections 1, 9, 11, 12.4)",
    ),
    (
        "nexrad-moment-header",
        "ours keeps the Message 31 data-moment header values FM301 has no field for as source attributes of the field: nexrad_tover_db, nexrad_snr_threshold_db (ICD 2620002 Table XVII-B bytes 14-17, in dB) and nexrad_recombination (byte 18, the code). xradar and Py-ART drop them (section 9)",
    ),
    (
        "message-1-location",
        "xradar and Py-ART write 0 for the unknown site location of a Message 1 volume; the model keeps None (section 11)",
    ),
    (
        "pyart-padded-fields",
        "Py-ART gives every sweep every field, masked where the sweep has none (6.3); ours has no such field",
    ),
    (
        "pyart-odim-time-reference",
        "Py-ART's ODIM reader counts time from the first dataset's start (iesha 01:15:04); ours from the root what/date and what/time, the nominal volume time before it (01:15:00, section 11)",
    ),
    (
        "units-spelling",
        "xradar keeps a CfRadial file's unit spellings (`db`, `meters per second`, `degrees per second`, `unitless`, `count`); ours writes xradar's own spellings and UDUNITS symbols (section 12.4), so these units compare by meaning",
    ),
    (
        "cfradial-prt-ratio-units",
        "Radx writes `units = seconds` on the dimensionless `prt_ratio` (IRENE, DOW8); ours writes a ratio's empty unit (section 12.4)",
    ),
];

/// `key` if it is listed in [`EXPECTED`].
fn expected(key: &'static str) -> &'static str {
    assert!(
        EXPECTED.iter().any(|(listed, _)| *listed == key),
        "expected difference {key} is not listed in EXPECTED"
    );
    key
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

struct Case {
    id: String,
    kind: String,
    /// The file the Rust side decodes. Equal to `id` except where the index
    /// declares a `decoded_id` (see [`decoded_id`]).
    decoded_id: String,
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
            let id = entry["id"].as_str().unwrap().to_owned();
            Case {
                decoded_id: entry["decoded_id"]
                    .as_str()
                    .map_or_else(|| id.clone(), str::to_owned),
                id,
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

/// The file the Rust side decodes for a case: the case's own file unless
/// `index.json` declares a `decoded_id`, which only the netCDF-4 X-SAPR case
/// does (an HDF5 container the classic netCDF reader cannot open; it decodes
/// the committed classic-container twin, a raw variable-for-variable copy
/// with identical data — `testdata/other/manifest.toml`, `derived_from`).
/// `every_substituted_case_is_declared` pins the list.
fn decoded_id(case: &Case) -> &str {
    &case.decoded_id
}

/// The goldens of a case are made from the file the Rust side decodes,
/// except where `index.json` says otherwise — today exactly one case, the
/// netCDF-4 X-SAPR PPI. A new substitution has to be declared there, beside
/// the goldens, before this test accepts it.
#[test]
fn every_substituted_case_is_declared() {
    let substituted: Vec<(String, String)> = cases()
        .into_iter()
        .filter(|case| case.decoded_id != case.id)
        .map(|case| (case.id, case.decoded_id))
        .collect();
    assert_eq!(
        substituted,
        vec![(
            "cfrad1-xsapr-sgp-20110520-ppi-netcdf4".to_owned(),
            "cfrad1-xsapr-sgp-20110520-ppi-classic".to_owned()
        )],
        "cases whose Rust side reads a different file than the goldens were made from"
    );
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

/// How [`compare_summary`] compares values.
#[derive(Clone, Copy)]
enum Tolerance {
    /// [`TOLERANCE`] relative; with a nonzero margin, also within that margin,
    /// and then `first` and `last` are not compared.
    Relative(f64),
    /// Within an absolute margin only, `first` and `last` included (absolute
    /// times).
    Absolute(f64),
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
    /// A mapped field: its native gate count, gate mapping and fill.
    mapping: Option<(usize, GateMapping, Scalar)>,
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
                let mapping = match &var.values {
                    Values::Mapped {
                        native_gates,
                        mapping,
                        fill,
                        ..
                    } => Some((*native_gates, *mapping, *fill)),
                    _ => None,
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
                        mapping,
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
    /// Golden items left uncompared under an [`EXPECTED`] rule.
    known: usize,
    /// [`EXPECTED`] keys applied.
    used: BTreeSet<&'static str>,
    /// Hashes of fields the view repeats over several range gates (stride > 1).
    strided: usize,
}

impl Report {
    fn error(&mut self, what: impl Into<String>) {
        self.errors.push(what.into());
    }

    /// A golden item left uncompared under the listed difference `key`.
    fn skip(&mut self, key: &'static str) {
        self.known += 1;
        self.used.insert(expected(key));
    }

    /// An item compared (or an extra item of ours accepted) under the listed
    /// difference `key`.
    fn note(&mut self, key: &'static str) {
        self.used.insert(expected(key));
    }
}

// ---------------------------------------------------------------------------
// xradar comparison
// ---------------------------------------------------------------------------

/// Variables ours writes that xradar 0.12 does not, with their [`EXPECTED`]
/// key.
const OURS_ONLY_VARIABLES: &[(&str, &str)] = &[
    ("nyquist_velocity", "nyquist-per-ray"),
    ("unambiguous_range", "nyquist-per-ray"),
    ("polarization_mode", "fm301-sentinels"),
    ("frequency", "fm301-sentinels"),
    ("sweep_group_name", "sweep-group-names"),
    ("sweep_fixed_angle", "sweep-group-names"),
    // FM301 items a source may lack; xradar writes only what the file has.
    ("rays_are_indexed", "fm301-extras"),
    ("rays_angle_resolution", "fm301-extras"),
    ("target_scan_rate", "fm301-extras"),
    ("calib_index", "fm301-extras"),
    ("instrument_type", "fm301-extras"),
    ("platform_type", "fm301-extras"),
    ("primary_axis", "fm301-extras"),
    ("follow_mode", "fm301-extras"),
    ("prt_mode", "fm301-extras"),
];

/// Root attributes ours writes that xradar 0.12 does not.
const OURS_ONLY_ATTRS: &[(&str, &str)] = &[
    ("site_name", "fm301-extras"),
    ("ray_times_increase", "fm301-extras"),
    ("scan_id", "fm301-extras"),
    ("platform_is_mobile", "fm301-extras"),
];

/// Variable attributes ours writes that xradar 0.12 does not.
const OURS_ONLY_VAR_ATTRS: &[(&str, &str)] = &[
    ("_FillValue", "fm301-sentinels"),
    ("_Undetect", "fm301-sentinels"),
    ("valid_range", "fm301-sentinels"),
    ("flag_values", "fm301-sentinels"),
    ("flag_meanings", "fm301-sentinels"),
    ("flag_masks", "fm301-sentinels"),
    ("coordinates", "fm301-extras"),
    ("comment", "fm301-extras"),
    ("standard_name", "fm301-extras"),
    ("long_name", "fm301-extras"),
    ("units", "fm301-extras"),
    ("positive", "fm301-extras"),
    ("axis", "fm301-extras"),
    ("sampling_ratio", "fm301-extras"),
    ("nexrad_tover_db", "nexrad-moment-header"),
    ("nexrad_snr_threshold_db", "nexrad-moment-header"),
    ("nexrad_recombination", "nexrad-moment-header"),
];

/// The [`EXPECTED`] key of an item in one of the ours-only tables.
fn ours_only(table: &[(&str, &'static str)], name: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(listed, _)| *listed == name)
        .map(|(_, key)| *key)
}

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
    if matches!(golden, Value::Null | Value::Object(_)) {
        report.error(format!(
            "{what}: golden attribute {name} = {golden} has no comparable value"
        ));
        return;
    }
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
        Value::Null | Value::Object(_) => false,
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
            report.skip("xradar-none-text");
            continue;
        }
        if ctx.zero_vcp && VCP_ROOT_ATTRS.contains(&name.as_str()) {
            report.skip("zero-vcp");
            continue;
        }
        let Some(mine) = ours.attrs.get(name) else {
            report.error(format!("{what}: attribute {name} = {value} missing"));
            continue;
        };
        if name == "Conventions" && ctx.source == SourceFormat::OdimH5 {
            report.attrs += 1;
            report.note("odim-conventions");
            let mine = attr_text(mine).unwrap_or_default();
            if value != "ODIM_H5/V2_2" || !mine.starts_with("ODIM_H5/") {
                report.error(format!("{what}: Conventions {mine} vs golden {value}"));
            }
            continue;
        }
        compare_attr(report, &what, name, value, mine);
    }
    // Every attribute of ours must be in the golden, except the listed FM301
    // root attributes.
    {
        for name in ours.attrs.keys() {
            if golden_attrs.contains_key(name) {
                continue;
            }
            if let Some(key) = ours_only(OURS_ONLY_ATTRS, name).filter(|_| path == "/") {
                report.note(key);
            } else if ctx.zero_vcp && VCP_ROOT_ATTRS.contains(&name.as_str()) {
                report.note("zero-vcp");
            } else {
                report.error(format!("{what}: attribute {name} is not in the golden"));
            }
        }
    }
}

/// The reference of a `units` attribute of the form `seconds since
/// YYYY-MM-DDThh:mm:ssZ` (FM301 Table 301-6b: whole seconds, UTC).
fn seconds_since(units: Option<&AttrValue>) -> Option<DateTime<Utc>> {
    let text = attr_text(units?)?;
    let stamp = text.strip_prefix("seconds since ")?;
    let valid = stamp.len() == 20
        && stamp.ends_with('Z')
        && stamp.as_bytes()[10] == b'T'
        && !stamp.contains('.');
    if !valid {
        return None;
    }
    stamp.parse::<DateTime<Utc>>().ok()
}

/// Our `time` coordinate (seconds since `reference`) as int64 nanoseconds
/// since the epoch, the golden's `datetime64[ns]` hash input.
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
        // xradar's float noise: the epoch-second summaries within
        // TIME_TOLERANCE_S, never relative.
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
            Tolerance::Absolute(TIME_TOLERANCE_S),
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
        Tolerance::Relative(0.0),
    );
}

/// Count, min, max, mean, first and last of a golden summary against ours.
fn compare_summary(
    report: &mut Report,
    what: &str,
    golden: &Value,
    ours: &Summary,
    tolerance: Tolerance,
) {
    report.summarized += 1;
    let near = |mine: f64, theirs: f64| match tolerance {
        Tolerance::Relative(margin) => close(mine, theirs) || (mine - theirs).abs() <= margin,
        Tolerance::Absolute(margin) => {
            (mine.is_nan() && theirs.is_nan()) || (mine - theirs).abs() <= margin
        }
    };
    let whole = matches!(tolerance, Tolerance::Relative(0.0) | Tolerance::Absolute(_));
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
    if whole {
        if let Some(first) = golden["first"].as_array() {
            for (i, item) in first.iter().enumerate() {
                match (json_f64(item), ours.first.get(i)) {
                    (Some(theirs), Some(mine)) if near(*mine, theirs) => {}
                    (theirs, mine) => {
                        report.error(format!("{what}: first[{i}] {mine:?}, golden {theirs:?}"));
                    }
                }
            }
        }
        if let (Some(theirs), Some(mine)) = (json_f64(&golden["last"]), ours.last)
            && !near(mine, theirs)
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

/// What xradar stores for a field of a coarse-range sweep: the field's native
/// gates, padded with its fill to `gates` gates per row. Taken from the view's
/// materialized variable (`values`, `row_len` range gates per row, in the
/// view's ray order): native gate `g` of a row is range gate
/// `start + g * stride`.
fn native_from_view(
    values: &ArrayBuf,
    row_len: usize,
    (native_gates, mapping, fill): (usize, GateMapping, Scalar),
    gates: usize,
) -> Option<ArrayBuf> {
    let stride = mapping.stride.max(1) as usize;
    let start = mapping.start as usize;
    let take = native_gates.min(gates);
    if row_len == 0 || !values.len().is_multiple_of(row_len) {
        return None;
    }
    if take > 0 && start + (take - 1) * stride >= row_len {
        return None;
    }
    let rows = values.len() / row_len;
    macro_rules! native {
        ($values:expr, $fill:expr, $variant:ident) => {{
            let mut out = Vec::with_capacity(rows * gates);
            for row in 0..rows {
                let base = row * row_len;
                for g in 0..take {
                    out.push($values[base + start + g * stride]);
                }
                out.extend(std::iter::repeat_n($fill, gates - take));
            }
            ArrayBuf::$variant(out)
        }};
    }
    Some(match (values, fill) {
        (ArrayBuf::U8(v), Scalar::U8(f)) => native!(v, f, U8),
        (ArrayBuf::U16(v), Scalar::U16(f)) => native!(v, f, U16),
        (ArrayBuf::I8(v), Scalar::I8(f)) => native!(v, f, I8),
        (ArrayBuf::I16(v), Scalar::I16(f)) => native!(v, f, I16),
        (ArrayBuf::I32(v), Scalar::I32(f)) => native!(v, f, I32),
        (ArrayBuf::F32(v), Scalar::F32(f)) => native!(v, f, F32),
        (ArrayBuf::F64(v), Scalar::F64(f)) => native!(v, f, F64),
        _ => return None,
    })
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
    // xradar's range coordinate is not the sweep's (6.2, A.3).
    if name == "range" && coarse_range {
        report.skip("coarse-range");
        return;
    }
    if name == "range" && ctx.negative_first_gate(path) {
        report.skip("message-1-xradar");
        return;
    }
    if name == "range"
        && ctx.cfradial()
        && let Some(array) = &ours.array
    {
        // A CfRadial range the decoder found uniform is regenerated from its
        // first centre and spacing; the file's float32 values can differ in
        // the last bit, so the coordinate compares within the tolerance.
        report.note("cfradial-range");
        compare_summary(
            report,
            &what,
            values,
            &summarize(our_f64s(array).into_iter()),
            Tolerance::Relative(0.0),
        );
        return;
    }
    if name == "sweep_number" && ctx.cfradial() {
        // xradar copies the file's sweep_number (DOW8 trim: 2 for its only
        // sweep); ours numbers the groups it writes.
        report.skip("xradar-ni-and-sweep-numbers");
        return;
    }

    // Attributes: every golden attribute, and the golden encoding's
    // `coordinates`, must be in ours. Ours may add the FM301 attributes.
    // CfRadial coordinate and instrument variables carry the file's own
    // attributes in xradar and the model's typed slots in ours: there `units`
    // compare by meaning and `positive` as text, and only the dataset
    // variables (two dimensions) compare every attribute.
    let is_field = golden_dims.len() == 2;
    let compare_attrs = !ctx.cfradial() || is_field;
    if let Some(attrs) = golden["attrs"].as_object() {
        if !compare_attrs {
            for (key, value) in attrs {
                match key.as_str() {
                    "units" => compare_units(report, &what, name, value, ours.attrs.get("units")),
                    "positive" => match ours.attrs.get("positive") {
                        Some(mine) => compare_attr(report, &what, key, value, mine),
                        None => {
                            report.error(format!("{what}: attribute positive = {value} missing"))
                        }
                    },
                    _ => report.skip("cfradial-verbatim-attrs"),
                }
            }
        } else {
            for (key, value) in attrs {
                if key == "units" && name == "TH" && ctx.source == SourceFormat::OdimH5 {
                    report.skip("odim-th-units");
                    continue;
                }
                match ours.attrs.get(key) {
                    Some(mine) => compare_attr(report, &what, key, value, mine),
                    None => report.error(format!("{what}: attribute {key} = {value} missing")),
                }
            }
            for key in ours.attrs.keys() {
                if attrs.contains_key(key) {
                    continue;
                }
                if key == "units" && name == "time" {
                    // Compared through the decoded values below.
                    continue;
                }
                match ours_only(OURS_ONLY_VAR_ATTRS, key) {
                    Some(listed) => report.note(listed),
                    None => report.error(format!("{what}: attribute {key} is not in the golden")),
                }
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
        report.scalars += 1;
        if same_text(theirs, text) {
            return;
        }
        if matches!(name, "follow_mode" | "prt_mode") && theirs == "not_set" && text == "none" {
            report.note("follow-mode");
            return;
        }
        report.error(format!("{what}: {text:?}, golden {theirs:?}"));
        return;
    }
    if let Some(scalar) = ours.scalar {
        let mine = scalar.as_f64();
        match json_f64(&values["value"]) {
            Some(theirs) if close(mine, theirs) => report.scalars += 1,
            // xradar writes 0 for the missing site location of a Message 1
            // volume; the model keeps None (section 11).
            Some(theirs) if theirs == 0.0 && mine.is_nan() && ctx.message_1 && path == "/" => {
                report.skip("message-1-location");
            }
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
        report.note("calib-squeeze");
        if name == "time" {
            // xradar keeps the calibration time as text; ours is seconds
            // since the reference its `units` names.
            let Some(reference) = seconds_since(ours.attrs.get("units")) else {
                report.error(format!(
                    "{what}: units {:?} is not `seconds since <whole second>`",
                    ours.attrs.get("units")
                ));
                return;
            };
            report.attrs += 1;
            let mine = array
                .get_f64(0)
                .and_then(|s| {
                    reference.checked_add_signed(chrono::Duration::milliseconds(
                        (s * 1000.0).round() as i64,
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
        let theirs = json_f64(&values["value"]).unwrap_or(f64::NAN);
        if theirs.is_nan() {
            // xradar reads only the dataset's how/NI; ours falls back to the
            // root how/NI (odim.rs), which xradar reports as NaN.
            report.skip("xradar-ni-and-sweep-numbers");
            return;
        }
        report.scalars += 1;
        report.note("nyquist-per-ray");
        if !our_f64s(array).iter().all(|mine| close(*mine, theirs)) {
            report.error(format!(
                "{what}: per-ray values differ from golden scalar {theirs}"
            ));
        }
        return;
    }
    if ours_name == "sweep_group_name" {
        // xradar's ODIM sweep_group_name holds sweep indices, and its
        // CfRadial one the file's sweep numbers (DOW8 trim: `sweep_2` for
        // group `sweep_0`); ours names the groups it writes. Only the count
        // compares.
        report.scalars += 1;
        report.note(if ctx.cfradial() {
            "xradar-ni-and-sweep-numbers"
        } else {
            "sweep-group-names"
        });
        let count = values["count"]
            .as_u64()
            .or_else(|| values["values"].as_array().map(|v| v.len() as u64));
        if count != Some(array.len() as u64) {
            report.error(format!("{what}: {} sweeps, golden {count:?}", array.len()));
        }
        return;
    }
    if coarse_range && let Some(native) = native {
        report.note("coarse-range");
        compare_array(report, &what, values, golden_dtype, native, None);
        return;
    }
    // A time coordinate decodes with its own units: the reference the view
    // writes is part of what is compared.
    let time = if golden_dtype == "datetime64[ns]" {
        match seconds_since(ours.attrs.get("units")) {
            Some(reference) => {
                report.attrs += 1;
                Some(reference)
            }
            None => {
                report.error(format!(
                    "{what}: units {:?} is not `seconds since <whole second>`",
                    ours.attrs.get("units")
                ));
                return;
            }
        }
    } else {
        None
    };
    compare_array(report, &what, values, golden_dtype, array, time);
}

/// Unit spellings with one meaning, keyed by the lowercase spelling.
fn canonical_unit(units: &str) -> String {
    let lower = units.trim().to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "" | "1" | "unitless" | "count" | "none" | "-" => "1",
        "s" | "sec" | "second" | "seconds" => "s",
        "db" => "dB",
        "dbm" => "dBm",
        "dbz" => "dBZ",
        "deg" | "degree" | "degrees" => "degree",
        "m" | "meter" | "meters" | "metre" | "metres" => "m",
        "m s-1" | "m/s" | "meters per second" | "meters_per_second" | "meters per seconds" => {
            "m s-1"
        }
        "degrees/s" | "deg/s" | "degree s-1" | "degrees per second" | "degrees_per_second" => {
            "degree s-1"
        }
        "s-1" | "hz" => "s-1",
        _ => return units.trim().to_owned(),
    };
    canonical.to_owned()
}

/// A golden `units` attribute of a CfRadial coordinate or instrument
/// variable against ours, by meaning: an empty or dimensionless golden unit
/// also matches no attribute.
fn compare_units(
    report: &mut Report,
    what: &str,
    name: &str,
    golden: &Value,
    ours: Option<&AttrValue>,
) {
    let Some(theirs) = golden.as_str() else {
        report.error(format!("{what}: golden units {golden} is not text"));
        return;
    };
    report.attrs += 1;
    let mine = ours.and_then(attr_text);
    let (theirs_canonical, mine_canonical) =
        (canonical_unit(theirs), mine.as_deref().map(canonical_unit));
    match mine_canonical {
        None if theirs_canonical == "1" => {}
        Some(m) if m == theirs_canonical => {
            if mine.as_deref() != Some(theirs) {
                report.note("units-spelling");
            }
        }
        Some(m) if name == "prt_ratio" && theirs_canonical == "s" && m == "1" => {
            report.note("cfradial-prt-ratio-units");
        }
        _ => report.error(format!("{what}: units {mine:?}, golden {theirs:?}")),
    }
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
            if empty {
                report.skip("empty-groups");
            } else if all_fill {
                report.skip("calibration-fill");
            } else {
                report.error(format!("{what}: group missing"));
            }
            continue;
        };
        let coarse_range = coarse_xradar_range(ctx, golden_group, mine);
        // Dimensions.
        if let Some(dims) = golden_group["dims"].as_object() {
            for (dim, len) in dims {
                let len = len.as_u64().unwrap() as usize;
                if dim == "frequency" && path == "/" {
                    // FM301 puts frequency in every sweep (section 1).
                    report.note("cfradial-frequency");
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
                    report.note("platform-track");
                    let rays: usize = volume.sweeps.iter().map(Sweep::nrays).sum();
                    if rays != len {
                        report.error(format!("{what}: root time dim {rays} rays, golden {len}"));
                    }
                    continue;
                }
                if dim == "range" && coarse_range {
                    report.skip("coarse-range");
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
                report.note("platform-track");
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
                report.note("cfradial-frequency");
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
                if fill.is_some() && value == fill {
                    report.skip("calibration-fill");
                    continue;
                }
                if empty_text {
                    report.skip("xradar-none-text");
                    continue;
                }
                report.error(format!("{what}/{name}: variable missing"));
                continue;
            };
            // Native gates of a field on a coarse-range sweep, padded like
            // xradar, from the view's materialized variable (its ray order,
            // gate offset and stride).
            let native = match (coarse_range, golden_gates, &my_var.array) {
                (true, Some(gates), Some(array)) if golden_dims.len() == 2 => {
                    let row_len = mine.dims.get("range").copied().unwrap_or(0);
                    // A borrowed variable is its own native gates.
                    let mapping = my_var.mapping.or_else(|| {
                        let fill = match my_var.attrs.get("_FillValue") {
                            Some(AttrValue::Scalar(fill)) => *fill,
                            _ => return None,
                        };
                        Some((row_len, GateMapping::IDENTITY, fill))
                    });
                    let native = mapping
                        .and_then(|mapping| native_from_view(array, row_len, mapping, gates));
                    if native.is_none() {
                        report.error(format!(
                            "{what}/{name}: the view's variable does not hold its native gates"
                        ));
                        continue;
                    }
                    native
                }
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
            if seen.contains(name) {
                continue;
            }
            if let Some(key) = ours_only(OURS_ONLY_VARIABLES, name) {
                report.note(key);
                continue;
            }
            if path == "/" && name == "frequency" {
                report.note("cfradial-frequency");
                continue;
            }
            // xradar emits no spectrum width for Message 1 volumes (A.3).
            if ctx.message_1 && name == "WRADH" {
                report.note("message-1-xradar");
                continue;
            }
            // The platform track and georeference variables of a moving
            // platform (xradar: root coordinates; FM301: per sweep).
            if has_platform_track(volume) && PLATFORM_TRACK.contains(&name.as_str()) {
                report.note("platform-track");
                continue;
            }
            // Site parameters xradar leaves out for NEXRAD and ODIM.
            let site_source = matches!(
                ctx.source,
                SourceFormat::NexradLevel2 | SourceFormat::OdimH5
            );
            let site_group = matches!(path, "/radar_parameters" | "/radar_calibration");
            let odim_ray_constants = ctx.source == SourceFormat::OdimH5
                && matches!(name.as_str(), "pulse_width" | "r_calib_index");
            if (site_source && site_group) || odim_ray_constants {
                report.note("site-parameters");
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

/// Physical value of element `index` of an encoded array under `coding`,
/// NaN for every sentinel; `None` when the types disagree.
fn decode_value(values: &ArrayBuf, index: usize, coding: Coding) -> Option<f32> {
    let gate = match (values, coding) {
        (ArrayBuf::U8(v), Coding::U8(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::U16(v), Coding::U16(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::I8(v), Coding::I8(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::I16(v), Coding::I16(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::I32(v), Coding::I32(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::F32(v), Coding::F32(c)) => c.resolve(*v.get(index)?),
        (ArrayBuf::F64(v), Coding::F64(c)) => c.resolve(*v.get(index)?),
        _ => return None,
    };
    Some(gate.value().unwrap_or(f32::NAN))
}

/// A dataset variable of the view, materialized (ray order, padding and gate
/// repetition applied by the view), decoded with the field's `coding`, and put
/// back in storage ray order, which is Py-ART's: `(values, range gates per
/// ray)`.
fn view_field_physical(
    variable: &Variable<'_>,
    coding: Coding,
    nrays: usize,
) -> Option<(Vec<f32>, usize)> {
    let encoded = variable.values.materialize()?;
    if nrays == 0 || encoded.len() % nrays != 0 {
        return None;
    }
    let gates = encoded.len() / nrays;
    let order = match &variable.values {
        Values::Mapped { rows, .. } => rows.clone(),
        Values::Borrowed(_) => RowOrder::Identity,
        _ => return None,
    };
    let mut out = vec![f32::NAN; encoded.len()];
    let mut seen = vec![false; nrays];
    for position in 0..nrays {
        let row = order.source_row(position)?;
        if row >= nrays || std::mem::replace(&mut seen[row], true) {
            return None;
        }
        for gate in 0..gates {
            out[row * gates + gate] = decode_value(&encoded, position * gates + gate, coding)?;
        }
    }
    Some((out, gates))
}

/// First centre and spacing of a view sweep's `range` coordinate, from its
/// attributes (or its first two values when the spacing is not constant).
fn view_range(group: &Group<'_>) -> Option<(f64, f64)> {
    let range = group.variable("range")?;
    let attr = |name: &str| match range.attr(name) {
        Some(AttrValue::Scalar(value)) => Some(value.as_f64()),
        _ => None,
    };
    let centres = range.values.materialize()?;
    let first = attr("meters_to_center_of_first_gate").or_else(|| centres.get_f64(0))?;
    let spacing = attr("meters_between_gates")
        .or_else(|| Some(centres.get_f64(1)? - centres.get_f64(0)?))
        .unwrap_or(0.0);
    Some((first, spacing))
}

/// A view sweep's values (`gates` range gates per ray, centres
/// `first + j * spacing`) laid out on Py-ART's volume range (`r0`, `dr`,
/// `ngates`): each range gate repeated `spacing / dr` times from the Py-ART
/// gate its leading edge falls in, NaN outside (`tools/fm301_golden.py`,
/// `verify_nexrad`). `None` when the sweep range does not sit on Py-ART's.
fn pyart_layout(
    values: &[f32],
    gates: usize,
    (first, spacing): (f64, f64),
    (r0, dr, ngates): (f64, f64, usize),
) -> Option<Vec<f32>> {
    if dr <= 0.0 || gates == 0 {
        return None;
    }
    let k = (spacing / dr).round();
    let m = ((first - spacing / 2.0) - (r0 - dr / 2.0)) / dr;
    if k < 1.0 || (k * dr - spacing).abs() > 1e-3 * dr || (m - m.round()).abs() > 1e-3 {
        return None;
    }
    let (k, m) = (k as usize, m.round() as i64);
    let rows = values.len() / gates;
    Some(
        (0..rows)
            .flat_map(|row| {
                (0..ngates).map(move |index| {
                    let offset = index as i64 - m;
                    if offset < 0 {
                        return f32::NAN;
                    }
                    let gate = offset as usize / k;
                    if gate < gates {
                        values[row * gates + gate]
                    } else {
                        f32::NAN
                    }
                })
            })
            .collect(),
    )
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
    // Fields and the time reference are read through the FM301 view, in
    // acquisition order (NEXRAD and CfRadial storage order; ODIM rows go back
    // to storage order, which is Py-ART's).
    let options = ViewOptions {
        flavor: Flavor::Xradar012,
        first_dim: FirstDim::Time,
        passthrough: Passthrough::Flavor,
    };
    let view = fm301::volume_view(volume, options, None).unwrap();
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
        match (mine, theirs) {
            (Some(mine), Some(theirs)) if close(mine, theirs) => report.scalars += 1,
            // Py-ART writes 0 for an unknown Message 1 site.
            (None, Some(0.0)) if message_1 => report.skip("message-1-location"),
            (mine, theirs) => {
                report.error(format!("{id} pyart: {name} {mine:?}, golden {theirs:?}"))
            }
        }
    }
    // Range: Py-ART's volume range and whether ours fits it.
    let (r0, dr, ngates) = pyart_range(radar);
    if odim {
        report.note("pyart-odim-first-gate");
    }
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
        report.note("pyart-odim-rstart");
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
    // Py-ART's reference is the first ray's time floored to the second (the
    // file's for CfRadial, the first dataset's start for ODIM), which is what
    // the view's `time.units` must name.
    let pyart_units = radar["variables"]["time"]["meta"]["units"].as_str();
    let view_units = view
        .group("sweep_0")
        .and_then(|group| group.variable("time"))
        .and_then(|time| time.attr("units"))
        .and_then(attr_text);
    report.attrs += 1;
    let time_reference = pyart_units
        .and_then(|units| seconds_since(Some(&AttrValue::Text(units.into()))))
        .unwrap_or(volume.time_reference);
    let view_reference = view
        .group("sweep_0")
        .and_then(|group| group.variable("time"))
        .and_then(|time| seconds_since(time.attr("units")))
        .unwrap_or(volume.time_reference);
    if pyart_units.is_none() || view_units.as_deref() != pyart_units {
        if odim && view_reference < time_reference {
            report.note("pyart-odim-time-reference");
        } else {
            report.error(format!(
                "{id} pyart: time units {view_units:?}, golden {pyart_units:?}"
            ));
        }
    }
    // Whatever the convention, the reference is at or before every ray.
    let earliest = volume
        .sweeps
        .iter()
        .flat_map(|sweep| sweep.rays.time_s.iter().copied())
        .filter(|time| time.is_finite())
        .fold(f64::INFINITY, f64::min);
    if earliest < 0.0 {
        report.error(format!(
            "{id}: a ray is {earliest} s before the time reference {view_reference}"
        ));
    }
    let shift = (view_reference - time_reference).num_milliseconds() as f64 / 1000.0;
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
                report.note("pyart-message-1-fixed-angle");
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
                report.note("pyart-odim-rays");
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
                    Tolerance::Relative(1e-2),
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
                report.note("pyart-odim-rays");
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
                    Tolerance::Relative(0.05),
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
            let absolute = if odim {
                report.note("pyart-odim-rays");
                1.0
            } else {
                1e-3
            };
            compare_summary(
                &mut report,
                &format!("{what}/time"),
                golden,
                &summarize(seconds.iter().copied()),
                Tolerance::Relative(absolute),
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
                        report.skip("pyart-zero-nyquist");
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
                    Tolerance::Relative(0.0),
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
                    report.skip("pyart-padded-fields");
                    continue;
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
            let group = view.group(&format!("sweep_{index}"));
            let variable = group.and_then(|group| group.variable(field.name.as_str()));
            let (Some(group), Some(variable)) = (group, variable) else {
                report.error(format!("{what}: no view variable {}", field.name));
                continue;
            };
            let Some((physical, gates)) =
                view_field_physical(variable, field.data.coding(), sweep.nrays())
            else {
                report.error(format!("{what}: the view's variable does not materialize"));
                continue;
            };
            let unmasked = golden["count_unmasked"].as_u64().unwrap_or(0) as usize;
            let hash = golden["sha256"].as_str();
            if range_matches {
                let laid_out = view_range(group)
                    .and_then(|range| pyart_layout(&physical, gates, range, (r0, dr, ngates)));
                match laid_out {
                    Some(values) if Some(sha256_f32(&values).as_str()) == hash => {
                        report.hashed += 1;
                        if field.gates.stride > 1 {
                            report.strided += 1;
                        }
                    }
                    Some(values) => report.error(format!(
                        "{what}: sha256 {} != golden {} ({} valid gates, golden {unmasked})",
                        sha256_f32(&values),
                        hash.unwrap_or("?"),
                        values.iter().filter(|v| v.is_finite()).count()
                    )),
                    None => report.error(format!(
                        "{what}: the view's range {:?} does not sit on Py-ART's ({r0}, {dr})",
                        view_range(group)
                    )),
                }
                continue;
            }
            // Py-ART misread the range (espdg, listed): the values only.
            report.note("pyart-odim-rstart");
            let summary = summarize(physical.iter().map(|v| f64::from(*v)));
            report.summarized += 1;
            if summary.finite != unmasked {
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
    let mut skipped = Vec::new();
    let mut used = BTreeSet::new();
    let mut strided = 0usize;
    for case in &cases {
        // `None` only when the file is not committed, not cached, and cannot
        // be downloaded.
        let Some(decoded) = decode(case) else {
            skipped.push(case.id.as_str());
            continue;
        };
        let mut reports = Vec::new();
        if let Some(golden) = &case.xradar {
            reports.push(("xradar", check_xradar(case, &decoded, golden)));
        }
        if let Some(golden) = &case.pyart {
            reports.push(("pyart", check_pyart(case, &decoded, golden)));
        }
        for (side, report) in reports {
            eprintln!(
                "{} {side}: {} hashes ({} of repeated gates), {} summaries, {} scalars, {} attributes compared; {} known differences; {} errors",
                case.id,
                report.hashed,
                report.strided,
                report.summarized,
                report.scalars,
                report.attrs,
                report.known,
                report.errors.len()
            );
            strided += report.strided;
            used.extend(report.used.iter().copied());
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
    assert!(failures.is_empty(), "\n{failures}");
    // A case whose file cannot be fetched is skipped only when offline runs
    // were asked for; otherwise the acceptance test would pass on the six
    // committed files alone.
    if !skipped.is_empty() {
        assert!(
            offline_requested(),
            "{} of {} cases have no file (not cached, download failed): {skipped:?}; set {}=1 to run the available cases only",
            skipped.len(),
            cases.len(),
            recast_radar_testdata::OFFLINE_ENV
        );
        eprintln!(
            "{} of {} cases skipped offline ({skipped:?}); the EXPECTED coverage check needs all of them",
            skipped.len(),
            cases.len()
        );
        return;
    }
    let unused: Vec<&str> = EXPECTED
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| !used.contains(key))
        .collect();
    assert!(
        unused.is_empty(),
        "EXPECTED differences no case applies: {unused:?}"
    );
    // The Message 1 and legacy-resolution cases repeat 1 km reflectivity over
    // 250 m gates: the view's gate repetition is compared with Py-ART.
    assert!(strided > 0, "no Py-ART hash went through a repeated field");
}

/// `RECAST_RADAR_TESTDATA_OFFLINE` is set (as `recast-radar-testdata` reads
/// it).
fn offline_requested() -> bool {
    std::env::var_os(recast_radar_testdata::OFFLINE_ENV)
        .is_some_and(|value| !value.is_empty() && value != "0")
}

#[test]
fn expected_differences_are_listed() {
    // Keys and descriptions are unique and every description cites its source.
    let keys: BTreeSet<&str> = EXPECTED.iter().map(|(key, _)| *key).collect();
    let texts: BTreeSet<&str> = EXPECTED.iter().map(|(_, text)| *text).collect();
    assert_eq!(keys.len(), EXPECTED.len());
    assert_eq!(texts.len(), EXPECTED.len());
    for (key, text) in EXPECTED {
        assert!(
            text.contains("section") || text.contains('(') || text.contains("README"),
            "{key}: no source cited"
        );
    }
}
