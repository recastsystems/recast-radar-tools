//! Radial (16, 0xAF1F) and generic (28) packets and data level mapping against
//! the real Level III corpus (`testdata/level3/manifest.toml`) and its golden
//! JSON (`testdata/level3/golden/<id>.json`, `tools/level3_golden.py`).
//!
//! For every file whose golden packet codes include 16, 0xAF1F, 28 or 29:
//!
//! - no packet with those codes is left as `Packet::Unknown`;
//! - each golden `data` grid (MetPy 1.7.1 levels, cut to the header's bin
//!   count) matches the decoded packet: header fields, dimensions, SHA-256 of
//!   the levels and the level histogram;
//! - where MetPy maps the product (`physical` not null), finite/masked counts
//!   and min/max/mean of [`DataLevels`] values match within 1e-4 relative. Where
//!   the ICD and MetPy disagree the comparison is adjusted and the ICD reading
//!   is checked against the file's own header instead (see
//!   [`metpy_equivalent`]);
//! - fields MetPy does not expose are checked against ICD semantics with values
//!   taken from the file itself: radial angles, generic product description
//!   against the Product Description Block, class labels of categorical
//!   products, product 138's maximum accumulation halfword.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use common::{Entry, Json};
use recast_radar_io_level3::levels::{DataLevels, Level, LevelEncoding, LevelFlag, Threshold};
use recast_radar_io_level3::packets::generic::{GenericComponent, GenericPacket};
use recast_radar_io_level3::packets::radial::{MAX_RADIAL_CELLS, RadialPacket};
use recast_radar_io_level3::{Level3Error, Level3Product, Packet, decode_product};

const FAMILY_CODES: [u16; 4] = [16, 0xAF1F, 28, 29];

/// Records a mismatch between a decoded value and its golden value.
macro_rules! check_eq {
    ($problems:expr, $what:expr, $decoded:expr, $golden:expr $(,)?) => {{
        let (decoded, golden) = (&$decoded, &$golden);
        if decoded != golden {
            $problems.push(format!(
                "{}: decoded {:?}, golden {:?}",
                $what, decoded, golden
            ));
        }
    }};
}

#[derive(Default)]
struct Counts {
    files: usize,
    radial_packets: usize,
    generic_packets: usize,
    grids: usize,
    physical_vs_metpy: usize,
    icd_checks: usize,
}

#[test]
fn radial_and_generic_packets_match_golden() {
    let mut counts = Counts::default();
    let mut failures = Vec::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let codes: Vec<u16> = golden
            .get("packet_codes")
            .items()
            .iter()
            .map(|c| u16::try_from(c.int("packet code")).unwrap())
            .collect();
        if !codes.iter().any(|c| FAMILY_CODES.contains(c)) {
            continue;
        }
        counts.files += 1;
        let problems = check_file(&entry, &golden, &mut counts);
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", entry.id, problems.join("\n    ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} files differ:\n{}",
        failures.len(),
        counts.files,
        failures.join("\n")
    );
    // The corpus has 136 radial and 5 generic product files (reference.md section 7).
    assert_eq!(counts.files, 141);
    assert_eq!(counts.radial_packets, 136);
    assert_eq!(counts.generic_packets, 5);
    assert_eq!(counts.grids, 138);
    eprintln!(
        "{} files: {} radial packets, {} generic packets, {} grids matched, \
         {} physical summaries matched MetPy, {} ICD checks",
        counts.files,
        counts.radial_packets,
        counts.generic_packets,
        counts.grids,
        counts.physical_vs_metpy,
        counts.icd_checks
    );
}

/// Every packet of the product (symbology layers and graphic pages).
fn all_packets(product: &Level3Product) -> Vec<&Packet> {
    let mut packets: Vec<&Packet> = Vec::new();
    if let Some(sym) = &product.symbology {
        packets.extend(sym.layers.iter().flatten());
    }
    if let Some(graphic) = &product.graphic {
        packets.extend(graphic.pages.iter().flat_map(|p| &p.packets));
    }
    packets
}

