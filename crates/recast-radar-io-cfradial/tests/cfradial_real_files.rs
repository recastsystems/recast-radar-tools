//! Golden-fixture tests for the CfRadial 1.x decoder against REAL radar
//! data — a fixed-site PPI and a mobile DOW8 RHI.
//!
//! Fixture provenance (fetched 2026-06-11; golden values extracted with an
//! independent Python reader — netCDF4 1.7.4 — not with this crate):
//!
//! - `tests/data/cfrad.xsapr_sgp_ppi_20110520.classic.nc`: ARM X-SAPR
//!   (X-band scanning ARM precipitation radar) PPI at the SGP site,
//!   2011-05-20 10:54:16 UTC, CF/Radial 1.2, 40 rays x 42 gates,
//!   reflectivity_horizontal. Source: ARM-DOE/pyart
//!   `pyart/testing/data/example_cfradial_ppi.nc` (BSD-3-Clause; itself
//!   gate/ray-decimated from the full X-SAPR file by Py-ART's
//!   `make_small_cfradial_ppi.py`). CONTAINER CONVERSION: the published
//!   file is netCDF-4; converted to NETCDF3_CLASSIC (CDF-1) with a raw
//!   variable-for-variable copy (netCDF4-python, no mask/scale applied) —
//!   identical data, classic container. Wild CfRadial 1.x is
//!   netCDF-4-dominant today; the classic container is what early Radx
//!   wrote and what this decoder reads.
//! - `tests/data/cfrad.20211011_223602_DOW8_RHI.trim3.nc`: FARM facility
//!   DOW8 truck NATIVE RHI near Urbana, Illinois, 2021-10-11 22:36:02 UTC,
//!   CF-Radial-1.4 (Radx), 148 rays sweeping -0.73 deg to 70 deg elevation
//!   at fixed ~184 deg azimuth, 950 gates at 125 m. Source:
//!   openradar/open-radar-data
//!   `cfrad.20211011_223602.712_to_20211011_223612.091_DOW8_RHI.nc` (MIT).
//!   Same netCDF-4 -> CDF-1 raw conversion; TRIMMED: of the 8 field
//!   variables only DBZHC/VEL/WIDTH are kept (drops DBMHC, NCP, SNRHC,
//!   VL1, VS1) to stay under the fixture size budget. Kept fields are
//!   byte-identical to the published file.
//! - `tests/data/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc`: the SAME Py-ART
//!   file in its published netCDF-4 container (HDF5 superblock v2,
//!   unmodified bytes) — pins the routing + guidance for the wild-file
//!   case the classic decoder cannot read.

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{FieldName, Quantity, RangeCoord, SweepMode};

