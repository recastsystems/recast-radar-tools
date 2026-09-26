//! CfRadial 2 files from both writers (Radx and xradar) against what xradar
//! and netCDF4-python read from them.
//!
//! Goldens: `testdata/golden/cfradial2/<id>.json`, written by
//! `tools/cfradial2_golden.py` (xradar 0.12.0 `open_cfradial2_datatree`
//! for the sweep order, fixed angles, modes and coordinates; netCDF4-python
//! for the raw field values and attributes in file order). The S-Pol
//! volume is also published as CfRadial 1 (netCDF-4); both decodes must
//! agree.

use recast_radar_core::model::{
    FieldData, IntCoding, LinearTransform, PackedInt, SourceFormat, Sweep, Volume,
};
use recast_radar_io_cfradial::{
    read_cfradial_volume, read_cfradial1_volume, read_cfradial2_volume,
};
use serde_json::Value;

fn corpus(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes(id) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            None
        }
        Err(err) => panic!("{err}"),
    }
}

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("cfradial2")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn sha_f32(values: &[f32]) -> String {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    recast_radar_testdata::sha256_hex(&bytes)
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn close(have: f64, want: f64, tolerance: f64, what: &str) {
    assert!(
        (have - want).abs() <= tolerance,
        "{what}: {have} != {want} (tolerance {tolerance})"
    );
}

/// numpy dtype name, CF packing (scale, offset) and fill of a field.
type Coding = (&'static str, Option<(f64, f64)>, Option<f64>);

fn coding(data: &FieldData) -> Coding {
    fn int<T: PackedInt + Copy + Into<f64>>(
        coding: &IntCoding<T>,
    ) -> (Option<(f64, f64)>, Option<f64>) {
        let packing = match coding.transform {
            LinearTransform::CfScaleOffset {
                scale_factor,
                add_offset,
                ..
            } => Some((scale_factor, add_offset)),
            _ => None,
        };
        (packing, coding.fill_value.map(Into::into))
    }
    let float_packing =
        |transform: Option<LinearTransform>| transform.and_then(|t| Some((t.scale_factor()?, t.add_offset()?)));
    match data {
        FieldData::I8 { coding, .. } => {
            let (packing, fill) = int(coding);
            ("int8", packing, fill)
        }
        FieldData::U8 { coding, .. } => {
            let (packing, fill) = int(coding);
            ("uint8", packing, fill)
        }
        FieldData::I16 { coding, .. } => {
            let (packing, fill) = int(coding);
            ("int16", packing, fill)
        }
        FieldData::U16 { coding, .. } => {
            let (packing, fill) = int(coding);
            ("uint16", packing, fill)
        }
        FieldData::I32 { coding, .. } => {
            let (packing, fill) = int(coding);
            ("int32", packing, fill)
        }
        FieldData::F32 { coding, .. } => (
            "float32",
            float_packing(coding.transform),
            coding.fill_value.map(f64::from),
        ),
        FieldData::F64 { coding, .. } => (
            "float64",
            float_packing(coding.transform),
            coding.fill_value,
        ),
    }
}

/// SHA-256 of a field's elements `range` in their stored width,
/// little-endian (the goldens' encoding).
fn stored_sha(data: &FieldData, range: std::ops::Range<usize>) -> String {
    fn le<T: Copy, const N: usize>(values: &[T], encode: fn(T) -> [u8; N]) -> String {
        let bytes: Vec<u8> = values.iter().flat_map(|v| encode(*v)).collect();
        recast_radar_testdata::sha256_hex(&bytes)
    }
    match data {
        FieldData::I8 { values, .. } => le(&values[range], i8::to_le_bytes),
        FieldData::U8 { values, .. } => le(&values[range], u8::to_le_bytes),
        FieldData::I16 { values, .. } => le(&values[range], i16::to_le_bytes),
        FieldData::U16 { values, .. } => le(&values[range], u16::to_le_bytes),
        FieldData::I32 { values, .. } => le(&values[range], i32::to_le_bytes),
        FieldData::F32 { values, .. } => le(&values[range], f32::to_le_bytes),
        FieldData::F64 { values, .. } => le(&values[range], f64::to_le_bytes),
    }
}

fn check(id: &str) -> Option<Volume> {
    let bytes = corpus(id)?;
    let golden = golden(id);
    assert_eq!(
        golden["sha256"].as_str(),
        Some(recast_radar_testdata::sha256_hex(&bytes).as_str())
    );
    let volume = read_cfradial2_volume(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"));
    // The format router's entry point agrees (compared as text: NaN fill
    // values of the source's coordinate attributes are not equal to
    // themselves).
    assert_eq!(
        format!("{:?}", read_cfradial_volume(&bytes).ok()),
        format!("{:?}", Some(&volume))
    );
    assert_eq!(volume.provenance.source_format, SourceFormat::CfRadial2);
    assert_eq!(
        volume.provenance.compression.as_deref(),
        Some("cfradial2-netcdf4")
    );
    if let Some(name) = golden["instrument_name"].as_str() {
        assert_eq!(volume.attrs.instrument_name, name, "{id}: instrument_name");
    }
    if let Some(number) = golden["volume_number"].as_i64() {
        // xradar writes 0 when the file has none.
        assert_eq!(
            volume.volume_number.map_or(0, i64::from),
            number,
            "{id}: volume_number"
        );
    }
    for (name, have) in [
        ("latitude", volume.location.latitude_deg),
        ("longitude", volume.location.longitude_deg),
        ("altitude", volume.location.altitude_m),
    ] {
        if let Some(want) = number(&golden[name]) {
            close(
                have.unwrap_or(f64::NAN),
                want,
                1e-9,
                &format!("{id}: {name}"),
            );
        }
    }
    let sweeps = golden["sweeps"].as_array().cloned().unwrap_or_default();
    assert_eq!(volume.sweeps.len(), sweeps.len(), "{id}: sweep count");
    for (index, (sweep, want)) in volume.sweeps.iter().zip(&sweeps).enumerate() {
        check_sweep(id, index, &volume, sweep, want);
    }
    Some(volume)
}

fn check_sweep(id: &str, index: usize, volume: &Volume, sweep: &Sweep, want: &Value) {
    let context = format!("{id} sweep {index} ({})", want["source_group"]);
    assert_eq!(
        sweep.nrays() as u64,
        want["nrays"].as_u64().unwrap_or(0),
        "{context}: rays"
    );
    assert_eq!(
        sweep.range.ngates() as u64,
        want["ngates"].as_u64().unwrap_or(0),
        "{context}: gates"
    );
    close(
        f64::from(sweep.fixed_angle_deg),
        number(&want["fixed_angle"]).unwrap_or(f64::NAN),
        1e-5,
        &format!("{context}: fixed angle"),
    );
    assert_eq!(
        sweep.sweep_mode.as_str(),
        want["sweep_mode"].as_str().unwrap_or_default(),
        "{context}: sweep_mode"
    );
    // xradar fills follow_mode "none" and prt_mode "fixed" when the file
    // has none; a value we read must be xradar's.
    if let Some(mode) = &sweep.follow_mode {
        assert_eq!(
            mode.as_str(),
            want["follow_mode"].as_str().unwrap_or_default()
        );
    }
    if let Some(mode) = &sweep.prt_mode {
        assert_eq!(mode.as_str(), want["prt_mode"].as_str().unwrap_or_default());
    }
    let centers: Vec<f64> = (0..sweep.range.ngates().min(2))
        .filter_map(|gate| sweep.range.center_m(gate))
        .collect();
    close(
        centers[0],
        number(&want["range_first"]).unwrap_or(f64::NAN),
        1e-3,
        &format!("{context}: first range centre"),
    );
    if let Some(spacing) = number(&want["range_spacing"]) {
        close(
            centers[1] - centers[0],
            spacing,
            1e-3,
            &format!("{context}: spacing"),
        );
    }
    assert_eq!(
        sha_f32(&sweep.rays.azimuth_deg),
        want["azimuth_sha256"].as_str().unwrap_or_default(),
        "{context}: azimuth"
    );
    assert_eq!(
        sha_f32(&sweep.rays.elevation_deg),
        want["elevation_sha256"].as_str().unwrap_or_default(),
        "{context}: elevation"
    );
    let reference = volume.time_reference.timestamp_millis() as f64 / 1000.0;
    let times = want["unix_time_first_last"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (first, last) = (
        sweep.rays.time_s.first().copied().unwrap_or(f64::NAN),
        sweep.rays.time_s.last().copied().unwrap_or(f64::NAN),
    );
    close(
        reference + first,
        number(&times[0]).unwrap_or(f64::NAN),
        1e-3,
        &format!("{context}: first ray time"),
    );
    close(
        reference + last,
        number(&times[1]).unwrap_or(f64::NAN),
        1e-3,
        &format!("{context}: last ray time"),
    );

    let fields = want["fields"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    let want_names: Vec<&str> = fields.iter().filter_map(|f| f["name"].as_str()).collect();
    assert_eq!(names, want_names, "{context}: fields in file order");
    for (field, want) in sweep.fields.iter().zip(&fields) {
        let context = format!("{context} {}", field.name.as_str());
        let (dtype, packing, fill) = coding(&field.data);
        assert_eq!(
            dtype,
            want["dtype"].as_str().unwrap_or_default(),
            "{context}: storage"
        );
        assert_eq!(
            stored_sha(&field.data, 0..field.data.len()),
            want["sha256"].as_str().unwrap_or_default(),
            "{context}: raw values"
        );
        if let (Some(scale), Some(offset)) =
            (number(&want["scale_factor"]), number(&want["add_offset"]))
        {
            let (have_scale, have_offset) = packing.unwrap_or((f64::NAN, f64::NAN));
            close(
                have_scale,
                scale,
                scale.abs() * 1e-7,
                &format!("{context}: scale_factor"),
            );
            close(have_offset, offset, 1e-6, &format!("{context}: add_offset"));
        }
        match (number(&want["fill_value"]), want["fill_value"].as_str()) {
            (Some(value), _) => close(
                fill.unwrap_or(f64::NAN),
                value,
                0.0,
                &format!("{context}: fill"),
            ),
            (None, Some("NaN")) => assert!(fill.is_some_and(f64::is_nan), "{context}: NaN fill"),
            _ => {}
        }
    }
}

#[test]
fn spol_xradar_2023_writer() {
    check("cfrad2-spol-20080604-002217-sur");
}

#[test]
fn irene_radx_writer_with_monitoring_and_calibration() {
    let Some(volume) = check("cfrad2-radx-irene-sr2-20110827-120420-sur-r30km") else {
        return;
    };
    // Radx: one r_calib entry, frequency in radar_parameters, the scan rate
    // and measured powers in each sweep's monitoring group.
    assert_eq!(volume.radar_calibration.len(), 1);
    assert_eq!(volume.radar_parameters.frequency_hz.len(), 1);
    for sweep in &volume.sweeps {
        assert!(sweep.ray_vars.scan_rate_deg_per_s.is_some());
        assert!(sweep.monitoring.is_some());
        assert!(sweep.ray_vars.nyquist_velocity_mps.is_some());
    }
}

#[test]
fn iesha_radx_int32_keeps_int32_storage_and_per_sweep_packing() {
    let Some(volume) = check("cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32") else {
        return;
    };
    let gates: Vec<usize> = volume.sweeps.iter().map(|s| s.range.ngates()).collect();
    assert_eq!(gates, [350, 350, 240, 100]);
    for sweep in &volume.sweeps {
        for field in &sweep.fields {
            assert!(matches!(field.data, FieldData::I32 { .. }));
        }
    }
}

/// Each ray of `sweep` by its (time, azimuth, elevation), with the hash of
/// every field's stored values for that ray.
fn rays_by_key(sweep: &Sweep) -> std::collections::BTreeMap<(u64, u32, u32), Vec<String>> {
    let nrays = sweep.nrays();
    (0..nrays)
        .map(|ray| {
            let key = (
                sweep.rays.time_s[ray].to_bits(),
                sweep.rays.azimuth_deg[ray].to_bits(),
                sweep.rays.elevation_deg[ray].to_bits(),
            );
            let rows = sweep
                .fields
                .iter()
                .map(|field| {
                    let row = field.data.len() / nrays;
                    stored_sha(&field.data, ray * row..(ray + 1) * row)
                })
                .collect();
            (key, rows)
        })
        .collect()
}

/// Two decodes of the same sweep written by different writers: the same
/// rays (in whatever order the writer stored them) with the same field
/// bytes.
fn same_rays(a: &Sweep, b: &Sweep, context: &str) {
    assert_eq!(a.nrays(), b.nrays(), "{context}: rays");
    let names_a: Vec<&str> = a.fields.iter().map(|f| f.name.as_str()).collect();
    let names_b: Vec<&str> = b.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names_a, names_b, "{context}: fields");
    for (fa, fb) in a.fields.iter().zip(&b.fields) {
        let (dtype_a, packing_a, fill_a) = coding(&fa.data);
        let (dtype_b, packing_b, fill_b) = coding(&fb.data);
        assert_eq!(
            (dtype_a, packing_a, fill_a.map(f64::to_bits)),
            (dtype_b, packing_b, fill_b.map(f64::to_bits)),
            "{context}: {} coding",
            fa.name.as_str()
        );
    }
    assert_eq!(
        rays_by_key(a),
        rays_by_key(b),
        "{context}: rays and field rows"
    );
}

#[test]
fn xsapr_xradar_writer_azimuth_ray_dimension() {
    let (Some(bytes), Some(classic)) = (
        corpus("cfrad2-xradar-xsapr-sgp-20110520-ppi"),
        corpus("cfrad1-xsapr-sgp-20110520-ppi-classic"),
    ) else {
        return;
    };
    check("cfrad2-xradar-xsapr-sgp-20110520-ppi");
    let cf2 = read_cfradial2_volume(&bytes).expect("decode");
    let cf1 = read_cfradial1_volume(&classic).expect("classic decode");
    // xradar stored the source's rays sorted by azimuth (its ray dimension
    // is `azimuth`), with their times, angles and field bytes unchanged.
    assert_ne!(cf2.sweeps[0].rays, cf1.sweeps[0].rays);
    assert_eq!(cf1.time_reference, cf2.time_reference);
    same_rays(&cf1.sweeps[0], &cf2.sweeps[0], "xsapr");
}

#[test]
fn dow8_xradar_writer_rhi_with_root_platform_track() {
    let Some(volume) = check("cfrad2-xradar-dow8-20211011-223602-rhi-r300") else {
        return;
    };
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_mode.as_str(), "rhi");
    let track = sweep
        .platform_track
        .as_ref()
        .expect("root track split onto the sweep");
    assert_eq!(track.latitude_deg.len(), sweep.nrays());
}

/// The S-Pol volume in both layouts: same sweeps, rays, gates and fields.
#[test]
fn spol_cfradial2_matches_its_cfradial1_publication() {
    let (Some(cf1), Some(cf2)) = (
        corpus("cfrad1-spol-20080604-002217-sur"),
        corpus("cfrad2-spol-20080604-002217-sur"),
    ) else {
        return;
    };
    let cf1 = read_cfradial1_volume(&cf1).expect("CfRadial 1");
    let cf2 = read_cfradial2_volume(&cf2).expect("CfRadial 2");
    assert_eq!(cf1.time_reference, cf2.time_reference);
    assert_eq!(cf1.sweeps.len(), cf2.sweeps.len());
    // The CfRadial 2 publication stores a few rays per sweep one position
    // later around north (netCDF4-python: sweep 0 azimuth [317] is 359.25
    // in the CfRadial 1 file, 0.0 in the CfRadial 2 one); the rays and
    // their field bytes are the same.
    for (index, (a, b)) in cf1.sweeps.iter().zip(&cf2.sweeps).enumerate() {
        assert_eq!(a.range, b.range, "sweep {index} range");
        assert_eq!(a.sweep_mode, b.sweep_mode, "sweep {index} mode");
        close(
            f64::from(a.fixed_angle_deg),
            f64::from(b.fixed_angle_deg),
            1e-4,
            &format!("sweep {index} fixed angle"),
        );
        same_rays(a, b, &format!("S-Pol sweep {index}"));
    }
    assert_eq!(cf1.location, cf2.location);
}

/// Radx's CfRadial 2 of IRENE keeps the classic file's rays and int8 codes
/// for the 400 gates within 30 km.
#[test]
fn irene_radx_cfradial2_matches_the_classic_source_within_30_km() {
    let (Some(cf1), Some(cf2)) = (
        corpus("cfrad1-irene-sr2-20110827-120420-sur-sweeps01"),
        corpus("cfrad2-radx-irene-sr2-20110827-120420-sur-r30km"),
    ) else {
        return;
    };
    let cf1 = read_cfradial1_volume(&cf1).expect("CfRadial 1");
    let cf2 = read_cfradial2_volume(&cf2).expect("CfRadial 2");
    assert_eq!(cf1.time_reference, cf2.time_reference);
    assert_eq!(cf1.sweeps.len(), cf2.sweeps.len());
    for (index, (a, b)) in cf1.sweeps.iter().zip(&cf2.sweeps).enumerate() {
        // Radx rewrites ray times from its own clock arithmetic (2.76 s
        // becomes 2.759999999 s); the angles are the source's.
        assert_eq!(
            a.rays.azimuth_deg, b.rays.azimuth_deg,
            "sweep {index} azimuth"
        );
        assert_eq!(
            a.rays.elevation_deg, b.rays.elevation_deg,
            "sweep {index} elevation"
        );
        assert_eq!(a.nrays(), b.nrays());
        for (ta, tb) in a.rays.time_s.iter().zip(&b.rays.time_s) {
            close(*ta, *tb, 1e-6, &format!("sweep {index} ray time"));
        }
        assert_eq!(
            a.ray_vars.nyquist_velocity_mps,
            b.ray_vars.nyquist_velocity_mps
        );
        let gates = b.range.ngates();
        assert_eq!(gates, 400);
        for (fa, fb) in a.fields.iter().zip(&b.fields) {
            assert_eq!(fa.name, fb.name);
            assert_eq!(coding(&fa.data), coding(&fb.data));
            let (row_a, row_b) = (fa.data.len() / a.nrays(), fb.data.len() / b.nrays());
            for ray in 0..a.nrays() {
                assert_eq!(
                    stored_sha(&fa.data, ray * row_a..ray * row_a + row_b),
                    stored_sha(&fb.data, ray * row_b..(ray + 1) * row_b),
                    "sweep {index} {} ray {ray}",
                    fa.name.as_str()
                );
            }
        }
    }
    assert_eq!(cf1.radar_calibration, cf2.radar_calibration);
}