fn check_file(entry: &Entry, golden: &Json, counts: &mut Counts) -> Vec<String> {
    let mut problems = Vec::new();
    let product = match decode_product(&entry.bytes()) {
        Ok(product) => product,
        Err(e) => return vec![format!("decode failed: {e}")],
    };
    for packet in all_packets(&product) {
        match packet {
            Packet::Unknown { code, .. } if FAMILY_CODES.contains(code) => {
                problems.push(format!("packet {code} left as Unknown"));
            }
            Packet::Radial(radial) => {
                counts.radial_packets += 1;
                check_radial_angles(radial, &mut problems);
                counts.icd_checks += 1;
            }
            Packet::Generic(generic) => {
                counts.generic_packets += 1;
                check_generic_description(generic, &product, entry, &mut problems);
                counts.icd_checks += 1;
            }
            _ => {}
        }
    }

    let levels = DataLevels::from_description(&product.description);
    let data = golden.get("data").items();
    for (n, entry_data) in data.iter().enumerate() {
        let what = format!("data[{n}]");
        if let Some(error) = entry_data.get("error").as_str() {
            problems.push(format!("{what}: golden grid has an error: {error}"));
            continue;
        }
        let layer = entry_data.get("layer").int("layer") as usize;
        let index = entry_data.get("index").int("index") as usize;
        let Some(packet) = product
            .symbology
            .as_ref()
            .and_then(|s| s.layers.get(layer))
            .and_then(|l| l.get(index))
        else {
            problems.push(format!("{what}: no packet at layer {layer} index {index}"));
            continue;
        };
        let grid = match packet {
            Packet::Radial(radial) => {
                radial_grid(radial, entry_data.get("header"), &what, &mut problems)
            }
            Packet::Generic(generic) => generic_grid(generic, entry_data, &what, &mut problems),
            other => {
                problems.push(format!(
                    "{what}: packet {} is not radial or generic",
                    other.code()
                ));
                continue;
            }
        };
        let Some(grid) = grid else { continue };
        counts.grids += 1;
        check_grid(&grid, entry_data, &what, &mut problems);

        let physical = entry_data.get("physical");
        let Some(levels) = &levels else {
            problems.push(format!(
                "{what}: no data level mapping for product {}",
                product.description.product_code
            ));
            continue;
        };
        if physical.is_null() {
            // MetPy has no mapper for this product (197): ICD class table only.
            check_classes_without_metpy(&product, levels, &grid, &what, &mut problems);
            counts.icd_checks += 1;
        } else {
            check_physical(&product, levels, &grid, physical, &what, &mut problems);
            counts.physical_vs_metpy += 1;
        }
        counts.icd_checks += check_icd_levels(&product, levels, &grid, &what, &mut problems);
    }
    // Generic products whose components carry no grid (152) have an empty `data`.
    problems
}

/// A decoded grid of data levels in row-major order.
struct Grid {
    rows: usize,
    cols: usize,
    levels: Vec<u16>,
    /// SHA-256 input as the golden tool encodes it: `u8` for radial packets,
    /// big-endian `u16` for generic packets.
    bytes: Vec<u8>,
}

/// ICD Figure 3-10/3-11c: angles are tenths of a degree; every corpus sweep is
/// a full circle of 0.4-2.0 degree radials, each starting where the previous
/// one ends (start + delta, modulo 360 degrees).
fn check_radial_angles(radial: &RadialPacket, problems: &mut Vec<String>) {
    let mut total = 0i64;
    for (i, r) in radial.radials.iter().enumerate() {
        if !(0..3600).contains(&r.start_angle) || !(4..=20).contains(&r.delta_angle) {
            problems.push(format!(
                "packet {} radial {i}: start {} / delta {} tenths of a degree out of range",
                radial.code, r.start_angle, r.delta_angle
            ));
            return;
        }
        if let Some(next) = radial.radials.get(i + 1)
            && (r.start_angle + r.delta_angle) % 3600 != next.start_angle
        {
            problems.push(format!(
                "packet {} radial {i}: start {} + delta {} does not reach the next start {}",
                radial.code, r.start_angle, r.delta_angle, next.start_angle
            ));
            return;
        }
        total += i64::from(r.delta_angle);
    }
    if !(3590..=3630).contains(&total) {
        problems.push(format!(
            "packet {}: radial widths sum to {total} tenths of a degree",
            radial.code
        ));
    }
    if radial.levels.len() != radial.num_radials() * usize::from(radial.num_bins)
        || radial.rows().count() != radial.num_radials()
        || radial.row(radial.num_radials()).is_some()
    {
        problems.push(format!("packet {}: grid shape inconsistent", radial.code));
    }
}

