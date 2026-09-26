//! Every DORADE descriptor and ray block value reaches the model and the
//! FM301 view.
//!
//! Each committed sweepfile is decoded, and every value is read twice: from
//! the model (`Sweep::other`, `FieldAttrs::other`, `Sweep::extra_vars`,
//! `RayVariables`, `Sweep::platform_track`) and from the FM301 view with
//! every passthrough item (`Passthrough::All`). Both must equal what this
//! test reads from the file bytes with its own block walker and the
//! structure layouts of the DORADE format document (R. Oye and M. Case,
//! NCAR/ATD 1995, revised by W.-C. Lee; lrose-core `DoradeData.hh`):
//! SSWB, VOLD, RADD, CFAC, CSFD, CELV, SWIB, COMM, SEDS, every PARM, and
//! the RYIB and ASIB of every ray (antenna-transition rays included).
//!
//! The two NOAA P-3 N42RF tail radar sweeps are airborne: their ASIB blocks
//! hold every platform velocity, attitude, wind and change rate, where the
//! ground-based files store missing values. The full N42RF sweepfiles
//! (download entries, skipped offline) are checked too, with the SEDS block
//! that ends them.
//!
//! The layout tables here are this test's own reading of the format
//! document, so the descriptors are also checked against an independent
//! reader: `descriptors_match_radxprint_native` compares every SSWB, VOLD,
//! RADD, CFAC, CSFD, CELV, SWIB and PARM member with what LROSE
//! `RadxPrint -native` prints for each committed sweepfile
//! (`testdata/golden/dorade/radxprint_native.json`, from
//! `tools/dorade_radx_native_golden.py`).
//!
//! The RADD `radar_type` gives `platform_type` and `platform_is_mobile`, and
//! the ten CFAC corrections the decoder does not apply (pressure altitude
//! through tilt, offsets 28 and 36-71) are the volume's
//! `georeferencing_correction`, in the model and the view.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_core::fm301::{
    self, FirstDim, Flavor, Passthrough, Values, ViewOptions, VolumeView,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, FieldName, PlatformType, Scalar, Sweep, Volume,
};
use recast_radar_io_dorade::{read_dorade_sweep_volume, read_dorade_volume_from_slices};

/// Every committed DORADE sweepfile.
const SWEEPFILES: &[&str] = &[
    "dorade-cow2-20260521-225514-sur-head24",
    "dorade-noxp-20090501-190244-ppi",
    "dorade-noxp-20090501-190324-ppi",
    "dorade-noxp-20090525-203211-sector",
    "dorade-dow6-20211230-222139-rhi-head41",
    "dorade-noxp-20090610-003210-ppi-head6",
    "dorade-noxp-20090610-003222-ppi-head6",
    "dorade-noxp-20090610-003226-ppi-head6",
    "dorade-n42rf-ts-20181010-122951-air-head24",
    "dorade-n42rf-tm-20181010-123925-air-head48",
];

/// The full N42RF sweepfiles the airborne head trims come from (download
/// entries).
const FULL_AIRBORNE: &[&str] = &[
    "dorade-n42rf-ts-20181010-122951-air",
    "dorade-n42rf-tm-20181010-123925-air",
];

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

#[derive(Clone, Copy)]
enum Kind {
    I16,
    I32,
    F32,
    F64,
    Text(usize),
    F32s(usize),
    I16s(usize),
    I32s(usize),
}

use Kind::*;

/// SSWB of the 196-byte (packed) layout; the 200-byte layout moves every
/// field from `d_start_time` on 4 bytes further.
const SSWB: &[(&str, usize, Kind)] = &[
    ("last_used", 8, I32),
    ("start_time", 12, I32),
    ("stop_time", 16, I32),
    ("sizeof_file", 20, I32),
    ("compression_flag", 24, I32),
    ("volume_time_stamp", 28, I32),
    ("num_params", 32, I32),
    ("radar_name", 36, Text(8)),
    ("d_start_time", 44, F64),
    ("d_stop_time", 52, F64),
    ("version_num", 60, I32),
    ("num_key_tables", 64, I32),
    ("status", 68, I32),
    ("place_holder", 72, I32s(7)),
    ("key_table", 100, I32s(24)),
];

const VOLD: &[(&str, usize, Kind)] = &[
    ("format_version", 8, I16),
    ("volume_num", 10, I16),
    ("maximum_bytes", 12, I32),
    ("proj_name", 16, Text(20)),
    ("year", 36, I16),
    ("month", 38, I16),
    ("day", 40, I16),
    ("data_set_hour", 42, I16),
    ("data_set_minute", 44, I16),
    ("data_set_second", 46, I16),
    ("flight_num", 48, Text(8)),
    ("gen_facility", 56, Text(8)),
    ("gen_year", 64, I16),
    ("gen_month", 66, I16),
    ("gen_day", 68, I16),
    ("number_sensor_des", 70, I16),
];

const RADD: &[(&str, usize, Kind)] = &[
    ("radar_name", 8, Text(8)),
    ("radar_const_db", 16, F32),
    ("peak_power_kw", 20, F32),
    ("noise_power_dbm", 24, F32),
    ("receiver_gain_db", 28, F32),
    ("antenna_gain_db", 32, F32),
    ("system_gain_db", 36, F32),
    ("horz_beam_width_deg", 40, F32),
    ("vert_beam_width_deg", 44, F32),
    ("radar_type", 48, I16),
    ("scan_mode", 50, I16),
    ("req_rotat_vel_deg_per_s", 52, F32),
    ("scan_mode_pram0", 56, F32),
    ("scan_mode_pram1", 60, F32),
    ("num_parameter_des", 64, I16),
    ("total_num_des", 66, I16),
    ("data_compress", 68, I16),
    ("data_reduction", 70, I16),
    ("data_red_parm0", 72, F32),
    ("data_red_parm1", 76, F32),
    ("radar_longitude_deg", 80, F32),
    ("radar_latitude_deg", 84, F32),
    ("radar_altitude_km", 88, F32),
    ("eff_unamb_vel_mps", 92, F32),
    ("eff_unamb_range_km", 96, F32),
    ("num_freq_trans", 100, I16),
    ("num_ipps_trans", 102, I16),
    ("freq_ghz", 104, F32s(5)),
    ("interpulse_per_ms", 124, F32s(5)),
    ("extension_num", 144, I32),
    ("config_name", 148, Text(8)),
    ("config_num", 156, I32),
    ("aperture_size_cm", 160, F32),
    ("field_of_view", 164, F32),
    ("aperture_eff_percent", 168, F32),
    ("aux_freq_ghz", 172, F32s(11)),
    ("aux_ipp_ms", 216, F32s(11)),
    ("pulse_width_us", 260, F32),
    ("primary_cop_baseln", 264, F32),
    ("secondary_cop_baseln", 268, F32),
    ("pc_xmtr_bandwidth", 272, F32),
    ("pc_waveform_type", 276, I32),
    ("site_name", 280, Text(20)),
];

