//! Native FM301 decoding of real ODIM_H5 volumes, spot-checked against what
//! xradar 0.12.0 `open_odim_datatree(first_dim="time", mask_and_scale=False)`
//! returns for the same files (`tools/fm301_golden.py` goldens on the F.4
//! branch; the values below are copied from them). The full conformance
//! comparison is plan F.4; these tests pin the decoder's ray coordinates,
//! range, field names, codings and raw values.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::excessive_precision)]

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{
    AttrValue, FieldData, FieldName, LinearTransform, Quantity, RadarCalibration, RangeCoord,
    Scalar, Sweep, Volume,
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

// ---------------------------------------------------------------------------
// `how` metadata (ODIM_H5 v2.4 Table 8). Expected values: tools/
// golden_io_formats.py, section `odim`, key `how_constants` (h5py): the root
// and first-dataset site constants, and per dataset its constants, its
// other attributes and the names of its per-ray arrays.
// ---------------------------------------------------------------------------

fn names(attrs: &[(Box<str>, AttrValue)]) -> Vec<&str> {
    let mut names: Vec<&str> = attrs.iter().map(|(name, _)| &**name).collect();
    names.sort_unstable();
    names
}

fn attr<'a>(attrs: &'a [(Box<str>, AttrValue)], name: &str) -> &'a AttrValue {
    attrs
        .iter()
        .find(|(key, _)| &**key == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("no attribute {name}"))
}

fn f64_attr(value: f64) -> AttrValue {
    AttrValue::Scalar(Scalar::F64(value))
}

/// Every ray of `values` equals `expected`.
fn per_ray<T: PartialEq + std::fmt::Debug + Copy>(
    values: &Option<Vec<T>>,
    rays: usize,
    expected: T,
) {
    assert_eq!(values.as_deref(), Some(&vec![expected; rays][..]));
}

/// The radar constants of one calibration entry, in `f32`.
fn constants(entry: &RadarCalibration) -> (Option<i32>, [Option<f32>; 5]) {
    (
        entry.calib_index,
        [
            entry.radar_constant_h,
            entry.radar_constant_v,
            entry.antenna_gain_h_db,
            entry.antenna_gain_v_db,
            entry.pulse_width_s,
        ],
    )
}

