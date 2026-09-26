//! The Level II calibration values and pulse parameters that have FM301
//! slots reach them, in the model and in the FM301 view.
//!
//! `testdata/golden/level2/pulses_calibration.json`
//! (`tools/nexrad_pulses_golden.py`) holds what independent readers find in
//! the twelve committed Message 31 fixtures:
//!
//! - MetPy 1.7.1 `Level2File`: every radial's VOL calibration constant
//!   (dBZ0), system ZDR and initial system PhiDP and its RAD unambiguous
//!   range, the message 5 cuts, the VCP pulse width and the message 18
//!   transmitter pulse widths TAU_SP and TAU_LP;
//! - the golden script's own frame walk: the message 32 PRF tables;
//! - LROSE RadxPrint: the calibration it reads from the same VOL values
//!   (`baseDbz1kmHc`, `zdrCorrectionDb`, `systemPhidpDeg`), and its per-ray
//!   `nSamples` and `pulseWidthUsec`.
//!
//! Checked per ray: `calib_index` points at a `radar_calibration` entry whose
//! `base_1km_hc`, `zdr_correction` and `system_phidp` are the radial's VOL
//! values; `n_samples` is the cut's surveillance pulse count (contiguous
//! surveillance) or its Doppler sectors' pulse count; `pulse_width` is TAU_SP
//! or TAU_LP for the VCP's pulse; `prt` is 1 / PRF of the message 32 table
//! for the cut's PRF number, and gives the radial's own unambiguous range
//! (c prt / 2) to 0.5%; without a message 32 there is no `prt`.
//!
//! RadxPrint agrees on the calibration and on `n_samples`. Its pulse width
//! is a nominal 1.5 us for every short-pulse file, within 5% of the TAU_SP
//! each file records. Its PRTs are not compared: it reads 2.24 ms for
//! surveillance cuts whose radials record a 467 km unambiguous range
//! (3.11 ms), the PRF of message 32 waveform 2 number 1 rather than of the
//! cut's waveform 1 number 1.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, ViewOptions, VolumeView};
use recast_radar_core::model::{ArrayBuf, PrtMode, RadarCalibration, Volume};
use recast_radar_io_nexrad::read_volume_from_bytes;
use serde_json::Value;

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

/// Speed of light in m/s (unambiguous range = c / (2 PRF)).
const SPEED_OF_LIGHT_M_S: f64 = 299_792_458.0;