const CFAC: &[(&str, usize, Kind)] = &[
    ("azimuth_corr_deg", 8, F32),
    ("elevation_corr_deg", 12, F32),
    ("range_delay_corr_m", 16, F32),
    ("longitude_corr_deg", 20, F32),
    ("latitude_corr_deg", 24, F32),
    ("pressure_alt_corr_km", 28, F32),
    ("radar_alt_corr_km", 32, F32),
    ("ew_gndspd_corr_mps", 36, F32),
    ("ns_gndspd_corr_mps", 40, F32),
    ("vert_vel_corr_mps", 44, F32),
    ("heading_corr_deg", 48, F32),
    ("roll_corr_deg", 52, F32),
    ("pitch_corr_deg", 56, F32),
    ("drift_corr_deg", 60, F32),
    ("rot_angle_corr_deg", 64, F32),
    ("tilt_corr_deg", 68, F32),
];

const CSFD: &[(&str, usize, Kind)] = &[
    ("num_segments", 8, I32),
    ("dist_to_first_m", 12, F32),
    ("spacing_m", 16, F32s(8)),
    ("num_cells", 48, I16s(8)),
];

const CELV: &[(&str, usize, Kind)] = &[("number_cells", 8, I32)];

const SWIB: &[(&str, usize, Kind)] = &[
    ("radar_name", 8, Text(8)),
    ("sweep_num", 16, I32),
    ("num_rays", 20, I32),
    ("start_angle_deg", 24, F32),
    ("stop_angle_deg", 28, F32),
    ("fixed_angle_deg", 32, F32),
    ("filter_flag", 36, I32),
];

/// PARM fields after the name, description and units, which the field
/// carries as its name, `long_name` and `units`.
const PARM: &[(&str, usize, Kind)] = &[
    ("interpulse_time", 64, I16),
    ("xmitted_freq", 66, I16),
    ("recvr_bandwidth_mhz", 68, F32),
    ("pulse_width_m", 72, I16),
    ("polarization", 74, I16),
    ("num_samples", 76, I16),
    ("binary_format", 78, I16),
    ("threshold_field", 80, Text(8)),
    ("threshold_value", 88, F32),
    ("parameter_scale", 92, F32),
    ("parameter_bias", 96, F32),
    ("bad_data", 100, I32),
    ("extension_num", 104, I32),
    ("config_name", 108, Text(8)),
    ("config_num", 116, I32),
    ("offset_to_data", 120, I32),
    ("mks_conversion", 124, F32),
    ("num_qnames", 128, I32),
    ("qdata_names", 132, Text(32)),
    ("num_criteria", 164, I32),
    ("criteria_names", 168, Text(32)),
    ("number_cells", 200, I32),
    ("meters_to_first_cell", 204, F32),
    ("meters_between_cells", 208, F32),
    ("eff_unamb_vel_mps", 212, F32),
];

#[derive(Clone, Copy)]
struct Endian(bool);

impl Endian {
    fn bytes<const N: usize>(self, block: &[u8], at: usize) -> [u8; N] {
        let mut raw: [u8; N] = block[at..at + N].try_into().unwrap();
        if !self.0 {
            raw.reverse();
        }
        raw
    }
    fn i16(self, block: &[u8], at: usize) -> i16 {
        i16::from_be_bytes(self.bytes(block, at))
    }
    fn i32(self, block: &[u8], at: usize) -> i32 {
        i32::from_be_bytes(self.bytes(block, at))
    }
    fn f32(self, block: &[u8], at: usize) -> f32 {
        f32::from_be_bytes(self.bytes(block, at))
    }
    fn f64(self, block: &[u8], at: usize) -> f64 {
        f64::from_be_bytes(self.bytes(block, at))
    }
}

/// Every block of a sweepfile as `(identifier, bytes)`, in file order.
type Blocks = Vec<([u8; 4], Vec<u8>)>;

/// Block walker over the real sweepfile `id`: the byte order from the first
/// block's length, then every `(id, block)` up to the first block that does
/// not fit.
fn blocks(id: &str) -> (Endian, Blocks) {
    let bytes = recast_radar_testdata::bytes(id).unwrap();
    let big = i32::from_be_bytes(bytes[4..8].try_into().unwrap());
    let little = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let fits = |n: i32| n >= 8 && (n as usize) <= bytes.len();
    let endian = Endian(fits(big) || !fits(little));
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= bytes.len() {
        let size = endian.i32(&bytes, pos + 4);
        if size < 8 || pos + size as usize > bytes.len() {
            break;
        }
        let block_id: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap();
        out.push((block_id, bytes[pos..pos + size as usize].to_vec()));
        pos += size as usize;
    }
    (endian, out)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

fn read(endian: Endian, block: &[u8], at: usize, kind: Kind) -> Option<AttrValue> {
    let size = match kind {
        I16 => 2,
        I32 | F32 => 4,
        F64 => 8,
        Text(len) => len,
        F32s(n) | I32s(n) => 4 * n,
        I16s(n) => 2 * n,
    };
    if at + size > block.len() {
        return None;
    }
    Some(match kind {
        I16 => AttrValue::Scalar(Scalar::I16(endian.i16(block, at))),
        I32 => AttrValue::Scalar(Scalar::I32(endian.i32(block, at))),
        F32 => AttrValue::Scalar(Scalar::F32(endian.f32(block, at))),
        F64 => AttrValue::Scalar(Scalar::F64(endian.f64(block, at))),
        Text(len) => AttrValue::Text(text(&block[at..at + len]).into()),
        F32s(n) => AttrValue::Array(ArrayBuf::F32(
            (0..n).map(|i| endian.f32(block, at + 4 * i)).collect(),
        )),
        I16s(n) => AttrValue::Array(ArrayBuf::I16(
            (0..n).map(|i| endian.i16(block, at + 2 * i)).collect(),
        )),
        I32s(n) => AttrValue::Array(ArrayBuf::I32(
            (0..n).map(|i| endian.i32(block, at + 4 * i)).collect(),
        )),
    })
}

/// Equality with floats compared bit for bit (NaN sentinels included).
fn same(a: &AttrValue, b: &AttrValue) -> bool {
    match (a, b) {
        (AttrValue::Scalar(x), AttrValue::Scalar(y)) => x.bit_eq(*y),
        (AttrValue::Array(ArrayBuf::F32(x)), AttrValue::Array(ArrayBuf::F32(y))) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits())
        }
        (a, b) => a == b,
    }
}

fn lookup<'a>(attrs: &'a [(Box<str>, AttrValue)], name: &str) -> Option<&'a AttrValue> {
    attrs.iter().find(|(key, _)| &**key == name).map(|(_, v)| v)
}