/// iesha: site constants in the dataset `how` groups, and a different pulse
/// width and radar constant for dataset 10.
#[test]
fn iesha_how_constants_per_dataset() {
    let Some(volume) = decode("odim-iesha-20260305-0115-pvol") else {
        return;
    };
    let parameters = &volume.radar_parameters;
    assert_eq!(parameters.beam_width_h_deg, Some(0.955));
    assert_eq!(parameters.beam_width_v_deg, Some(0.942));
    assert_eq!(parameters.antenna_gain_h_db, Some(45.0));
    assert_eq!(parameters.antenna_gain_v_db, Some(45.0));
    assert_eq!(parameters.receiver_bandwidth_hz, None);
    let entries: Vec<_> = volume.radar_calibration.iter().map(constants).collect();
    assert_eq!(
        entries,
        [
            (
                Some(0),
                [
                    Some(67.949),
                    Some(68.456),
                    Some(45.0),
                    Some(45.0),
                    Some(2e-6)
                ]
            ),
            (
                Some(1),
                [
                    Some(70.167),
                    Some(70.674),
                    Some(45.0),
                    Some(45.0),
                    Some(1.2e-6)
                ]
            ),
        ]
    );
    // Datasets 1-9: rpm 4, pulsewidth 2.0; dataset 10: rpm 5, 1.2.
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        let rays = sweep.nrays();
        let (entry, rate, width) = if index < 9 {
            (0, 24.0, 2e-6)
        } else {
            (1, 30.0, 1.2e-6)
        };
        per_ray(&sweep.ray_vars.calib_index, rays, entry);
        per_ray(&sweep.ray_vars.pulse_width_s, rays, width);
        assert_eq!(sweep.target_scan_rate_deg_per_s, Some(rate));
        // The other 25 dataset attributes stay verbatim; startazA/stopazA are
        // the ray azimuths.
        assert_eq!(
            names(&sweep.other),
            [
                "BBC",
                "CSR",
                "Dclutter",
                "LOG",
                "NEZH",
                "NEZV",
                "RXlossH",
                "RXlossV",
                "SQI",
                "TXlossH",
                "TXlossV",
                "VPRCorr",
                "Vsamples",
                "anglesync",
                "anglesyncRes",
                "astart",
                "clutterType",
                "highprf",
                "lowprf",
                "polmode",
                "poltype",
                "radomelossH",
                "radomelossV",
                "scan_count",
                "scan_index",
            ],
            "sweep {index}"
        );
    }
    let first = &volume.sweeps[0].other;
    let last = &volume.sweeps[9].other;
    assert_eq!(attr(first, "NEZH"), &f64_attr(-47.7944));
    assert_eq!(attr(last, "NEZH"), &f64_attr(-43.3579));
    assert_eq!(attr(first, "Vsamples"), &AttrValue::Scalar(Scalar::I64(25)));
    assert_eq!(attr(last, "highprf"), &f64_attr(1000.0));
    assert_eq!(attr(first, "Dclutter"), &AttrValue::text("DFT,FFT,Spatial"));
    // Root: `beamwidth` is not used (the datasets give beamwH and beamwV).
    assert_eq!(
        names(&volume.attrs.other),
        [
            "beamwidth",
            "endepochs",
            "highprf",
            "lowprf",
            "scan_optimized",
            "software",
            "startepochs",
            "sw_version",
            "system",
            "wavelength",
        ]
    );
    assert_eq!(
        attr(&volume.attrs.other, "system"),
        &AttrValue::text("Leo. Meteor 735CDP")
    );
}

/// dkrom: site constants and rpm/pulsewidth in the root `how` only, with
/// the writer's own names (`antgain`, `TXpower`) kept verbatim.
#[test]
fn dkrom_how_constants_from_the_root() {
    let Some(volume) = decode("odim-dkrom-20260820-1130-pvol") else {
        return;
    };
    let parameters = &volume.radar_parameters;
    assert_eq!(parameters.beam_width_h_deg, Some(0.95));
    assert_eq!(parameters.beam_width_v_deg, Some(0.95));
    assert_eq!(parameters.antenna_gain_h_db, None, "`antgain` is not ODIM");
    assert_eq!(parameters.receiver_bandwidth_hz, Some(1.382e6));
    assert!(volume.radar_calibration.is_empty());
    for sweep in &volume.sweeps {
        let rays = sweep.nrays();
        assert_eq!(
            sweep.target_scan_rate_deg_per_s,
            Some((3.093_332_095_999_999_7 * 6.0) as f32)
        );
        per_ray(&sweep.ray_vars.pulse_width_s, rays, 0.8e-6);
        assert_eq!(sweep.ray_vars.calib_index, None);
        // Per-ray angles and times written as text stay verbatim.
        assert_eq!(names(&sweep.other), ["azangels", "aztimes", "elangels"]);
    }
    let azangels = attr(&volume.sweeps[0].other, "azangels").as_text().unwrap();
    assert_eq!(azangels.len(), 5659);
    assert!(azangels.starts_with("359.643:360.510864,0.664673:1.52161,"));
    let root = &volume.attrs.other;
    assert_eq!(
        names(root),
        [
            "RXloss",
            "SQI",
            "TXloss",
            "TXpower",
            "ZDR offset[dB]",
            "antgain",
            "beamwidth",
            "ccor",
            "clutterfilter",
            "gasattn",
            "log noise threshold",
            "lslope",
            "maxrange",
            "nscans",
            "number of rays",
            "polarity",
            "prf",
            "prffac",
            "pulseindex",
            "rgain",
            "samples",
            "sloss",
            "system",
            "task",
            "wavelength",
        ]
    );
    assert_eq!(attr(root, "ZDR offset[dB]"), &f64_attr(-1.188));
    assert_eq!(
        attr(root, "clutterfilter"),
        &AttrValue::Scalar(Scalar::I64(3))
    );
    assert_eq!(attr(root, "antgain"), &f64_attr(45.0));
}