const XSAPR_PPI: &[u8] = include_bytes!("data/cfrad.xsapr_sgp_ppi_20110520.classic.nc");
const DOW8_RHI: &[u8] = include_bytes!("data/cfrad.20211011_223602_DOW8_RHI.trim3.nc");

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn real_xsapr_ppi_decodes_site_geometry_and_gates() {
    assert!(recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(XSAPR_PPI));
    let volume = recast_radar_io_cfradial::cfradial::read_cfradial1_volume(XSAPR_PPI)
        .expect("decode X-SAPR PPI");

    assert_eq!(volume.attrs.instrument_name, "xsapr-sgp");
    assert_close(
        volume.location.latitude_deg.unwrap() as f32,
        36.4908,
        1e-4,
        "lat",
    );
    assert_close(
        volume.location.longitude_deg.unwrap() as f32,
        -97.5942,
        1e-4,
        "lon",
    );
    assert_close(
        volume.location.altitude_m.unwrap() as f32,
        214.0,
        1e-3,
        "alt",
    );
    assert_eq!(
        volume.time_coverage.map(|coverage| coverage.start),
        Some(Utc.with_ymd_and_hms(2011, 5, 20, 10, 54, 16).unwrap())
    );

    assert_eq!(volume.sweeps.len(), 1);
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
    assert_close(sweep.fixed_angle_deg, 0.5, 1e-3, "fixed angle");
    assert_eq!(sweep.nrays(), 40);
    assert_close(sweep.rays.azimuth_deg[0], 359.9368, 1e-3, "az0");
    assert_close(sweep.rays.elevation_deg[0], 0.4834, 1e-3, "el0");
    assert_close(
        sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .expect("nyquist")[0],
        17.2205,
        1e-3,
        "nyq",
    );
    // Py-ART's decimation left range centres 0, 960, ...; the range
    // coordinate keeps the centres, faithful to the file.
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 0.0,
            spacing_m: 960.0,
            ngates: 42,
        }
    );

    // Field name "reflectivity_horizontal" is kept verbatim (CfRadial names
    // are not renamed); its quantity is classified from the Py-ART name.
    let reflectivity = sweep
        .field(&FieldName::parse("reflectivity_horizontal"))
        .expect("reflectivity_horizontal");
    assert_eq!(reflectivity.quantity, Quantity::Reflectivity);
    assert_close(reflectivity.value(0, 0).unwrap(), -6.05, 1e-3, "v[0,0]");
    assert_close(reflectivity.value(0, 21).unwrap(), 23.30, 1e-3, "v[0,21]");
    assert_close(reflectivity.value(10, 14).unwrap(), 25.23, 1e-3, "v[10,14]");
    assert_close(reflectivity.value(20, 10).unwrap(), 20.54, 1e-3, "v[20,10]");
    assert_close(reflectivity.value(39, 41).unwrap(), 19.68, 1e-3, "v[39,41]");
}

