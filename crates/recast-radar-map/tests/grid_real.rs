//! Cartesian gridding of real Level II volumes against Py-ART's
//! `grid_from_radars`.
//!
//! Expected values come from `testdata/golden/map/grid.json`
//! (`tools/filters_map_golden.py grid`): Py-ART's `grid_from_radars`
//! (`map_gates_to_grid`) on its own reading of each file, with the gate
//! positions replaced by float64 values of `antenna_to_cartesian`'s formula
//! on Py-ART's ray angles and ranges (Py-ART computes gate heights in f32).
//! Py-ART reads only the gridded moments (`include_fields`): read with the
//! 250 m Doppler moments it interpolates the legacy 1 km reflectivity of the
//! 2008 KPAH and KVWX files onto 250 m gates, four gridded gates per
//! recorded one, where the crate grids the recorded gates.
//! Eight cases: the default Barnes2 weighting with the `dist_beam` radius on a
//! split cut (reflectivity and ZDR) and on a 19-tilt volume, Cressman with a
//! constant radius, an explicit grid origin 20 km from the radar with the
//! `dist` radius (the geographic path), two radars 160 km apart (KPAH and
//! KVWX, Barnes2 with `dist_beam` and the deprecated Barnes with `dist`, the
//! radius taking the minimum over both radar offsets), two volumes of one
//! radar gridded together (most points take gates from both), and nearest
//! weighting. For nearest, Py-ART runs with a gate filter that excludes
//! masked reflectivity: Py-ART otherwise lets a masked nearest gate blank the
//! point, where [`GridWeighting::Nearest`] ignores gates without a value.

mod common;

use common::{as_usize, golden, level2};
use recast_radar_core::FieldName;
use recast_radar_map::{
    GridOptions, GridOrigin, GridSpec, GridWeighting, RadiusOfInfluence, grid_from_volumes,
};
use serde_json::Value;

fn as_f64(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number, got {value}"))
}

fn pair(value: &Value) -> (f64, f64) {
    let items = value
        .as_array()
        .unwrap_or_else(|| panic!("expected a pair, got {value}"));
    (as_f64(&items[0]), as_f64(&items[1]))
}

fn options(case: &Value) -> GridOptions {
    let given = &case["options"];
    let mut options = GridOptions::default();
    match given["weighting_function"].as_str() {
        None | Some("Barnes2") => {}
        Some("Barnes") => options.weighting = GridWeighting::Barnes,
        Some("Cressman") => options.weighting = GridWeighting::Cressman,
        Some("Nearest") => options.weighting = GridWeighting::Nearest,
        Some(other) => panic!("unexpected weighting_function {other}"),
    }
    match given["roi_func"].as_str() {
        Some("constant") => {
            options.roi = RadiusOfInfluence::Constant {
                radius_m: as_f64(&given["constant_roi"]) as f32,
            }
        }
        Some("dist") => {
            options.roi = RadiusOfInfluence::Distance {
                z_factor: as_f64(&given["z_factor"]) as f32,
                xy_factor: as_f64(&given["xy_factor"]) as f32,
                min_radius_m: as_f64(&given["min_radius"]) as f32,
            }
        }
        None => {}
        Some(other) => panic!("unexpected roi_func {other}"),
    }
    options
}