/// One sweep's descriptor attributes against the sweepfile; returns the
/// number of values compared.
fn check_descriptors(id: &str, sweep: &Sweep, view: &VolumeView<'_>, index: usize) -> usize {
    let (endian, blocks) = blocks(id);
    let group = view.group(&format!("sweep_{index}")).unwrap();
    let mut compared = 0;
    let mut names = Vec::new();
    let mut comments = 0;
    let mut edit_summaries = 0;
    for (block_id, block) in &blocks {
        let (prefix, table): (&str, &[(&str, usize, Kind)]) = match block_id {
            b"SSWB" => ("sswb", SSWB),
            b"VOLD" => ("vold", VOLD),
            b"RADD" => ("radd", RADD),
            b"CFAC" => ("cfac", CFAC),
            b"CSFD" => ("csfd", CSFD),
            b"CELV" => ("celv", CELV),
            b"SWIB" => ("swib", SWIB),
            b"COMM" => {
                let expected = AttrValue::Text(text(&block[8..]).into());
                let name = if comments == 0 {
                    "dorade_comm_comment".to_owned()
                } else {
                    format!("dorade_comm_comment_{comments}")
                };
                comments += 1;
                assert_eq!(lookup(&sweep.other, &name), Some(&expected), "{id} {name}");
                assert_eq!(group.attr(&name), Some(&expected), "{id} {name} view");
                names.push(name);
                compared += 1;
                continue;
            }
            b"SEDS" => {
                // As stored, less the block's NUL padding.
                let stored = String::from_utf8_lossy(&block[8..]);
                let expected = AttrValue::Text(stored.trim_end_matches('\0').into());
                let name = if edit_summaries == 0 {
                    "dorade_seds_text".to_owned()
                } else {
                    format!("dorade_seds_text_{edit_summaries}")
                };
                edit_summaries += 1;
                assert_eq!(lookup(&sweep.other, &name), Some(&expected), "{id} {name}");
                assert_eq!(group.attr(&name), Some(&expected), "{id} {name} view");
                names.push(name);
                compared += 1;
                continue;
            }
            _ => continue,
        };
        for &(field, offset, kind) in table {
            // The 200-byte SSWB aligns d_start_time to 8 bytes.
            let offset = if prefix == "sswb" && block.len() >= 200 && offset >= 44 {
                offset + 4
            } else {
                offset
            };
            let name = format!("dorade_{prefix}_{field}");
            let expected = read(endian, block, offset, kind);
            let model = lookup(&sweep.other, &name);
            let viewed = group.attr(&name);
            match expected {
                Some(expected) => {
                    let model = model.unwrap_or_else(|| panic!("{id}: {name} missing"));
                    assert!(
                        same(model, &expected),
                        "{id} {name}: {model:?} != {expected:?}"
                    );
                    assert!(same(viewed.unwrap(), &expected), "{id} {name} view");
                    compared += 1;
                }
                None => assert!(model.is_none(), "{id} {name} past the block end"),
            }
            names.push(name);
        }
        if block_id == b"PARM" {
            unreachable!();
        }
    }
    // Every `dorade_*` sweep attribute is one this test read.
    for (name, _) in &sweep.other {
        if name.starts_with("dorade_") {
            assert!(names.iter().any(|n| n == &**name), "{id}: unchecked {name}");
        }
    }
    // PARM fields on each field.
    for (block_id, block) in &blocks {
        if block_id != b"PARM" {
            continue;
        }
        let parameter = text(&block[8..16]);
        let field = sweep
            .field(&FieldName::parse(&parameter))
            .unwrap_or_else(|| panic!("{id}: no field {parameter}"));
        let variable = group.variable(&parameter).unwrap();
        for &(name, offset, kind) in PARM {
            let name = format!("dorade_parm_{name}");
            let expected = read(endian, block, offset, kind);
            let model = lookup(&field.attrs.other, &name);
            match expected {
                Some(expected) => {
                    assert!(same(model.unwrap(), &expected), "{id} {parameter} {name}");
                    assert!(
                        same(variable.attr(&name).unwrap(), &expected),
                        "{id} {name} view"
                    );
                    compared += 1;
                }
                None => assert!(model.is_none(), "{id} {parameter} {name}"),
            }
        }
    }
    compared
}

/// A DORADE float that is not a missing-value sentinel, else NaN.
fn present(value: f32) -> f32 {
    if value.is_finite() && value > -999.0 {
        value
    } else {
        f32::NAN
    }
}

