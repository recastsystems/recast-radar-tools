//! Real-data test for the CfRadial 1.x decoder on a Radx-written classic file.
//!
//! Input: corpus entry `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`
//! (SMART-R2 C-band PPI volume during the Hurricane Irene landfall,
//! 2011-08-27 12:04:20Z; classic CfRadial 1.3, 2 sweeps, int8-packed DBZ/VEL
//! with scale/offset and `_FillValue`, per-ray prt / nyquist /
//! unambiguous_range). It replaces BowEcho's synthetic `cfrad_synth.nc`.
//!
//! Expected values: `tools/golden_io_formats.py`, section `cfradial`, key
//! `irene` (Py-ART `read_cfradial`, xradar `open_cfradial1_datatree`, and
//! netCDF4-python raw variables), plus key `xsapr_classic` for the
//! record-variable (UNLIMITED `time`) per-ray instrument path.

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{Field, FieldName, SweepMode};

const IRENE: &str = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01";
const XSAPR_CLASSIC: &str = "cfrad1-xsapr-sgp-20110520-ppi-classic";

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

fn field<'s>(sweep: &'s recast_radar_core::model::Sweep, name: &str) -> &'s Field {
    sweep
        .field(&FieldName::parse(name))
        .unwrap_or_else(|| panic!("no field {name}"))
}

/// Ray time in milliseconds after the volume time reference.
fn time_offset_ms(sweep: &recast_radar_core::model::Sweep, ray: usize) -> i64 {
    (sweep.rays.time_s[ray] * 1000.0).round() as i64
}

#[test]
fn decodes_real_irene_cfradial1_volume() {
    let bytes = corpus(IRENE);
    assert!(recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(&bytes));
    let volume = recast_radar_io_cfradial::cfradial::read_cfradial1_volume(&bytes)
        .expect("decode Irene CfRadial");

    // instrument_name / site_name, latitude/longitude/altitude (Py-ART).
    assert_eq!(volume.attrs.instrument_name, "CPOLRVP");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("CPOLRVP"));
    assert_close(
        volume.location.latitude_deg.unwrap(),
        34.733_105,
        1e-5,
        "lat",
    );
    assert_close(
        volume.location.longitude_deg.unwrap(),
        -76.661_91,
        1e-5,
        "lon",
    );
    assert_eq!(volume.location.altitude_m, Some(0.0));
    // time_coverage_start = 2011-08-27T12:04:20Z
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2011, 8, 27, 12, 4, 20).unwrap()
    );
    assert_eq!(
        volume.provenance.source_version.as_deref(),
        Some("CF-Radial-1.3")
    );
    // Py-ART nrays 719.
    assert_eq!(volume.provenance.decode.decoded_ray_count, 719);

    // fixed_angle [0.80200195, 1.4996338]; rays per sweep 360 / 359
    // (sweep_start/end_ray_index, xradar azimuth sizes).
    assert_eq!(volume.sweeps.len(), 2);
    assert_eq!(volume.sweeps[0].fixed_angle_deg, 0.802_001_95);
    assert_eq!(volume.sweeps[1].fixed_angle_deg, 1.499_633_8);
    assert_eq!(volume.sweeps[0].nrays(), 360);
    assert_eq!(volume.sweeps[1].nrays(), 359);

    for (index, sweep) in volume.sweeps.iter().enumerate() {
        // sweep_mode = azimuth_surveillance for both sweeps.
        assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
        // range centres 0, 75, ... (1107 gates).
        assert_eq!(sweep.range.center_m(0), Some(0.0), "sweep {index}");
        assert_eq!(sweep.range.spacing_m(), Some(75.0), "sweep {index}");
        assert_eq!(sweep.range.ngates(), 1107, "sweep {index}");
        // prt, unambiguous_range and nyquist_velocity are (time) variables,
        // kept per ray: prt 0.00055555557 s, unambiguous_range 83275.68 m,
        // nyquist_velocity 47.97 m/s on every ray.
        let vars = &sweep.ray_vars;
        let nrays = sweep.nrays();
        for (name, values, expected) in [
            ("prt", &vars.prt_s, 0.000_555_555_57f32),
            ("unambiguous_range", &vars.unambiguous_range_m, 83_275.68),
            ("nyquist_velocity", &vars.nyquist_velocity_mps, 47.97),
        ] {
            let values = values.as_deref().unwrap_or_else(|| panic!("{name}"));
            assert_eq!(values.len(), nrays, "sweep {index} {name}");
            assert!(
                values.iter().all(|value| *value == expected),
                "sweep {index} {name}"
            );
        }
        // The file has n_samples, not pulse_count / independent_samples.
        assert!(vars.n_samples.is_some());
        assert_eq!(vars.independent_samples, None);
    }

    let low = &volume.sweeps[0];
    let high = &volume.sweeps[1];
    // Ray 0: azimuth 314.20074, elevation 0.7910156, time 0.76 s.
    assert_eq!(low.rays.azimuth_deg[0], 314.200_74);
    assert_eq!(low.rays.elevation_deg[0], 0.791_015_6);
    assert!((time_offset_ms(low, 0) - 760).abs() <= 1);
    assert_eq!(low.rays.azimuth_deg[3], 317.197_27);
    // Ray 360 = second sweep row 0: azimuth 315.19775, time 12.811 s.
    assert_eq!(high.rays.azimuth_deg[0], 315.197_75);
    assert!((time_offset_ms(high, 0) - 12_811).abs() <= 1);
    // Ray 718 = second sweep row 358: elevation 1.472168, time 24.811 s.
    assert_eq!(high.rays.elevation_deg[358], 1.472_168);
    assert!((time_offset_ms(high, 358) - 24_811).abs() <= 1);

    // DBZ = raw * 0.5 + 32, _FillValue -128 masked (Py-ART masked array).
    let low_dbz = field(low, "DBZ");
    let high_dbz = field(high, "DBZ");
    for (dbz, row, gate, expected) in [
        (low_dbz, 0, 0, -31.5),
        (low_dbz, 0, 100, 27.0),
        (low_dbz, 10, 200, 21.5),
        (low_dbz, 180, 50, 16.0),
        (high_dbz, 0, 10, 42.0),
        (high_dbz, 140, 300, 23.0),
        (high_dbz, 358, 700, 34.5),
    ] {
        assert_eq!(dbz.value(row, gate), Some(expected), "DBZ[{row},{gate}]");
    }
    assert_eq!(
        low_dbz.value(359, 1106),
        None,
        "DBZ[359,1106] is _FillValue"
    );
    // Py-ART max 69.5 dBZ at ray 68 gate 17.
    assert_eq!(low_dbz.value(68, 17), Some(69.5));

    // VEL = raw * 0.37771654 (no fill gates in this file).
    let low_vel = field(low, "VEL");
    let high_vel = field(high, "VEL");
    for (vel, row, gate, expected) in [
        (low_vel, 0, 0, 20.018_976),
        (low_vel, 0, 100, -6.798_898),
        (low_vel, 10, 200, -3.399_449),
        (low_vel, 180, 50, 7.932_047),
        (low_vel, 359, 1106, -20.396_694),
        (high_vel, 0, 10, 1.510_866_2),
        (high_vel, 140, 300, 3.777_165_4),
    ] {
        assert_close(
            f64::from(vel.value(row, gate).expect("VEL gate")),
            expected,
            1e-4,
            &format!("VEL[{row},{gate}]"),
        );
    }

    // Whole-sweep valid-gate counts (Py-ART): DBZ 374741 / 370495; VEL has
    // no fill (398520 / 397413 = every gate).
    let finite = |field: &Field| -> usize {
        let (rays, gates) = field.shape();
        (0..rays)
            .flat_map(|ray| (0..gates).map(move |gate| (ray, gate)))
            .filter(|&(ray, gate)| field.value(ray, gate).is_some_and(f32::is_finite))
            .count()
    };
    assert_eq!(finite(low_dbz), 374_741);
    assert_eq!(finite(high_dbz), 370_495);
    assert_eq!(finite(low_vel), 398_520);
    assert_eq!(finite(high_vel), 397_413);
}

