//! Column products (column maximum, echo base/top/depth) on the KEWX 2016-04-13
//! hail volume.
//!
//! Expected values come from `testdata/golden/retrieve/volume.json`
//! (`tools/retrieve_golden.py volume`): a numpy column walk over the MetPy-read
//! reflectivity tilts following the documented sampler rules (lowest tilt's
//! azimuths and ground ranges, nearest azimuth, nearest ground-range gate,
//! 4/3-Earth beam geometry).

mod common;

use common::{array, as_f64, as_opt_f64, as_usize, assert_close, cell, golden, level2, tilt};
use recast_radar_core::FieldName;
use recast_radar_retrieve::{column_max, echo_base, echo_depth, echo_top_height};
use recast_radar_testdata::require_file;

#[test]
fn column_maximum_and_echo_depth_match_the_column_walk_reference() {
    let path = require_file!("l2-kewx-20160413-022531");
    let volume = level2(&path);
    let golden = golden("retrieve/volume.json");
    let threshold = as_f64(&golden["echo_threshold_dbz"]) as f32;

    let cmax = column_max(&volume, &FieldName::Dbzh).expect("CMAX");
    let base = echo_base(&volume, threshold).expect("EBASE");
    let top = echo_top_height(&volume, threshold).expect("ET");
    let depth = echo_depth(&volume, threshold).expect("EDEPTH");
    for grid in [&cmax, &base, &top, &depth] {
        assert_eq!(grid.shape().0, as_usize(&golden["rows"]));
        assert_eq!(grid.shape().1, as_usize(&golden["gates"]));
    }
    // The output geometry is the lowest reflectivity tilt's.
    let base_cut = &volume.sweeps[as_usize(&golden["base_sweep"])];
    assert_close(
        f64::from(tilt(&volume, as_usize(&golden["base_sweep"]))),
        as_f64(&golden["base_elevation_deg"]),
        1e-3,
        "base tilt elevation",
    );
    let base_reflectivity = base_cut.field(&FieldName::Dbzh).expect("base DBZH");
    assert_eq!(cmax.gates, base_reflectivity.gates);
    assert_eq!(cmax.shape(), base_reflectivity.shape());

    let mut upper_wins = 0usize;
    for entry in array(&golden["cells"]) {
        let (row, gate) = (as_usize(&entry["row"]), as_usize(&entry["gate"]));
        let what = format!("cell ({row}, {gate})");
        match as_opt_f64(&entry["cmax"]) {
            None => assert!(cell(&cmax, row, gate).is_none(), "{what}: CMAX present"),
            Some(expected) => {
                let value = f64::from(cell(&cmax, row, gate).expect("CMAX"));
                assert_close(value, expected, 0.01, &format!("{what} CMAX"));
                if let Some(base_value) = as_opt_f64(&entry["base_value"]) {
                    let lowest =
                        f64::from(cell(base_reflectivity, row, gate).expect("base tilt value"));
                    assert_close(lowest, base_value, 0.01, &format!("{what} lowest tilt"));
                    if value > lowest {
                        upper_wins += 1;
                    }
                }
            }
        }
        for (grid, key) in [
            (&base, "echo_base_m"),
            (&top, "echo_top_m"),
            (&depth, "depth_m"),
        ] {
            match as_opt_f64(&entry[key]) {
                None => assert!(cell(grid, row, gate).is_none(), "{what}: {key} present"),
                Some(expected) => {
                    let value = f64::from(cell(grid, row, gate).expect(key));
                    assert_close(value, expected, 0.5, &format!("{what} {key}"));
                    assert!(value >= 0.0, "{what} {key} negative: {value}");
                }
            }
        }
    }
    // Where the lowest tilt is not the strongest, the column maximum comes from an
    // upper tilt (267 of the 469 sampled cells in the reference).
    assert_eq!(upper_wins, as_usize(&golden["upper_tilt_exceeds_base"]));
    assert!(upper_wins > 200);

    // The hail core column: 70.5 dBZ from the 0.68 deg surveillance tilt tops a column
    // whose lowest tilt reads 61 dBZ, with echo above 18.3 dBZ up to the 19.5 dBZ sample
    // at 8.2 km.
    let core = &golden["hail_core"];
    let (row, gate) = (as_usize(&core["row"]), as_usize(&core["gate"]));
    let column = array(&core["column"]);
    let strongest = column
        .iter()
        .max_by(|a, b| as_f64(&a["dbz"]).total_cmp(&as_f64(&b["dbz"])))
        .expect("column");
    assert_close(
        f64::from(cell(&cmax, row, gate).expect("core CMAX")),
        as_f64(&strongest["dbz"]),
        0.01,
        "hail core CMAX",
    );
    assert!(as_f64(&strongest["dbz"]) >= 70.0);
    let echo: Vec<f64> = column
        .iter()
        .filter(|sample| as_f64(&sample["dbz"]) >= f64::from(threshold))
        .map(|sample| as_f64(&sample["height_m"]))
        .collect();
    let (lowest, highest) = (
        echo.iter().cloned().fold(f64::INFINITY, f64::min),
        echo.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    assert_close(
        f64::from(cell(&base, row, gate).expect("core EBASE")),
        lowest,
        0.5,
        "hail core echo base",
    );
    assert_close(
        f64::from(cell(&top, row, gate).expect("core ET")),
        highest,
        0.5,
        "hail core echo top",
    );
    assert_close(
        f64::from(cell(&depth, row, gate).expect("core EDEPTH")),
        highest - lowest,
        0.5,
        "hail core echo depth",
    );
    assert!(highest - lowest > 7_000.0);
}
