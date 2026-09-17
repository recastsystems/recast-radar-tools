//! Golden-fixture tests for the ODIM_H5 decoder against REAL operational
//! polar volumes (four writers, four HDF5 feature mixes).
//!
//! Fixture provenance (expected values extracted with an independent
//! Python reader — h5py 3.15.1 — not with this crate):
//!
//! - `tests/data/bejab.pvol.hdf`: RMI Belgium Jabbeke C-band PVOL,
//!   2019-06-06 00:00:22 UTC, 11 DBZH sweeps, H5rad 2.0, superblock v0,
//!   gzip-chunked u8 planes. From wradlib/wradlib-data (MIT),
//!   `data/hdf5/bejab.pvol.hdf`.
//! - `tests/data/20130429043000.rad.bewid.pvol.dbzh.scan1.hdf`: RMI Belgium
//!   Wideumont PVOL, 2013-04-29 04:30 UTC, 5 DBZH sweeps, H5rad 2.1 with
//!   VARIABLE-LENGTH string attributes (global-heap path) and a root
//!   /how NI. From wradlib/wradlib-data (MIT).
//! - `tests/data/T_PAGZ35_C_ENMI_20170421090837.hdf`: met.no Røst (norst)
//!   PVOL, 2017-04-21 09:08:37 UTC, 6 DBZH sweeps, H5rad 2.2 with
//!   SUPERBLOCK VERSION 1 and a 720-ray half-degree lowest sweep. From
//!   openradar/open-radar-data (MIT). (First three fetched 2026-06-11.)
//! - `tests/data/espdg.pvol.20260707.dbzh_vradh.h5`: AEMET Spain Perdiguera
//!   Doppler PVOL, 2026-07-07 19:27:49 UTC, 2 sweeps × (VRADH + DBZH),
//!   H5rad 2.4 (IRIS 10.3 export) with VERSION 2 OBJECT HEADERS
//!   (OHDR/OCHK + Jenkins lookup3 checksums) on the leaf metadata groups
//!   under a v0 superblock and old-style groups, and float64 gzip-chunked
//!   data planes. Fetched 2026-07-07 from the OPERA ORD 24h bucket
//!   (`.../2026/07/07/ES/espdg/PVOL/espdg@20260707T1927@0.5_1.5@
//!   DBZH_VRADH.h5`) — the exact object BowEcho's v0.30-RC1 live poll
//!   failed on before hdf5lite learned the v2 header dialect.

use chrono::{TimeZone, Utc};
use recast_radar_core::model::{Field, FieldData, FieldName, RangeCoord, SweepMode, Volume};

const BEJAB: &[u8] = include_bytes!("data/bejab.pvol.hdf");
const BEWID: &[u8] = include_bytes!("data/20130429043000.rad.bewid.pvol.dbzh.scan1.hdf");
const NORST: &[u8] = include_bytes!("data/T_PAGZ35_C_ENMI_20170421090837.hdf");
const ESPDG: &[u8] = include_bytes!("data/espdg.pvol.20260707.dbzh_vradh.h5");

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} != {expected} (tolerance {tolerance})"
    );
}

fn location(volume: &Volume) -> (f32, f32, f32) {
    (
        volume.location.latitude_deg.unwrap() as f32,
        volume.location.longitude_deg.unwrap() as f32,
        volume.location.altitude_m.unwrap() as f32,
    )
}

fn uniform_range(sweep: &recast_radar_core::model::Sweep) -> (f64, f64, u32) {
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = sweep.range
    else {
        panic!("ODIM ranges are uniform");
    };
    (first_center_m, spacing_m, ngates)
}

fn nyquist(sweep: &recast_radar_core::model::Sweep) -> f32 {
    sweep.ray_vars.nyquist_velocity_mps.as_ref().expect("NI")[0]
}