#[test]
fn record_variable_ray_metadata_decodes_from_unlimited_time() {
    // X-SAPR classic conversion: time is UNLIMITED, so prt(time),
    // unambiguous_range(time) and the field rows are record-interleaved.
    let bytes = corpus(XSAPR_CLASSIC);
    let volume = recast_radar_io_cfradial::cfradial::read_cfradial1_volume(&bytes)
        .expect("decode X-SAPR classic");
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 40);
    let vars = &sweep.ray_vars;
    let prt = vars.prt_s.as_deref().expect("prt");
    let unambiguous = vars
        .unambiguous_range_m
        .as_deref()
        .expect("unambiguous_range");
    let nyquist = vars
        .nyquist_velocity_mps
        .as_deref()
        .expect("nyquist_velocity");
    for ray in [0, 3, 39] {
        // golden xsapr_classic.ray: prt 0.00045004502 s, 67460.17 m.
        assert_eq!(prt[ray], 0.000_450_045_02, "ray {ray}");
        assert_close(f64::from(unambiguous[ray]), 67_460.17, 0.1, "unamb");
        assert_eq!(nyquist[ray], 17.220_499);
    }
    // refl_fill_first [3, 37]: the _FillValue gate decodes as no data.
    assert_eq!(field(sweep, "reflectivity_horizontal").value(3, 37), None);
}

#[test]
fn level2_decoder_is_not_fooled_by_netcdf_magic() {
    // The router must send CDF files here: the HDF5 and DORADE sniffers must
    // reject both real classic CfRadial files (manifest format `cfradial1`).
    for id in [IRENE, XSAPR_CLASSIC] {
        let bytes = corpus(id);
        assert!(
            recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(&bytes),
            "{id}"
        );
        assert!(
            !recast_radar_io_odim::odim::looks_like_hdf5_bytes(&bytes),
            "{id}"
        );
        assert!(
            !recast_radar_io_dorade::dorade::looks_like_dorade_bytes(&bytes),
            "{id}"
        );
    }
}
