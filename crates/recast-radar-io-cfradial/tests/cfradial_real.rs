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
use recast_radar_core::{MomentType, ScanMode};

const IRENE: &str = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01";
const XSAPR_CLASSIC: &str = "cfrad1-xsapr-sgp-20110520-ppi-classic";

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn decodes_real_irene_cfradial1_volume() {
    let bytes = corpus(IRENE);
    assert!(recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(&bytes));
    let volume = recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(&bytes)
        .expect("decode Irene CfRadial");

    // instrument_name / site_name, latitude/longitude/altitude (Py-ART).
    assert_eq!(volume.site.id, "CPOLRVP");
    assert_eq!(volume.site.name.as_deref(), Some("CPOLRVP"));
    assert_close(volume.site.latitude_deg.unwrap(), 34.733_105, 1e-5, "lat");
    assert_close(volume.site.longitude_deg.unwrap(), -76.661_91, 1e-5, "lon");
    assert_eq!(volume.site.elevation_m, Some(0.0));
    // time_coverage_start = 2011-08-27T12:04:20Z
    assert_eq!(
        volume.volume_time,
        Utc.with_ymd_and_hms(2011, 8, 27, 12, 4, 20).unwrap()
    );
    assert_eq!(
        volume.metadata.archive_version.as_deref(),
        Some("CF-Radial-1.3")
    );
    // sweep_mode = azimuth_surveillance for both sweeps.
    assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
    // Py-ART nrays 719.
    assert_eq!(volume.metadata.decoded_radial_count, 719);
    // prt/unambiguous_range are (time) variables: never collapsed to a
    // volume-level value.
    assert_eq!(volume.metadata.prt_s, None);
    assert_eq!(volume.metadata.unambiguous_range_km, None);

    // fixed_angle [0.80200195, 1.4996338]; rays per sweep 360 / 359
    // (sweep_start/end_ray_index, xradar azimuth sizes).
    assert_eq!(volume.cuts.len(), 2);
    assert_eq!(volume.cuts[0].elevation_deg, 0.802_001_95);
    assert_eq!(volume.cuts[1].elevation_deg, 1.499_633_8);
    assert_eq!(volume.cuts[0].radials.len(), 360);
    assert_eq!(volume.cuts[1].radials.len(), 359);

    for (cut_index, cut) in volume.cuts.iter().enumerate() {
        // range centres 0, 75, ... (1107 gates): spacing 75 m, gate start
        // half a gate before the first centre.
        let gates = &cut.radials[0].gate_range;
        assert_eq!(gates.gate_spacing_m, 75, "cut {cut_index}");
        assert_eq!(gates.gate_count, 1107, "cut {cut_index}");
        assert!(
            (f64::from(gates.first_gate_m) - (0.0 - 37.5)).abs() <= 0.5,
            "cut {cut_index} first gate {}",
            gates.first_gate_m
        );
        assert_eq!(cut.ray_instrument_metadata.len(), cut.radials.len());
        for (ray, radial) in cut.radials.iter().enumerate() {
            // nyquist_velocity 47.97 m/s on every ray.
            assert_eq!(
                radial.nyquist_velocity_mps,
                Some(47.97),
                "cut {cut_index} ray {ray}"
            );
            let meta = &cut.ray_instrument_metadata[ray];
            // prt 0.00055555557 s; unambiguous_range 83275.68 m.
            assert_eq!(meta.prt_s, Some(0.000_555_555_57));
            assert_close(
                meta.unambiguous_range_km.unwrap(),
                83.275_68,
                1e-4,
                "unamb km",
            );
            // The file has n_samples, not pulse_count / independent_samples.
            assert_eq!(meta.pulse_count, None);
            assert_eq!(meta.independent_samples, None);
        }
    }

    let low = &volume.cuts[0];
    let high = &volume.cuts[1];
    // Ray 0: azimuth 314.20074, elevation 0.7910156, time 0.76 s.
    assert_eq!(low.radials[0].azimuth_deg, 314.200_74);
    assert_eq!(low.radials[0].elevation_deg, 0.791_015_6);
    assert!((low.radials[0].time_offset_ms - 760).abs() <= 1);
    assert_eq!(low.radials[3].azimuth_deg, 317.197_27);
    // Ray 360 = second sweep row 0: azimuth 315.19775, time 12.811 s.
    assert_eq!(high.radials[0].azimuth_deg, 315.197_75);
    assert!((high.radials[0].time_offset_ms - 12_811).abs() <= 1);
    // Ray 718 = second sweep row 358: elevation 1.472168, time 24.811 s.
    assert_eq!(high.radials[358].elevation_deg, 1.472_168);
    assert!((high.radials[358].time_offset_ms - 24_811).abs() <= 1);

    // DBZ = raw * 0.5 + 32, _FillValue -128 masked (Py-ART masked array).
    let low_dbz = low.moments.get(&MomentType::Reflectivity).expect("DBZ");
    let high_dbz = high.moments.get(&MomentType::Reflectivity).expect("DBZ");
    for (grid, row, gate, expected) in [
        (low_dbz, 0, 0, -31.5),
        (low_dbz, 0, 100, 27.0),
        (low_dbz, 10, 200, 21.5),
        (low_dbz, 180, 50, 16.0),
        (high_dbz, 0, 10, 42.0),
        (high_dbz, 140, 300, 23.0),
        (high_dbz, 358, 700, 34.5),
    ] {
        assert_eq!(
            grid.scaled_value(row, gate),
            Some(expected),
            "DBZ[{row},{gate}]"
        );
    }
    assert!(
        low_dbz.scaled_value(359, 1106).is_none_or(f32::is_nan),
        "DBZ[359,1106] is _FillValue"
    );
    // Py-ART max 69.5 dBZ at ray 68 gate 17.
    assert_eq!(low_dbz.scaled_value(68, 17), Some(69.5));

    // VEL = raw * 0.37771654 (no fill gates in this file).
    let low_vel = low.moments.get(&MomentType::Velocity).expect("VEL");
    let high_vel = high.moments.get(&MomentType::Velocity).expect("VEL");
    for (grid, row, gate, expected) in [
        (low_vel, 0, 0, 20.018_976),
        (low_vel, 0, 100, -6.798_898),
        (low_vel, 10, 200, -3.399_449),
        (low_vel, 180, 50, 7.932_047),
        (low_vel, 359, 1106, -20.396_694),
        (high_vel, 0, 10, 1.510_866_2),
        (high_vel, 140, 300, 3.777_165_4),
    ] {
        assert_close(
            grid.scaled_value(row, gate).expect("VEL gate"),
            expected,
            1e-4,
            &format!("VEL[{row},{gate}]"),
        );
    }

    // Whole-sweep valid-gate counts (Py-ART): DBZ 374741 / 370495; VEL has
    // no fill (398520 / 397413 = every gate).
    let finite = |grid: &recast_radar_core::MomentGrid, rows: usize| -> usize {
        (0..rows)
            .flat_map(|row| (0..1107).map(move |gate| (row, gate)))
            .filter(|&(row, gate)| grid.scaled_value(row, gate).is_some_and(f32::is_finite))
            .count()
    };
    assert_eq!(finite(low_dbz, 360), 374_741);
    assert_eq!(finite(high_dbz, 359), 370_495);
    assert_eq!(finite(low_vel, 360), 398_520);
    assert_eq!(finite(high_vel, 359), 397_413);
}