/// espdg: root constants apply to both datasets; the pulse width 1e-06 (not
/// microseconds) and the zero bandwidth stay verbatim.
#[test]
fn espdg_how_constants_and_unconverted_values() {
    let Some(volume) = decode("odim-espdg-20260707-1927-pvol-dbzh-vradh") else {
        return;
    };
    let parameters = &volume.radar_parameters;
    assert_eq!(parameters.beam_width_h_deg, Some(0.950_000_04));
    assert_eq!(parameters.antenna_gain_v_db, Some(45.0));
    assert_eq!(parameters.receiver_bandwidth_hz, None);
    let entries: Vec<_> = volume.radar_calibration.iter().map(constants).collect();
    assert_eq!(
        entries,
        [(
            Some(0),
            [Some(67.79), Some(70.69), Some(45.0), Some(45.0), None]
        )]
    );
    for sweep in &volume.sweeps {
        let rays = sweep.nrays();
        per_ray(&sweep.ray_vars.calib_index, rays, 0);
        assert_eq!(sweep.ray_vars.pulse_width_s, None);
        assert_eq!(sweep.target_scan_rate_deg_per_s, Some(16.0), "antspeed");
        per_ray(&sweep.ray_vars.nyquist_velocity_mps, rays, 39.9217);
        assert_eq!(names(&sweep.other), ["scan_index"]);
    }
    let root = &volume.attrs.other;
    assert_eq!(
        names(root),
        [
            "Dclutter",
            "NEZH",
            "NEZV",
            "RXbandwidth",
            "RXlossH",
            "RXlossV",
            "Vsamples",
            "azmethod",
            "binmethod",
            "frequency",
            "highprf",
            "lowprf",
            "melting_layer_top",
            "peakpwr",
            "polmode",
            "poltype",
            "pulsewidth",
            "scan_count",
            "simulated",
            "software",
            "sw_version",
            "system",
            "task",
            "wavelength",
            "zcalH",
            "zcalV",
        ]
    );
    assert_eq!(attr(root, "pulsewidth"), &f64_attr(1e-06));
    assert_eq!(attr(root, "RXbandwidth"), &f64_attr(0.0));
}

/// norst: the older root `beamwidth`, rpm per dataset, and a non-standard
/// `radarconstH` kept verbatim (no calibration entry).
#[test]
fn norst_how_constants_with_older_names() {
    let Some(volume) = decode("odim-norst-20170421-0908-pvol") else {
        return;
    };
    let parameters = &volume.radar_parameters;
    assert_eq!(parameters.beam_width_h_deg, Some(0.95));
    assert_eq!(parameters.beam_width_v_deg, Some(0.95));
    assert!(volume.radar_calibration.is_empty());
    assert!(volume.attrs.other.is_empty());
    let rates: Vec<Option<f32>> = volume
        .sweeps
        .iter()
        .map(|sweep| sweep.target_scan_rate_deg_per_s)
        .collect();
    let rpm = [1.0, 1.166_666_666_666_666_7, 2.5, 2.5, 2.5, 2.5];
    let expected: Vec<Option<f32>> = rpm.iter().map(|rpm| Some((rpm * 6.0) as f32)).collect();
    assert_eq!(rates, expected);
    for sweep in &volume.sweeps {
        assert_eq!(names(&sweep.other), ["NEZ", "radarconstH"]);
        assert_eq!(attr(&sweep.other, "radarconstH"), &f64_attr(10.9826));
        assert_eq!(attr(&sweep.other, "NEZ"), &f64_attr(0.0));
        assert_eq!(sweep.ray_vars.pulse_width_s, None);
    }
}