#[test]
fn real_dow8_rhi_decodes_scan_mode_geometry_and_gates() {
    assert!(recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(DOW8_RHI));
    let volume = recast_radar_io_cfradial::cfradial::read_cfradial1_volume(DOW8_RHI)
        .expect("decode DOW8 RHI");

    // Mobile platform: latitude/longitude are (time) arrays; first sample.
    assert_eq!(volume.attrs.instrument_name, "DOW8");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("ILLINOIS"));
    assert_close(
        volume.location.latitude_deg.unwrap() as f32,
        40.0148,
        1e-4,
        "lat",
    );
    assert_close(
        volume.location.longitude_deg.unwrap() as f32,
        -88.3318,
        1e-4,
        "lon",
    );
    assert_close(
        volume.location.altitude_m.unwrap() as f32,
        214.0,
        0.5,
        "alt",
    );
    assert_eq!(
        volume.time_coverage.map(|coverage| coverage.start),
        Some(Utc.with_ymd_and_hms(2021, 10, 11, 22, 36, 2).unwrap())
    );

    assert_eq!(volume.sweeps.len(), 1);
    let sweep = &volume.sweeps[0];
    // sweep_mode = "rhi" must surface as SweepMode::Rhi.
    assert_eq!(sweep.sweep_mode, SweepMode::Rhi);
    // RHI convention: the fixed angle is the pointing AZIMUTH (184 deg).
    assert_close(sweep.fixed_angle_deg, 184.0, 1e-3, "fixed azimuth");
    assert_eq!(sweep.nrays(), 148);

    // Rays sweep in elevation at near-constant azimuth.
    assert_close(sweep.rays.azimuth_deg[0], 182.1149, 1e-3, "az0");
    assert_close(sweep.rays.elevation_deg[0], 1.5, 1e-3, "el0");
    assert_close(sweep.rays.elevation_deg[147], 70.0, 1e-3, "el147");
    let (mut el_min, mut el_max) = (f32::INFINITY, f32::NEG_INFINITY);
    for (azimuth, elevation) in sweep.rays.azimuth_deg.iter().zip(&sweep.rays.elevation_deg) {
        el_min = el_min.min(*elevation);
        el_max = el_max.max(*elevation);
        assert!((182.0..=184.2).contains(azimuth), "azimuth fixed");
    }
    assert_close(el_min, -0.7306, 1e-3, "el min");
    assert_close(el_max, 70.0, 1e-3, "el max");

    // Gate geometry from the range coordinate: centres 62.46, 187.37, ...
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = sweep.range
    else {
        panic!("DOW8 range is uniform");
    };
    assert_close(first_center_m as f32, 62.4565, 1e-3, "first centre");
    assert_close(spacing_m as f32, 124.913, 1e-3, "spacing");
    assert_eq!(ngates, 950);
    assert_close(
        sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .expect("nyquist")[0],
        19.8275,
        1e-3,
        "nyq",
    );
    // Ray times: 0.712 s and 10.091 s offsets from time_coverage_start
    // (float32 in the file).
    assert_close(sweep.rays.time_s[0] as f32, 0.712, 1e-5, "time 0");
    assert_close(sweep.rays.time_s[147] as f32, 10.091, 1e-5, "time 147");

    // Golden gates from the independent netCDF4 reader. Names stay
    // verbatim (DBZHC, VEL, WIDTH); quantities come from the name stems.
    let reflectivity = sweep.find(Quantity::Reflectivity).expect("DBZHC");
    assert_eq!(reflectivity.name.as_str(), "DBZHC");
    assert_close(reflectivity.value(0, 0).unwrap(), -2.48, 1e-3, "REF[0,0]");
    assert_close(
        reflectivity.value(0, 475).unwrap(),
        0.26,
        1e-3,
        "REF[0,475]",
    );
    assert_close(
        reflectivity.value(37, 316).unwrap(),
        0.79,
        1e-3,
        "REF[37,316]",
    );
    assert_close(
        reflectivity.value(74, 10).unwrap(),
        -23.30,
        1e-3,
        "REF[74,10]",
    );
    assert!(reflectivity.value(147, 949).is_none(), "REF[147,949] fill");

    let velocity = sweep.find(Quantity::RadialVelocity).expect("VEL");
    assert_eq!(velocity.name.as_str(), "VEL");
    assert_close(velocity.value(0, 0).unwrap(), 0.91, 1e-3, "VEL[0,0]");
    assert_close(velocity.value(0, 475).unwrap(), -16.56, 1e-3, "VEL[0,475]");
    assert_close(
        velocity.value(147, 949).unwrap(),
        -5.09,
        1e-3,
        "VEL[147,949]",
    );

    let width = sweep.find(Quantity::SpectrumWidth).expect("WIDTH");
    assert_eq!(width.name.as_str(), "WIDTH");
    assert_close(width.value(0, 475).unwrap(), 4.37, 1e-3, "SW[0,475]");
}

/// The PUBLISHED Py-ART file before container conversion: netCDF-4, i.e.
/// HDF5 superblock v2. This is what users will actually drop on the app.
const XSAPR_PPI_NETCDF4: &[u8] = include_bytes!("data/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc");

#[test]
fn netcdf4_cfradial_routes_to_hdf5_and_gets_conversion_guidance() {
    // The HDF5 signature must never sniff as netCDF3 — netCDF-4 CfRadial
    // routes to the HDF5/ODIM side (same precedence as the app's sniffer).
    assert!(!recast_radar_io_cfradial::cfradial::looks_like_netcdf3_bytes(XSAPR_PPI_NETCDF4));
    assert!(recast_radar_io_odim::odim::looks_like_hdf5_bytes(
        XSAPR_PPI_NETCDF4
    ));
    assert!(!recast_radar_io_odim::odim::looks_like_hdf5_bytes(
        XSAPR_PPI
    ));
    assert!(!recast_radar_io_dorade::dorade::looks_like_dorade_bytes(
        DOW8_RHI
    ));

    // The explicit error must tell a CfRadial user the fix that works.
    let err = recast_radar_io_odim::odim::read_odim_h5_volume(XSAPR_PPI_NETCDF4).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("netCDF-4 CfRadial") && message.contains("nccopy -k classic"),
        "unhelpful netCDF-4 error: {message}"
    );
}
