//! The DORADE decoder against LROSE RadxPrint, an independent reader.
//!
//! `testdata/golden/dorade/radxprint.json` holds what `RadxPrint -rays`
//! (LROSE release 20250811; `tools/dorade_radx_golden.py`) prints for each
//! ray of seven committed sweepfiles. Radx keeps the antenna-transition rays
//! and flags them, so the model must hold the same rays in the same order,
//! with the same `antenna_transition` flags, and the per-ray values Radx
//! reads from the RYIB and ASIB blocks: time, azimuth, elevation, true scan
//! rate, measured transmit power, Nyquist velocity and the whole platform
//! georeference (position, platform velocities, heading, roll, pitch, drift,
//! rotation, tilt, winds and the heading and pitch change rates). The two
//! NOAA P-3 N42RF tail radar sweeps (Hurricane Michael, 2018) are airborne:
//! every one of those values is set and changes along the sweep. Each value
//! is also read back through the FM301 view, as are the sweep mode, platform
//! type and primary axis.
//!
//! The full N42RF sweepfiles (download entries; skipped offline) also end
//! with a SEDS block, which Radx reads as the volume `history`.
//!
//! What LROSE RadxConvert writes to CfRadial from each file is in the golden
//! too: `platform_is_mobile` (true for the airborne sweeps) and the sixteen
//! geometry correction variables it fills from the CFAC block. The model
//! applies six of them to the coordinates; the other ten must be its
//! `georeferencing_correction`, in the model and the view.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{NaiveDateTime, TimeZone, Utc};
use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, Values, ViewOptions};
use recast_radar_core::model::{AttrValue, Scalar, Sweep, Volume};
use recast_radar_io_dorade::{read_dorade_sweep_volume, read_dorade_volume_from_slices};
use serde_json::Value;

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

