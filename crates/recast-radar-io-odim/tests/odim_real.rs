//! Real-data test for the ODIM_H5 decoder on an operational OPERA polar volume.
//!
//! Input: corpus entry `odim-iesha-20260305-0115-pvol` (Met Eireann Shannon,
//! 2026-03-05 01:15Z, H5rad 2.3, 10 sweeps 0.5-90 deg, DBZH + TH + VRADH u8
//! gzip-chunked planes, widespread echo). It replaces BowEcho's synthetic
//! `odim_pvol_synth.h5`.
//!
//! Expected values: `tools/golden_io_formats.py`, section `odim`, key `iesha`
//! (h5py `what`/`where`/`how` attributes and raw planes, physical =
//! gain * raw + offset with nodata/undetect masked; xradar
//! `open_odim_datatree` sweep sizes, range and azimuth coordinates).

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{Field, FieldName, RangeCoord, SweepMode};

const IESHA: &str = "odim-iesha-20260305-0115-pvol";

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

fn valid_gates(field: &Field) -> usize {
    let (rays, gates) = field.shape();
    (0..rays)
        .flat_map(|ray| (0..gates).map(move |gate| (ray, gate)))
        .filter(|&(ray, gate)| field.value(ray, gate).is_some_and(f32::is_finite))
        .count()
}

fn value(field: &Field, ray: usize, gate: usize) -> f64 {
    f64::from(field.value(ray, gate).expect("valid gate"))
}