/// Figure E-1 fields against the file's own Product Description Block and
/// AWIPS identifier (all generic corpus products are volume/on-demand products
/// named after the site).
fn check_generic_description(
    generic: &GenericPacket,
    product: &Level3Product,
    entry: &Entry,
    problems: &mut Vec<String>,
) {
    let p = &generic.product;
    let d = &product.description;
    check_eq!(
        problems,
        "generic product code",
        i64::from(p.product_code),
        i64::from(d.product_code)
    );
    check_eq!(
        problems,
        "generic generation time",
        i64::from(p.generation_time),
        d.generation_time.timestamp()
    );
    check_eq!(
        problems,
        "generic volume time",
        i64::from(p.volume_time),
        d.volume_scan_time.timestamp()
    );
    check_eq!(
        problems,
        "generic generation datetime",
        p.generation_datetime(),
        Some(d.generation_time)
    );
    check_eq!(
        problems,
        "generic volume number",
        i64::from(p.volume_number),
        i64::from(d.volume_scan_number)
    );
    check_eq!(problems, "generic VCP", i64::from(p.vcp), i64::from(d.vcp));
    if (f64::from(p.radar_latitude) - d.latitude_deg).abs() > 5e-4
        || (f64::from(p.radar_longitude) - d.longitude_deg).abs() > 5e-4
    {
        problems.push(format!(
            "generic radar location {} {} differs from the PDB {} {}",
            p.radar_latitude, p.radar_longitude, d.latitude_deg, d.longitude_deg
        ));
    }
    let site = entry.tag("awips").map(|awips| &awips[3..]).unwrap();
    if !p.radar_name.ends_with(site) {
        problems.push(format!(
            "generic radar name {:?} is not site {site}",
            p.radar_name
        ));
    }
    match d.product_code {
        // Archive III Status Product: status text lines (Table III "ASP").
        152 => {
            check_eq!(problems, "ASP name", p.name.as_str(), "ASP");
            check_eq!(
                problems,
                "ASP description",
                p.description.as_str(),
                "Archive III Status Product"
            );
            if generic.components.is_empty() {
                problems.push("ASP has no components".into());
            }
            for component in &generic.components {
                match component {
                    GenericComponent::Text { parameters, text }
                        if parameters.len() == 1
                            && parameters[0].id == "Msg Type"
                            && parameters[0].attributes.starts_with("Type=string; Value=")
                            && text.ends_with('\n')
                            && !text.contains('\0') => {}
                    other => {
                        problems.push(format!(
                            "ASP component is not a status text line: {other:?}"
                        ));
                        break;
                    }
                }
            }
        }
        176 => {
            check_eq!(
                problems,
                "DPR name",
                p.name.as_str(),
                "Digital Precipitation Rate (DPR)"
            );
            check_eq!(problems, "DPR components", generic.components.len(), 1);
        }
        code => problems.push(format!("unexpected generic product {code}")),
    }
}

fn radial_grid(
    radial: &RadialPacket,
    header: &Json,
    what: &str,
    problems: &mut Vec<String>,
) -> Option<Grid> {
    let fields = [
        ("first_bin", i64::from(radial.first_bin)),
        ("num_bins", i64::from(radial.num_bins)),
        ("i_center", i64::from(radial.i_center)),
        ("j_center", i64::from(radial.j_center)),
        ("scale_factor", i64::from(radial.scale_factor)),
        ("num_radials", radial.num_radials() as i64),
    ];
    for (name, decoded) in fields {
        check_eq!(
            problems,
            format!("{what} header.{name}"),
            decoded,
            header.get(name).int(name)
        );
    }
    Some(Grid {
        rows: radial.num_radials(),
        cols: usize::from(radial.num_bins),
        levels: radial.levels.iter().map(|&l| u16::from(l)).collect(),
        bytes: radial.levels.clone(),
    })
}