fn same_f64(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// A per-ray variable of the view, in storage order (the view orders rays
/// by time).
fn view_column(
    view: &VolumeView<'_>,
    sweep: &Sweep,
    index: usize,
    path: &str,
    name: &str,
) -> Option<Vec<f64>> {
    let group = view.group(&format!("sweep_{index}{path}"))?;
    let variable = group.variable(name)?;
    let values = variable.values.materialize()?;
    let mut order: Vec<usize> = (0..sweep.nrays()).collect();
    order.sort_by(|a, b| sweep.rays.time_s[*a].total_cmp(&sweep.rays.time_s[*b]));
    let mut out = vec![f64::NAN; order.len()];
    for (position, ray) in order.iter().enumerate() {
        out[*ray] = values.get_f64(position).unwrap();
    }
    Some(out)
}

/// Every RYIB and ASIB value of the kept rays; returns the rays compared.
fn check_rays(id: &str, sweep: &Sweep, view: &VolumeView<'_>, index: usize) -> usize {
    let (endian, blocks) = blocks(id);
    // (ryib, asib) per ray, in file order.
    let mut rays: Vec<(&[u8], Option<&[u8]>)> = Vec::new();
    for (block_id, block) in &blocks {
        match block_id {
            b"RYIB" => rays.push((block.as_slice(), None)),
            b"ASIB" => {
                if let Some(last) = rays.last_mut() {
                    last.1 = Some(block.as_slice());
                }
            }
            _ => {}
        }
    }
    let status = |ryib: &[u8]| endian.i32(ryib, 40);
    // Every ray is kept, in file order; the transition rays are flagged.
    let kept = rays;
    assert_eq!(kept.len(), sweep.nrays(), "{id}: rays");

    let extra = |name: &str| {
        sweep.extra_vars.iter().find(|v| &*v.name == name).map(|v| {
            (0..v.values.len())
                .map(|i| v.values.get_f64(i).unwrap())
                .collect::<Vec<_>>()
        })
    };
    let column = |path: &str, name: &str| view_column(view, sweep, index, path, name);

    // RYIB: ray status, sweep number, true scan rate.
    let expected_status: Vec<f64> = kept.iter().map(|(r, _)| f64::from(status(r))).collect();
    assert_eq!(
        extra("dorade_ryib_ray_status").unwrap(),
        expected_status,
        "{id}"
    );
    assert_eq!(
        column("", "dorade_ryib_ray_status").unwrap(),
        expected_status
    );
    let expected_sweep: Vec<f64> = kept
        .iter()
        .map(|(r, _)| f64::from(endian.i32(r, 8)))
        .collect();
    assert_eq!(
        extra("dorade_ryib_sweep_num").unwrap(),
        expected_sweep,
        "{id}"
    );
    let transition: Vec<f64> = expected_status
        .iter()
        .map(|s| f64::from(u8::from(*s == 1.0)))
        .collect();
    let model_transition: Vec<f64> = sweep
        .ray_vars
        .antenna_transition
        .as_ref()
        .unwrap()
        .iter()
        .map(|v| f64::from(*v))
        .collect();
    assert_eq!(model_transition, transition, "{id}");
    assert_eq!(
        column("", "antenna_transition").unwrap(),
        transition,
        "{id}"
    );
    let rates: Vec<f32> = kept
        .iter()
        .map(|(r, _)| present(endian.f32(r, 36)))
        .collect();
    match &sweep.ray_vars.scan_rate_deg_per_s {
        Some(model) => {
            assert!(
                model
                    .iter()
                    .zip(&rates)
                    .all(|(a, b)| same_f64(f64::from(*a), f64::from(*b))),
                "{id}"
            );
            let viewed = column("", "scan_rate").unwrap();
            assert!(
                viewed
                    .iter()
                    .zip(&rates)
                    .all(|(a, b)| same_f64(*a, f64::from(*b))),
                "{id}"
            );
        }
        None => assert!(rates.iter().all(|r| r.is_nan()), "{id}: scan rates dropped"),
    }

    // A stored column that the typed slots turn into NaN (a sentinel) is
    // also kept verbatim; a column without one is not.
    let verbatim = |name: &str, raw: Vec<f32>, lost: &dyn Fn(f32) -> bool| {
        let lossy = raw.iter().any(|v| !v.is_nan() && lost(*v));
        match extra(name) {
            Some(model) => {
                assert!(lossy, "{id} {name} kept without a sentinel");
                let matches = |values: &[f64]| {
                    values.len() == raw.len()
                        && values
                            .iter()
                            .zip(&raw)
                            .all(|(a, b)| same_f64(*a, f64::from(*b)))
                };
                assert!(matches(&model), "{id} {name}");
                assert!(matches(&column("", name).unwrap()), "{id} {name} view");
                1
            }
            None => {
                assert!(!lossy, "{id} {name} lost its sentinels");
                0
            }
        }
    };
    let sentinel = |value: f32| present(value).is_nan();
    let mut verbatim_columns = verbatim(
        "dorade_ryib_true_scan_rate",
        kept.iter().map(|(r, _)| endian.f32(r, 36)).collect(),
        &sentinel,
    );
    verbatim_columns += verbatim(
        "dorade_ryib_peak_power_kw",
        kept.iter().map(|(r, _)| endian.f32(r, 32)).collect(),
        &|kw: f32| !(kw.is_finite() && kw > 0.0),
    );
    // ASIB members (platform_i, DoradeData.hh) in block order.
    for (index, name) in [
        "longitude_deg",
        "latitude_deg",
        "altitude_msl_km",
        "altitude_agl_km",
        "ew_velocity_mps",
        "ns_velocity_mps",
        "vert_velocity_mps",
        "heading_deg",
        "roll_deg",
        "pitch_deg",
        "drift_angle_deg",
        "rotation_angle_deg",
        "tilt_deg",
        "ew_horiz_wind_mps",
        "ns_horiz_wind_mps",
        "vert_wind_mps",
        "heading_change_deg_per_s",
        "pitch_change_deg_per_s",
    ]
    .into_iter()
    .enumerate()
    {
        let raw = kept
            .iter()
            .map(|(_, asib)| asib.map_or(f32::NAN, |a| endian.f32(a, 8 + 4 * index)))
            .collect();
        verbatim_columns += verbatim(&format!("dorade_asib_{name}"), raw, &sentinel);
    }
    eprintln!("{id}: {verbatim_columns} verbatim sentinel columns");

    // ASIB: platform track and the CfRadial georeference variables.
    let asib = |index: usize| -> Vec<f32> {
        kept.iter()
            .map(|(_, asib)| asib.map_or(f32::NAN, |a| present(endian.f32(a, 8 + 4 * index))))
            .collect()
    };
    let track = sweep
        .platform_track
        .as_deref()
        .expect("ASIB platform track");
    let check = |name: &str, model: &[f64], expected: Vec<f64>| {
        assert!(
            model.iter().zip(&expected).all(|(a, b)| same_f64(*a, *b))
                && model.len() == expected.len(),
            "{id} {name}"
        );
        let viewed = column("", name).unwrap();
        assert!(
            viewed.iter().zip(&expected).all(|(a, b)| same_f64(*a, *b)),
            "{id} {name} view"
        );
    };
    let degrees = |values: Vec<f32>| values.into_iter().map(f64::from).collect::<Vec<_>>();
    let metres = |values: Vec<f32>| {
        values
            .into_iter()
            .map(|km| f64::from(km) * 1000.0)
            .collect::<Vec<_>>()
    };
    check("longitude", &track.longitude_deg, degrees(asib(0)));
    check("latitude", &track.latitude_deg, degrees(asib(1)));
    check("altitude", &track.altitude_m, metres(asib(2)));
    let optional = [
        (
            "altitude_agl",
            track.altitude_agl_m.clone(),
            metres(asib(3)),
        ),
        (
            "heading",
            track.heading_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(7)),
        ),
        (
            "roll",
            track.roll_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(8)),
        ),
        (
            "pitch",
            track.pitch_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(9)),
        ),
        (
            "drift",
            track.drift_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(10)),
        ),
        (
            "rotation",
            track.rotation_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(11)),
        ),
        (
            "tilt",
            track.tilt_deg.as_ref().map(|v| degrees(v.clone())),
            degrees(asib(12)),
        ),
    ];
    for (name, model, expected) in optional {
        match model {
            Some(model) => check(name, &model, expected),
            None => assert!(expected.iter().all(|v| v.is_nan()), "{id} {name} dropped"),
        }
    }
    for (index, name) in [
        (4, "eastward_velocity"),
        (5, "northward_velocity"),
        (6, "vertical_velocity"),
        (13, "eastward_wind"),
        (14, "northward_wind"),
        (15, "vertical_wind"),
        (16, "heading_change_rate"),
        (17, "pitch_change_rate"),
    ] {
        let expected = degrees(asib(index));
        match extra(name) {
            Some(model) => check(name, &model, expected),
            None => assert!(expected.iter().all(|v| v.is_nan()), "{id} {name} dropped"),
        }
    }

    // CELV distances as stored.
    if let Some((_, celv)) = blocks.iter().find(|(block_id, _)| block_id == b"CELV") {
        let cells = endian.i32(celv, 8) as usize;
        let expected: Vec<f64> = (0..cells)
            .map(|i| f64::from(endian.f32(celv, 12 + 4 * i)))
            .collect();
        assert_eq!(extra("dorade_celv_distance").unwrap(), expected, "{id}");
        let viewed = view
            .group(&format!("sweep_{index}"))
            .unwrap()
            .variable("dorade_celv_distance")
            .unwrap();
        assert!(matches!(viewed.values, Values::Borrowed(_)));
    } else {
        assert!(extra("dorade_celv_distance").is_none(), "{id}");
    }
    kept.len()
}