fn golden() -> Value {
    let path = recast_radar_testdata::testdata_dir().join("golden/dorade/radxprint.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// RadxPrint's missing value (-9999, or the file's -32768 passed through).
fn radx_missing(value: f64) -> bool {
    value <= -999.0
}

/// Equal to the six significant digits RadxPrint prints.
fn close(model: f64, radx: f64) -> bool {
    (model - radx).abs() <= 1e-5 * radx.abs().max(1.0)
}

/// A model value against a RadxPrint value: NaN where Radx prints its
/// missing value, else equal to its precision.
fn check(what: &str, model: f64, radx: f64) {
    if radx_missing(radx) {
        assert!(model.is_nan(), "{what}: model {model}, RadxPrint missing");
    } else {
        assert!(
            close(model, radx),
            "{what}: model {model}, RadxPrint {radx}"
        );
    }
}

/// An azimuth against RadxPrint's, which it prints in -180 to 180 degrees
/// for some files.
fn check_azimuth(what: &str, model: f64, radx: f64) {
    let difference = (model - radx).rem_euclid(360.0);
    let difference = difference.min(360.0 - difference);
    assert!(
        difference <= 1e-5 * radx.abs().max(1.0),
        "{what}: model {model}, RadxPrint {radx}"
    );
}

/// A sweep variable of the FM301 view (rays in time order) back in storage
/// order.
fn view_column(volume: &Volume, name: &str, path: &str) -> Vec<f64> {
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    let group = view.group(&format!("sweep_0{path}")).unwrap();
    let values = group
        .variable(name)
        .unwrap_or_else(|| panic!("view has no {name}"))
        .values
        .materialize()
        .unwrap();
    let sweep = &volume.sweeps[0];
    let mut order: Vec<usize> = (0..sweep.nrays()).collect();
    order.sort_by(|a, b| sweep.rays.time_s[*a].total_cmp(&sweep.rays.time_s[*b]));
    let mut out = vec![f64::NAN; order.len()];
    for (position, ray) in order.iter().enumerate() {
        out[*ray] = values.get_f64(position).unwrap();
    }
    out
}

/// A per-ray variable of the sweep (`extra_vars`), or all NaN without one.
fn extra_column(sweep: &Sweep, name: &str) -> Vec<f64> {
    sweep
        .extra_vars
        .iter()
        .find(|variable| &*variable.name == name)
        .map_or_else(
            || vec![f64::NAN; sweep.nrays()],
            |variable| {
                (0..variable.values.len())
                    .map(|index| variable.values.get_f64(index).unwrap())
                    .collect()
            },
        )
}

/// An optional per-ray column as `f64`, all NaN when absent.
fn optional_column<T: Copy + Into<f64>>(values: Option<&Vec<T>>, nrays: usize) -> Vec<f64> {
    values.map_or_else(
        || vec![f64::NAN; nrays],
        |values| values.iter().map(|value| (*value).into()).collect(),
    )
}

/// A view variable as a column in storage order, or all NaN when the view
/// has no such variable.
fn optional_view_column(volume: &Volume, name: &str) -> Vec<f64> {
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    if view.group("sweep_0").unwrap().variable(name).is_some() {
        view_column(volume, name, "")
    } else {
        vec![f64::NAN; volume.sweeps[0].nrays()]
    }
}

/// The platform georeference of the model, per RadxGeoref member of the
/// golden: (golden key, model column, FM301 view variable). Altitudes in km.
fn georeference(volume: &Volume) -> Vec<(&'static str, Vec<f64>, &'static str)> {
    let sweep = &volume.sweeps[0];
    let nrays = sweep.nrays();
    let track = sweep.platform_track.as_deref().unwrap();
    let km = |metres: &[f64]| metres.iter().map(|m| m / 1000.0).collect::<Vec<_>>();
    let optional = |values: Option<&Vec<f32>>| optional_column(values, nrays);
    vec![
        ("longitude_deg", track.longitude_deg.clone(), "longitude"),
        ("latitude_deg", track.latitude_deg.clone(), "latitude"),
        ("altitude_msl_km", km(&track.altitude_m), "altitude"),
        (
            "altitude_agl_km",
            track
                .altitude_agl_m
                .as_deref()
                .map_or_else(|| vec![f64::NAN; nrays], km),
            "altitude_agl",
        ),
        (
            "ew_velocity_mps",
            extra_column(sweep, "eastward_velocity"),
            "eastward_velocity",
        ),
        (
            "ns_velocity_mps",
            extra_column(sweep, "northward_velocity"),
            "northward_velocity",
        ),
        (
            "vert_velocity_mps",
            extra_column(sweep, "vertical_velocity"),
            "vertical_velocity",
        ),
        (
            "heading_deg",
            optional(track.heading_deg.as_ref()),
            "heading",
        ),
        ("roll_deg", optional(track.roll_deg.as_ref()), "roll"),
        ("pitch_deg", optional(track.pitch_deg.as_ref()), "pitch"),
        ("drift_deg", optional(track.drift_deg.as_ref()), "drift"),
        (
            "rotation_deg",
            optional(track.rotation_deg.as_ref()),
            "rotation",
        ),
        ("tilt_deg", optional(track.tilt_deg.as_ref()), "tilt"),
        (
            "ew_wind_mps",
            extra_column(sweep, "eastward_wind"),
            "eastward_wind",
        ),
        (
            "ns_wind_mps",
            extra_column(sweep, "northward_wind"),
            "northward_wind",
        ),
        (
            "vert_wind_mps",
            extra_column(sweep, "vertical_wind"),
            "vertical_wind",
        ),
        (
            "heading_rate_deg_per_s",
            extra_column(sweep, "heading_change_rate"),
            "heading_change_rate",
        ),
        (
            "pitch_rate_deg_per_s",
            extra_column(sweep, "pitch_change_rate"),
            "pitch_change_rate",
        ),
    ]
}

/// A text variable of the FM301 view.
fn view_text(volume: &Volume, group: &str, name: &str) -> Option<String> {
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    let variable = view.group(group).unwrap().variable(name)?;
    match &variable.values {
        Values::Text(text) => Some(text.to_string()),
        other => panic!("{name} is not text: {other:?}"),
    }
}

/// The CFAC corrections the decoder applies to the coordinates, as Radx
/// names them: they must not be in the model's `georeferencing_correction`.
const APPLIED: [&str; 6] = [
    "azimuth_correction",
    "elevation_correction",
    "range_correction",
    "longitude_correction",
    "latitude_correction",
    "altitude_correction",
];

/// The other ten: (Radx CfRadial name, model and FM301 name, factor from the
/// Radx value to the model's). Radx writes the CFAC pressure altitude
/// correction's kilometres; the model holds metres, the CfRadial unit.
const UNAPPLIED: [(&str, &str, f64); 10] = [
    (
        "pressure_altitude_correction",
        "pressure_altitude_correction",
        1000.0,
    ),
    (
        "eastward_velocity_correction",
        "eastward_ground_speed_correction",
        1.0,
    ),
    (
        "northward_velocity_correction",
        "northward_ground_speed_correction",
        1.0,
    ),
    (
        "vertical_velocity_correction",
        "vertical_velocity_correction",
        1.0,
    ),
    ("heading_correction", "heading_correction", 1.0),
    ("roll_correction", "roll_correction", 1.0),
    ("pitch_correction", "pitch_correction", 1.0),
    ("drift_correction", "drift_correction", 1.0),
    ("rotation_correction", "rotation_correction", 1.0),
    ("tilt_correction", "tilt_correction", 1.0),
];

/// `platform_is_mobile` and the georeferencing corrections against what
/// RadxConvert writes, in the model and the view.
fn check_mobility_and_corrections(id: &str, volume: &Volume, radx: &Value) -> usize {
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    let mobile = radx["platform_is_mobile"].as_str().unwrap();
    assert_eq!(
        volume.attrs.platform_is_mobile.to_string(),
        mobile,
        "{id}: platform_is_mobile"
    );
    assert_eq!(
        view.root.attr("platform_is_mobile"),
        Some(&AttrValue::text(mobile)),
        "{id}: view platform_is_mobile"
    );
    let corrections = radx["corrections"].as_object().unwrap();
    let group = view.group("georeferencing_correction");
    let Some(model) = volume.georeferencing_correction.as_deref() else {
        assert!(corrections.is_empty(), "{id}: corrections dropped");
        assert!(group.is_none(), "{id}: view group without corrections");
        return 0;
    };
    let group = group.expect("the view's georeferencing_correction group");
    let entries = model.entries();
    let entry = |name: &str| {
        entries
            .iter()
            .find(|(known, _)| *known == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .1
    };
    let viewed = |name: &str| {
        group.variable(name).map(|variable| match &variable.values {
            Values::Scalar(Scalar::F32(value)) => *value,
            other => panic!("{id}: {name} is {other:?}"),
        })
    };
    for name in APPLIED {
        let model_name = if name == "altitude_correction" {
            "radar_altitude_correction"
        } else {
            name
        };
        assert!(corrections.contains_key(name), "{id}: Radx wrote {name}");
        assert_eq!(entry(model_name), None, "{id}: {model_name} is applied");
        assert_eq!(viewed(model_name), None, "{id}: view {model_name}");
    }
    for (radx_name, name, factor) in UNAPPLIED {
        let expected = corrections[radx_name].as_f64().unwrap() * factor;
        let value = entry(name).unwrap_or_else(|| panic!("{id}: {name} missing"));
        check(&format!("{id} {name}"), f64::from(value), expected);
        assert_eq!(viewed(name), Some(value), "{id}: view {name}");
    }
    UNAPPLIED.len()
}

/// The sweep mode, platform type and primary axis against RadxPrint's, in
/// the model and the view. Radx prints `axis_z`, the CfRadial default, where
/// the model has no primary axis (and the view writes none).
fn check_volume(id: &str, volume: &Volume, radx: &Value) {
    let sweep_mode = radx["sweep_mode"].as_str().unwrap();
    assert_eq!(volume.sweeps[0].sweep_mode.as_str(), sweep_mode, "{id}");
    assert_eq!(
        view_text(volume, "sweep_0", "sweep_mode").as_deref(),
        Some(sweep_mode),
        "{id} view"
    );
    let platform_type = radx["platform_type"].as_str().unwrap();
    assert_eq!(volume.platform_type.as_str(), platform_type, "{id}");
    assert_eq!(
        view_text(volume, "", "platform_type").as_deref(),
        Some(platform_type),
        "{id} view"
    );
    let primary_axis = radx["primary_axis"].as_str().unwrap();
    assert_eq!(
        volume.primary_axis.map_or("axis_z", |axis| axis.as_str()),
        primary_axis,
        "{id}"
    );
    assert_eq!(
        view_text(volume, "", "primary_axis").unwrap_or_else(|| "axis_z".into()),
        primary_axis,
        "{id} view"
    );
}

#[test]
fn every_ray_matches_radxprint_in_the_model_and_the_view() {
    let golden = golden();
    let mut compared = 0;
    let mut georeference_values = 0;
    for (id, file) in golden["files"].as_object().unwrap() {
        let bytes = recast_radar_testdata::bytes(id).unwrap();
        let volume = read_dorade_sweep_volume(&bytes).unwrap();
        let sweep = &volume.sweeps[0];
        let rays = file["rays"].as_array().unwrap();
        assert_eq!(sweep.nrays(), rays.len(), "{id}: ray count");
        assert_eq!(file["n_rays"].as_u64(), Some(rays.len() as u64));

        let transition = sweep.ray_vars.antenna_transition.as_ref().unwrap();
        let viewed_transition = view_column(&volume, "antenna_transition", "");
        let viewed_azimuth = view_column(&volume, "azimuth", "");
        let viewed_elevation = view_column(&volume, "elevation", "");
        let scan_rate = sweep.ray_vars.scan_rate_deg_per_s.as_ref();
        let power = sweep
            .monitoring
            .as_ref()
            .and_then(|monitoring| monitoring.radar_measured_transmit_power_h_dbm.as_ref());
        let nyquist = sweep.ray_vars.nyquist_velocity_mps.as_ref().unwrap();
        check_volume(id, &volume, file);
        let georeference = georeference(&volume);
        // The view's altitudes are in metres, RadxPrint's in km.
        let viewed_georeference: Vec<Vec<f64>> = georeference
            .iter()
            .map(|(key, _, name)| {
                let column = optional_view_column(&volume, name);
                let scale = if key.ends_with("_km") { 1000.0 } else { 1.0 };
                column.into_iter().map(|value| value / scale).collect()
            })
            .collect();
        // RadxPrint prints NOXP ray times that are not in the file (it spreads
        // the whole-second RYIB times); times are compared where every one it
        // prints is a whole millisecond, as the RYIB stores them.
        let whole_ms = rays.iter().all(|ray| {
            let text = ray["time"].as_str().unwrap();
            let fraction = text.rsplit('.').next().unwrap();
            fraction.len() == 6 && fraction.ends_with("000")
        });
        for (index, ray) in rays.iter().enumerate() {
            let what = |name: &str| format!("{id} ray {index} {name}");
            let flag = ray["antenna_transition"].as_u64().unwrap();
            assert_eq!(u64::from(transition[index]), flag, "{}", what("transition"));
            assert_eq!(viewed_transition[index], flag as f64, "{}", what("view"));
            let azimuth = ray["azimuth_deg"].as_f64().unwrap();
            check_azimuth(
                &what("azimuth"),
                f64::from(sweep.rays.azimuth_deg[index]),
                azimuth,
            );
            check_azimuth(&what("azimuth view"), viewed_azimuth[index], azimuth);
            let elevation = ray["elevation_deg"].as_f64().unwrap();
            check(
                &what("elevation"),
                f64::from(sweep.rays.elevation_deg[index]),
                elevation,
            );
            check(&what("elevation view"), viewed_elevation[index], elevation);
            let rate = ray["true_scan_rate_deg_per_s"].as_f64().unwrap();
            check(
                &what("scan rate"),
                scan_rate.map_or(f64::NAN, |rates| f64::from(rates[index])),
                rate,
            );
            check(
                &what("transmit power"),
                power.map_or(f64::NAN, |power| f64::from(power[index])),
                ray["transmit_power_h_dbm"].as_f64().unwrap(),
            );
            check(
                &what("nyquist"),
                f64::from(nyquist[index]),
                ray["nyquist_mps"].as_f64().unwrap(),
            );
            for ((key, model, _), viewed) in georeference.iter().zip(&viewed_georeference) {
                let radx = ray[*key].as_f64().unwrap();
                check(&what(key), model[index], radx);
                check(&what(&format!("{key} view")), viewed[index], radx);
                if !radx_missing(radx) {
                    georeference_values += 1;
                }
            }
            if whole_ms {
                let expected = NaiveDateTime::parse_from_str(
                    ray["time"].as_str().unwrap(),
                    "%Y/%m/%d %H:%M:%S%.f",
                )
                .unwrap();
                let expected = Utc.from_utc_datetime(&expected);
                let model = volume.ray_time(0, index).unwrap();
                assert_eq!(model, expected, "{}", what("time"));
            }
            compared += 1;
        }
    }
    assert_eq!(compared, 24 + 41 + 51 + 100 + 6 + 24 + 48);
    // The ground-based files store the position only; the airborne ones
    // every ASIB value of every ray.
    eprintln!("{georeference_values} georeference values compared");
    assert!(
        georeference_values >= 18 * (24 + 48),
        "{georeference_values}"
    );
}

#[test]
fn mobility_and_georeferencing_corrections_match_radxconvert() {
    let golden = golden();
    let (mut mobile, mut corrections) = (0, 0);
    for (id, file) in golden["files"].as_object().unwrap() {
        let bytes = recast_radar_testdata::bytes(id).unwrap();
        let volume = read_dorade_sweep_volume(&bytes).unwrap();
        corrections += check_mobility_and_corrections(id, &volume, file);
        mobile += usize::from(volume.attrs.platform_is_mobile);
    }
    // The two airborne sweeps are mobile; every file but the COW2 one (no
    // CFAC block) has corrections, and the N42RF-TM sweep's are not zero.
    assert_eq!(mobile, 2);
    assert_eq!(corrections, 6 * 10);
    let tm = read_dorade_sweep_volume(
        &recast_radar_testdata::bytes("dorade-n42rf-tm-20181010-123925-air-head48").unwrap(),
    )
    .unwrap();
    let correction = tm.georeferencing_correction.unwrap();
    assert!(correction.rotation_correction.unwrap() != 0.0);
    assert!(correction.pressure_altitude_correction.unwrap() != 0.0);
}

#[test]
fn full_airborne_sweeps_match_radxprint_with_their_seds_history() {
    let golden = golden();
    let mut compared = 0;
    for (id, file) in golden["full_files"].as_object().unwrap() {
        let bytes = match recast_radar_testdata::bytes(id) {
            Ok(bytes) => bytes,
            Err(error) if error.is_offline() => {
                eprintln!("skipping {id}: {error}");
                continue;
            }
            Err(error) => panic!("{id}: {error}"),
        };
        let volume = read_dorade_sweep_volume(&bytes).unwrap();
        // Every ray, read past the NULL and RKTB blocks to the SEDS block.
        assert_eq!(
            volume.sweeps[0].nrays() as u64,
            file["n_rays"].as_u64().unwrap(),
            "{id}"
        );
        check_volume(id, &volume, file);
        assert_eq!(check_mobility_and_corrections(id, &volume, file), 10);
        // Radx's history is the SEDS text without its trailing whitespace;
        // the sweep keeps it as stored.
        let history = file["history"].as_str().unwrap();
        assert!(history.lines().count() > 20, "{id}: {history}");
        assert_eq!(volume.attrs.history.as_deref(), Some(history), "{id}");
        let view = fm301::volume_view(&volume, ALL, None).unwrap();
        assert_eq!(
            view.group("").unwrap().attr("history"),
            Some(&AttrValue::text(history)),
            "{id} view"
        );
        let seds = volume.sweeps[0]
            .other
            .iter()
            .find(|(name, _)| &**name == "dorade_seds_text")
            .map(|(_, value)| value);
        let Some(AttrValue::Text(seds)) = seds else {
            panic!("{id}: no dorade_seds_text");
        };
        assert_eq!(seds.trim_end(), history, "{id}");
        assert_eq!(
            view.group("sweep_0").unwrap().attr("dorade_seds_text"),
            Some(&AttrValue::Text(seds.clone())),
            "{id} view"
        );
        // Two sweeps with the same edit summary: each keeps its own, and the
        // volume's history holds the text once.
        let twice = read_dorade_volume_from_slices(&[&bytes, &bytes]).unwrap();
        assert_eq!(twice.sweeps.len(), 2, "{id}");
        for sweep in &twice.sweeps {
            let own = sweep
                .other
                .iter()
                .find(|(name, _)| &**name == "dorade_seds_text");
            assert_eq!(
                own.map(|(_, value)| value),
                Some(&AttrValue::Text(seds.clone()))
            );
        }
        assert_eq!(twice.attrs.history.as_deref(), Some(history), "{id}");
        compared += 1;
    }
    eprintln!("{compared} full airborne sweepfiles compared");
}