fn generic_grid(
    generic: &GenericPacket,
    data: &Json,
    what: &str,
    problems: &mut Vec<String>,
) -> Option<Grid> {
    let info = data.get("generic_component");
    let Some(component) = generic
        .radial_components()
        .find(|c| Some(c.description.as_str()) == info.get("description").as_str())
    else {
        problems.push(format!(
            "{what}: no radial component {:?}",
            info.get("description")
        ));
        return None;
    };
    check_eq!(
        problems,
        format!("{what} bin size"),
        Some(f64::from(component.bin_size)),
        info.get("gate_width").as_f64()
    );
    check_eq!(
        problems,
        format!("{what} range to first bin"),
        Some(f64::from(component.range_to_first_bin)),
        info.get("first_gate").as_f64()
    );
    let cols = component.radials.first().map_or(0, |r| r.num_bins);
    let mut levels = Vec::new();
    for (i, radial) in component.radials.iter().enumerate() {
        let bins = usize::try_from(radial.num_bins).unwrap_or(0);
        if radial.num_bins != cols || radial.values.len() < bins {
            problems.push(format!(
                "{what}: radial {i} has {} bins and {} values",
                radial.num_bins,
                radial.values.len()
            ));
            return None;
        }
        for &v in &radial.values[..bins] {
            let Ok(level) = u16::try_from(v) else {
                problems.push(format!(
                    "{what}: radial {i} value {v} is not a 16-bit level"
                ));
                return None;
            };
            levels.push(level);
        }
    }
    Some(Grid {
        rows: component.radials.len(),
        cols: usize::try_from(cols).unwrap_or(0),
        bytes: levels.iter().flat_map(|l| l.to_be_bytes()).collect(),
        levels,
    })
}

fn check_grid(grid: &Grid, data: &Json, what: &str, problems: &mut Vec<String>) {
    check_eq!(
        problems,
        format!("{what} rows"),
        grid.rows as i64,
        data.get("rows").int("rows")
    );
    check_eq!(
        problems,
        format!("{what} cols"),
        grid.cols as i64,
        data.get("cols").int("cols")
    );
    let digest = sha256_hex(&grid.bytes);
    check_eq!(
        problems,
        format!("{what} raw levels sha256"),
        Some(digest.as_str()),
        data.get("raw_sha256").as_str()
    );
    let decoded = histogram(&grid.levels);
    let golden: BTreeMap<u16, u64> = match data.get("histogram") {
        Json::Obj(members) => members
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.int("histogram count") as u64))
            .collect(),
        _ => BTreeMap::new(),
    };
    check_eq!(problems, format!("{what} level histogram"), decoded, golden);
}

fn histogram(levels: &[u16]) -> BTreeMap<u16, u64> {
    let mut h = BTreeMap::new();
    for &level in levels {
        *h.entry(level).or_insert(0) += 1;
    }
    h
}