/// The CFAC corrections the decoder leaves to the consumer: (model and
/// FM301 name, CFAC offset, factor to the model's unit).
const UNAPPLIED_CFAC: [(&str, usize, f64); 10] = [
    ("pressure_altitude_correction", 28, 1000.0),
    ("eastward_ground_speed_correction", 36, 1.0),
    ("northward_ground_speed_correction", 40, 1.0),
    ("vertical_velocity_correction", 44, 1.0),
    ("heading_correction", 48, 1.0),
    ("roll_correction", 52, 1.0),
    ("pitch_correction", 56, 1.0),
    ("drift_correction", 60, 1.0),
    ("rotation_correction", 64, 1.0),
    ("tilt_correction", 68, 1.0),
];

/// `platform_type`, `platform_is_mobile` and `georeferencing_correction` of
/// a one-sweep volume against the RADD and CFAC bytes of `id`; returns the
/// number of corrections compared.
fn check_platform_and_corrections(id: &str, volume: &Volume, view: &VolumeView<'_>) -> usize {
    let (endian, blocks) = blocks(id);
    let block = |name: &[u8; 4]| blocks.iter().find(|(b, _)| b == name).map(|(_, b)| b);
    let radar_type = endian.i16(block(b"RADD").unwrap(), 48);
    assert_eq!(volume.platform_type, platform_type(radar_type), "{id}");
    // Airborne (1 to 4) and shipborne (5) radars move.
    let mobile = (1..=5).contains(&radar_type);
    assert_eq!(volume.attrs.platform_is_mobile, mobile, "{id}");
    assert_eq!(
        view.root.attr("platform_is_mobile"),
        Some(&AttrValue::text(mobile.to_string())),
        "{id}: view"
    );
    let group = view.group("georeferencing_correction");
    let Some(cfac) = block(b"CFAC").filter(|cfac| cfac.len() >= 72) else {
        assert!(volume.georeferencing_correction.is_none(), "{id}");
        assert!(group.is_none(), "{id}: view");
        return 0;
    };
    let model = volume.georeferencing_correction.as_deref().unwrap();
    let group = group.unwrap();
    let entries = model.entries();
    for (name, value) in entries {
        let expected = UNAPPLIED_CFAC
            .iter()
            .find(|(known, _, _)| *known == name)
            .map(|(_, offset, factor)| (f64::from(endian.f32(cfac, *offset)) * factor) as f32);
        // The six corrections the decoder applies are left out.
        assert_eq!(
            value.map(f32::to_bits),
            expected.map(f32::to_bits),
            "{id}: {name}"
        );
        let viewed = group.variable(name).map(|variable| match &variable.values {
            Values::Scalar(Scalar::F32(value)) => *value,
            other => panic!("{id}: {name} is {other:?}"),
        });
        assert_eq!(viewed, value, "{id}: view {name}");
    }
    UNAPPLIED_CFAC.len()
}

fn platform_type(code: i16) -> PlatformType {
    match code {
        1 => PlatformType::AircraftFore,
        2 => PlatformType::AircraftAft,
        3 => PlatformType::AircraftTail,
        4 => PlatformType::AircraftBelly,
        5 => PlatformType::Ship,
        _ => PlatformType::Fixed,
    }
}

#[test]
fn every_descriptor_and_ray_block_reaches_the_model_and_the_view() {
    let (mut values, mut rays, mut corrections) = (0, 0, 0);
    for id in SWEEPFILES {
        let bytes = recast_radar_testdata::bytes(id).unwrap();
        let volume: Volume = read_dorade_sweep_volume(&bytes).unwrap();
        let view = fm301::volume_view(&volume, ALL, None).unwrap();
        values += check_descriptors(id, &volume.sweeps[0], &view, 0);
        rays += check_rays(id, &volume.sweeps[0], &view, 0);
        corrections += check_platform_and_corrections(id, &volume, &view);
    }
    eprintln!("{values} descriptor values, {rays} rays and {corrections} corrections compared");
    assert!(values > 600, "{values}");
    assert!(rays > 250, "{rays}");
    assert!(corrections >= 60, "{corrections}");
}

#[test]
fn full_airborne_sweepfiles_reach_the_model_and_the_view() {
    for id in FULL_AIRBORNE {
        let bytes = match recast_radar_testdata::bytes(id) {
            Ok(bytes) => bytes,
            Err(error) if error.is_offline() => {
                eprintln!("skipping {id}: {error}");
                continue;
            }
            Err(error) => panic!("{id}: {error}"),
        };
        let volume: Volume = read_dorade_sweep_volume(&bytes).unwrap();
        let view = fm301::volume_view(&volume, ALL, None).unwrap();
        let values = check_descriptors(id, &volume.sweeps[0], &view, 0);
        let rays = check_rays(id, &volume.sweeps[0], &view, 0);
        assert_eq!(rays, 360, "{id}");
        assert_eq!(check_platform_and_corrections(id, &volume, &view), 10);
        assert!(volume.attrs.platform_is_mobile, "{id}");
        assert!(
            lookup(&volume.sweeps[0].other, "dorade_seds_text").is_some(),
            "{id}: SEDS"
        );
        eprintln!("{id}: {values} descriptor values and {rays} rays compared");
    }
}

#[test]
fn each_sweep_of_a_volume_keeps_its_own_descriptors() {
    let ids = [
        "dorade-noxp-20090610-003210-ppi-head6",
        "dorade-noxp-20090610-003222-ppi-head6",
        "dorade-noxp-20090610-003226-ppi-head6",
    ];
    let files: Vec<Vec<u8>> = ids
        .iter()
        .map(|id| recast_radar_testdata::bytes(id).unwrap())
        .collect();
    let volume = read_dorade_volume_from_slices(&files).unwrap();
    let view = fm301::volume_view(&volume, ALL, None).unwrap();
    assert_eq!(volume.sweeps.len(), 3);
    for (index, id) in ids.iter().enumerate() {
        assert!(check_descriptors(id, &volume.sweeps[index], &view, index) > 50);
        assert_eq!(check_rays(id, &volume.sweeps[index], &view, index), 6);
    }
}

// ---------------------------------------------------------------------------
// LROSE RadxPrint -native, an independent reader of the descriptors

/// How a RadxPrint member is printed.
#[derive(Clone, Copy)]
enum Printed {
    /// A number, floats with about six significant digits.
    Number,
    /// Text, trimmed.
    Text,
    /// Seconds since 1970 as UTC `YYYY/MM/DD hh:mm:ss`.
    Time,
    /// An enum name for the stored code, "UNKNOWN" for another code.
    Enum(&'static [(&'static str, i64)]),
}

use Printed::{Enum, Number, Time};

/// lrose-core `DoradeData.hh` `radar_type_t`.
const RADAR_TYPES: &[(&str, i64)] = &[
    ("RADAR_GROUND", 0),
    ("RADAR_AIR_FORE", 1),
    ("RADAR_AIR_AFT", 2),
    ("RADAR_AIR_TAIL", 3),
    ("RADAR_AIR_LF", 4),
    ("RADAR_SHIP", 5),
    ("RADAR_AIR_NOSE", 6),
    ("RADAR_SATELLITE", 7),
    ("LIDAR_MOVING", 8),
    ("LIDAR_FIXED", 9),
];

