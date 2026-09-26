//! Native FM301 decoding of real CfRadial 1 files, spot-checked against what
//! xradar 0.12.0 `open_cfradial1_datatree(first_dim="time",
//! mask_and_scale=False)` returns for the same files (`tools/fm301_golden.py`
//! goldens on the F.4 branch; the values below are copied from them). The
//! full conformance comparison is plan F.4; these tests pin the decoder's
//! packed storage, codings, coordinates, per-ray variables and passthrough.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::excessive_precision)]

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{
    AttrValue, FieldData, FieldName, FloatWidth, InstrumentType, LinearTransform, PlatformType,
    PrimaryAxis, PrtMode, RangeCoord, SweepMode, Volume,
};
use recast_radar_io_cfradial::read_cfradial1_volume;
use recast_radar_testdata::sha256_hex;

fn decode(id: &str) -> Option<Volume> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(error) if error.is_offline() => {
            eprintln!("skipping: {error}");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let bytes = std::fs::read(path).expect("read testdata");
    Some(read_cfradial1_volume(&bytes).expect("decode CfRadial 1"))
}

#[test]
fn irene_keeps_int8_packing_time_reference_and_per_ray_variables() {
    let Some(volume) = decode("cfrad1-irene-sr2-20110827-120420-sur-sweeps01") else {
        return;
    };
    // Root.
    assert_eq!(volume.attrs.instrument_name, "CPOLRVP");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("CPOLRVP"));
    assert_eq!(volume.attrs.title.as_deref(), Some("IRENE-WINDS"));
    assert_eq!(
        volume.attrs.references.as_deref(),
        Some("Conversion software: Radx::SigmetRadxFile")
    );
    assert!(!volume.attrs.platform_is_mobile);
    assert_eq!(volume.attrs.ray_times_increase, Some(true));
    assert_eq!(volume.scan.name.as_deref(), Some("IRENE_WINDS"));
    assert_eq!(volume.scan.id, Some(0));
    assert_eq!(volume.volume_number, Some(395));
    assert_eq!(volume.platform_type, PlatformType::Fixed);
    assert_eq!(volume.instrument_type, InstrumentType::Radar);
    assert_eq!(volume.primary_axis, Some(PrimaryAxis::AxisZ));
    assert_eq!(volume.location.latitude_deg, Some(34.73310470581055));
    assert_eq!(volume.location.longitude_deg, Some(-76.66191101074219));
    assert_eq!(volume.location.altitude_m, Some(0.0));
    assert_eq!(volume.location.altitude_agl_m, Some(-65.0));
    assert_eq!(volume.radar_parameters.frequency_hz, vec![5624624128.0]);
    assert_eq!(
        volume.provenance.source_version.as_deref(),
        Some("CF-Radial-1.3")
    );
    assert_eq!(
        volume.provenance.source_conventions.as_deref(),
        Some("CF-1.6")
    );
    // Global attributes xradar drops but Py-ART keeps pass through.
    let other: Vec<&str> = volume.attrs.other.iter().map(|(name, _)| &**name).collect();
    for name in [
        "Sub_conventions",
        "original_format",
        "driver",
        "n_gates_vary",
    ] {
        assert!(other.contains(&name), "{name} in {other:?}");
    }
    // `time.units` is the epoch; `time_coverage_*` are the file's strings.
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2011, 8, 27, 12, 4, 20).unwrap()
    );
    let coverage = volume.time_coverage.unwrap();
    assert_eq!(
        coverage.start,
        Utc.with_ymd_and_hms(2011, 8, 27, 12, 4, 20).unwrap()
    );
    assert_eq!(
        coverage.end,
        Utc.with_ymd_and_hms(2011, 8, 27, 12, 8, 2).unwrap()
    );

    assert_eq!(volume.sweeps.len(), 2);
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
    assert_eq!(sweep.prt_mode, Some(PrtMode::Fixed));
    assert_eq!(sweep.fixed_angle_deg, 0.802001953125);
    assert_eq!(volume.sweeps[1].fixed_angle_deg, 1.4996337890625);
    assert_eq!(sweep.nrays(), 360);
    assert_eq!(volume.sweeps[1].nrays(), 359);
    assert_eq!(sweep.rays.azimuth_deg[0], 314.20074462890625);
    assert_eq!(sweep.rays.elevation_deg[0], 0.791015625);
    assert_eq!(sweep.rays.time_s[0], 0.76);
    assert_eq!(sweep.rays.time_s[359], 12.76);
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 0.0,
            spacing_m: 75.0,
            ngates: 1107
        }
    );
    // Per-ray instrument variables (Table 301-8a).
    let vars = &sweep.ray_vars;
    assert_eq!(
        vars.nyquist_velocity_mps.as_ref().unwrap()[0],
        47.970001220703125
    );
    assert_eq!(vars.unambiguous_range_m.as_ref().unwrap()[0], 83275.6796875);
    assert_eq!(vars.prt_s.as_ref().unwrap()[0], 0.00055555557);
    assert_eq!(vars.prt_ratio.as_ref().unwrap()[0], 1.0);
    assert_eq!(vars.n_samples.as_ref().unwrap()[0], 32);
    assert_eq!(vars.pulse_width_s.as_ref().unwrap()[0], 5.0e-7);
    assert_eq!(vars.antenna_transition.as_ref().unwrap()[0], 0);
    assert_eq!(vars.calib_index.as_ref().unwrap()[0], 0);
    assert_eq!(
        sweep
            .monitoring
            .as_ref()
            .unwrap()
            .radar_measured_transmit_power_h_dbm
            .as_ref()
            .unwrap()[0],
        -9999.0
    );
    let extra: Vec<&str> = sweep.extra_vars.iter().map(|v| &*v.name).collect();
    assert!(extra.contains(&"ray_start_range") && extra.contains(&"ray_gate_spacing"));
    let start_range = sweep
        .extra_vars
        .iter()
        .find(|v| &*v.name == "ray_start_range")
        .unwrap();
    assert_eq!(start_range.dims, vec!["time".into()]);
    assert_eq!(start_range.shape, vec![360]);

    // Fields verbatim, int8 packed with the file's attributes.
    let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["DBZ", "VEL"]);
    let dbz = sweep.field(&FieldName::Dbz).unwrap();
    let FieldData::I8 { values, coding } = &dbz.data else {
        panic!("DBZ is int8");
    };
    assert_eq!(coding.fill_value, Some(-128));
    assert_eq!(
        coding.transform,
        LinearTransform::CfScaleOffset {
            scale_factor: 0.5,
            add_offset: 32.0,
            attr_width: FloatWidth::F32,
        }
    );
    assert_eq!(dbz.shape(), (360, 1107));
    assert_eq!(
        values.iter().map(|v| i64::from(*v)).sum::<i64>(),
        -9_337_543
    );
    let bytes: Vec<u8> = values.iter().map(|v| *v as u8).collect();
    assert_eq!(
        sha256_hex(&bytes),
        "8eb39e15c1244610e4c885547716b5ba3ddb8321e0e5d36c26de2b857a3bfc9a"
    );
    assert_eq!(dbz.attrs.units.as_deref(), Some("dBZ"));
    assert_eq!(dbz.attrs.sampling_ratio, Some(1.0));
    assert!(
        dbz.attrs
            .other
            .iter()
            .any(|(name, value)| &**name == "grid_mapping"
                && *value == AttrValue::Text("grid_mapping".into()))
    );
    // Physical values follow the CF packing: raw 75 -> 69.5 dBZ; the fill
    // reads as missing.
    let vel = sweep.field(&FieldName::Other("VEL".into())).unwrap();
    assert_eq!(
        vel.data.transform().and_then(|t| t.scale_factor()),
        Some(0.3777165412902832)
    );
    let fill_gate = values.iter().position(|v| *v == -128).unwrap();
    assert_eq!(dbz.value(fill_gate / 1107, fill_gate % 1107), None);
    let max_gate = values.iter().position(|v| *v == 75).unwrap();
    assert_eq!(dbz.value(max_gate / 1107, max_gate % 1107), Some(69.5));
}

