//! Golden-fixture test for the CfRadial 1.x decoder.
//!
//! Fixture provenance: `tests/data/cfrad_synth.nc` is SYNTHETIC, generated
//! by `tests/data/gen_cfradial_fixture.py` with the netCDF4 python library
//! (1.7.4) in NETCDF3_CLASSIC (CDF-1) format following CfRadial 1.4
//! (Dixon and Lee, NCAR/EOL, 2016). `time` is UNLIMITED, so every per-ray
//! and field variable exercises the record-interleaved data path. The fixture
//! includes varying physical timing/sample metadata on every ray. Values
//! are deterministic ramps: REF[t,r] = t + 0.5·r dBZ (float, one forced
//! fill), VEL raw[t,r] = 10·t + r packed as shorts with scale 0.01,
//! offset −0.5.

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{FieldName, RangeCoord, SweepMode};

const FIXTURE: &[u8] = include_bytes!("data/cfrad_synth.nc");

#[test]
fn decodes_synthetic_cfradial1_volume() {
    assert!(recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(FIXTURE));
    let volume = recast_radar_io_cfradial::cfradial::read_cfradial1_volume(FIXTURE)
        .expect("decode CfRadial fixture");

    assert_eq!(volume.attrs.instrument_name, "SYNTH1");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Synthetic Pad"));
    assert_eq!(volume.location.latitude_deg, Some(39.74));
    assert_eq!(volume.location.longitude_deg, Some(-103.2927));
    assert_eq!(volume.location.altitude_m, Some(1519.0));
    let start = Utc.with_ymd_and_hms(2026, 6, 9, 5, 51, 0).unwrap();
    assert_eq!(volume.time_reference, start);
    assert_eq!(
        volume.time_coverage.map(|coverage| coverage.start),
        Some(start)
    );
    assert_eq!(volume.provenance.decode.decoded_ray_count, 24);
    // Per-time instrument variables must not be collapsed into a misleading
    // volume-level value from the first ray.
    assert_eq!(volume.radar_parameters.prt_s, None);
    assert_eq!(volume.radar_parameters.unambiguous_range_m, None);

    assert_eq!(volume.sweeps.len(), 2);
    assert_eq!(volume.sweeps[0].fixed_angle_deg, 0.5);
    assert_eq!(volume.sweeps[1].fixed_angle_deg, 1.5);
    assert_eq!(volume.sweeps[0].sweep_mode, SweepMode::AzimuthSurveillance);

    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 12);
    assert_eq!(sweep.rays.azimuth_deg[0], 15.0);
    assert_eq!(sweep.rays.azimuth_deg[3], 105.0);
    assert_eq!(sweep.rays.elevation_deg[0], 0.5);
    // range centres start at 125 m with 250 m spacing.
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 125.0,
            spacing_m: 250.0,
            ngates: 20,
        }
    );
    let nyquist = sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()
        .expect("nyquist");
    assert_eq!(nyquist[0], 26.4);
    let prt = sweep.ray_vars.prt_s.as_ref().expect("prt");
    assert_eq!(prt.len(), sweep.nrays());
    assert_eq!(prt[0], 0.001);
    assert!((prt[3] - 0.001_003).abs() < 1.0e-9);
    let unambiguous = sweep
        .ray_vars
        .unambiguous_range_m
        .as_ref()
        .expect("unambiguous range");
    assert!((unambiguous[0] - 149_896.0).abs() < 1.0e-2);
    assert!((unambiguous[3] - 149_596.0).abs() < 1.0);
    let samples = sweep.ray_vars.n_samples.as_ref().expect("n_samples");
    assert_eq!(samples[0], 60);
    assert_eq!(samples[3], 63);
    let independent = sweep
        .ray_vars
        .independent_samples
        .as_ref()
        .expect("independent samples");
    assert_eq!(independent[0], 15.0);
    assert_eq!(independent[3], 15.75);
    // time(time) = 0.5 s steps from time_coverage_start.
    assert_eq!(sweep.rays.time_s[2], 1.0);

    // REF (float): value = t + 0.5·r.
    let reflectivity = sweep.field(&FieldName::parse("REF")).expect("REF");
    assert_eq!(reflectivity.value(0, 2), Some(1.0)); // t=0, r=2
    assert_eq!(reflectivity.value(5, 4), Some(7.0)); // t=5, r=4
    // VEL (packed short): physical = raw·0.01 − 0.5, raw = 10·t + r.
    let velocity = sweep.field(&FieldName::parse("VEL")).expect("VEL");
    assert_eq!(velocity.value(0, 0), Some(-0.5));
    let sampled = velocity.value(3, 7).expect("VEL t=3 r=7");
    assert!((sampled - (37.0 * 0.01 - 0.5)).abs() < 1.0e-4);

    // Sweep 2 (rays 12-23): REF[14,3] was forced to _FillValue, which
    // resolves to no value.
    let upper = &volume.sweeps[1];
    let upper_ref = upper.field(&FieldName::parse("REF")).expect("REF");
    assert_eq!(upper_ref.value(2, 3), None); // global ray 14
    assert_eq!(upper_ref.value(2, 4), Some(16.0)); // 14 + 0.5·4
    assert_eq!(upper.rays.elevation_deg[0], 1.5);
}

#[test]
fn level2_decoder_is_not_fooled_by_netcdf_magic() {
    // The router must send CDF files here, not to the Archive II path; the
    // sniffers must be mutually exclusive on this fixture.
    assert!(!recast_radar_io_odim::odim::looks_like_hdf5_bytes(FIXTURE));
    assert!(!recast_radar_io_dorade::dorade::looks_like_dorade_bytes(
        FIXTURE
    ));
}