#[test]
fn gridding_matches_pyart_grid_from_radars() {
    let golden = golden("map/grid.json");
    for case in golden["cases"].as_array().expect("cases") {
        let name = case["case"].as_str().expect("case");
        let mut volumes = Vec::new();
        for id in case["ids"].as_array().expect("ids") {
            match recast_radar_testdata::path(id.as_str().expect("id")) {
                Ok(path) => volumes.push(level2(&path)),
                Err(error) if error.is_offline() => {
                    eprintln!("skipping {name}: {error}");
                    break;
                }
                Err(error) => panic!("{error}"),
            }
        }
        let radars = case["radars"].as_array().expect("radars");
        if volumes.len() != radars.len() {
            continue;
        }
        // The decoder's radar positions are Py-ART's.
        for (volume, radar) in volumes.iter().zip(radars) {
            assert_eq!(
                volume.location.altitude_m,
                Some(as_f64(&radar["altitude_m"])),
                "{name}: altitude"
            );
            assert!(
                (volume.location.latitude_deg.expect("lat") - as_f64(&radar["latitude_deg"])).abs()
                    < 1e-6
            );
            assert!(
                (volume.location.longitude_deg.expect("lon") - as_f64(&radar["longitude_deg"]))
                    .abs()
                    < 1e-6
            );
        }

        let shape = case["shape"].as_array().expect("shape");
        let origin = (!case["origin"].is_null()).then(|| GridOrigin {
            latitude_deg: as_f64(&case["origin"]["latitude_deg"]),
            longitude_deg: as_f64(&case["origin"]["longitude_deg"]),
            altitude_m: as_f64(&case["origin"]["altitude_m"]),
        });
        let spec = GridSpec {
            shape: (
                as_usize(&shape[0]),
                as_usize(&shape[1]),
                as_usize(&shape[2]),
            ),
            z_limits_m: pair(&case["z_limits_m"]),
            y_limits_m: pair(&case["y_limits_m"]),
            x_limits_m: pair(&case["x_limits_m"]),
            origin,
        };
        let fields: Vec<FieldName> = case["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| FieldName::parse(field["name"].as_str().expect("name")))
            .collect();
        let started = std::time::Instant::now();
        let volumes: Vec<_> = volumes.iter().collect();
        let grid = grid_from_volumes(&volumes, &fields, &spec, &options(case)).expect("grid");
        let elapsed = started.elapsed();

        for expected in case["fields"].as_array().expect("fields") {
            let field = grid
                .field(&FieldName::parse(expected["name"].as_str().expect("name")))
                .expect("gridded field");
            let defined: Vec<f64> = field
                .values
                .iter()
                .filter(|value| value.is_finite())
                .map(|value| f64::from(*value))
                .collect();
            let what = format!("{name} {}", field.name);
            // A point is defined when some gate's radius of influence reaches it.
            assert_eq!(
                defined.len(),
                as_usize(&expected["defined"]),
                "{what}: defined points"
            );
            let sum: f64 = defined.iter().sum();
            let expected_sum = as_f64(&expected["sum"]);
            assert!(
                (sum - expected_sum).abs() <= 1e-5 * expected_sum.abs().max(1.0),
                "{what}: sum {sum} != {expected_sum}"
            );
            let mut worst = 0.0f64;
            for cell in expected["cells"].as_array().expect("cells") {
                let cell = cell.as_array().expect("cell");
                let index = grid.index(as_usize(&cell[0]), as_usize(&cell[1]), as_usize(&cell[2]));
                let actual = field.values[index];
                if cell[3].is_null() {
                    assert!(actual.is_nan(), "{what}: point {cell:?} is {actual}");
                    continue;
                }
                let expected = as_f64(&cell[3]);
                worst = worst.max((f64::from(actual) - expected).abs());
                assert!(
                    (f64::from(actual) - expected).abs() <= 1e-4 * expected.abs().max(1.0),
                    "{what}: point {cell:?} is {actual}"
                );
            }
            eprintln!(
                "{what}: {} defined, worst sampled |diff| {worst:.2e}, {:.0} ms",
                defined.len(),
                elapsed.as_secs_f64() * 1000.0
            );
        }
        // Py-ART squares the offsets with `**2` inside a double sqrt: the radius can
        // differ from the f32 port in the last bit.
        for cell in case["roi_cells"].as_array().expect("roi") {
            let cell = cell.as_array().expect("cell");
            let index = grid.index(as_usize(&cell[0]), as_usize(&cell[1]), as_usize(&cell[2]));
            let expected = as_f64(&cell[3]);
            assert!(
                (f64::from(grid.roi_m[index]) - expected).abs() <= 1e-6 * expected,
                "{name}: ROI at {cell:?} is {}",
                grid.roi_m[index]
            );
        }
    }
}

#[test]
fn empty_and_oversized_grids_are_refused() {
    let golden = golden("map/grid.json");
    let case = &golden["cases"][0];
    let Ok(path) = recast_radar_testdata::path(case["id"].as_str().expect("id")) else {
        return;
    };
    let volume = level2(&path);
    let mut spec = GridSpec {
        shape: (0, 10, 10),
        z_limits_m: (0.0, 1000.0),
        y_limits_m: (-1000.0, 1000.0),
        x_limits_m: (-1000.0, 1000.0),
        origin: None,
    };
    let fields = [FieldName::Dbzh];
    assert!(grid_from_volumes(&[&volume], &fields, &spec, &GridOptions::default()).is_err());
    spec.shape = (4096, 4096, 4096);
    assert!(matches!(
        grid_from_volumes(&[&volume], &fields, &spec, &GridOptions::default()),
        Err(recast_radar_map::GridError::TooLarge { .. })
    ));
    assert!(grid_from_volumes(&[], &fields, &spec, &GridOptions::default()).is_err());
}