#[test]
fn dow8_rhi_keeps_int16_packing_and_the_moving_platform_track() {
    let Some(volume) = decode("cfrad1-dow8-20211011-223602-rhi-trim3-classic") else {
        return;
    };
    assert_eq!(volume.attrs.instrument_name, "DOW8");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("ILLINOIS"));
    assert_eq!(
        volume.provenance.source_conventions.as_deref(),
        Some("CF-1.7")
    );
    assert_eq!(volume.volume_number, Some(255));
    assert_eq!(volume.radar_parameters.frequency_hz, vec![9449999360.0]);
    assert_eq!(volume.location.latitude_deg, Some(40.01481246948242));
    assert_eq!(volume.location.altitude_m, Some(214.00000154972076));
    assert_eq!(volume.sweeps.len(), 1);
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_mode, SweepMode::Rhi);
    assert_eq!(sweep.prt_mode, Some(PrtMode::Staggered));
    assert_eq!(sweep.fixed_angle_deg, 184.00023);
    assert_eq!(sweep.nrays(), 148);
    assert_eq!(sweep.rays.azimuth_deg[0], 182.1148681640625);
    assert_eq!(sweep.rays.elevation_deg[0], 1.5);
    assert_eq!(sweep.rays.elevation_deg[147], 70.0);
    // The file stores `time` as float32 (0.712 reads as 0.71199989).
    assert!((volume.ray_time(0, 0).unwrap().timestamp_millis() - 1_633_991_762_712).abs() <= 1);
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2021, 10, 11, 22, 36, 2).unwrap()
    );
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = sweep.range
    else {
        panic!("DOW8 range is uniform");
    };
    assert_eq!(first_center_m, 62.456512451171875);
    assert!((spacing_m - 124.913025).abs() < 1e-4);
    assert_eq!(ngates, 950);
    // Per-ray platform position and attitude (not FM301) and the georef
    // passthrough.
    let track = sweep.platform_track.as_ref().unwrap();
    assert_eq!(track.latitude_deg.len(), 148);
    assert_eq!(track.latitude_deg[0], 40.01481246948242);
    assert_eq!(track.altitude_agl_m.as_ref().unwrap()[0], -9999.0);
    let extra: Vec<&str> = sweep.extra_vars.iter().map(|v| &*v.name).collect();
    for name in [
        "georefs_applied",
        "georef_time",
        "georef_unit_num",
        "georef_unit_id",
    ] {
        assert!(extra.contains(&name), "{name} in {extra:?}");
    }
    assert_eq!(sweep.ray_vars.antenna_transition.as_ref().unwrap()[0], 1);
    assert_eq!(sweep.ray_vars.n_samples.as_ref().unwrap()[0], 60);
    assert_eq!(
        sweep.ray_vars.nyquist_velocity_mps.as_ref().unwrap()[0],
        19.827543258666992
    );

    let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["DBZHC", "VEL", "WIDTH"]);
    let dbzhc = sweep.field(&FieldName::Other("DBZHC".into())).unwrap();
    let FieldData::I16 { values, coding } = &dbzhc.data else {
        panic!("DBZHC is int16");
    };
    assert_eq!(coding.fill_value, Some(-32768));
    assert_eq!(coding.transform.attr_width(), FloatWidth::F32);
    assert_eq!(coding.transform.scale_factor(), Some(0.009999999776482582));
    assert_eq!(
        values.iter().map(|v| i64::from(*v)).sum::<i64>(),
        -2_395_589_720
    );
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(
        sha256_hex(&bytes),
        "cb8c0ac31239b09fd3d7250fb1cc610c42bda864ab8564516494e7fc23c379da"
    );
    assert_eq!(dbzhc.attrs.standard_name.as_deref(), Some("DBZHC"));
    assert_eq!(dbzhc.quantity, recast_radar_core::Quantity::Reflectivity);
    let max_gate = values.iter().position(|v| *v == 4953).unwrap();
    let value = dbzhc.value(max_gate / 950, max_gate % 950).unwrap();
    assert!((value - 49.53).abs() < 1e-4);
    let georef_time = sweep
        .extra_vars
        .iter()
        .find(|v| &*v.name == "georef_time")
        .unwrap();
    assert_eq!(georef_time.shape, vec![148]);
    assert!(matches!(
        georef_time.values,
        recast_radar_core::model::ArrayBuf::F64(_)
    ));
}