#[test]
fn real_bejab_pvol_decodes_site_geometry_and_gates() {
    assert!(recast_radar_io_odim::odim::looks_like_hdf5_bytes(BEJAB));
    let volume = recast_radar_io_odim::odim::read_odim_h5_volume(BEJAB).expect("decode bejab");

    // source = "WMO:06410,RAD:BX42,PLC:Jabbeke,NOD:bejab,..." — NOD wins.
    assert_eq!(volume.attrs.instrument_name, "BEJAB");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Jabbeke"));
    let (lat, lon, height) = location(&volume);
    assert_close(lat, 51.1917, 1e-4, "lat");
    assert_close(lon, 3.0642, 1e-4, "lon");
    assert_close(height, 50.0, 1e-3, "height");
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2019, 6, 6, 0, 0, 22).unwrap()
    );
    assert_eq!(volume.sweeps[0].sweep_mode, SweepMode::AzimuthSurveillance);
    assert_eq!(volume.sweeps.len(), 11);
    assert_eq!(volume.provenance.decode.decoded_ray_count, 3960);

    // Lowest sweep: 0.3 deg, 360 rays x 598 gates, 500 m spacing from 0 km
    // (centres from 250 m).
    let sweep = &volume.sweeps[0];
    assert_close(sweep.fixed_angle_deg, 0.3, 1e-5, "elangle");
    assert_eq!(sweep.nrays(), 360);
    assert_close(sweep.rays.azimuth_deg[0], 0.5, 1e-5, "az0");
    assert_eq!(uniform_range(sweep), (250.0, 500.0, 598));

    // DBZH golden gates (h5py: phys = 0.5*raw - 32; 0=undetect, 255=nodata).
    let dbzh = sweep.field(&FieldName::Dbzh).expect("DBZH");
    assert_close(dbzh.value(0, 0).unwrap(), 22.5, 1e-4, "v[0,0]");
    assert_close(dbzh.value(0, 299).unwrap(), 33.5, 1e-4, "v[0,299]");
    assert_eq!(dbzh.value(90, 199), None, "v[90,199] undetect");
    assert_close(dbzh.value(180, 10).unwrap(), 28.5, 1e-4, "v[180,10]");
    assert_close(dbzh.value(359, 597).unwrap(), 18.5, 1e-4, "v[359,597]");

    // Top sweep changes geometry (25 deg, 300 gates) — chunk clipping etc.
    let top = &volume.sweeps[10];
    assert_close(top.fixed_angle_deg, 25.0, 1e-5, "top elangle");
    assert_eq!(top.range.ngates(), 300);
    let top_dbzh = top.field(&FieldName::Dbzh).expect("DBZH");
    assert_close(top_dbzh.value(0, 0).unwrap(), 34.5, 1e-4, "top v[0,0]");
    assert_close(
        top_dbzh.value(180, 10).unwrap(),
        27.5,
        1e-4,
        "top v[180,10]",
    );
    assert_eq!(top_dbzh.value(359, 299), None, "top v[359,299]");
}

#[test]
fn real_bewid_pvol_reads_vlen_string_attrs_and_root_nyquist() {
    let volume = recast_radar_io_odim::odim::read_odim_h5_volume(BEWID).expect("decode bewid");

    assert_eq!(volume.attrs.instrument_name, "BEWID");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Wideumont"));
    let (lat, lon, height) = location(&volume);
    assert_close(lat, 49.9143, 1e-4, "lat");
    assert_close(lon, 5.5056, 1e-4, "lon");
    assert_close(height, 592.0, 1e-3, "height");
    // /what date+time are VARIABLE-LENGTH strings in this writer — decoding
    // them exercises the hdf5lite global-heap path on real bytes.
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2013, 4, 29, 4, 30, 0).unwrap()
    );

    assert_eq!(volume.sweeps.len(), 5);
    let sweep = &volume.sweeps[0];
    assert_close(sweep.fixed_angle_deg, 0.3, 1e-5, "elangle");
    assert_eq!(sweep.nrays(), 360);
    assert_eq!(uniform_range(sweep), (125.0, 250.0, 960));
    // Root /how NI = 7.98 m/s applies to every sweep.
    for sweep in &volume.sweeps {
        assert_close(nyquist(sweep), 7.98, 1e-3, "root NI");
    }

    let dbzh = sweep.field(&FieldName::Dbzh).expect("DBZH");
    assert_eq!(dbzh.value(0, 0), None, "v[0,0] undetect");
    assert_close(dbzh.value(180, 10).unwrap(), -22.0, 1e-4, "v[180,10]");
    let top = volume.sweeps[4].field(&FieldName::Dbzh).expect("DBZH");
    assert_close(top.value(180, 10).unwrap(), -20.5, 1e-4, "top v[180,10]");
}