fn golden() -> Value {
    let path = recast_radar_testdata::testdata_dir().join("golden/level2/pulses_calibration.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// A run-length encoded golden column in full.
fn expand(rle: &Value) -> Vec<Value> {
    rle.as_array()
        .unwrap()
        .iter()
        .flat_map(|run| {
            let run = run.as_array().unwrap();
            std::iter::repeat_n(run[0].clone(), run[1].as_u64().unwrap() as usize)
        })
        .collect()
}

fn values_f64(values: &ArrayBuf) -> Vec<f64> {
    (0..values.len())
        .map(|i| values.get_f64(i).unwrap())
        .collect()
}

/// A per-ray FM301 variable of sweep `index` in the view (Level II rays are
/// stored in time order, so the view keeps the model's order).
fn viewed(view: &VolumeView<'_>, index: usize, name: &str) -> Option<Vec<f64>> {
    let group = view.group(&format!("sweep_{index}")).unwrap();
    group
        .variable(name)
        .map(|variable| values_f64(&variable.values.materialize().unwrap()))
}

/// A `radar_calibration` variable of the view, one value per entry.
fn viewed_calibration(view: &VolumeView<'_>, name: &str) -> Vec<f64> {
    let group = view.group("radar_calibration").expect("radar_calibration");
    values_f64(
        &group
            .variable(name)
            .unwrap_or_else(|| panic!("radar_calibration/{name}"))
            .values
            .materialize()
            .unwrap(),
    )
}

/// Equal as `f32` (MetPy reads the float32 fields; JSON keeps them exactly).
fn same_f32(actual: f64, expected: f64) -> bool {
    (actual as f32).to_bits() == (expected as f32).to_bits()
}

/// Equal to the six significant digits RadxPrint prints.
fn close(model: f64, radx: f64) -> bool {
    (model - radx).abs() <= 5e-6 * radx.abs().max(1.0)
}

/// Checks one fixture; returns the number of rays compared.
fn check(id: &str, case: &Value) -> usize {
    let bytes = recast_radar_testdata::bytes(id).unwrap();
    let volume: Volume = read_volume_from_bytes(&bytes).unwrap();
    let view = fm301::volume_view(&volume, ALL, None).unwrap();
    let metpy = &case["metpy"];
    let sweeps = metpy["sweeps"].as_array().unwrap();
    assert_eq!(volume.sweeps.len(), sweeps.len(), "{id}: sweeps");

    // The calibration entries in the model and the view.
    let calibration = &volume.radar_calibration;
    assert!(!calibration.is_empty(), "{id}: no calibration entry");
    for (name, get) in [
        (
            "base_1km_hc",
            (|c| c.base_1km_hc_dbz) as fn(&RadarCalibration) -> Option<f32>,
        ),
        ("zdr_correction", |c| c.zdr_correction_db),
        ("system_phidp", |c| c.system_phidp_deg),
    ] {
        let model: Vec<f64> = calibration
            .iter()
            .map(|entry| f64::from(get(entry).unwrap()))
            .collect();
        assert_eq!(viewed_calibration(&view, name), model, "{id} {name} view");
    }
    if let Some(radx) = case.get("radxprint") {
        let radx = &radx["calibration"];
        let entry = &calibration[0];
        for (name, model) in [
            ("baseDbz1kmHc", entry.base_1km_hc_dbz),
            ("zdrCorrectionDb", entry.zdr_correction_db),
            ("systemPhidpDeg", entry.system_phidp_deg),
        ] {
            let expected = radx[name].as_f64().unwrap();
            let model = f64::from(model.unwrap());
            assert!(
                close(model, expected),
                "{id} {name}: {model} vs RadxPrint {expected}"
            );
        }
    }

    let cuts = metpy["cuts"].as_array().unwrap();
    let prf_tables = &case["message_32_prf_mhz"];
    let tau_ns = match metpy["vcp_pulse_width"].as_str() {
        Some("Short") => metpy["tau_sp_ns"].as_f64(),
        Some("Long") => metpy["tau_lp_ns"].as_f64(),
        _ => None,
    };
    let mut compared = 0;
    for (index, (sweep, golden)) in volume.sweeps.iter().zip(sweeps).enumerate() {
        let what = format!("{id} sweep {index}");
        let el_num = golden["el_num"].as_u64().unwrap();
        assert_eq!(sweep.elevation_number, Some(el_num as u16), "{what}");
        let nrays = sweep.nrays();
        assert_eq!(nrays as u64, golden["rays"].as_u64().unwrap(), "{what}");

        // calib_index -> the radial's own VOL values.
        let vol = expand(&golden["vol_calibration_rle"]);
        let indices = sweep.ray_vars.calib_index.as_ref().expect("calib_index");
        let viewed_indices = viewed(&view, index, "calib_index").expect("calib_index view");
        for (ray, expected) in vol.iter().enumerate() {
            let index = indices[ray];
            assert_eq!(viewed_indices[ray], f64::from(index), "{what} ray {ray}");
            let entry = &calibration[usize::try_from(index).unwrap()];
            let expected = expected.as_array().unwrap();
            for (model, expected) in [
                entry.base_1km_hc_dbz,
                entry.zdr_correction_db,
                entry.system_phidp_deg,
            ]
            .into_iter()
            .zip(expected)
            {
                assert!(
                    same_f32(f64::from(model.unwrap()), expected.as_f64().unwrap()),
                    "{what} ray {ray}: {model:?} vs {expected}"
                );
            }
        }

        // n_samples from the message 5 cut.
        let cut = &cuts[el_num as usize - 1];
        let waveform = cut["waveform"].as_u64().unwrap();
        let expected_samples = if waveform == 1 {
            cut["surv_pulse_count"].as_i64().unwrap()
        } else {
            let counts: Vec<i64> = cut["sectors"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|sector| sector[1].as_u64() != Some(0))
                .map(|sector| sector[2].as_i64().unwrap())
                .collect();
            // Every Doppler cut of the corpus has one pulse count in all its
            // sectors, so the count does not depend on the ray's azimuth.
            assert!(counts.windows(2).all(|pair| pair[0] == pair[1]), "{what}");
            counts[0]
        };
        let samples = sweep.ray_vars.n_samples.as_ref().expect("n_samples");
        assert!(
            samples.iter().all(|n| i64::from(*n) == expected_samples),
            "{what}"
        );
        assert_eq!(
            viewed(&view, index, "n_samples").unwrap(),
            vec![expected_samples as f64; nrays],
            "{what} n_samples view"
        );

        // pulse_width from message 18 TAU for the VCP's pulse.
        let pulse_width = sweep.ray_vars.pulse_width_s.as_ref();
        match tau_ns {
            Some(ns) => {
                let expected = (ns * 1e-9) as f32;
                let model = pulse_width.expect("pulse_width");
                assert!(model.iter().all(|w| *w == expected), "{what}: {model:?}");
                assert_eq!(
                    viewed(&view, index, "pulse_width").unwrap(),
                    vec![f64::from(expected); nrays],
                    "{what} pulse_width view"
                );
            }
            None => assert!(pulse_width.is_none(), "{what}"),
        }

        // prt from message 32, and the radials' unambiguous range.
        let prt = sweep.ray_vars.prt_s.as_ref();
        match prf_tables.as_object() {
            None => {
                assert!(prt.is_none(), "{what}: prt without message 32");
                assert!(viewed(&view, index, "prt").is_none(), "{what}");
            }
            Some(tables) => {
                // Table XVIII note 1: surveillance PRF numbers index the
                // waveform 1 table; Doppler PRF numbers of waveforms 2, 3
                // and 4 the waveform 2 table (the corpus has no staggered
                // pulse pair cut, waveform 5).
                let (table, number) = match waveform {
                    1 => ("1", cut["surv_prf_num"].as_u64().unwrap()),
                    2..=4 => ("2", cut["sectors"][0][1].as_u64().unwrap()),
                    other => panic!("{what}: waveform {other}"),
                };
                let millihertz = tables[table].as_array().unwrap()[number as usize - 1]
                    .as_f64()
                    .unwrap();
                let expected = (1000.0 / millihertz) as f32;
                let model = prt.expect("prt");
                assert!(model.iter().all(|p| *p == expected), "{what}: {model:?}");
                assert_eq!(
                    viewed(&view, index, "prt").unwrap(),
                    vec![f64::from(expected); nrays],
                    "{what} prt view"
                );
                assert_eq!(sweep.prt_mode, Some(PrtMode::Fixed), "{what}");
                let unambiguous = expand(&golden["unamb_range_km_rle"]);
                for (ray, recorded) in unambiguous.iter().enumerate() {
                    let recorded = recorded.as_f64().unwrap();
                    let range_km = SPEED_OF_LIGHT_M_S * f64::from(model[ray]) / 2.0 / 1000.0;
                    assert!(
                        (range_km - recorded).abs() / recorded < 0.005,
                        "{what} ray {ray}: prt gives {range_km} km, the radial {recorded} km"
                    );
                }
            }
        }

        // RadxPrint merges a split cut into one sweep, numbered from 0: its
        // sweep 0 holds this file's first (surveillance) sweep.
        if index == 0
            && let Some(radx) = case.get("radxprint")
        {
            let radx = &radx["sweeps"]["0"];
            assert_eq!(
                radx["n_samples"].as_array().unwrap(),
                &vec![Value::from(expected_samples as f64)],
                "{what} RadxPrint nSamples"
            );
            if let Some(model) = pulse_width {
                assert_eq!(radx["pulse_width_us"], serde_json::json!([1.5]), "{what}");
                let relative = (f64::from(model[0]) * 1e6 - 1.5).abs() / 1.5;
                assert!(relative < 0.05, "{what}: {} us", model[0] * 1e6);
            }
        }
        compared += nrays;
    }
    compared
}

#[test]
fn calibration_and_pulses_match_independent_readers() {
    let golden = golden();
    let cases = golden["cases"].as_object().unwrap();
    assert_eq!(cases.len(), 12);
    let mut compared = 0;
    let mut with_prt = 0;
    for (id, case) in cases {
        compared += check(id, case);
        with_prt += usize::from(case["message_32_prf_mhz"].is_object());
    }
    eprintln!("{compared} rays compared, {with_prt} fixture(s) with message 32");
    assert_eq!(with_prt, 1);
    assert_eq!(
        compared,
        2 * (720 + 480 + 360 + 240 + 120 + 120 + 240 + 360 + 240 + 360 + 480 + 240)
    );
}