#[test]
fn record_variable_ray_metadata_decodes_from_unlimited_time() {
    // X-SAPR classic conversion: time is UNLIMITED, so prt(time),
    // unambiguous_range(time) and the field rows are record-interleaved.
    let bytes = corpus(XSAPR_CLASSIC);
    let volume = recast_radar_io_cfradial::cfradial::decode_cfradial1_volume(&bytes)
        .expect("decode X-SAPR classic");
    let cut = &volume.cuts[0];
    assert_eq!(cut.ray_instrument_metadata.len(), 40);
    for ray in [0, 3, 39] {
        let meta = &cut.ray_instrument_metadata[ray];
        // golden xsapr_classic.ray: prt 0.00045004502 s, 67460.17 m.
        assert_eq!(meta.prt_s, Some(0.000_450_045_02), "ray {ray}");
        assert_close(meta.unambiguous_range_km.unwrap(), 67.460_17, 1e-4, "unamb");
        assert_eq!(cut.radials[ray].nyquist_velocity_mps, Some(17.220_499));
    }
    // refl_fill_first [3, 37]: the _FillValue gate decodes as no data.
    let field = cut
        .moments
        .get(&MomentType::Unknown("reflectivity_horizontal".to_owned()))
        .expect("reflectivity_horizontal");
    assert!(field.scaled_value(3, 37).is_none_or(f32::is_nan));
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