#[test]
fn decodes_real_iesha_pvol() {
    let bytes = corpus(IESHA);
    assert!(recast_radar_io_odim::odim::looks_like_hdf5_bytes(&bytes));
    let volume =
        recast_radar_io_odim::odim::read_odim_h5_volume(&bytes).expect("decode iesha PVOL");

    // /what source "WMO:03962,NOD:iesha,PLC:Shannon", date 20260305 time 011500.
    assert_eq!(volume.attrs.instrument_name, "IESHA");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Shannon"));
    // /where lat 52.692787, lon -8.919994, height 29.0.
    assert_close(
        volume.location.latitude_deg.unwrap(),
        52.692_787,
        1e-9,
        "lat",
    );
    assert_close(
        volume.location.longitude_deg.unwrap(),
        -8.919_994,
        1e-9,
        "lon",
    );
    assert_eq!(volume.location.altitude_m, Some(29.0));
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2026, 3, 5, 1, 15, 0).unwrap()
    );
    assert_eq!(
        volume.provenance.source_version.as_deref(),
        Some("H5rad 2.3")
    );
    // /how wavelength 5.319 cm -> 299792458 / 0.05319 Hz.
    assert_eq!(volume.radar_parameters.frequency_hz.len(), 1);
    assert_close(
        volume.radar_parameters.frequency_hz[0],
        299_792_458.0 / 0.05319,
        1e-3,
        "frequency",
    );

    // Ten datasets, elangle ascending; 360 rays each; nbins by tier; rscale
    // 500 m from rstart 0 km (xradar range centres start at 250 m); dataset
    // how/NI 7.9785 m/s, 13.2975 m/s on the vertical sweep.
    let expected = [
        (0.5f32, 497u32, 7.9785f32),
        (1.1, 497, 7.9785),
        (2.2, 497, 7.9785),
        (3.2, 497, 7.9785),
        (4.3, 497, 7.9785),
        (6.9, 497, 7.9785),
        (8.5, 350, 7.9785),
        (10.1, 350, 7.9785),
        (20.0, 240, 7.9785),
        (90.0, 100, 13.2975),
    ];
    assert_eq!(volume.sweeps.len(), expected.len());
    assert_eq!(volume.provenance.decode.decoded_ray_count, 3600);
    for (sweep, (elangle, nbins, nyquist)) in volume.sweeps.iter().zip(expected) {
        assert_eq!(sweep.fixed_angle_deg, elangle);
        assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
        assert_eq!(sweep.nrays(), 360, "{elangle}");
        assert_eq!(
            sweep.range,
            RangeCoord::Uniform {
                first_center_m: 250.0,
                spacing_m: 500.0,
                ngates: nbins
            },
            "{elangle} gates"
        );
        assert!(
            sweep
                .ray_vars
                .nyquist_velocity_mps
                .as_ref()
                .is_some_and(|values| values.iter().all(|value| *value == nyquist)),
            "{elangle} NI"
        );
        // Every plane under its ODIM quantity: DBZH, the unfiltered TH and
        // VRADH.
        let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["DBZH", "TH", "VRADH"], "{elangle} fields");
    }
    // xradar azimuth (from how/startazA, stopazA) of rays 0 and 1 on the
    // lowest sweep: 0.508, 1.524 deg; ray i spans [i, i+1) degrees.
    let azimuths = &volume.sweeps[0].rays.azimuth_deg;
    assert_close(f64::from(azimuths[0]), 0.508_117_7, 1e-6, "az0");
    assert_close(f64::from(azimuths[1]), 1.524_353, 1e-6, "az1");

    // Lowest sweep (dataset1, 0.5 deg): DBZH gain 0.5 offset -32, VRADH gain
    // 0.062992126 offset -8.062992; nodata 255, undetect 0.
    let low = &volume.sweeps[0];
    let dbzh = low.field(&FieldName::Dbzh).expect("DBZH");
    let vradh = low.field(&FieldName::Vradh).expect("VRADH");
    assert_eq!(dbzh.value(0, 0), None, "raw 0 = undetect");
    assert_eq!(dbzh.value(45, 10), None, "raw 0 = undetect");
    assert_eq!(dbzh.value(90, 50), Some(12.0)); // raw 88
    assert_eq!(dbzh.value(180, 100), Some(14.0)); // raw 92
    assert_eq!(dbzh.value(270, 20), Some(17.5)); // raw 99
    assert_close(value(vradh, 90, 50), 2.582_677, 1e-4, "VRADH raw 169");
    assert_close(value(vradh, 180, 100), 5.732_284, 1e-4, "VRADH raw 219");
    assert_close(value(vradh, 270, 20), -2.204_724, 1e-4, "VRADH raw 93");
    assert_eq!(valid_gates(dbzh), 116_929);
    assert_eq!(valid_gates(vradh), 111_860);

    // dataset2 (1.1 deg): DBZH raw 145 -> 40.5, raw 100 -> 18.0; VRADH raw
    // 129 -> 0.0629921.
    let second = &volume.sweeps[1];
    let dbzh = second.field(&FieldName::Dbzh).expect("DBZH");
    let vradh = second.field(&FieldName::Vradh).expect("VRADH");
    assert_eq!(dbzh.value(0, 0), Some(40.5));
    assert_eq!(dbzh.value(45, 10), Some(18.0));
    assert_close(value(vradh, 0, 0), 0.062_992_13, 1e-5, "VRADH 1.1 [0,0]");
    assert_eq!(valid_gates(dbzh), 112_887);
    assert_eq!(valid_gates(vradh), 108_149);

    // 20 deg sweep (240 bins) and the vertical-pointing top sweep (100 bins,
    // DBZH and VRADH entirely undetect in h5py).
    let steep = volume.sweeps[8].field(&FieldName::Dbzh).expect("DBZH");
    assert_eq!(valid_gates(steep), 11_993);
    assert_eq!(steep.value(45, 10), Some(13.0));
    let top = &volume.sweeps[9];
    assert_eq!(valid_gates(top.field(&FieldName::Dbzh).expect("DBZH")), 0);
    assert_eq!(valid_gates(top.field(&FieldName::Vradh).expect("VRADH")), 0);
}

#[test]
fn non_odim_hdf5_is_rejected_with_guidance() {
    // The published netCDF-4 CfRadial file is HDF5 with superblock version 2
    // (golden signatures.netcdf4_superblock_version): not ODIM, and the error
    // tells the user how to convert it.
    let netcdf4 = corpus("cfrad1-xsapr-sgp-20110520-ppi-netcdf4");
    assert!(recast_radar_io_odim::odim::looks_like_hdf5_bytes(&netcdf4));
    assert_eq!(netcdf4[8], 2);
    let message = recast_radar_io_odim::odim::read_odim_h5_volume(&netcdf4)
        .expect_err("netCDF-4 CfRadial is not an ODIM volume")
        .to_string();
    assert!(
        message.contains("netCDF-4 CfRadial") && message.contains("nccopy -k classic"),
        "{message}"
    );

    // An ODIM IMAGE product (/what object = IMAGE, golden
    // signatures.imgw_kdp_object) is HDF5 ODIM but not a polar volume.
    let image = corpus("odim-imgw-ram-20260711-0015-kdp-max");
    let message = recast_radar_io_odim::odim::read_odim_h5_volume(&image)
        .expect_err("IMAGE is not a PVOL/SCAN")
        .to_string();
    assert!(
        message.contains("'IMAGE'") && message.contains("PVOL and SCAN only"),
        "{message}"
    );
}