/// lrose-core `DoradeData.hh` `scan_mode_t`.
const SCAN_MODES: &[(&str, i64)] = &[
    ("SCAN_MODE_CAL", 0),
    ("SCAN_MODE_PPI", 1),
    ("SCAN_MODE_COP", 2),
    ("SCAN_MODE_RHI", 3),
    ("SCAN_MODE_VER", 4),
    ("SCAN_MODE_TAR", 5),
    ("SCAN_MODE_MAN", 6),
    ("SCAN_MODE_IDL", 7),
    ("SCAN_MODE_SUR", 8),
    ("SCAN_MODE_AIR", 9),
    ("SCAN_MODE_HOR", 10),
];

/// lrose-core `DoradeData.hh` `binary_format_t`.
const BINARY_FORMATS: &[(&str, i64)] = &[
    ("BINARY_FORMAT_INT8", 1),
    ("BINARY_FORMAT_INT16", 2),
    ("BINARY_FORMAT_INT32", 3),
    ("BINARY_FORMAT_FLOAT32", 4),
];

/// A RadxPrint member, the model member it prints (`dorade_<block>_<name>`)
/// with the element of an array member, and how it is printed.
type Member = (&'static str, &'static str, Option<usize>, Printed);

const RADX_SSWB: &[Member] = &[
    ("last_used", "last_used", None, Time),
    ("start_time", "start_time", None, Time),
    ("stop_time", "stop_time", None, Time),
    ("sizeof_file", "sizeof_file", None, Number),
    ("compression_flag", "compression_flag", None, Number),
    ("volume_time_stamp", "volume_time_stamp", None, Number),
    ("num_params", "num_params", None, Number),
    ("radar_name", "radar_name", None, Printed::Text),
    ("d_start_time", "d_start_time", None, Number),
    ("d_stop_time", "d_stop_time", None, Number),
    ("version_num", "version_num", None, Number),
    ("num_key_tables", "num_key_tables", None, Number),
    ("status", "status", None, Number),
];

const RADX_VOLD: &[Member] = &[
    ("format_version", "format_version", None, Number),
    ("volume_num", "volume_num", None, Number),
    ("maximum_bytes", "maximum_bytes", None, Number),
    ("proj_name", "proj_name", None, Printed::Text),
    ("year", "year", None, Number),
    ("month", "month", None, Number),
    ("day", "day", None, Number),
    ("data_set_hour", "data_set_hour", None, Number),
    ("data_set_minute", "data_set_minute", None, Number),
    ("data_set_second", "data_set_second", None, Number),
    ("flight_num", "flight_num", None, Printed::Text),
    ("gen_facility", "gen_facility", None, Printed::Text),
    ("gen_year", "gen_year", None, Number),
    ("gen_month", "gen_month", None, Number),
    ("gen_day", "gen_day", None, Number),
    ("number_sensor_des", "number_sensor_des", None, Number),
];

/// RADD members; `scan_mode_pram0` and `_pram1` are left out: RadxPrint
/// prints them through its scan mode names ("UNKNOWN" for -9999).
const RADX_RADD: &[Member] = &[
    ("radar_name", "radar_name", None, Printed::Text),
    ("radar_const", "radar_const_db", None, Number),
    ("peak_power", "peak_power_kw", None, Number),
    ("noise_power", "noise_power_dbm", None, Number),
    ("receiver_gain", "receiver_gain_db", None, Number),
    ("antenna_gain", "antenna_gain_db", None, Number),
    ("system_gain", "system_gain_db", None, Number),
    ("horz_beam_width", "horz_beam_width_deg", None, Number),
    ("vert_beam_width", "vert_beam_width_deg", None, Number),
    ("radar_type", "radar_type", None, Enum(RADAR_TYPES)),
    ("scan_mode", "scan_mode", None, Enum(SCAN_MODES)),
    ("req_rotat_vel", "req_rotat_vel_deg_per_s", None, Number),
    ("num_parameter_des", "num_parameter_des", None, Number),
    ("total_num_des", "total_num_des", None, Number),
    ("data_compress", "data_compress", None, Number),
    ("data_reduction", "data_reduction", None, Number),
    ("data_red_parm0", "data_red_parm0", None, Number),
    ("data_red_parm1", "data_red_parm1", None, Number),
    ("radar_longitude", "radar_longitude_deg", None, Number),
    ("radar_latitude", "radar_latitude_deg", None, Number),
    ("radar_altitude", "radar_altitude_km", None, Number),
    ("eff_unamb_vel", "eff_unamb_vel_mps", None, Number),
    ("eff_unamb_range", "eff_unamb_range_km", None, Number),
    ("num_freq_trans", "num_freq_trans", None, Number),
    ("num_ipps_trans", "num_ipps_trans", None, Number),
    ("freq1", "freq_ghz", Some(0), Number),
    ("freq2", "freq_ghz", Some(1), Number),
    ("freq3", "freq_ghz", Some(2), Number),
    ("freq4", "freq_ghz", Some(3), Number),
    ("freq5", "freq_ghz", Some(4), Number),
    ("prt1", "interpulse_per_ms", Some(0), Number),
    ("prt2", "interpulse_per_ms", Some(1), Number),
    ("prt3", "interpulse_per_ms", Some(2), Number),
    ("prt4", "interpulse_per_ms", Some(3), Number),
    ("prt5", "interpulse_per_ms", Some(4), Number),
    ("extension_num", "extension_num", None, Number),
    ("config_name", "config_name", None, Printed::Text),
    ("config_num", "config_num", None, Number),
    ("aperture_size", "aperture_size_cm", None, Number),
    ("field_of_view", "field_of_view", None, Number),
    ("aperture_eff", "aperture_eff_percent", None, Number),
    ("aux_freq[0]", "aux_freq_ghz", Some(0), Number),
    ("aux_freq[1]", "aux_freq_ghz", Some(1), Number),
    ("aux_freq[2]", "aux_freq_ghz", Some(2), Number),
    ("aux_freq[3]", "aux_freq_ghz", Some(3), Number),
    ("aux_freq[4]", "aux_freq_ghz", Some(4), Number),
    ("aux_freq[5]", "aux_freq_ghz", Some(5), Number),
    ("aux_freq[6]", "aux_freq_ghz", Some(6), Number),
    ("aux_freq[7]", "aux_freq_ghz", Some(7), Number),
    ("aux_freq[8]", "aux_freq_ghz", Some(8), Number),
    ("aux_freq[9]", "aux_freq_ghz", Some(9), Number),
    ("aux_freq[10]", "aux_freq_ghz", Some(10), Number),
    ("aux_prt[0]", "aux_ipp_ms", Some(0), Number),
    ("aux_prt[1]", "aux_ipp_ms", Some(1), Number),
    ("aux_prt[2]", "aux_ipp_ms", Some(2), Number),
    ("aux_prt[3]", "aux_ipp_ms", Some(3), Number),
    ("aux_prt[4]", "aux_ipp_ms", Some(4), Number),
    ("aux_prt[5]", "aux_ipp_ms", Some(5), Number),
    ("aux_prt[6]", "aux_ipp_ms", Some(6), Number),
    ("aux_prt[7]", "aux_ipp_ms", Some(7), Number),
    ("aux_prt[8]", "aux_ipp_ms", Some(8), Number),
    ("aux_prt[9]", "aux_ipp_ms", Some(9), Number),
    ("aux_prt[10]", "aux_ipp_ms", Some(10), Number),
    ("pulse_width (us)", "pulse_width_us", None, Number),
    ("primary_cop_baseln", "primary_cop_baseln", None, Number),
    ("secondary_cop_baseln", "secondary_cop_baseln", None, Number),
    ("pc_xmtr_bandwidth", "pc_xmtr_bandwidth", None, Number),
    ("pc_waveform_type", "pc_waveform_type", None, Number),
    ("site_name", "site_name", None, Printed::Text),
];

