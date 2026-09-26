//! Storm attribute tables ([`recast_radar_io_level3::tables`]) against an
//! separate same-author reading of MetPy 1.7.1's page text, or of the page block of
//! the stand-alone products 101-104 that MetPy cannot read
//! (`tools/level3_tables_golden.py`, `testdata/level3/golden-tables.json`):
//! every row of every Storm Tracking Information, Hail Index, Mesocyclone,
//! TVS, Mesocyclone Detection and combined attribute table in the corpus
//! (products 58-61 and their stand-alone 101-104, 141, and 35-39), in the
//! SCIT, HDA and TDA layouts and in those of 1993-1997.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::Json;
use recast_radar_io_level3::decode_product;
use recast_radar_io_level3::tables::{AzRan, Motion, Qualified, Xy};

fn num(json: &Json) -> f32 {
    json.as_f64().unwrap_or_else(|| panic!("number: {json:?}")) as f32
}

fn opt_num(json: &Json) -> Option<f32> {
    (!json.is_null()).then(|| num(json))
}

fn golden_motion(json: &Json) -> Motion {
    match json.as_str() {
        Some("NEW") => Motion::New,
        _ => {
            let pair = json.items();
            Motion::Moving {
                direction_deg: num(&pair[0]),
                speed_kt: num(&pair[1]),
            }
        }
    }
}

fn golden_qualified(json: &Json) -> Qualified {
    let pair = json.items();
    Qualified {
        value: num(&pair[0]),
        qualifier: pair[1].as_str().unwrap().chars().next(),
    }
}

fn azran(az: &Json, ran: &Json) -> AzRan {
    AzRan {
        azimuth_deg: num(az),
        range_nm: num(ran),
    }
}

/// The graphic product whose tables a product carries: the stand-alone
/// alphanumeric products 101-104 carry the pages of 58-61, and the composite
/// reflectivity products 35-39 the combined attribute table.
fn table_family(code: i16) -> i16 {
    match code {
        101 => 58,
        102 => 59,
        103 => 60,
        104 => 61,
        35..=39 => 37,
        other => other,
    }
}

