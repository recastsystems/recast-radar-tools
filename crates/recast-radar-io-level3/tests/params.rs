//! Table V product dependent values ([`ProductDescription::parameters`])
//! against MetPy 1.7.1's `Level3File.metadata` for every corpus file MetPy
//! reads with a product-specific entry (golden `metpy_detail.metadata`,
//! `tools/level3_golden.py`).
//!
//! MetPy names its values differently and in its own units; [`expected`]
//! maps each MetPy key to the parameter it reads and the factor between the
//! two (MetPy gives echo tops and layer heights in feet, the rate of product
//! 176 and the percentage of product 177 unscaled, and scales correlation
//! coefficient halfwords by 0.00333 where the ICD's "x300" gives 1/300). Three MetPy values
//! combine a date and a minutes halfword the parameters keep apart (products
//! 32, 113 and 173); they are compared with the pair. MetPy reads the cross
//! section end points (50, 51) from halfwords 27, 28, 30 and 47, not Table V's
//! 47-50; the test confirms it reads those halfwords, and the parameters
//! follow Table V (the product's own labels agree, `tests/volume.rs`).
//! Products MetPy reads with default metadata (`defaultVals`: 39, 42-47, 53,
//! 60, 73, 84, 197) are skipped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::Json;
use recast_radar_io_level3::{ParameterValue, ProductParameter, decode_product};

