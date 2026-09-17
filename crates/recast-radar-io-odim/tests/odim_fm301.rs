//! Native FM301 decoding of real ODIM_H5 volumes, spot-checked against what
//! xradar 0.12.0 `open_odim_datatree(first_dim="time", mask_and_scale=False)`
//! returns for the same files (`tools/fm301_golden.py` goldens on the F.4
//! branch; the values below are copied from them). The full conformance
//! comparison is plan F.4; these tests pin the decoder's ray coordinates,
//! range, field names, codings and raw values.

#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::excessive_precision)]

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{
    FieldData, FieldName, LinearTransform, Quantity, RangeCoord, Sweep, Volume,
};
use recast_radar_io_odim::read_odim_h5_volume;
use recast_radar_testdata::{require_file, sha256_hex};

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
    Some(read_odim_h5_volume(&bytes).expect("decode ODIM_H5"))
}

/// SHA-256 of a `u8` field's rows in acquisition order (rays sorted by time,
/// stable), the row-major hash the xradar `time` view golden records.
fn raw_u8_hash_in_time_order(sweep: &Sweep, name: FieldName) -> String {
    let field = sweep.field(&name).expect("field");
    let FieldData::U8 { values, .. } = &field.data else {
        panic!("{name} is not uint8");
    };
    let mut order: Vec<usize> = (0..sweep.nrays()).collect();
    order.sort_by(|a, b| sweep.rays.time_s[*a].total_cmp(&sweep.rays.time_s[*b]));
    let ngates = field.ngates as usize;
    let mut bytes = Vec::with_capacity(values.len());
    for ray in order {
        bytes.extend_from_slice(&values[ray * ngates..(ray + 1) * ngates]);
    }
    sha256_hex(&bytes)
}

fn raw_u8_sum(sweep: &Sweep, name: FieldName) -> u64 {
    let FieldData::U8 { values, .. } = &sweep.field(&name).expect("field").data else {
        panic!("{name} is not uint8");
    };
    values.iter().map(|v| u64::from(*v)).sum()
}

#[test]
fn iesha_pvol_matches_xradar_ray_coordinates_and_raw_planes() {
    let Some(volume) = decode("odim-iesha-20260305-0115-pvol") else {
        return;
    };
    assert_eq!(volume.attrs.instrument_name, "IESHA");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Shannon"));
    assert_eq!(volume.attrs.wmo.id.as_deref(), Some("03962"));
    assert_eq!(
        volume.provenance.source_conventions.as_deref(),
        Some("ODIM_H5/V2_3")
    );
    assert_eq!(
        volume.provenance.source_version.as_deref(),
        Some("H5rad 2.3")
    );
    assert_eq!(volume.location.latitude_deg, Some(52.692787));
    assert_eq!(volume.location.longitude_deg, Some(-8.919994));
    assert_eq!(volume.location.altitude_m, Some(29.0));
    // The nominal volume time is the epoch of every ray time.
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2026, 3, 5, 1, 15, 0).unwrap()
    );
    let coverage = volume.time_coverage.expect("time coverage");
    assert_eq!(
        coverage.start.timestamp(),
        Utc.with_ymd_and_hms(2026, 3, 5, 1, 15, 4)
            .unwrap()
            .timestamp()
    );
    assert_eq!(
        coverage.end.timestamp(),
        Utc.with_ymd_and_hms(2026, 3, 5, 1, 19, 27)
            .unwrap()
            .timestamp()
    );
    assert_eq!(volume.sweeps.len(), 10);
    assert_eq!(volume.provenance.decode.decoded_ray_count, 3600);
    let fixed: Vec<f32> = volume.sweeps.iter().map(|s| s.fixed_angle_deg).collect();
    assert_eq!(&fixed[..3], &[0.5, 1.1, 2.2]);
    assert_eq!(fixed[9], 90.0);

    // sweep_0: 360 rays x 497 gates, gate centres 250 + 500 j.
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_number, 0);
    assert_eq!(sweep.nrays(), 360);
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 250.0,
            spacing_m: 500.0,
            ngates: 497
        }
    );
    // Measured azimuths from how/startazA and stopazA; the first radiated
    // ray (where/a1gate = 136) is at storage index 136.
    assert_eq!(sweep.rays.azimuth_deg[136], 136.5107421875);
    assert_eq!(sweep.rays.azimuth_deg[137], 137.51861572265625);
    assert_eq!(
        sweep
            .rays
            .azimuth_deg
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min),
        0.50811767578125
    );
    assert!(sweep.rays.elevation_deg.iter().all(|e| *e == 0.5));
    // Ray times spread over what/starttime..endtime from a1gate.
    let reference = volume.time_reference.timestamp() as f64;
    assert!((sweep.rays.time_s[136] + reference - 1772673553.0208333).abs() < 1e-5);
    assert!((sweep.rays.time_s[137] + reference - 1772673553.0625).abs() < 1e-5);
    // The last radiated ray: numpy's `arange` accumulates rounding at this
    // magnitude (xradar's value is 2.9e-5 s later), so compare loosely.
    assert!((sweep.rays.time_s[135] + reference - 1772673567.979195).abs() < 1e-3);
    assert_eq!(
        sweep.ray_vars.nyquist_velocity_mps.as_deref().map(|v| v[0]),
        Some(7.9785)
    );

    // Fields verbatim, in file order, uint8 with nodata / undetect kept apart.
    let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["DBZH", "TH", "VRADH"]);
    let dbzh = sweep.field(&FieldName::Dbzh).unwrap();
    let FieldData::U8 { coding, .. } = &dbzh.data else {
        panic!("DBZH storage");
    };
    assert_eq!(coding.fill_value, Some(255));
    assert_eq!(coding.undetect, Some(0));
    assert_eq!(coding.transform.scale_factor(), 0.5);
    assert_eq!(coding.transform.add_offset(), -32.0);
    assert_eq!(dbzh.shape(), (360, 497));
    assert_eq!(raw_u8_sum(sweep, FieldName::Dbzh), 11_025_028);
    assert_eq!(
        raw_u8_hash_in_time_order(sweep, FieldName::Dbzh),
        "b727d69b62614ddcd15ea97d1b72a0652035550db3d9f46f088e9a8c3c4ed4fc"
    );
    assert_eq!(
        raw_u8_hash_in_time_order(sweep, FieldName::Vradh),
        "4233cfc0aa770bb60c7ccde3ef5d2db74899646d0cded6e7efa455833807b92b"
    );
    let vradh = sweep.field(&FieldName::Vradh).unwrap();
    assert_eq!(
        vradh.data.transform(),
        Some(LinearTransform::CfScaleOffset {
            scale_factor: 0.06299212598425197,
            add_offset: -8.062992125984252,
            attr_width: recast_radar_core::model::FloatWidth::F64,
        })
    );
    // ODIM TH is logarithmic total power (dBZ), whatever the FM301 table says.
    let th = sweep.field(&FieldName::Th).unwrap();
    assert_eq!(th.quantity, Quantity::TotalPower);
    assert_eq!(th.attrs.units.as_deref(), Some("dBZ"));
    // undetect (0) reads as Undetect, nodata (255) as Missing, and physical
    // values follow the CF packing.
    let top = &volume.sweeps[9];
    assert_eq!(top.range.ngates(), 100);
    assert_eq!(raw_u8_sum(top, FieldName::Th), 81_944);
    assert!(top.fields.iter().all(|f| f.shape() == (360, 100)));
}