#[test]
fn storm_tables_match_golden() {
    let path = common::testdata_dir().join("level3/golden-tables.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut rows = 0;
    let mut tables = 0;
    for file in golden.get("files").items() {
        let id = file.get("id").as_str().unwrap();
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        let table = file.get("table");
        let expected = table.get("rows").items();
        match table_family(product.description.product_code) {
            58 => {
                let Some(ours) = product.storm_tracking() else {
                    assert!(table.is_null(), "{id}: no table");
                    continue;
                };
                assert_eq!(
                    ours.average_speed_kt,
                    opt_num(table.get("average_speed")),
                    "{id}"
                );
                assert_eq!(
                    ours.average_direction_deg,
                    opt_num(table.get("average_direction")),
                    "{id}"
                );
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (cell, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(cell.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(cell.position, azran(&r[1], &r[2]), "{id}");
                    assert_eq!(cell.motion, golden_motion(&r[3]), "{id}");
                    for (k, forecast) in cell.forecasts.iter().enumerate() {
                        let g = &r[4 + k];
                        let expected = (!g.is_null()).then(|| {
                            let p = g.items();
                            azran(&p[0], &p[1])
                        });
                        assert_eq!(*forecast, expected, "{id} {}", cell.id);
                    }
                    assert_eq!(cell.forecast_error_nm, num(&r[8]), "{id}");
                    assert_eq!(cell.mean_error_nm, num(&r[9]), "{id}");
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            59 => {
                let Some(ours) = product.hail_index() else {
                    assert!(table.is_null(), "{id}: no table");
                    continue;
                };
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (cell, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(cell.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(cell.posh_percent.map(f32::from), opt_num(&r[1]), "{id}");
                    assert_eq!(cell.poh_percent.map(f32::from), opt_num(&r[2]), "{id}");
                    assert_eq!(
                        cell.max_size_in,
                        (!r[3].is_null()).then(|| golden_qualified(&r[3])),
                        "{id}"
                    );
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            60 => {
                let ours = product.mesocyclone_table().expect("mesocyclone table");
                assert_eq!(ours.features.len(), expected.len(), "{id}");
                for (f, row) in ours.features.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(f64::from(f.feature_id), r[0].as_f64().unwrap(), "{id}");
                    assert_eq!(f.storm_id, r[1].as_str().unwrap(), "{id}");
                    assert_eq!(f.feature_type, r[2].as_str().unwrap(), "{id}");
                    assert_eq!(f.tvs_id.map(f64::from), r[3].as_f64(), "{id}");
                    assert_eq!([f.base_kft, f.top_kft], [num(&r[4]), num(&r[5])], "{id}");
                    assert_eq!(f.position, azran(&r[6], &r[7]), "{id}");
                    assert_eq!(
                        [
                            f.height_kft,
                            f.radial_diameter_nm,
                            f.azimuthal_diameter_nm,
                            f.shear
                        ],
                        [num(&r[8]), num(&r[9]), num(&r[10]), num(&r[11])],
                        "{id}"
                    );
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.features.len();
            }
            61 => {
                let Some(ours) = product.tvs_table() else {
                    assert!(table.is_null(), "{id}: no table");
                    continue;
                };
                assert_eq!(ours.features.len(), expected.len(), "{id}");
                for (f, row) in ours.features.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(f.kind, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(f.storm_id, r[1].as_str().unwrap(), "{id}");
                    assert_eq!(f.position, azran(&r[2], &r[3]), "{id}");
                    assert_eq!(
                        [
                            f.average_dv_kt,
                            f.low_level_dv_kt,
                            f.max_dv_kt,
                            f.max_dv_height_kft
                        ],
                        [num(&r[4]), num(&r[5]), num(&r[6]), num(&r[7])],
                        "{id}"
                    );
                    assert_eq!(f.depth_kft, golden_qualified(&r[8]), "{id}");
                    assert_eq!(f.base_kft, golden_qualified(&r[9]), "{id}");
                    assert_eq!(f.top_kft, golden_qualified(&r[10]), "{id}");
                    assert_eq!(f.max_shear, num(&r[11]), "{id}");
                    assert_eq!(f.max_shear_height_kft, num(&r[12]), "{id}");
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.features.len();
            }
            141 => {
                let ours = product.mesocyclone_detections().expect("MD table");
                assert_eq!(ours.circulations.len(), expected.len(), "{id}");
                for (c, row) in ours.circulations.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(f64::from(c.id), r[0].as_f64().unwrap(), "{id}");
                    assert_eq!(c.position, azran(&r[1], &r[2]), "{id}");
                    assert_eq!(f64::from(c.strength_rank), r[3].as_f64().unwrap(), "{id}");
                    assert_eq!(
                        c.strength_rank_type,
                        r[4].as_str().unwrap().chars().next(),
                        "{id}"
                    );
                    assert_eq!(c.storm_id, r[5].as_str().unwrap(), "{id}");
                    assert_eq!(c.low_level_rv_kt, num(&r[6]), "{id}");
                    assert_eq!(c.low_level_dv_kt, num(&r[7]), "{id}");
                    assert_eq!(c.base_kft, golden_qualified(&r[8]), "{id}");
                    assert_eq!(c.depth_kft, golden_qualified(&r[9]), "{id}");
                    assert_eq!(c.storm_relative_depth_percent, num(&r[10]), "{id}");
                    assert_eq!(c.max_rv_height_kft, num(&r[11]), "{id}");
                    assert_eq!(c.max_rv_kt, num(&r[12]), "{id}");
                    assert_eq!(Some(c.tvs), r[13].as_bool(), "{id}");
                    assert_eq!(
                        c.motion,
                        (!r[14].is_null()).then(|| golden_motion(&r[14])),
                        "{id}"
                    );
                    assert_eq!(f64::from(c.msi), r[15].as_f64().unwrap(), "{id}");
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.circulations.len();
            }
            37 => {
                let Some(ours) = product.cell_attributes() else {
                    assert!(table.is_null(), "{id}: no table");
                    continue;
                };
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (c, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(c.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(c.position, azran(&r[1], &r[2]), "{id}");
                    assert_eq!(c.tvs.as_deref(), r[3].as_str(), "{id}");
                    assert_eq!(c.mda_rank.map(|v| v as f32), opt_num(&r[4]), "{id}");
                    assert_eq!(c.posh_percent.map(f32::from), opt_num(&r[5]), "{id}");
                    assert_eq!(c.poh_percent.map(f32::from), opt_num(&r[6]), "{id}");
                    assert_eq!(
                        c.max_size_in,
                        (!r[7].is_null()).then(|| golden_qualified(&r[7])),
                        "{id}"
                    );
                    assert_eq!(
                        [c.vil, c.max_dbz, c.max_dbz_height_kft],
                        [num(&r[8]), num(&r[9]), num(&r[10])],
                        "{id}"
                    );
                    assert_eq!(c.top_kft, golden_qualified(&r[11]), "{id}");
                    assert_eq!(c.motion, golden_motion(&r[12]), "{id}");
                    assert_eq!(c.meso_feature.as_deref(), r[13].as_str(), "{id}");
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            other => panic!("{id}: product {other}"),
        }
        tables += 1;
    }
    // 33 tables of these layouts (the 1993-1997 tables of other layouts:
    // see `legacy_storm_tables_match_golden`).
    assert_eq!(tables, 33);
    assert_eq!(rows, 520);
}

/// The tables of the storm series, hail and TVS algorithms of 1995-1997
/// (`legacy` in the golden JSON): every row of the corpus legacy Storm
/// Tracking, hail and TVS tables.
#[test]
fn legacy_storm_tables_match_golden() {
    let path = common::testdata_dir().join("level3/golden-tables.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (mut rows, mut tables) = (0, 0);
    for file in golden.get("files").items() {
        let id = file.get("id").as_str().unwrap();
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        let legacy = file.get("legacy");
        let expected = legacy.get("rows").items();
        match table_family(product.description.product_code) {
            58 => {
                let Some(ours) = product.legacy_storm_tracking() else {
                    assert!(legacy.is_null(), "{id}: no legacy table");
                    continue;
                };
                assert!(product.storm_tracking().is_none(), "{id}");
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (cell, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(cell.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(cell.position, azran(&r[1], &r[2]), "{id}");
                    assert_eq!(cell.motion, golden_motion(&r[3]), "{id}");
                    assert_eq!(
                        [cell.speed_x_kt, cell.speed_y_kt],
                        [num(&r[4]), num(&r[5])],
                        "{id}"
                    );
                    for (k, forecast) in cell.forecasts.iter().enumerate() {
                        let g = &r[6 + k];
                        let expected = (!g.is_null()).then(|| {
                            let p = g.items();
                            Xy {
                                x_nm: num(&p[0]),
                                y_nm: num(&p[1]),
                            }
                        });
                        assert_eq!(*forecast, expected, "{id} {}", cell.id);
                    }
                    assert_eq!(
                        [
                            cell.forecast_error_nm,
                            cell.mean_error_nm,
                            cell.track_variance_x_nm,
                            cell.track_variance_y_nm
                        ],
                        [num(&r[10]), num(&r[11]), num(&r[12]), num(&r[13])],
                        "{id} {}",
                        cell.id
                    );
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            59 => {
                let Some(ours) = product.legacy_hail_index() else {
                    assert!(legacy.is_null(), "{id}: no legacy table");
                    continue;
                };
                assert!(product.hail_index().is_none(), "{id}");
                assert_eq!(
                    ours.average_speed_kt,
                    opt_num(legacy.get("average_speed")),
                    "{id}"
                );
                assert_eq!(
                    ours.average_direction_deg,
                    opt_num(legacy.get("average_direction")),
                    "{id}"
                );
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (cell, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(cell.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(cell.status, r[1].as_str().unwrap(), "{id}");
                    assert_eq!(
                        [
                            cell.positive_weight,
                            cell.probable_weight,
                            cell.confidence_factor,
                            cell.score
                        ],
                        [num(&r[2]), num(&r[3]), num(&r[4]), num(&r[5])],
                        "{id} {}",
                        cell.id
                    );
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            61 => {
                let Some(ours) = product.legacy_tvs_table() else {
                    assert!(legacy.is_null(), "{id}: no legacy table");
                    continue;
                };
                assert!(product.tvs_table().is_none(), "{id}");
                assert_eq!(ours.features.len(), expected.len(), "{id}");
                for (t, row) in ours.features.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(f64::from(t.tvs_id), r[0].as_f64().unwrap(), "{id}");
                    assert_eq!(f64::from(t.meso_id), r[1].as_f64().unwrap(), "{id}");
                    assert_eq!(t.storm_id, r[2].as_str().unwrap(), "{id}");
                    assert_eq!(t.base_height_kft, num(&r[3]), "{id}");
                    assert_eq!(t.base_position, azran(&r[4], &r[5]), "{id}");
                    assert_eq!(t.max_shear_height_kft, num(&r[6]), "{id}");
                    assert_eq!(t.max_shear_position, azran(&r[7], &r[8]), "{id}");
                    assert_eq!(
                        [t.shear, t.orientation_deg, t.rotation_rad],
                        [num(&r[9]), num(&r[10]), num(&r[11])],
                        "{id}"
                    );
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.features.len();
            }
            37 => {
                let Some(ours) = product.legacy_cell_attributes() else {
                    assert!(legacy.is_null(), "{id}: no legacy table");
                    continue;
                };
                assert!(product.cell_attributes().is_none(), "{id}");
                assert_eq!(ours.cells.len(), expected.len(), "{id}");
                for (c, row) in ours.cells.iter().zip(expected) {
                    let r = row.items();
                    assert_eq!(c.id, r[0].as_str().unwrap(), "{id}");
                    assert_eq!(c.position, azran(&r[1], &r[2]), "{id}");
                    assert_eq!(
                        (Some(c.tvs), Some(c.mesocyclone)),
                        (r[3].as_bool(), r[4].as_bool()),
                        "{id}"
                    );
                    assert_eq!(c.hail, r[5].as_str().unwrap(), "{id}");
                    assert_eq!(
                        [c.max_dbz, c.max_dbz_height_kft, c.low_level_velocity_kt],
                        [num(&r[6]), num(&r[7]), num(&r[8])],
                        "{id}"
                    );
                    assert_eq!(c.top_kft, golden_qualified(&r[9]), "{id}");
                    assert_eq!(
                        c.motion,
                        Motion::Moving {
                            direction_deg: num(&r[10]),
                            speed_kt: num(&r[11])
                        },
                        "{id}"
                    );
                    assert_eq!(c.mass_weighted_volume, num(&r[12]), "{id}");
                }
                assert!(ours.unparsed.is_empty(), "{id}: {:?}", ours.unparsed);
                rows += ours.cells.len();
            }
            _ => continue,
        }
        tables += 1;
    }
    assert_eq!(tables, 12);
    assert_eq!(rows, 84);
}

/// The legacy TVS table names the mesocyclone feature of product 60 whose
/// TVS ID is the TVS: KILX 1996-04-19 23:03 and KLZK 1997-03-01 20:27.
#[test]
fn legacy_tvs_links_the_mesocyclone_table() {
    for (tvs, meso) in [
        ("l3-ilx-ntv-19960419-2303", "l3-ilx-nme-19960419-2303"),
        ("l3-lzk-ntv-19970301-2027", "l3-lzk-nme-19970301-2027"),
    ] {
        let tvs = decode_product(&common::entry(tvs).bytes())
            .unwrap()
            .legacy_tvs_table()
            .unwrap();
        let meso = decode_product(&common::entry(meso).bytes())
            .unwrap()
            .mesocyclone_table()
            .unwrap();
        assert_eq!(tvs.features.len(), 1);
        for t in &tvs.features {
            let feature = meso
                .features
                .iter()
                .find(|f| f.feature_type == "MESO" && f.feature_id == t.meso_id)
                .unwrap();
            assert_eq!(feature.tvs_id, Some(t.tvs_id));
            assert_eq!(feature.storm_id, t.storm_id);
        }
    }
}