/// The value MetPy 1.7.1 `map_data` gives a level, derived from the decoded level.
///
/// - Classes: MetPy maps hydrometeor classes (165, 177) to `level // 10` and
///   power removed control (113) through its threshold halfwords, which hold
///   the level itself.
/// - Product 138: the ICD makes level 0 "no accumulation" (0 in) and level `N`
///   `N * increment`; MetPy masks levels 0 and 1 and maps `N >= 2` to
///   `(N - 2) * increment`. The ICD reading is checked separately against
///   halfword 47 in [`check_icd_levels`].
/// - Product 34: MetPy maps the threshold halfwords (all zero in the corpus) to
///   0; the ICD defines classes. Checked in [`check_icd_levels`].
fn metpy_equivalent(product_code: i16, levels: &DataLevels, n: u16) -> Option<f64> {
    match (product_code, levels.level(n)) {
        (138, _) if n < 2 => None,
        (138, Level::Value(v)) => match levels.encoding() {
            LevelEncoding::Linear(l) => Some(v - 2.0 * l.increment),
            _ => None,
        },
        (165 | 177, Level::Class(class)) => Some(f64::from(class.code / 10)),
        (113, Level::Class(class)) => Some(f64::from(class.code)),
        (34, Level::Class(_)) => Some(0.0),
        (_, level) => level.value(),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-4 * a.abs().max(b.abs()) + 1e-12
}

fn check_physical(
    product: &Level3Product,
    levels: &DataLevels,
    grid: &Grid,
    golden: &Json,
    what: &str,
    problems: &mut Vec<String>,
) {
    let code = product.description.product_code;
    let (mut finite, mut masked, mut topped) = (0i64, 0i64, 0i64);
    let (mut min, mut max, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
    for (&level, &count) in &histogram(&grid.levels) {
        match metpy_equivalent(code, levels, level) {
            Some(v) => {
                finite += count as i64;
                min = min.min(v);
                max = max.max(v);
                sum += v * count as f64;
            }
            None => masked += count as i64,
        }
        if matches!(levels.level(level), Level::Topped(_)) {
            topped += count as i64;
        }
    }
    check_eq!(
        problems,
        format!("{what} finite"),
        finite,
        golden.get("finite").int("finite")
    );
    check_eq!(
        problems,
        format!("{what} masked"),
        masked,
        golden.get("masked").int("masked")
    );
    if !golden.get("topped").is_null() {
        check_eq!(
            problems,
            format!("{what} topped"),
            topped,
            golden.get("topped").int("topped")
        );
    }
    if finite > 0 {
        let mean = sum / finite as f64;
        for (name, decoded) in [("min", min), ("max", max), ("mean", mean)] {
            let expected = golden.get(name).as_f64().unwrap();
            if !close(decoded, expected) {
                problems.push(format!(
                    "{what} physical {name}: decoded {decoded}, MetPy {expected}"
                ));
            }
        }
    } else if !golden.get("min").is_null() {
        problems.push(format!(
            "{what}: no finite values, MetPy min {:?}",
            golden.get("min")
        ));
    }
}

/// Explicit ICD checks with values from the file's own header. Returns the
/// number of checks run.
fn check_icd_levels(
    product: &Level3Product,
    levels: &DataLevels,
    grid: &Grid,
    what: &str,
    problems: &mut Vec<String>,
) -> usize {
    let d = &product.description;
    let hw = |n: usize| d.halfword(n).unwrap();
    let present = histogram(&grid.levels);
    // No level in any corpus grid is undefined by its product's encoding.
    for &level in present.keys() {
        if levels.level(level) == Level::Undefined {
            problems.push(format!(
                "{what}: level {level} is undefined for product {}",
                d.product_code
            ));
        }
    }
    match d.product_code {
        // Figure 3-6 sheet 6 Note 1: level 0 is no accumulation, level N is
        // (hw31 + N * hw32) / 100 inches; halfword 47 is the maximum
        // accumulation in 0.01 in, so the highest level present must lie within
        // one increment of it (levels are quantized).
        138 => {
            let increment = f64::from(hw(32)) / 100.0;
            check_eq!(
                problems,
                format!("{what} DSP level 0"),
                levels.value(0),
                Some(f64::from(hw(31)) / 100.0)
            );
            check_eq!(
                problems,
                format!("{what} DSP level 1"),
                levels.value(1),
                Some(f64::from(hw(31) + hw(32)) / 100.0)
            );
            check_eq!(
                problems,
                format!("{what} DSP units"),
                levels.units(),
                Some("in")
            );
            let highest = *present.keys().next_back().unwrap();
            let max_accumulation = f64::from(hw(47)) / 100.0;
            let value = levels.value(highest).unwrap();
            if (value - max_accumulation).abs() > increment + 1e-9 {
                problems.push(format!(
                    "{what}: highest level {highest} is {value} in, halfword 47 says {max_accumulation} in"
                ));
            }
            1
        }
        // Note 1: coefficients are 16-bit floats; levels below halfword 33 use
        // the linear relation, levels from it the log relation; 254 is the cap
        // for VIL above 80 kg m-2.
        134 => {
            let float16 = |raw: u16| {
                let (sign, exponent, fraction) = (
                    raw >> 15,
                    i32::from((raw >> 10) & 0x1F),
                    f64::from(raw & 0x3FF),
                );
                let magnitude = if exponent == 0 {
                    2.0 * fraction / 1024.0
                } else {
                    2f64.powi(exponent - 16) * (1.0 + fraction / 1024.0)
                };
                if sign == 1 { -magnitude } else { magnitude }
            };
            let log_start = hw(33);
            let linear = |n: u16| (f64::from(n) - float16(hw(32))) / float16(hw(31));
            let log = |n: u16| ((f64::from(n) - float16(hw(35))) / float16(hw(34))).exp();
            for (n, expected) in [
                (2, linear(2)),
                (log_start - 1, linear(log_start - 1)),
                (log_start, log(log_start)),
                (254, log(254)),
            ] {
                let decoded = levels.value(n).unwrap();
                if !close(decoded, expected) {
                    problems.push(format!(
                        "{what}: VIL level {n} is {decoded}, ICD relation gives {expected}"
                    ));
                }
            }
            if !(79.0..=80.0).contains(&levels.value(254).unwrap()) {
                problems.push(format!(
                    "{what}: VIL level 254 is {:?}, not the 80 kg m-2 cap",
                    levels.value(254)
                ));
            }
            check_eq!(
                problems,
                format!("{what} VIL flags"),
                [levels.level(0), levels.level(1), levels.level(255)],
                [
                    Level::Flag(LevelFlag::BelowThreshold),
                    Level::Flag(LevelFlag::Flagged),
                    Level::Flag(LevelFlag::Reserved)
                ]
            );
            1
        }
        // 2620003AE 34.2.2: version 1 levels 0, 1, 4, 7. The corpus files use
        // bypass map levels 1 and 4 only.
        34 => {
            for (&level, label) in present.keys().zip(["No clutter", "Clutter"]) {
                match levels.level(level) {
                    Level::Class(class)
                        if class.label == label && class.description == "Bypass map in control" => {
                    }
                    other => problems.push(format!(
                        "{what}: CFC level {level} decoded as {other:?}, expected {label}"
                    )),
                }
            }
            check_eq!(
                problems,
                format!("{what} CFC levels"),
                present.keys().copied().collect::<Vec<_>>(),
                vec![1, 4]
            );
            let thresholds: Vec<u16> = (31..=46).map(hw).collect();
            check_eq!(
                problems,
                format!("{what} CFC thresholds"),
                thresholds,
                vec![0; 16]
            );
            1
        }
        // Note 1 hydrometeor table.
        165 | 177 => {
            let expected = [
                (10, "BI"),
                (20, "GC"),
                (30, "IC"),
                (40, "DS"),
                (50, "WS"),
                (60, "RA"),
                (70, "HR"),
                (80, "BD"),
                (90, "GR"),
                (100, "HA"),
                (110, "LH"),
                (140, "UK"),
            ];
            for &level in present.keys().filter(|&&l| l != 0) {
                let label = expected
                    .iter()
                    .find(|(l, _)| *l == level)
                    .map(|(_, name)| *name);
                match levels.level(level) {
                    Level::Class(class) if Some(class.label) == label => {}
                    other => {
                        problems.push(format!("{what}: class level {level} decoded as {other:?}"))
                    }
                }
            }
            1
        }
        // Threshold-coded products: the thresholds decode as MetPy's LegacyMapper
        // labels them (checked for the halfword shapes present in the corpus).
        _ if matches!(levels.encoding(), LevelEncoding::Thresholds(_)) => {
            if let LevelEncoding::Thresholds(thresholds) = levels.encoding() {
                for (i, t) in thresholds.iter().enumerate() {
                    let expected = expected_threshold_label(t.raw);
                    if t.label() != expected {
                        problems.push(format!(
                            "{what}: threshold {} ({:#06x}) label {:?}, expected {expected:?}",
                            i + 1,
                            t.raw,
                            t.label()
                        ));
                    }
                }
            }
            1
        }
        _ => 0,
    }
}

/// Threshold label per Figure 3-6 sheet 6 Note 1, written out independently of
/// the decoder for the halfwords found in the corpus.
fn expected_threshold_label(raw: u16) -> String {
    const CODES: [&str; 4] = ["BLANK", "TH", "ND", "RF"];
    let [high, low] = raw.to_be_bytes();
    let mut label = String::new();
    if high & 0x04 != 0 {
        label.push('<');
    } else if high & 0x08 != 0 {
        label.push('>');
    }
    if high & 0x80 != 0 {
        label.push_str(CODES[usize::from(low)]);
        return label;
    }
    if high & 0x01 != 0 {
        label.push('-');
    } else if high & 0x02 != 0 {
        label.push('+');
    }
    match high & 0x70 {
        0x20 => label.push_str(&format!("{}.{:02}", low / 20, (u32::from(low) % 20) * 5)),
        0x10 => label.push_str(&format!("{}.{}", low / 10, low % 10)),
        0 => label.push_str(&low.to_string()),
        other => panic!("threshold scale bits {other:#x} do not occur in the corpus"),
    }
    label
}

/// Product 197 (no MetPy mapper): every level present is a class of the ICD
/// rain rate table (Note 1) with its displayed code.
fn check_classes_without_metpy(
    product: &Level3Product,
    levels: &DataLevels,
    grid: &Grid,
    what: &str,
    problems: &mut Vec<String>,
) {
    let expected: BTreeMap<u16, &str> = match product.description.product_code {
        197 => [
            (0, "NP"),
            (10, "UF"),
            (20, "CZ"),
            (30, "TZ"),
            (40, "SA"),
            (50, "KL"),
            (60, "KH"),
            (70, "Z1"),
            (80, "Z6"),
            (90, "Z8"),
            (100, "SI"),
        ]
        .into_iter()
        .collect(),
        code => {
            problems.push(format!(
                "{what}: MetPy has no physical values for product {code}"
            ));
            return;
        }
    };
    for (&level, &count) in &histogram(&grid.levels) {
        match levels.level(level) {
            Level::Class(class)
                if expected.get(&level) == Some(&class.label) && class.code == level => {}
            other => problems.push(format!(
                "{what}: level {level} ({count} bins) decoded as {other:?}"
            )),
        }
    }
}

/// File id, halfword number, expected value, code and label.
type ThresholdCase = (&'static str, usize, Option<f64>, Option<u8>, &'static str);

/// Real threshold halfwords from corpus files, decoded field by field
/// (Figure 3-6 sheet 6 Note 1).
#[test]
fn threshold_halfwords_decode_per_icd() {
    let cases: [ThresholdCase; 8] = [
        ("l3-tlx-n0r-20220908-131957", 31, None, Some(2), "ND"),
        ("l3-tlx-n0r-20220908-131957", 32, Some(-28.0), None, "-28"),
        ("l3-tlx-n0r-20220908-131957", 40, Some(4.0), None, "+4"),
        ("l3-tlx-n1p-20130520-2016", 31, None, Some(2), "ND"),
        ("l3-tlx-n1p-20130520-2016", 32, Some(0.0), None, ">0.00"),
        ("l3-tlx-n1p-20130520-2016", 34, Some(0.25), None, "0.25"),
        ("l3-tlx-ntp-20130520-2016", 33, Some(0.3), None, "0.3"),
        ("l3-tlx-n0v-20130520-2016", 46, None, Some(3), "RF"),
    ];
    let manifest = common::level3_manifest();
    for (id, n, value, code, label) in cases {
        let entry = manifest.iter().find(|e| e.id == id).unwrap();
        let product = decode_product(&entry.bytes()).unwrap();
        let t = Threshold {
            raw: product.description.halfword(n).unwrap(),
        };
        assert_eq!(
            (t.value(), t.code(), t.label().as_str()),
            (value, code, label),
            "{id} halfword {n}"
        );
        let levels = DataLevels::from_description(&product.description).unwrap();
        assert_eq!(
            levels.value(u16::try_from(n - 31).unwrap()),
            value,
            "{id} level {}",
            n - 31
        );
    }
}

/// Real files with header fields corrupted: decoding returns an error or a
/// well-formed result, allocates nothing unbounded and never panics.
#[test]
fn corrupted_radial_and_generic_packets() {
    let manifest = common::level3_manifest();
    let find = |id: &str| manifest.iter().find(|e| e.id == id).unwrap();

    // KFWS 1995 N0R: uncompressed, 30-byte WMO/AWIPS heading, one 0xAF1F packet.
    let entry = find("l3-fws-n0r-19950517-2304");
    let bytes = entry.bytes();
    let original = decode_product(&bytes).unwrap();
    let Packet::Radial(radial) = &original.symbology.as_ref().unwrap().layers[0][0] else {
        panic!("expected a radial packet");
    };
    let packet = 30 + 2 * original.description.symbology_offset as usize + 16;
    assert_eq!(bytes[packet..packet + 2], [0xAF, 0x1F]);
    assert_eq!(
        u16::from_be_bytes([bytes[packet + 4], bytes[packet + 5]]),
        230
    );

    // More bins than the cell limit allows: rejected before allocation.
    let mut huge = bytes.clone();
    huge[packet + 4..packet + 6].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(367 * usize::from(u16::MAX) > MAX_RADIAL_CELLS);
    match decode_product(&huge) {
        Err(Level3Error::InvalidPacket { code: 0xAF1F, .. }) => {}
        other => panic!("expected InvalidPacket, got {:?}", other.map(|_| ())),
    }

    // Fewer bins: runs past the declared width are cut, rows keep their prefix.
    let mut narrow = bytes.clone();
    narrow[packet + 4..packet + 6].copy_from_slice(&100u16.to_be_bytes());
    let decoded = decode_product(&narrow).unwrap();
    let Packet::Radial(cut) = &decoded.symbology.as_ref().unwrap().layers[0][0] else {
        panic!("expected a radial packet");
    };
    assert_eq!(cut.levels.len(), 367 * 100);
    for (i, row) in cut.rows().enumerate() {
        assert_eq!(row, &radial.row(i).unwrap()[..100], "radial {i}");
    }

    // KTLX 2013 DPR and ASP: bzip2 products. Decompress, corrupt the XDR data
    // and hand the decoder the uncompressed message.
    let unpacked = |id: &str| {
        let entry = find(id);
        let golden = entry.golden();
        let bytes = entry.bytes();
        let framing = golden.get("framing");
        let trailer = if framing.get("trailer").is_null() {
            0
        } else {
            4
        };
        let start =
            bytes.len() - trailer - framing.get("message_bytes").int("message_bytes") as usize;
        let message = &bytes[start..bytes.len() - trailer];
        let mut out = bytes[..start + 120].to_vec();
        std::io::Read::read_to_end(&mut bzip2::read::BzDecoder::new(&message[120..]), &mut out)
            .unwrap();
        let product = decode_product(&out).unwrap();
        // XDR data starts after the symbology block/layer headers and the packet header.
        let xdr = start + 2 * product.description.symbology_offset as usize + 16 + 8;
        (out, xdr)
    };
    let word = |b: &[u8], at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
    let expect_invalid = |b: &[u8], what: &str| match decode_product(b) {
        Err(Level3Error::InvalidPacket { code: 28, reason }) => eprintln!("{what}: {reason}"),
        other => panic!(
            "{what}: expected InvalidPacket, got {:?}",
            other.map(|_| ())
        ),
    };

    let (dpr, dpr_xdr) = unpacked("l3-tlx-dpr-20130520-2016");
    let xdr = dpr_xdr;
    // Radial count and its array length (XDR words 54 and 55), then the first
    // radial's value count (word 70).
    assert_eq!(
        (
            word(&dpr, xdr + 216),
            word(&dpr, xdr + 220),
            word(&dpr, xdr + 280)
        ),
        (360, 360, 920)
    );
    let mut bad = dpr.clone();
    bad[xdr + 216..xdr + 224].copy_from_slice(&[0x10, 0, 0, 0, 0x10, 0, 0, 0]);
    expect_invalid(&bad, "radial count beyond the data");
    let mut bad = dpr.clone();
    bad[xdr + 220..xdr + 224].copy_from_slice(&359u32.to_be_bytes());
    expect_invalid(&bad, "radial array length differs from the count");
    let mut bad = dpr.clone();
    bad[xdr + 280..xdr + 284].copy_from_slice(&u32::MAX.to_be_bytes());
    expect_invalid(&bad, "bin value count beyond the data");
    let mut bad = dpr.clone();
    bad[xdr..xdr + 4].copy_from_slice(&0x7FFF_FFF0u32.to_be_bytes());
    expect_invalid(&bad, "product name longer than the data");

    let (asp, asp_xdr) = unpacked("l3-tlx-rsl-20130520-2358");
    let xdr = asp_xdr;
    // Component count and its array length (words 28 and 29), first present flag (30).
    assert_eq!(
        (
            word(&asp, xdr + 112),
            word(&asp, xdr + 116),
            word(&asp, xdr + 120)
        ),
        (1493, 1493, 1)
    );
    let mut bad = asp.clone();
    bad[xdr + 112..xdr + 120].copy_from_slice(&[0x7F, 0xFF, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF]);
    expect_invalid(&bad, "component count beyond the data");
    let mut bad = asp.clone();
    bad[xdr + 120..xdr + 124].copy_from_slice(&7u32.to_be_bytes());
    expect_invalid(&bad, "component present flag not 0 or 1");
    // An unknown component type keeps the rest of the data undecoded.
    let mut other = asp.clone();
    other[xdr + 124..xdr + 128].copy_from_slice(&3u32.to_be_bytes());
    let decoded = decode_product(&other).unwrap();
    let Packet::Generic(generic) = &decoded.symbology.as_ref().unwrap().layers[0][0] else {
        panic!("expected a generic packet");
    };
    match generic.components.as_slice() {
        [GenericComponent::Undecoded { kind: 3, bytes }] => {
            assert_eq!(bytes.len(), asp.len() - (xdr + 128))
        }
        other => panic!(
            "expected one undecoded component, got {} components",
            other.len()
        ),
    }

    // Every cut of the uncompressed messages inside the packet fails cleanly.
    for (message, xdr) in [(&dpr, dpr_xdr), (&asp, asp_xdr)] {
        for cut in [xdr - 4, xdr + 3, xdr + 100, xdr + 300, message.len() - 1] {
            assert!(decode_product(&message[..cut]).is_err(), "cut at {cut}");
        }
    }
}

#[test]
fn sha256_known_answers() {
    // FIPS 180-4 examples (one and two blocks) and the empty message.
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// SHA-256 (FIPS 180-4), hex encoded. The crate has no dev-dependencies.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            w[i] = w[i - 16]
                .wrapping_add(
                    w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3),
                )
                .wrapping_add(w[i - 7])
                .wrapping_add(
                    w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10),
                );
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let t1 = hh
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