const RADX_CFAC: &[Member] = &[
    ("azimuth_corr", "azimuth_corr_deg", None, Number),
    ("elevation_corr", "elevation_corr_deg", None, Number),
    ("range_delay_corr", "range_delay_corr_m", None, Number),
    ("longitude_corr", "longitude_corr_deg", None, Number),
    ("latitude_corr", "latitude_corr_deg", None, Number),
    ("pressure_alt_corr", "pressure_alt_corr_km", None, Number),
    ("radar_alt_corr", "radar_alt_corr_km", None, Number),
    ("ew_gndspd_corr", "ew_gndspd_corr_mps", None, Number),
    ("ns_gndspd_corr", "ns_gndspd_corr_mps", None, Number),
    ("vert_vel_corr", "vert_vel_corr_mps", None, Number),
    ("heading_corr", "heading_corr_deg", None, Number),
    ("roll_corr", "roll_corr_deg", None, Number),
    ("pitch_corr", "pitch_corr_deg", None, Number),
    ("drift_corr", "drift_corr_deg", None, Number),
    ("rot_angle_corr", "rot_angle_corr_deg", None, Number),
    ("tilt_corr", "tilt_corr_deg", None, Number),
];

const RADX_SWIB: &[Member] = &[
    ("radar_name", "radar_name", None, Printed::Text),
    ("sweep_num", "sweep_num", None, Number),
    ("num_rays", "num_rays", None, Number),
    ("start_angle", "start_angle_deg", None, Number),
    ("stop_angle", "stop_angle_deg", None, Number),
    ("fixed_angle", "fixed_angle_deg", None, Number),
    ("filter_flag", "filter_flag", None, Number),
];

/// PARM members after the name, description and units.
const RADX_PARM: &[Member] = &[
    ("interpulse_time", "interpulse_time", None, Number),
    ("xmitted_freq", "xmitted_freq", None, Number),
    ("recvr_bandwidth", "recvr_bandwidth_mhz", None, Number),
    ("pulse_width(m)", "pulse_width_m", None, Number),
    ("polarization", "polarization", None, Number),
    ("num_samples", "num_samples", None, Number),
    ("binary_format", "binary_format", None, Enum(BINARY_FORMATS)),
    ("threshold_field", "threshold_field", None, Printed::Text),
    ("threshold_value", "threshold_value", None, Number),
    ("parameter_scale", "parameter_scale", None, Number),
    ("parameter_bias", "parameter_bias", None, Number),
    ("bad_data", "bad_data", None, Number),
    ("extension_num", "extension_num", None, Number),
    ("config_name", "config_name", None, Printed::Text),
    ("config_num", "config_num", None, Number),
    ("offset_to_data", "offset_to_data", None, Number),
    ("mks_conversion", "mks_conversion", None, Number),
    ("num_qnames", "num_qnames", None, Number),
    ("qdata_names", "qdata_names", None, Printed::Text),
    ("num_criteria", "num_criteria", None, Number),
    ("criteria_names", "criteria_names", None, Printed::Text),
    ("number_cells", "number_cells", None, Number),
    ("meters_to_first_cell", "meters_to_first_cell", None, Number),
    ("meters_between_cells", "meters_between_cells", None, Number),
    ("eff_unamb_vel", "eff_unamb_vel_mps", None, Number),
];

/// Element `index` of a model value (the value itself for a scalar).
fn model_number(value: &AttrValue, index: Option<usize>) -> f64 {
    match (value, index) {
        (AttrValue::Scalar(scalar), None) => scalar.as_f64(),
        (AttrValue::Array(array), Some(index)) => array.get_f64(index).unwrap(),
        (other, index) => panic!("{other:?} [{index:?}]"),
    }
}

/// `printed` is what C's `%g` (six significant digits) prints for `value`:
/// they differ by at most half a unit in the sixth digit.
fn prints_as(printed: &str, value: f64) -> bool {
    let radx: f64 = printed.parse().unwrap_or_else(|_| panic!("{printed}"));
    if radx == value || (radx.is_nan() && value.is_nan()) {
        return true;
    }
    let exponent = value.abs().log10().floor();
    (radx - value).abs() <= 0.5 * 10f64.powf(exponent - 5.0) * (1.0 + 1e-9)
}

/// Whether the RadxPrint text `printed` shows the model value.
fn radx_agrees(printed: &str, model: &AttrValue, index: Option<usize>, how: Printed) -> bool {
    match how {
        Number => prints_as(printed, model_number(model, index)),
        Printed::Text => match model {
            AttrValue::Text(text) => text.trim() == printed.trim(),
            other => panic!("{other:?}"),
        },
        Time => {
            let seconds = model_number(model, index) as i64;
            chrono::DateTime::from_timestamp(seconds, 0)
                .unwrap()
                .format("%Y/%m/%d %H:%M:%S")
                .to_string()
                == printed
        }
        // RadxPrint prints "UNKNOWN" for a code outside the enum.
        Enum(names) => {
            let code = model_number(model, index) as i64;
            if printed == "UNKNOWN" {
                return names.iter().all(|(_, value)| *value != code);
            }
            names
                .iter()
                .find(|(name, _)| *name == printed)
                .is_some_and(|(_, value)| *value == code)
        }
    }
}

/// A member past the end of a short block, which RadxPrint prints from its
/// zeroed structure.
fn printed_as_zero(printed: &str) -> bool {
    printed.is_empty() || printed.parse::<f64>().is_ok_and(|value| value == 0.0)
}

/// Every member of a RadxPrint block against the model attributes `attrs`
/// (and the view's `viewed`); returns the members compared and the model
/// names read.
fn check_radx_block(
    id: &str,
    block: &str,
    printed: &serde_json::Map<String, serde_json::Value>,
    members: &[Member],
    attrs: &[(Box<str>, AttrValue)],
    viewed: &dyn Fn(&str) -> Option<AttrValue>,
    names: &mut Vec<String>,
) -> usize {
    let mut compared = 0;
    for &(radx, member, index, how) in members {
        let text = printed
            .get(radx)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{id} {block}: RadxPrint has no {radx}"));
        let name = format!("dorade_{block}_{member}");
        match lookup(attrs, &name) {
            Some(model) => {
                assert!(
                    radx_agrees(text, model, index, how),
                    "{id} {name}[{index:?}]: model {model:?}, RadxPrint {text}"
                );
                let view = viewed(&name).unwrap_or_else(|| panic!("{id} {name} view"));
                assert!(radx_agrees(text, &view, index, how), "{id} {name} view");
                compared += 1;
                names.push(name);
            }
            // Past the end of a short block: RadxPrint prints its zeroed
            // structure.
            None => assert!(
                printed_as_zero(text),
                "{id}: {name} missing, RadxPrint {radx} = {text}"
            ),
        }
    }
    compared
}