/// The parameter a MetPy key reads, and the factor from the parameter's
/// value to MetPy's.
fn expected(product: i16, key: &str) -> Option<(&'static str, f64)> {
    let velocity = matches!(product, 22..=27 | 55 | 56 | 93 | 99 | 154 | 182 | 183);
    Some(match key {
        "el_angle" => match product {
            35..=38 | 41 | 57 | 65..=67 | 90 | 97 | 134 | 135 => {
                ("avset_termination_elevation_angle", 1.0)
            }
            _ => ("elevation_angle", 1.0),
        },
        "max" => match product {
            _ if velocity => ("max_positive_velocity", 1.0),
            28..=30 | 155 | 184 | 185 => ("max_spectrum_width", 1.0),
            41 | 135 => ("max_echo_top", 1000.0),
            48 => ("max_wind_speed", 1.0),
            57 | 134 => ("max_vil", 1.0),
            138 => ("max_rainfall", 1.0),
            159 => ("max_zdr", 1.0),
            // MetPy scales by 0.00333, the ICD by 1/300 (halfword x300).
            161 | 167 => ("max_cc", 300.0 * 0.00333),
            163 => ("max_kdp", 1.0),
            169..=173 => ("max_accumulation", 1.0),
            174 | 175 => ("max_difference", 1.0),
            176 => ("max_rate", 1000.0),
            _ => ("max_reflectivity", 1.0),
        },
        "min" => match product {
            _ if velocity => ("max_negative_velocity", 1.0),
            159 => ("min_zdr", 1.0),
            161 | 167 => ("min_cc", 300.0 * 0.00333),
            163 => ("min_kdp", 1.0),
            174 | 175 => ("min_difference", 1.0),
            _ => return None,
        },
        "delta_time" => ("elevation_delta_time", 1.0),
        "supplemental_scan" => ("supplemental_scan", 1.0),
        "compression" => ("compression_method", 1.0),
        "uncompressed_size" => ("uncompressed_size", 1.0),
        "calib_const" => ("calibration_constant", 1.0),
        "bias" => ("mean_field_bias", 1.0),
        "gr_pairs" => ("gage_radar_pairs", 1.0),
        "max_rainfall" => ("max_rainfall", 1.0),
        "rainfall_begin" => match product {
            171 | 172 | 175 => ("accumulation_begin", 1.0),
            176 => ("hybrid_rate_scan_time", 1.0),
            _ => ("rainfall_begin", 1.0),
        },
        "rainfall_end" => ("rainfall_end", 1.0),
        "layer_bottom" => ("layer_bottom", 1000.0),
        "layer_top" => ("layer_top", 1000.0),
        "null_product" => ("null_product_flag", 1.0),
        "num_edited" => ("edited_radials", 1.0),
        "ref_thresh" => ("reflectivity_threshold", 1.0),
        "points_removed" => ("spurious_points_removed", 1.0),
        "num_storms" => ("number_of_storms", 1.0),
        "num_tvs" => ("number_of_tvs", 1.0),
        "num_etvs" => ("number_of_etvs", 1.0),
        "min_ref_thresh" => ("min_reflectivity_threshold", 1.0),
        "overlap_display_filter" => ("overlap_display_filter", 1.0),
        "min_strength_rank" => ("min_display_strength_rank", 1.0),
        // Product 55 (2620001H Table V): the storm motion of the region.
        "avg_dir" if product == 55 => ("storm_direction", 1.0),
        "avg_speed" if product == 55 => ("storm_speed", 1.0),
        "avg_dir" => ("average_storm_direction", 1.0),
        "avg_speed" => ("average_storm_speed", 1.0),
        "window_az" => ("window_azimuth", 1.0),
        "window_range" => ("window_range", 1.0),
        "height" => ("height_of_phenomena", 1.0),
        "alert_category" => ("alert_category", 1.0),
        "azimuth1" => ("point1_azimuth", 1.0),
        "range1" => ("point1_range", 1.0),
        "azimuth2" => ("point2_azimuth", 1.0),
        "range2" => ("point2_range", 1.0),
        "source" => ("motion_source_flag", 1.0),
        "dir_max" => ("max_wind_direction", 1.0),
        "alt_max" => ("max_wind_altitude", 1.0),
        "bypass_map_date" => ("bypass_map_time", 1.0),
        "notchwidth_map_date" => ("clutter_filter_map_time", 1.0),
        "clutter_bitmap" => ("segment_bit_map", 1.0),
        "cmd_map" => ("cmd_generated_bypass_map", 1.0),
        "cmd_generated" => ("cmd_generated", 1.0),
        "rpg_cut_num" => ("rpg_cut_number", 1.0),
        "period" => ("time_span", 1.0),
        "missing_period" => ("missing_period_flag", 1.0),
        "start_time" => ("start_time", 1.0),
        "precip_detected" => ("precipitation_detected_flag", 1.0),
        "need_bias" => ("bias_applied_flag", 1.0),
        "percent_filled" => ("percent_bins_filled", 1.0),
        "hybrid_percent_filled" => ("percent_bins_filled", 100.0),
        "max_elev" => ("highest_elevation_angle", 1.0),
        "mode_filter_size" => ("mode_filter_size", 1.0),
        _ => return None,
    })
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .with_timezone(&Utc)
}

fn number(value: &ParameterValue) -> Option<f64> {
    match value {
        ParameterValue::Int(v) => Some(*v as f64),
        ParameterValue::Float(v) => Some(*v),
        _ => None,
    }
}

/// A date parameter (`YYYY-MM-DD`) plus a minutes parameter.
fn date_plus_minutes(
    params: &BTreeMap<&str, &ProductParameter>,
    date: &str,
    minutes: &str,
) -> Option<DateTime<Utc>> {
    let ParameterValue::Date(day) = &params.get(date)?.value else {
        return None;
    };
    let minutes = number(&params.get(minutes)?.value)?;
    let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc() + Duration::minutes(minutes as i64))
}