#[test]
fn real_norst_pvol_reads_superblock_v1_and_half_degree_sweep() {
    let volume = recast_radar_io_odim::odim::read_odim_h5_volume(NORST).expect("decode norst");

    // source = "WMO:01104,NOD:norst" — NOD preferred over the leading WMO.
    assert_eq!(volume.attrs.instrument_name, "NORST");
    let (lat, lon, _) = location(&volume);
    assert_close(lat, 67.5307, 1e-4, "lat");
    assert_close(lon, 12.0986, 1e-4, "lon");
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2017, 4, 21, 9, 8, 37).unwrap()
    );

    assert_eq!(volume.sweeps.len(), 6);
    assert_eq!(volume.provenance.decode.decoded_ray_count, 2520);

    // Lowest sweep is 720 half-degree rays; centres at 0.25, 0.75, ...
    let sweep = &volume.sweeps[0];
    assert_close(sweep.fixed_angle_deg, 0.5, 1e-5, "elangle");
    assert_eq!(sweep.nrays(), 720);
    assert_close(sweep.rays.azimuth_deg[0], 0.25, 1e-5, "az0");
    assert_close(sweep.rays.azimuth_deg[1], 0.75, 1e-5, "az1");
    assert_eq!(uniform_range(sweep), (125.0, 250.0, 960));

    let dbzh = sweep.field(&FieldName::Dbzh).expect("DBZH");
    assert_eq!(dbzh.value(0, 0), None, "v[0,0] undetect");
    assert_close(dbzh.value(180, 320).unwrap(), 2.5, 1e-4, "v[180,320]");
    assert_close(dbzh.value(360, 10).unwrap(), -7.0, 1e-4, "v[360,10]");
    assert_eq!(dbzh.value(719, 959), None, "v[719,959] undetect");

    // Upper sweeps drop back to 360 rays and shorter ranges.
    let top = &volume.sweeps[5];
    assert_close(top.fixed_angle_deg, 9.4, 1e-5, "top elangle");
    assert_eq!(top.nrays(), 360);
    assert_eq!(top.range.ngates(), 300);
    let top_dbzh = top.field(&FieldName::Dbzh).expect("DBZH");
    assert_close(
        top_dbzh.value(180, 10).unwrap(),
        -18.5,
        1e-4,
        "top v[180,10]",
    );
}

