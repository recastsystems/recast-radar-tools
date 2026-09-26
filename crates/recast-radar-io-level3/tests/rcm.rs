//! Radar Coded Message groups ([`RadarCodedMessage`]) against the
//! separate Appendix B reading by the same author in `tools/level3_rcm_golden.py`
//! (`testdata/level3/golden-rcm.json`) for every product 74 file of the
//! corpus: header, the three parts, every intensity group on the fine grid
//! (SHA-256 and histogram of the 100 x 100 grid), echo top, centroids,
//! winds, TVS, mesocyclones and storm tops. `/NI` equals the number of fine
//! boxes above level 0 in every message.
//!
//! The unedited Radar Coded Message (product 83, DSI-7000 Figure 3-22) carries the
//! radar coded message of its volume: `irm_is_the_radar_coded_message_as_graphics`
//! checks KILX 1996-04-19 23:09 against the product 74 of the same volume.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use common::Json;
use recast_radar_io_level3::hrap::{self, LocalGrid};
use recast_radar_io_level3::packets::IrmPacket;
use recast_radar_io_level3::rcm::{FineBox, PartA, PartB, PartC};
use recast_radar_io_level3::{
    Level3Product, Packet, RadarCodedMessage, TabularLayout, decode_product,
};

fn int(json: &Json) -> i64 {
    json.as_i64().unwrap_or_else(|| panic!("integer: {json:?}"))
}

fn time(json: &Json) -> Option<String> {
    json.as_str().map(str::to_owned)
}

fn fine(location: FineBox) -> [i64; 2] {
    [i64::from(location.row), i64::from(location.column)]
}

fn check_a(a: &PartA, golden: &Json) {
    assert_eq!(Some(a.header.site.as_str()), golden.get("site").as_str());
    assert_eq!(
        a.header
            .time
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string()),
        time(golden.get("time"))
    );
    assert_eq!(Some(a.status.as_str()), golden.get("status").as_str());
    assert_eq!(Some(a.no_reportable_echoes), golden.get("radne").as_bool());
    assert_eq!(Some(a.radar_down), golden.get("radom").as_bool());
    assert_eq!(a.mode.as_deref(), golden.get("mode").as_str());
    assert_eq!(
        a.scan_strategy.as_deref(),
        golden.get("scan_strategy").as_str()
    );
    assert_eq!(a.intensity_count.map(i64::from), golden.get("ni").as_i64());
    assert_eq!(a.intensities.len() as i64, int(golden.get("groups")));
    assert_eq!(a.reported_cells() as i64, int(golden.get("cells")));
    assert_eq!(a.nonzero_cells() as i64, int(golden.get("nonzero")));
    assert_eq!(
        a.intensity_count.map(|n| n as usize),
        Some(a.nonzero_cells())
    );
    let grid = a.intensity_grid();
    assert_eq!(
        common::sha256_hex(&grid),
        golden.get("grid_sha256").as_str().unwrap()
    );
    let mut histogram: BTreeMap<String, i64> = BTreeMap::new();
    for &level in grid.iter().filter(|&&l| l > 0) {
        *histogram.entry(level.to_string()).or_default() += 1;
    }
    let Json::Obj(golden_histogram) = golden.get("histogram") else {
        panic!("histogram");
    };
    let golden_histogram: BTreeMap<String, i64> = golden_histogram
        .iter()
        .map(|(k, v)| (k.clone(), int(v)))
        .collect();
    assert_eq!(histogram, golden_histogram);
    let top = a.max_echo_top.unwrap();
    let golden_top: Vec<i64> = golden.get("max_top").items().iter().map(int).collect();
    assert_eq!(
        [
            i64::from(top.height_hft),
            fine(top.location)[0],
            fine(top.location)[1]
        ],
        golden_top[..]
    );
    assert_eq!(a.centroid_count.map(i64::from), golden.get("ncen").as_i64());
    let centroids: Vec<(String, [i64; 2], i64, i64)> = a
        .centroids
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                fine(c.location),
                i64::from(c.direction_deg.unwrap()),
                i64::from(c.speed_kt.unwrap()),
            )
        })
        .collect();
    let golden_centroids: Vec<(String, [i64; 2], i64, i64)> = golden
        .get("centroids")
        .items()
        .iter()
        .map(|c| {
            let c = c.items();
            (
                c[0].as_str().unwrap().to_owned(),
                [int(&c[1]), int(&c[2])],
                int(&c[3]),
                int(&c[4]),
            )
        })
        .collect();
    assert_eq!(centroids, golden_centroids);
}