#[test]
fn table_v_values_match_metpy() {
    let mut compared = 0usize;
    let mut products = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let metadata = golden.get("metpy_detail").get("metadata");
        let Json::Obj(members) = metadata else {
            continue;
        };
        if members.iter().any(|(k, _)| k == "defaultVals") {
            continue;
        }
        let Ok(product) = decode_product(&entry.bytes()) else {
            continue;
        };
        let code = product.description.product_code;
        let parameters = product.description.parameters();
        let params: BTreeMap<&str, &ProductParameter> =
            parameters.iter().map(|p| (p.name, p)).collect();
        for (key, value) in members {
            if matches!(key.as_str(), "msg_time" | "vol_time" | "prod_time") {
                continue;
            }
            let problem = match (code, key.as_str()) {
                // MetPy reads the cross section end points from its dep1-dep4
                // (halfwords 27, 28, 30, 47); Table V (2620001H) puts them in
                // halfwords 47-50, where the KLOT 1994 product 50 prints them
                // (`tests/volume.rs`, `cross_section_geometry_matches_its_axes`).
                (50 | 51, "azimuth1" | "range1" | "azimuth2" | "range2") => {
                    let hw = |n| f64::from(product.description.halfword(n).unwrap() as i16);
                    let metpy_halfword = match key.as_str() {
                        "azimuth1" => 27,
                        "range1" => 28,
                        "azimuth2" => 30,
                        _ => 47,
                    };
                    let n = value.as_f64().unwrap();
                    ((hw(metpy_halfword) * 0.1 - n).abs() > 1e-9)
                        .then(|| format!("halfword {metpy_halfword} {}", hw(metpy_halfword)))
                }
                (32, "avg_time") => {
                    let ours =
                        date_plus_minutes(&params, "hybrid_scan_date", "hybrid_scan_average_time");
                    (ours != Some(parse_time(value.as_str().unwrap()))).then(|| format!("{ours:?}"))
                }
                (113, "clutter_filter_map_dt") => {
                    let ours = date_plus_minutes(
                        &params,
                        "clutter_filter_map_date",
                        "clutter_filter_map_minutes",
                    );
                    (ours != Some(parse_time(value.as_str().unwrap()))).then(|| format!("{ours:?}"))
                }
                (173, "rainfall_end") => {
                    let ours = date_plus_minutes(&params, "end_date", "end_time");
                    (ours != Some(parse_time(value.as_str().unwrap()))).then(|| format!("{ours:?}"))
                }
                _ => {
                    let Some((name, factor)) = expected(code, key) else {
                        failures.push(format!(
                            "{} product {code}: MetPy key {key} not mapped",
                            entry.id
                        ));
                        continue;
                    };
                    let Some(ours) = params.get(name) else {
                        failures.push(format!(
                            "{} product {code}: no parameter {name} for MetPy {key}",
                            entry.id
                        ));
                        continue;
                    };
                    match (&ours.value, value) {
                        (ParameterValue::Time(t), Json::Str(s)) => {
                            (*t != parse_time(s)).then(|| format!("{t}"))
                        }
                        (ParameterValue::Text(t), Json::Str(s)) => {
                            let metpy = match s.as_str() {
                                "Non-supplemental scan" => "none",
                                "SAILS scan" => "sails",
                                "MRLE scan" => "mrle",
                                other => other,
                            };
                            (*t != metpy).then(|| (*t).to_owned())
                        }
                        (v, Json::Num(n)) => {
                            let ours = number(v).map(|x| x * factor);
                            let ok = ours.is_some_and(|x| (x - n).abs() <= 1e-9 * n.abs().max(1.0));
                            (!ok).then(|| format!("{ours:?}"))
                        }
                        (v, other) => Some(format!("{v:?} vs {other:?}")),
                    }
                }
            };
            match problem {
                Some(ours) => failures.push(format!(
                    "{} product {code}: {key}: MetPy {value:?}, parameters give {ours}",
                    entry.id
                )),
                None => {
                    compared += 1;
                    products.insert(code);
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!(
        "{compared} values of {} products equal MetPy",
        products.len()
    );
    assert!(compared > 600, "{compared} values compared");
    assert!(products.len() >= 55, "{} products", products.len());
}

/// Velocity Azimuth Display (product 84, KLOT 1993-11-20 07:21Z; MetPy has no
/// entry for it): the product prints its fit, `FIT = 4 + 37 SIN( AZ + 287 )`
/// (knots, degrees). Table V's wind speed (halfword 47) is the amplitude and
/// the wind direction (halfword 48) the direction the wind comes from: the
/// fit is largest (outbound) at azimuth `90 - 287`, so the wind blows from
/// `270 - 287` modulo 360 = 343 degrees.
#[test]
fn vad_parameters_match_the_printed_fit() {
    use recast_radar_io_level3::Packet;

    let product = decode_product(&common::entry("l3-lot-084-19931120-0721").bytes()).unwrap();
    let fit = product
        .symbology
        .as_ref()
        .unwrap()
        .layers
        .iter()
        .flatten()
        .find_map(|p| match p {
            Packet::Text(t) if t.text.starts_with("FIT =") => Some(t.text.clone()),
            _ => None,
        })
        .unwrap();
    let numbers: Vec<f64> = fit
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter_map(|w| w.parse().ok())
        .collect();
    let [_, amplitude, phase] = numbers[..] else {
        panic!("{fit}");
    };
    let value = |name: &str| {
        product
            .description
            .parameters()
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| match p.value {
                ParameterValue::Int(v) => v as f64,
                ParameterValue::Float(v) => v,
                ref other => panic!("{other:?}"),
            })
            .unwrap()
    };
    assert_eq!(value("wind_speed"), amplitude);
    assert_eq!(value("wind_direction"), (270.0 - phase).rem_euclid(360.0));
}

/// Products 39 (Composite Reflectivity Contour), 42 (Echo Tops Contour) and
/// 53 (Weak Echo Region), whose Table V is the 1990s one NCDC reproduces in
/// DSI-7000 and which MetPy reads with default metadata: the halfwords
/// `params` names agree with the products' own content.
///
/// - 39 (KGRR 2001-10-11): `max_reflectivity` (halfword 47) is the largest
///   DBZM of the cell attribute table on its graphic pages, and
///   `contour_interval` (halfword 53, 5 dBZ) the step of its thresholds.
/// - 42 (KIND 1994-09-10): `contour_interval` (halfword 53, 5000 ft) is the
///   step of its thresholds (25-70 kft), and `max_echo_top` (halfword 47,
///   37 kft) is at or above its highest contour (30 kft) and below the next
///   threshold.
/// - Both: the highest contour drawn (packet 0x0802 colour level) is at or
///   below the maximum.
/// - 53 (KCAE 1994-06-29, KLOT 1994-11-06): `max_reflectivity` lies in the
///   highest data level of the slices, `elevation_bit_map` (halfwords 49-50,
///   bit `n` from the most significant = elevation cut `n`) selects cuts 1 to
///   N for its N slices, and `storm_id` is `53` with the window at 185.5 deg
///   / 59.5 nmi (KCAE's storm tracking product of that volume, NCEI archive,
///   lists storm 53 at 186/60) and `NS` with the window at the radar (KLOT;
///   the placement of the window is checked against the base reflectivity
///   in `tests/volume.rs`).
/// - 74 and 83: the edit halfwords of the radar coded message are 0 in the
///   final messages (checked for 83 in `tests/rcm.rs`).
#[test]
fn dsi_7000_parameters_of_products_39_42_53_and_74() {
    use recast_radar_io_level3::Packet;
    use recast_radar_io_level3::levels::{DataLevels, Level};
    use recast_radar_io_level3::packets::contour::{Contour, ContourPacket};
    use recast_radar_io_level3::packets::raster::RasterGrid;

    let value = |product: &recast_radar_io_level3::Level3Product, name: &str| {
        product
            .description
            .parameters()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .value
    };
    let contour = decode_product(&common::entry("l3-grr-039-20011011-0631").bytes()).unwrap();
    let largest = contour
        .cell_attributes()
        .unwrap()
        .cells
        .iter()
        .map(|c| c.max_dbz)
        .fold(f32::MIN, f32::max);
    assert_eq!(largest, 41.0);
    assert_eq!(value(&contour, "max_reflectivity"), ParameterValue::Int(41));

    // Contour products: the interval is the threshold step, and the highest
    // contour drawn is at or below the maximum.
    for (id, max_name, max, interval, interval_per_unit) in [
        ("l3-grr-039-20011011-0631", "max_reflectivity", 41, 5, 1.0),
        ("l3-ind-042-19940910-1642", "max_echo_top", 37, 5000, 1000.0),
    ] {
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        assert_eq!(value(&product, max_name), ParameterValue::Int(max), "{id}");
        assert_eq!(
            value(&product, "contour_interval"),
            ParameterValue::Int(interval),
            "{id}"
        );
        let levels = DataLevels::from_description(&product.description).unwrap();
        let thresholds: Vec<f64> = (0..16).filter_map(|n| levels.level(n).value()).collect();
        assert!(thresholds.len() >= 10, "{id}: {thresholds:?}");
        for pair in thresholds.windows(2) {
            assert_eq!(
                (pair[1] - pair[0]) * interval_per_unit,
                interval as f64,
                "{id}: {thresholds:?}"
            );
        }
        let drawn = product
            .symbology
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .flatten()
            .filter_map(|p| match p {
                Packet::Contour(ContourPacket {
                    contour: Contour::ColorLevel(level),
                    ..
                }) => levels.level(*level).value(),
                _ => None,
            })
            .fold(f64::MIN, f64::max);
        assert!(drawn <= max as f64, "{id}: contour {drawn} above {max}");
        if id.contains("042") {
            assert_eq!(drawn, 30.0);
            assert_eq!(levels.level(3).value(), Some(35.0));
        }
    }
    for id in [
        "l3-fws-rcm-19950517-2310",
        "l3-tlx-rcm-20130520-2016",
        "l3-tlx-rcm-20220503-004553",
    ] {
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        for name in ["edit_decision_time", "editing_timeout", "edited_indicator"] {
            assert_eq!(value(&product, name), ParameterValue::Int(0), "{id} {name}");
        }
    }

    for (id, storm, azimuth, range) in [
        ("l3-cae-053-19940629-1906", "53", 185.5, 59.5),
        ("l3-lot-053-19941106-0246", "NS", 0.0, 0.0),
    ] {
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        assert_eq!(
            value(&product, "storm_id"),
            ParameterValue::Characters(storm.into()),
            "{id}"
        );
        assert_eq!(
            value(&product, "window_azimuth"),
            ParameterValue::Float(azimuth)
        );
        assert_eq!(
            value(&product, "window_range"),
            ParameterValue::Float(range)
        );
        let grids: Vec<&RasterGrid> = product
            .symbology
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .flatten()
            .filter_map(|p| match p {
                Packet::Raster(r) => Some(&r.grid),
                _ => None,
            })
            .collect();
        let ParameterValue::Int(bits) = value(&product, "elevation_bit_map") else {
            panic!("{id}: bit map");
        };
        let cuts: Vec<u32> = (1..=20).filter(|n| bits & (1 << (31 - n)) != 0).collect();
        assert_eq!(
            cuts,
            (1..=grids.len() as u32).collect::<Vec<_>>(),
            "{id}: {bits:#x}"
        );
        assert_eq!(bits.count_ones() as usize, grids.len(), "{id}");
        // The highest level of the slices brackets halfword 47.
        let top = grids
            .iter()
            .flat_map(|g| g.levels())
            .copied()
            .max()
            .unwrap();
        let levels = DataLevels::from_description(&product.description).unwrap();
        let lower = match levels.level(u16::from(top)) {
            Level::Value(v) => v,
            other => panic!("{other:?}"),
        };
        let upper = match levels.level(u16::from(top) + 1) {
            Level::Value(v) => v,
            _ => f64::INFINITY,
        };
        let ParameterValue::Int(max) = value(&product, "max_reflectivity") else {
            panic!("{id}: max");
        };
        let max = max as f64;
        assert!(
            lower <= max && max < upper,
            "{id}: {max} not in [{lower}, {upper})"
        );
    }
}