#[test]
fn dkrom_pvol_synthesizes_azimuths_and_keeps_every_quantity() {
    let Some(volume) = decode("odim-dkrom-20260820-1130-pvol") else {
        return;
    };
    assert_eq!(volume.attrs.instrument_name, "DKROM");
    assert_eq!(volume.sweeps.len(), 10);
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2026, 8, 20, 11, 30, 0).unwrap()
    );
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 360);
    assert!((sweep.fixed_angle_deg - 0.472_412).abs() < 1e-5);
    // No how/startazA: storage-order centres. Equal start and end times:
    // every ray at the sweep start (11:30:00, the nominal time).
    assert_eq!(sweep.rays.azimuth_deg[0], 0.5);
    assert_eq!(sweep.rays.azimuth_deg[359], 359.5);
    assert!(sweep.rays.time_s.iter().all(|t| *t == 0.0));
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 750.0,
            spacing_m: 500.0,
            ngates: 474
        }
    );
    let mut names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["DBZH", "LDR", "PHIDP", "RHOHV", "TH", "VRAD", "WRAD", "ZDR"]
    );
    assert_eq!(raw_u8_sum(sweep, FieldName::Vrad), 4_931_210);
    assert_eq!(
        raw_u8_hash_in_time_order(sweep, FieldName::Vrad),
        "d0db834c5ad9c44c9361f4cb9f2a74a51917a4082879e78f06176ec3f51f04c9"
    );
    assert_eq!(
        raw_u8_hash_in_time_order(sweep, FieldName::Zdr),
        "cc44d14cac95293585961a743c0f13926adf53ce7d6c16ac453eb3992677cfee"
    );
    let top = &volume.sweeps[9];
    assert!((top.fixed_angle_deg - 14.9963).abs() < 1e-4);
    assert_eq!(top.rays.time_s[0], 193.0);
}

#[test]
fn espdg_float64_planes_stay_float64_with_their_sentinels() {
    let Some(volume) = decode("odim-espdg-20260707-1927-pvol-dbzh-vradh") else {
        return;
    };
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 360);
    // rstart 0.2 km: centres 450 + 500 j.
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 450.0,
            spacing_m: 500.0,
            ngates: 299
        }
    );
    // Per-ray elevations from startelA / stopelA.
    assert!(
        sweep
            .rays
            .elevation_deg
            .iter()
            .any(|e| *e != sweep.fixed_angle_deg)
    );
    let dbzh = sweep.field(&FieldName::Dbzh).unwrap();
    let FieldData::F64 { values, coding } = &dbzh.data else {
        panic!("espdg DBZH is float64");
    };
    assert_eq!(values.len(), 360 * 299);
    assert_eq!(coding.fill_value, Some(95.5));
    assert_eq!(coding.undetect, Some(-32.0));
    assert_eq!(coding.transform, None);
    let (min, max) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        });
    assert_eq!((min, max), (-32.0, 46.0));
    // -32 is `_Undetect`, not a value.
    let undetect_gate = values.iter().position(|v| *v == -32.0).unwrap();
    assert_eq!(
        dbzh.gate(undetect_gate / 299, undetect_gate % 299),
        Some(recast_radar_core::Gate::Undetect)
    );
    let vradh = sweep.field(&FieldName::Vradh).unwrap();
    assert!(matches!(vradh.data, FieldData::F64 { .. }));
    let _ = require_file!("odim-espdg-20260707-1927-pvol-dbzh-vradh");
}
