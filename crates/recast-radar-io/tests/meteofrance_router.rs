//! Meteo-France BUFR through the shared byte router, and on to Level II.
//!
//! The files are not redistributed (`testdata/other/manifest.toml`): the
//! tests skip when they are not in the testdata cache.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::{FieldName, SourceFormat, merge_volumes};
use recast_radar_io::{
    SupportedVolumeFormat, read_supported_volume_bytes, sniff_supported_volume_format,
};
use recast_radar_io_nexrad::write::{WriteOptions, write_volume_with_source};

const PAG: &str = "meteofrance-pag-07274-20130619-1200-a";
const PAM: &str = "meteofrance-pam-07274-20130619-1200-a";

/// Both products sniff and route as Meteo-France BUFR, the PAM file's
/// final compress member included.
#[test]
fn router_reads_pag_and_pam_files() {
    for id in [PAG, PAM] {
        let Some(bytes) = recast_radar_testdata::bytes_if_available(id) else {
            continue;
        };
        assert_eq!(
            sniff_supported_volume_format(&bytes),
            SupportedVolumeFormat::MeteoFranceBufr,
            "{id}"
        );
        assert_eq!(
            sniff_supported_volume_format(&bytes[..512]),
            SupportedVolumeFormat::MeteoFranceBufr,
            "{id} head"
        );
        let volume = read_supported_volume_bytes(&bytes).unwrap();
        assert_eq!(
            volume.provenance.source_format,
            SourceFormat::MeteoFranceBufr
        );
        assert_eq!(volume.attrs.instrument_name, "07274");
    }
}

/// The merged elevation written as Level II (the default, standard coding)
/// reads back with every reflectivity, RHOHV, PHIDP, ZDR and velocity gate
/// within half a coding step of the BUFR values: reflectivity (whole dBZ,
/// on NOAA's 0.5 dBZ grid) comes back exact, velocity (-60.2 + 0.5 per code,
/// 0.2 m/s off NOAA's 0.5 m/s grid) within 0.25 m/s.
#[test]
fn merged_elevation_round_trips_through_level2() {
    let (Some(pag), Some(pam)) = (
        recast_radar_testdata::bytes_if_available(PAG),
        recast_radar_testdata::bytes_if_available(PAM),
    ) else {
        return;
    };
    let parts = vec![
        read_supported_volume_bytes(&pag).unwrap(),
        read_supported_volume_bytes(&pam).unwrap(),
    ];
    let (mut volume, _) = merge_volumes(parts).unwrap();
    volume.attrs.instrument_name = "LFBH".to_owned();
    for sweep in &mut volume.sweeps {
        sweep
            .fields
            .retain(|field| field.name != FieldName::parse("DBZH_SD"));
    }
    let (bytes, summary) =
        write_volume_with_source(&volume, Default::default(), &WriteOptions::default()).unwrap();
    assert!(
        summary.skipped_fields.is_empty(),
        "{:?}",
        summary.skipped_fields
    );
    let again = recast_radar_io_nexrad::read_volume_from_bytes(&bytes).unwrap();
    assert_eq!(again.sweeps.len(), volume.sweeps.len());

    for (name, nexrad, tolerance) in [
        (FieldName::Dbzh, "REF", 0.0),
        (FieldName::Vradh, "VEL", 0.25 + 1e-4),
        (FieldName::Zdr, "ZDR", 1.0 / 64.0),
        (FieldName::Rhohv, "RHO", 1.0 / 600.0 + 1e-6),
        (FieldName::Phidp, "PHI", 1.0 / (2.0 * 2.8361) + 1e-4),
    ] {
        let (index, source) = volume
            .sweeps
            .iter()
            .enumerate()
            .find_map(|(i, sweep)| sweep.field(&name).map(|field| (i, field)))
            .unwrap();
        let written = again
            .sweeps
            .iter()
            .find(|sweep| {
                sweep.nrays() == volume.sweeps[index].nrays()
                    && sweep
                        .fields
                        .iter()
                        .any(|f| f.name == FieldName::from_nexrad_block(nexrad.as_bytes()))
            })
            .unwrap();
        let back = written
            .field(&FieldName::from_nexrad_block(nexrad.as_bytes()))
            .unwrap();
        // Rays are written from the first collected; compare by azimuth.
        let order: Vec<usize> = {
            let azimuths = &written.rays.azimuth_deg;
            volume.sweeps[index]
                .rays
                .azimuth_deg
                .iter()
                .map(|az| {
                    azimuths
                        .iter()
                        .position(|other| (other - az).abs() < 0.01)
                        .unwrap()
                })
                .collect()
        };
        let mut compared = 0;
        for (ray, &written_ray) in order.iter().enumerate() {
            for gate in 0..source.ngates as usize {
                match (source.value(ray, gate), back.value(written_ray, gate)) {
                    (Some(a), Some(b)) => {
                        assert!(
                            (a - b).abs() <= tolerance,
                            "{name} ray {ray} gate {gate}: {a} {b}"
                        );
                        compared += 1;
                    }
                    (None, None) => {}
                    // Values below the coding's first code read back as
                    // below threshold (RHO's 0.208 floor is not reached).
                    (a, b) => panic!("{name} ray {ray} gate {gate}: {a:?} {b:?}"),
                }
            }
        }
        assert!(compared > 1000, "{name}: {compared} gates");
    }
}