/// AEMET Perdiguera: the version-2 object header (OHDR/OCHK) dialect.
/// Golden values from h5py 3.15.1: float64 planes with gain=1/offset=0,
/// nodata=95.5, undetect=-32.0. Both DBZH AND VRADH declare those two dBZ
/// sentinels (IRIS 10.3 copies the reflectivity `what` group onto velocity),
/// but only DBZH's no-echo gates hold undetect (-32.0); VRADH's no-echo gates
/// are filled with offset (0 m/s), which matches no declared sentinel. DBZH
/// valid gates 18389/107640 (0.5 deg) and 16771/107640 (1.5 deg). The
/// decoder stores the planes verbatim (VRADH decodes to a 0 m/s wall, as in
/// xradar and Py-ART); after the opt-in reflectivity-gated velocity
/// recovery, VRADH keeps 18403/107640 (0.5 deg) and 16794/107640 (1.5 deg):
/// the 0 m/s fill co-located with DBZH no-echo is masked, genuine 0 m/s
/// gates that have echo are kept.
#[test]
fn real_espdg_pvol_decodes_v2_object_headers_end_to_end() {
    assert!(recast_radar_io_odim::odim::looks_like_hdf5_bytes(ESPDG));
    let mut volume = recast_radar_io_odim::odim::read_odim_h5_volume(ESPDG).expect("decode espdg");

    // source = "WMO:08162,RAD:SP47,PLC:Perdiguera,NOD:espdg".
    assert_eq!(volume.attrs.instrument_name, "ESPDG");
    assert_eq!(volume.attrs.site_name.as_deref(), Some("Perdiguera"));
    let (lat, lon, height) = location(&volume);
    assert_close(lat, 41.734, 1e-4, "lat");
    assert_close(lon, -0.54594, 1e-4, "lon");
    assert_close(height, 835.0, 1e-3, "height");
    assert_eq!(
        volume.time_reference,
        Utc.with_ymd_and_hms(2026, 7, 7, 19, 27, 49).unwrap()
    );
    assert_eq!(
        volume.provenance.source_version.as_deref(),
        Some("H5rad 2.4")
    );
    assert_eq!(volume.sweeps[0].sweep_mode, SweepMode::AzimuthSurveillance);
    // /how frequency = 5624623977.49 Hz (C band).
    assert_eq!(volume.radar_parameters.frequency_hz.len(), 1);
    assert_close(
        volume.radar_parameters.frequency_hz[0] as f32 / 1.0e6,
        5624.624,
        1e-2,
        "frequency",
    );

    // Two Doppler sweeps in dataset order, as xradar keeps them (dataset1 =
    // 1.5 deg, dataset2 = 0.5 deg).
    assert_eq!(volume.sweeps.len(), 2);
    assert_eq!(volume.provenance.decode.decoded_ray_count, 720);
    assert_close(volume.sweeps[0].fixed_angle_deg, 1.4996338, 1e-5, "el0");
    assert_close(volume.sweeps[1].fixed_angle_deg, 0.4998779, 1e-5, "el1");
    let (low, high) = (&volume.sweeps[1], &volume.sweeps[0]);

    for (label, sweep, az0) in [("0.5deg", low, 1.0079956), ("1.5deg", high, 1.0025024)] {
        assert_eq!(sweep.nrays(), 360, "{label} rays");
        // Measured azimuths: (how/startazA + how/stopazA) / 2, as xradar.
        assert_close(sweep.rays.azimuth_deg[0], az0, 1e-5, "az0");
        // AEMET writes where/rstart = 200.0 METRES (IRIS quirk; spec says
        // km). The decoder's physical-sanity rule reinterprets it, so the
        // first bin starts at the true 200 m (centre 450 m) — not 200 km
        // downrange.
        assert_eq!(uniform_range(sweep), (450.0, 500.0, 299), "{label} gates");
        // No per-dataset NI: root /how NI applies to both sweeps.
        assert_close(nyquist(sweep), 39.9217, 1e-4, "NI");
        assert_eq!(sweep.fields.len(), 2, "{label} fields");
        assert!(sweep.field(&FieldName::Dbzh).is_some());
        assert!(sweep.field(&FieldName::Vradh).is_some());
    }

    // Float64 planes stay float64 with their sentinels; check the
    // whole-plane health h5py reports through the coding, not just spot
    // gates.
    let plane_stats = |field: &Field| -> (usize, usize, f32, f32) {
        let FieldData::F64 { values, .. } = &field.data else {
            panic!("espdg planes must stay F64 storage");
        };
        let (rows, gates) = field.shape();
        let mut valid = 0usize;
        let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
        for row in 0..rows {
            for gate in 0..gates {
                if let Some(value) = field.value(row, gate) {
                    valid += 1;
                    min = min.min(value);
                    max = max.max(value);
                }
            }
        }
        (values.len(), valid, min, max)
    };

    // Verbatim decode: VRADH is a 0 m/s wall (nothing masked but the copied
    // dBZ sentinels), exactly what xradar and Py-ART return.
    let vradh_raw = low.field(&FieldName::Vradh).expect("VRADH");
    let (total, valid, _, _) = plane_stats(vradh_raw);
    assert_eq!(
        (total, valid),
        (107_640, 107_640),
        "VRADH0.5 verbatim gates"
    );
    assert_close(vradh_raw.value(0, 0).unwrap(), 0.0, 1e-6, "vel[0,0] fill");

    // Opt-in recovery: 89_237 + 89_237 + ... no-echo fill gates masked.
    let masked = recast_radar_io_odim::recover_copied_whatgroup_velocity_nodata(&mut volume);
    assert_eq!(masked, (107_640 - 18_403) + (107_640 - 16_794));

    let sweep = &volume.sweeps[1]; // 0.5 deg
    let dbzh = sweep.field(&FieldName::Dbzh).expect("DBZH");
    let (total, valid, min, max) = plane_stats(dbzh);
    assert_eq!((total, valid), (107_640, 18_389), "DBZH0.5 valid gates");
    assert_close(min, -31.5, 1e-4, "DBZH0.5 min");
    assert_close(max, 49.5, 1e-4, "DBZH0.5 max");
    assert_eq!(dbzh.value(0, 0), None, "v[0,0] undetect");
    assert_close(dbzh.value(0, 7).unwrap(), -16.5, 1e-4, "v[0,7]");
    assert_close(dbzh.value(209, 5).unwrap(), -22.0, 1e-4, "v[209,5]");
    assert_close(dbzh.value(270, 287).unwrap(), 24.0, 1e-4, "v[270,287]");
    assert_close(dbzh.value(359, 47).unwrap(), -14.0, 1e-4, "v[359,47]");

    // VRADH: AEMET stamps the DBZH sentinels (nodata 95.5 / undetect -32.0)
    // onto the velocity plane and fills no-echo gates with offset (0 m/s).
    // The reflectivity-gated recovery masks the 89_237 co-located no-echo
    // fill gates (velocity on offset where DBZH is no-echo) and keeps the
    // 12_869 genuine 0 m/s gates that have real echo.
    let vradh = sweep.field(&FieldName::Vradh).expect("VRADH");
    let (total, valid, min, max) = plane_stats(vradh);
    assert_eq!((total, valid), (107_640, 18_403), "VRADH0.5 valid gates");
    // Only the 0 m/s fill is removed, so the real velocity extremes stand.
    assert_close(min, -36.7537, 1e-3, "VRADH0.5 min");
    assert_close(max, 39.8951, 1e-3, "VRADH0.5 max");
    // Fill gate (0,0): DBZH there is undetect (-32.0 no-echo) -> velocity masked.
    assert_eq!(vradh.value(0, 0), None, "vel[0,0] fill");
    // Genuine 0 m/s gate (0,32): DBZH there is a real -18.0 dBZ echo -> kept.
    assert_close(vradh.value(0, 32).unwrap(), 0.0, 1e-6, "vel[0,32] real 0");
    // Velocity present where DBZH is no-echo (119,24) is off `offset`, so the
    // recovery leaves it untouched.
    assert_close(vradh.value(119, 24).unwrap(), 6.5968, 1e-3, "vel[119,24]");

    let sweep = &volume.sweeps[0]; // 1.5 deg
    let dbzh = sweep.field(&FieldName::Dbzh).expect("DBZH");
    let (total, valid, min, max) = plane_stats(dbzh);
    assert_eq!((total, valid), (107_640, 16_771), "DBZH1.5 valid gates");
    assert_close(min, -31.5, 1e-4, "DBZH1.5 min");
    assert_close(max, 46.0, 1e-4, "DBZH1.5 max");
    assert_close(dbzh.value(209, 273).unwrap(), 17.0, 1e-4, "v[209,273]");
    assert_close(dbzh.value(272, 241).unwrap(), 6.0, 1e-4, "v[272,241]");
    let vradh = sweep.field(&FieldName::Vradh).expect("VRADH");
    let (total, valid, _min, _max) = plane_stats(vradh);
    assert_eq!((total, valid), (107_640, 16_794), "VRADH1.5 valid gates");
    // A real Doppler gate is untouched by the recovery.
    assert_close(
        vradh.value(270, 200).unwrap(),
        3.4554768,
        1e-4,
        "vel[270,200]",
    );
    // Fill gate (0,0) masked; genuine 0 m/s gate (1,20) with DBZH -15.5 kept.
    assert_eq!(vradh.value(0, 0), None, "vel[0,0] fill");
    assert_close(vradh.value(1, 20).unwrap(), 0.0, 1e-6, "vel[1,20] real 0");
}