fn check_b(b: &PartB, golden: &Json) {
    assert_eq!(Some(b.header.site.as_str()), golden.get("site").as_str());
    assert_eq!(Some(b.not_available), golden.get("vadna").as_bool());
    let winds: Vec<(i64, String, i64, i64)> = b
        .winds
        .iter()
        .map(|w| {
            (
                i64::from(w.height_hft),
                w.confidence.to_string(),
                i64::from(w.direction_deg),
                i64::from(w.speed_kt),
            )
        })
        .collect();
    let golden_winds: Vec<(i64, String, i64, i64)> = golden
        .get("winds")
        .items()
        .iter()
        .map(|w| {
            let w = w.items();
            (
                int(&w[0]),
                w[1].as_str().unwrap().to_owned(),
                int(&w[2]),
                int(&w[3]),
            )
        })
        .collect();
    assert_eq!(winds, golden_winds);
    assert!(b.winds.iter().all(|w| w.rms_kt().is_some()));
}

fn check_c(c: &PartC, golden: &Json) {
    assert_eq!(Some(c.header.site.as_str()), golden.get("site").as_str());
    assert_eq!(c.tvs_count.map(i64::from), golden.get("ntvs").as_i64());
    assert_eq!(
        c.mesocyclone_count.map(i64::from),
        golden.get("nmes").as_i64()
    );
    assert_eq!(c.centroid_count.map(i64::from), golden.get("ncen").as_i64());
    let numbered = |items: &[(u32, FineBox)]| -> Vec<[i64; 3]> {
        items
            .iter()
            .map(|(n, b)| [i64::from(*n), fine(*b)[0], fine(*b)[1]])
            .collect()
    };
    let golden_numbered = |json: &Json| -> Vec<[i64; 3]> {
        json.items()
            .iter()
            .map(|t| {
                let t = t.items();
                [int(&t[0]), int(&t[1]), int(&t[2])]
            })
            .collect()
    };
    assert_eq!(numbered(&c.tvs), golden_numbered(golden.get("tvs")));
    assert_eq!(
        numbered(&c.mesocyclones),
        golden_numbered(golden.get("mesocyclones"))
    );
    let tops: Vec<(String, [i64; 2], i64, String)> = c
        .storm_tops
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                fine(t.location),
                i64::from(t.top_hft),
                t.hail.to_string(),
            )
        })
        .collect();
    let golden_tops: Vec<(String, [i64; 2], i64, String)> = golden
        .get("storm_tops")
        .items()
        .iter()
        .map(|t| {
            let t = t.items();
            (
                t[0].as_str().unwrap().to_owned(),
                [int(&t[1]), int(&t[2])],
                int(&t[3]),
                t[4].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(tops, golden_tops);
    let remarks: Vec<&str> = golden
        .get("remarks")
        .items()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(c.remarks, remarks);
}

#[test]
fn radar_coded_messages_match_golden() {
    let path = common::testdata_dir().join("level3/golden-rcm.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let files = golden.get("files").items();
    // Four product 74 files and the messages inside two products 83; the
    // KLOT 1994 product 83 carries no message (only its grid is recorded).
    assert_eq!(files.len(), 7);
    let mut messages = 0;
    for file in files {
        let id = file.get("id").as_str().unwrap();
        if file.get("no_message").as_bool() == Some(true) {
            assert_eq!(id, "l3-lot-irm-19941031-0503");
            let product = decode_product(&common::entry(id).bytes()).unwrap();
            assert!(product.radar_coded_message().unwrap().is_none(), "{id}");
            continue;
        }
        messages += 1;
        let product = decode_product(&common::entry(id).bytes()).unwrap();
        let rcm: RadarCodedMessage = product.radar_coded_message().unwrap().expect("RCM text");
        assert_eq!(Some(rcm.node.as_str()), file.get("node").as_str(), "{id}");
        assert_eq!(
            Some(rcm.category.as_str()),
            file.get("category").as_str(),
            "{id}"
        );
        assert_eq!(Some(rcm.site.as_str()), file.get("site").as_str(), "{id}");
        check_a(rcm.part_a.as_ref().unwrap(), file.get("part_a"));
        check_b(rcm.part_b.as_ref().unwrap(), file.get("part_b"));
        check_c(rcm.part_c.as_ref().unwrap(), file.get("part_c"));
        assert!(rcm.unparsed.is_empty(), "{id}: {:?}", rcm.unparsed);
    }
    assert_eq!(messages, 6);
}

/// KILX 1996-04-19 23:09: the unedited Radar Coded Message (product 83,
/// `docs/level3/reference.md` section 4.4) is the radar coded message of its
/// volume as graphics. Its Tabular Alphanumeric Block holds the product 74
/// of the same volume (second headers of product 74, then the same text);
/// packet 32 is that message's Part A intensity grid (the separate
/// Python reading of packet 32 and of the text agree in
/// `golden-rcm.json`); packet 31 counts the storm ID and symbol pairs that
/// follow, which name the message's centroids in order, each within one fine
/// box of its named box when placed on the 1/16 LFM grid
/// ([`LocalGrid::radar_coded_message`]).
#[test]
fn irm_is_the_radar_coded_message_as_graphics() {
    let irm = decode_product(&common::entry("l3-ilx-irm-19960419-2309").bytes()).unwrap();
    let rcm = decode_product(&common::entry("l3-ilx-rcm-19960419-2309").bytes()).unwrap();
    let desc = &irm.description;
    assert_eq!((irm.message_header.code, desc.product_code), (83, 83));

    // The embedded message: headers of product 74 and the product's text.
    let tab = irm.tabular.as_ref().unwrap();
    assert_eq!(tab.layout, TabularLayout::RadarCodedMessage);
    assert_eq!(tab.message_header.unwrap().code, 74);
    assert_eq!(tab.description.as_ref().unwrap().product_code, 74);
    assert_eq!(tab.data, rcm.tabular.as_ref().unwrap().data);
    let message = irm.radar_coded_message().unwrap().unwrap();
    assert_eq!(message, rcm.radar_coded_message().unwrap().unwrap());

    let layers = &irm.symbology.as_ref().unwrap().layers;
    // Layer 0: packet 30.
    let [Packet::Irm(IrmPacket::Parameters { values })] = layers[0].as_slice() else {
        panic!("layer 0: {:?}", layers[0]);
    };
    assert_eq!(*values, [-36.025, 114.0, 117.5, 21.0, 0.0]);
    check_irm_graphics("l3-ilx-irm-19960419-2309", &irm);

    // The volume: the grid as one sweep on the 1/16 LFM grid, like product 74's.
    let irm_volume = irm.to_volume().unwrap();
    let rcm_volume = rcm.to_volume().unwrap();
    let (a, b) = (&irm_volume.sweeps[0], &rcm_volume.sweeps[0]);
    assert_eq!(a.fields[0].data, b.fields[0].data);
    assert_eq!(a.range, b.range);
    assert_eq!(a.extra_vars, b.extra_vars);
}

/// Every product 83 of the corpus with its message (KILX 1996, KTLX 1994;
/// the KLOT 1994 one names a tabular block at the end of its message and
/// has none): the graphics are its radar coded message's.
#[test]
fn every_irm_draws_its_radar_coded_message() {
    let mut checked = 0;
    for entry in common::level3_manifest() {
        if entry.tag("product") != Some("83") {
            continue;
        }
        let irm = decode_product(&entry.bytes()).unwrap();
        if irm.tabular.is_none() {
            assert_eq!(
                u64::from(irm.description.tabular_offset) * 2,
                u64::from(irm.message_header.length),
                "{}",
                entry.id
            );
            continue;
        }
        check_irm_graphics(&entry.id, &irm);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// Checks the three symbology layers of a product 83 against its radar coded
/// message: packet 32 is the message's Part A grid (also equal to the
/// separate reading, `golden-rcm.json`), and packet 31 counts the storm ID
/// and symbol pairs that follow, which name the message's centroids in
/// order, each within one fine box of its named box on the 1/16 LFM grid.
fn check_irm_graphics(file: &str, irm: &Level3Product) {
    let desc = &irm.description;
    let message = irm.radar_coded_message().unwrap().unwrap();
    let path = common::testdata_dir().join("level3/golden-rcm.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let golden = golden
        .get("files")
        .items()
        .iter()
        .find(|f| f.get("id").as_str() == Some(file))
        .unwrap_or_else(|| panic!("{file}: not in golden-rcm.json"))
        .clone();

    let layers = &irm.symbology.as_ref().unwrap().layers;
    assert_eq!(layers.len(), 3, "{file}");
    assert!(
        matches!(
            layers[0].as_slice(),
            [Packet::Irm(IrmPacket::Parameters { .. })]
        ),
        "{file}"
    );
    // Layer 2: packet 32, the Part A grid.
    let [Packet::Raster(grid)] = layers[2].as_slice() else {
        panic!("layer 2: {:?}", layers[2]);
    };
    assert_eq!(
        (grid.code, grid.grid.rows(), grid.grid.columns()),
        (32, 100, 100)
    );
    let part_a = message.part_a.as_ref().unwrap();
    assert_eq!(grid.grid.levels(), part_a.intensity_grid().as_slice());
    let sha = common::sha256_hex(grid.grid.levels());
    assert_eq!(Some(sha.as_str()), golden.get("irm_grid_sha256").as_str());
    assert_eq!(
        Some(sha.as_str()),
        golden.get("part_a").get("grid_sha256").as_str()
    );

    // Layer 1: packet 31 and the storm pairs.
    let Some((Packet::Irm(IrmPacket::StormCount { count }), pairs)) = layers[1].split_first()
    else {
        panic!("layer 1: {:?}", layers[1]);
    };
    assert_eq!(usize::from(*count) * 2, pairs.len());
    let centroids = golden.get("part_a").get("centroids").items();
    assert_eq!(usize::from(*count), centroids.len());
    let local = LocalGrid::radar_coded_message(desc.latitude_deg, desc.longitude_deg);
    for (pair, centroid) in pairs.chunks_exact(2).zip(centroids) {
        let [Packet::Symbol(ids), Packet::Text(symbol)] = pair else {
            panic!("storm pair {pair:?}");
        };
        let recast_radar_io_level3::SymbolPacket::StormIds(ids) = ids else {
            panic!("storm pair {pair:?}");
        };
        let [id] = ids.as_slice() else {
            panic!("storm IDs {ids:?}");
        };
        assert_eq!((symbol.code, symbol.i, symbol.j), (2, id.i, id.j));
        let c = centroid.items();
        assert_eq!(id.id.trim(), c[0].as_str().unwrap());
        // I, J in 1/4 km east and north of the radar, along the great circle.
        let (east, north) = (f64::from(id.i) / 4.0, f64::from(id.j) / 4.0);
        let (lat, lon) = destination(
            desc.latitude_deg,
            desc.longitude_deg,
            east.atan2(north),
            east.hypot(north) / (hrap::EARTH_RADIUS_M / 1000.0),
        );
        let (x, y) = hrap::to_grid(lat, lon);
        let column = ((x - local.west) / local.box_size).floor() as i64;
        let row = ((local.north - y) / local.box_size).floor() as i64;
        let (named_row, named_column) = (int(&c[1]), int(&c[2]));
        assert!(
            (row - named_row).abs() <= 1 && (column - named_column).abs() <= 1,
            "storm {}: fine box ({row}, {column}), message ({named_row}, {named_column})",
            id.id
        );
    }
}

/// The point at `distance` (radians) from `lat`, `lon` (degrees) along the
/// great circle with initial bearing `bearing` (radians).
fn destination(lat: f64, lon: f64, bearing: f64, distance: f64) -> (f64, f64) {
    let (phi, lambda) = (lat.to_radians(), lon.to_radians());
    let phi2 = (phi.sin() * distance.cos() + phi.cos() * distance.sin() * bearing.cos()).asin();
    let lambda2 = lambda
        + (bearing.sin() * distance.sin() * phi.cos())
            .atan2(distance.cos() - phi.sin() * phi2.sin());
    (phi2.to_degrees(), lambda2.to_degrees())
}

/// KTLX 2013-05-20 20:16 (the Moore tornado): four TVS, the radar's own box
/// is `NM` (fine row 52-55, column 48-51), and the strongest mesocyclone
/// (rank 13) sits at fine box `MMH`.
#[test]
fn ktlx_2013_rcm_in_detail() {
    let product = decode_product(&common::entry("l3-tlx-rcm-20130520-2016").bytes()).unwrap();
    let rcm = product.radar_coded_message().unwrap().unwrap();
    assert_eq!(rcm.category, "ROBUU");
    let a = rcm.part_a.unwrap();
    assert_eq!(a.mode.as_deref(), Some("PCPN"));
    assert_eq!(a.scan_strategy.as_deref(), Some(""));
    assert_eq!(a.intensity_count, Some(356));
    let c = rcm.part_c.unwrap();
    assert_eq!(c.tvs.len(), 4);
    // M13MMH: box M (row 12), M (column 12), sub-box H = second column,
    // fourth row: fine row 12 * 4 + 3, column 12 * 4 + 1.
    assert_eq!(c.mesocyclones[0].0, 13);
    assert_eq!(
        c.mesocyclones[0].1,
        FineBox {
            row: 51,
            column: 49
        }
    );
    assert_eq!(c.mesocyclones[0].1.grid_box().row, 12);
    let b = rcm.part_b.unwrap();
    assert_eq!(b.winds[0].height_hft, 20);
    assert_eq!(b.winds[0].rms_kt(), Some(4));
}

/// Packet 30, the LFM grid adaptation parameters of DSI-7000 Figure 3-22
/// sheet 7 ([`IrmPacket::PARAMETER_NAMES`]: rotation, X and Y offset
/// distances to the upper right corner of the radar's `MM` box, 1/16 LFM
/// box size, spare), against the national grid of `hrap` at the three corpus
/// sites. The stored numbers are not in the figure's kilometres (0-45 km
/// offsets, 8.75-11.25 km box): the box size is twice the 1/16 LFM box at
/// the site, `2 * 11.90625 * (1 + sin(lat)) / (1 + sin 60 deg)` km (KTLX
/// 1994 to four decimals, the others to one), and the offsets are four
/// times the distances in km from the radar east and north to the edges of
/// its 1/4 LFM box, the X offset within 0.4 km and the Y offset within 2 km
/// here (thirteen sites of the NCEI archive, 1994-2000: box within 0.05, X
/// within 0.1 km at nine, Y within 4.1 km; `docs/level3/reference.md`
/// section 4.4). The spare is 0. The rotation is kept as stored: it is not
/// the grid convergence (longitude + 105 degrees).
#[test]
fn irm_lfm_grid_parameters() {
    assert_eq!(
        IrmPacket::PARAMETER_NAMES,
        [
            "angle_rotation",
            "x_offset_distance",
            "y_offset_distance",
            "sixteenth_lfm_grid_box_size",
            "spare"
        ]
    );
    for (id, box_tolerance, x_tolerance_km, y_tolerance_km) in [
        ("l3-tlx-irm-19940308-1115", 5e-5, 0.1, 2.0),
        ("l3-ilx-irm-19960419-2309", 0.05, 0.1, 0.1),
        ("l3-lot-irm-19941031-0503", 0.05, 0.4, 1.1),
    ] {
        let irm = decode_product(&common::entry(id).bytes()).unwrap();
        let layers = &irm.symbology.as_ref().unwrap().layers;
        let [Packet::Irm(IrmPacket::Parameters { values })] = layers[0].as_slice() else {
            panic!("{id} layer 0: {:?}", layers[0]);
        };
        let [rotation, x_offset, y_offset, box_size, spare] = values.map(f64::from);
        let (lat, lon) = (irm.description.latitude_deg, irm.description.longitude_deg);
        let scale = (1.0 + lat.to_radians().sin()) / (1.0 + 60f64.to_radians().sin());
        let sixteenth_km = 2.5 * hrap::MESH_M / 1000.0 * scale;
        assert!(
            (box_size - 2.0 * sixteenth_km).abs() < box_tolerance,
            "{id}: box {box_size} vs 2 x {sixteenth_km}"
        );
        // Distances from the radar to the east and north edges of its 1/4 LFM
        // box, whose edges lie at HRAP 401 + 10k, 1601 + 10k.
        let (hx, hy) = hrap::to_grid(lat, lon);
        let quarter_km = 4.0 * sixteenth_km;
        let to_edge = |h: f64, pole: f64| {
            let boxes = (h - pole) / hrap::QUARTER_LFM;
            (boxes.floor() + 1.0 - boxes) * quarter_km
        };
        let (east_km, north_km) = (to_edge(hx, hrap::POLE.0), to_edge(hy, hrap::POLE.1));
        assert!(
            (x_offset / 4.0 - east_km).abs() < x_tolerance_km,
            "{id}: X {x_offset} vs 4 x {east_km}"
        );
        assert!(
            (y_offset / 4.0 - north_km).abs() < y_tolerance_km,
            "{id}: Y {y_offset} vs 4 x {north_km}"
        );
        let convergence = lon + 105.0;
        assert!(
            (rotation - convergence).abs() > 5.0 && (rotation + convergence).abs() > 5.0,
            "{id}: rotation {rotation}, convergence {convergence}"
        );
        assert_eq!(spare, 0.0, "{id}");
        // Halfwords 49-50: the edit decision time and editing timeout.
        let seconds = |name: &str| {
            irm.description
                .parameters()
                .into_iter()
                .find(|p| p.name == name)
                .map(|p| p.value)
        };
        let expected = if id.contains("tlx") { 120 } else { 60 };
        for name in ["edit_decision_time", "editing_timeout"] {
            assert_eq!(
                seconds(name),
                Some(recast_radar_io_level3::ParameterValue::Int(expected)),
                "{id} {name}"
            );
        }
    }
}