/// RadxPrint -native (tools/dorade_radx_native_golden.py) reads every
/// descriptor member with LROSE's own structure layouts: SSWB (with its key
/// tables), VOLD, RADD, CFAC, CSFD, CELV (with every cell distance), SWIB
/// and every PARM. The model and the view hold what it prints for each
/// sweepfile, and every `dorade_*` attribute of those blocks is compared
/// except the SSWB place holder and the RADD scan mode parameters, which
/// RadxPrint does not print as numbers.
#[test]
fn descriptors_match_radxprint_native() {
    let path = recast_radar_testdata::testdata_dir().join("golden/dorade/radxprint_native.json");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let files = golden["files"].as_object().unwrap();
    assert_eq!(files.len(), SWEEPFILES.len());
    let mut compared = 0;
    for id in SWEEPFILES {
        let printed = files[*id].as_object().unwrap();
        let bytes = recast_radar_testdata::bytes(id).unwrap();
        let volume: Volume = read_dorade_sweep_volume(&bytes).unwrap();
        let view = fm301::volume_view(&volume, ALL, None).unwrap();
        let sweep = &volume.sweeps[0];
        let group = view.group("sweep_0").unwrap();
        let viewed = |name: &str| group.attr(name).cloned();
        let mut names = Vec::new();
        for (block, members) in [
            ("sswb", RADX_SSWB),
            ("vold", RADX_VOLD),
            ("radd", RADX_RADD),
            ("cfac", RADX_CFAC),
            ("swib", RADX_SWIB),
        ] {
            match printed.get(block).and_then(serde_json::Value::as_object) {
                Some(block_printed) => {
                    compared += check_radx_block(
                        id,
                        block,
                        block_printed,
                        members,
                        &sweep.other,
                        &viewed,
                        &mut names,
                    );
                }
                None => assert!(
                    sweep
                        .other
                        .iter()
                        .all(|(name, _)| !name.starts_with(&format!("dorade_{block}_"))),
                    "{id}: no {block} in RadxPrint"
                ),
            }
        }
        // The SSWB key tables: offset, size and type of each.
        let sswb = printed["sswb"].as_object().unwrap();
        let tables = sswb["key_tables"].as_array().unwrap();
        let model_tables = lookup(&sweep.other, "dorade_sswb_key_table").unwrap();
        for (table, fields) in tables.iter().enumerate() {
            for (field, member) in ["offset", "size", "type"].iter().enumerate() {
                let text = fields[member].as_str().unwrap();
                assert!(
                    prints_as(text, model_number(model_tables, Some(3 * table + field))),
                    "{id} key table {table} {member}"
                );
                compared += 1;
            }
        }
        names.push("dorade_sswb_key_table".to_owned());
        // CSFD: the segment count, first distance, and each segment's
        // spacing and cell count.
        if let Some(csfd) = printed.get("csfd").and_then(serde_json::Value::as_object) {
            let segments: usize = csfd["num_segments"].as_str().unwrap().parse().unwrap();
            let mut members: Vec<Member> = vec![
                ("num_segments", "num_segments", None, Number),
                ("dist_to_first", "dist_to_first_m", None, Number),
            ];
            let spacing: Vec<String> = (0..segments).map(|i| format!("spacing[{i}]")).collect();
            let cells: Vec<String> = (0..segments).map(|i| format!("num_cells[{i}]")).collect();
            for i in 0..segments {
                members.push((
                    Box::leak(spacing[i].clone().into_boxed_str()),
                    "spacing_m",
                    Some(i),
                    Number,
                ));
                members.push((
                    Box::leak(cells[i].clone().into_boxed_str()),
                    "num_cells",
                    Some(i),
                    Number,
                ));
            }
            compared += check_radx_block(
                id,
                "csfd",
                csfd,
                &members,
                &sweep.other,
                &viewed,
                &mut names,
            );
        }
        // CELV: the cell count and every cell distance.
        if let Some(celv) = printed.get("celv").and_then(serde_json::Value::as_object) {
            compared += check_radx_block(
                id,
                "celv",
                celv,
                &[("number_cells", "number_cells", None, Number)],
                &sweep.other,
                &viewed,
                &mut names,
            );
            let distances = sweep
                .extra_vars
                .iter()
                .find(|variable| &*variable.name == "dorade_celv_distance")
                .unwrap();
            let printed_distances = celv["cell_distances"].as_array().unwrap();
            assert_eq!(printed_distances.len(), distances.values.len(), "{id}");
            for (cell, text) in printed_distances.iter().enumerate() {
                assert!(
                    prints_as(
                        text.as_str().unwrap(),
                        distances.values.get_f64(cell).unwrap()
                    ),
                    "{id} cell {cell}"
                );
            }
            compared += printed_distances.len();
        }
        // Every PARM on its field, by name.
        for parm in printed["parm"].as_array().unwrap() {
            let parm = parm.as_object().unwrap();
            let parameter = parm["parameter_name"].as_str().unwrap();
            let field = sweep
                .field(&FieldName::parse(parameter))
                .unwrap_or_else(|| panic!("{id}: no field {parameter}"));
            let variable = group.variable(parameter).unwrap();
            let field_viewed = |name: &str| variable.attr(name).cloned();
            compared += check_radx_block(
                id,
                "parm",
                parm,
                RADX_PARM,
                &field.attrs.other,
                &field_viewed,
                &mut Vec::new(),
            );
            let units = parm["param_units"].as_str().unwrap();
            if !units.trim().is_empty() {
                assert_eq!(field.attrs.units.as_deref(), Some(units.trim()), "{id}");
                compared += 1;
            }
        }
        // Every descriptor attribute of these blocks was compared, but the
        // two RadxPrint does not print as numbers.
        for (name, _) in &sweep.other {
            let block = ["sswb", "vold", "radd", "cfac", "csfd", "celv", "swib"]
                .iter()
                .any(|block| name.starts_with(&format!("dorade_{block}_")));
            let not_printed = [
                "dorade_sswb_place_holder",
                "dorade_radd_scan_mode_pram0",
                "dorade_radd_scan_mode_pram1",
            ];
            if block && !not_printed.contains(&&**name) {
                assert!(
                    names.iter().any(|n| n == &**name),
                    "{id}: {name} not compared"
                );
            }
        }
    }
    eprintln!("{compared} descriptor values compared with RadxPrint -native");
    assert!(compared > 1500, "{compared}");
}