/// Every `how` attribute of a real dataset group is accounted for: the
/// decoder either reads it into a ray coordinate or a typed model slot, or
/// leaves it verbatim in `Sweep::other`. Nothing is dropped for being one
/// value per ray.
///
/// The `how` names of each file come from `hdf5lite` (the same bytes the
/// decoder reads, listed independently of it); the expected splits were
/// read with h5py. `ray` is the arrays the ray coordinates are built from
/// (`odim` module docs), `typed` the names a typed slot takes from this
/// dataset group. Everything else must appear in `Sweep::other`, a per-ray
/// array included: a producer's `TXpower` or `startelT`/`stopelT` array
/// reaches the model instead of being filtered out by its length.
#[test]
fn every_dataset_how_attribute_reaches_a_ray_coordinate_a_slot_or_sweep_other() {
    use std::collections::BTreeSet;

    use recast_radar_io_odim::hdf5lite::H5File;

    let set = |names: &[&str]| -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    };
    for (label, bytes, dataset, sweep_index, ray, typed) in [
        (
            "espdg",
            ESPDG,
            "/dataset1",
            0usize,
            &["startazA", "startelA", "stopazA", "stopelA"][..],
            &[][..],
        ),
        (
            "bewid",
            BEWID,
            "/dataset1",
            0usize,
            &[][..],
            &["NI", "pulsewidth", "rpm"][..],
        ),
        ("norst", NORST, "/dataset1", 0usize, &[][..], &["rpm"][..]),
    ] {
        let volume = recast_radar_io_odim::odim::read_odim_h5_volume(bytes)
            .unwrap_or_else(|e| panic!("decode {label}: {e}"));
        let file = H5File::open(bytes).unwrap_or_else(|e| panic!("open {label}: {e}"));
        let how: BTreeSet<String> = file
            .attrs(&format!("{dataset}/how"))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(!how.is_empty(), "{label} {dataset}: no `how` attributes");
        let other: BTreeSet<String> = volume.sweeps[sweep_index]
            .other
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();

        // The names the decoder holds back are exactly the ray arrays it
        // read plus the typed slots it filled from this group; every other
        // `how` attribute, whatever its length, is in `Sweep::other`.
        let held: BTreeSet<String> = how.difference(&other).cloned().collect();
        let expected: BTreeSet<String> = set(ray).union(&set(typed)).cloned().collect();
        assert_eq!(
            held, expected,
            "{label} {dataset}: `how` attributes the decoder does not pass through"
        );
    }
}
