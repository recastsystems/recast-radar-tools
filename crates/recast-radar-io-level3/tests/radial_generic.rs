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
//! - where MetPy maps the product (`physical` not null), the public physical
//!   value API ([`DataLevels::for_packet`], [`RadialPacket::values`],
//!   [`GenericRadialComponent::values`]) is compared with MetPy's `map_data`
//!   summary: finite/masked counts equal, min/max/mean within 1e-4 relative
//!   ([`MetpyRelation`] lists the products in each case). Where MetPy and the
//!   ICD disagree the decoder follows the ICD and the documented difference is
//!   asserted instead: categorical products and product 138
//!   ([`product_138_follows_the_icd_and_differs_from_metpy_as_documented`]);
//! - fields MetPy does not expose are checked against ICD semantics with values
//!   taken from the file itself: radial angles, generic product description
//!   against the Product Description Block, class labels of categorical
//!   products.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{Entry, Json, PhysicalSummary};
use recast_radar_io_level3::levels::{DataLevels, Level, LevelEncoding, LevelFlag, Threshold};
use recast_radar_io_level3::packets::generic::{
    GenericComponent, GenericPacket, GenericRadialComponent,
};
use recast_radar_io_level3::packets::radial::{MAX_RADIAL_CELLS, RadialPacket};
use recast_radar_io_level3::{Level3Error, Level3Product, Packet, decode_product};

const FAMILY_CODES: [u16; 4] = [16, 0xAF1F, 28, 29];

/// How decoded physical values relate to MetPy 1.7.1 `map_data` for a product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MetpyRelation {
    /// `values` summarize exactly as MetPy's physical values.
    Equal,
    /// Categorical: the decoder gives classes (NaN values); MetPy gives class
    /// numbers, reproduced from the decoded classes by [`metpy_class_number`].
    Classes,
    /// Product 138: MetPy masks levels 0 and 1 and shifts the rest two
    /// increments down ([`dsp_as_metpy`]).
    Dsp,
}

impl MetpyRelation {
    fn of(product_code: i16) -> Self {
        match product_code {
            34 | 113 | 165 | 177 => Self::Classes,
            138 => Self::Dsp,
            _ => Self::Equal,
        }
    }
}

/// Radial and generic products whose physical values MetPy maps, per relation.
const METPY_EQUAL: [i16; 34] = [
    19, 20, 25, 27, 28, 30, 32, 56, 78, 79, 80, 94, 99, 134, 135, 153, 154, 155, 159, 161, 163,
    167, 169, 170, 171, 172, 173, 174, 175, 176, 180, 181, 182, 186,
];
const METPY_CLASSES: [i16; 4] = [34, 113, 165, 177];
const METPY_DSP: [i16; 1] = [138];

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
    /// Product codes compared with MetPy, per relation.
    metpy_products: BTreeMap<MetpyRelation, BTreeSet<i16>>,
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
    // Every grid but product 197's (no MetPy mapper) is compared with MetPy.
    assert_eq!(counts.physical_vs_metpy, 137);
    let relation = |r| counts.metpy_products.get(&r).cloned().unwrap_or_default();
    assert_eq!(relation(MetpyRelation::Equal), BTreeSet::from(METPY_EQUAL));
    assert_eq!(
        relation(MetpyRelation::Classes),
        BTreeSet::from(METPY_CLASSES)
    );
    assert_eq!(relation(MetpyRelation::Dsp), BTreeSet::from(METPY_DSP));
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
        let Some(levels) = DataLevels::for_packet(&product.description, packet.code()) else {
            problems.push(format!(
                "{what}: no data level mapping for packet {} of product {}",
                packet.code(),
                product.description.product_code
            ));
            continue;
        };
        let grid = match packet {
            Packet::Radial(radial) => radial_grid(
                radial,
                &levels,
                entry_data.get("header"),
                &what,
                &mut problems,
            ),
            Packet::Generic(generic) => {
                generic_grid(generic, &levels, entry_data, &what, &mut problems)
            }
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
        check_values_match_levels(&grid, &levels, &what, &mut problems);

        let physical = entry_data.get("physical");
        if physical.is_null() {
            // MetPy has no mapper for this product (197): ICD class table only.
            check_classes_without_metpy(&product, &levels, &grid, &what, &mut problems);
            counts.icd_checks += 1;
        } else {
            let relation = check_physical(&product, &levels, &grid, physical, &what, &mut problems);
            counts.physical_vs_metpy += 1;
            counts
                .metpy_products
                .entry(relation)
                .or_default()
                .insert(product.description.product_code);
        }
        counts.icd_checks += check_icd_levels(&product, &levels, &grid, &what, &mut problems);
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
    /// Physical values from the public API (`RadialPacket::values`,
    /// `GenericRadialComponent::values`).
    values: Vec<f32>,
    /// `Level` of each cell from the public API (`level_at`), sampled every
    /// [`LEVEL_AT_STRIDE`] cells: (cell index, level).
    level_at: Vec<(usize, Option<Level>)>,
}

/// Cells between two `level_at` samples (a prime, so every bin column is hit).
const LEVEL_AT_STRIDE: usize = 97;

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
    levels: &DataLevels,
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
    let bins = usize::from(radial.num_bins).max(1);
    let mut level_at: Vec<_> = (0..radial.levels.len() + 2 * bins)
        .step_by(LEVEL_AT_STRIDE)
        .map(|i| (i, radial.level_at(i / bins, i % bins, levels)))
        .collect();
    // Past the last bin of a radial: no level (cell index outside every grid).
    level_at.push((
        usize::MAX,
        radial.level_at(0, usize::from(radial.num_bins), levels),
    ));
    Some(Grid {
        rows: radial.num_radials(),
        cols: usize::from(radial.num_bins),
        levels: radial.levels.iter().map(|&l| u16::from(l)).collect(),
        bytes: radial.levels.clone(),
        values: radial.values(levels),
        level_at,
    })
}

fn generic_grid(
    generic: &GenericPacket,
    levels: &DataLevels,
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
    let mut grid_levels = Vec::new();
    for (i, radial) in component.radials.iter().enumerate() {
        let bins = usize::try_from(radial.num_bins).unwrap_or(0);
        if radial.num_bins != cols || radial.values.len() < bins || radial.bins().len() != bins {
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
            grid_levels.push(level);
        }
    }
    let cols = usize::try_from(cols).unwrap_or(0);
    check_generic_value_accessors(component, levels, cols, what, problems);
    let step = cols.max(1);
    let mut level_at: Vec<_> = (0..grid_levels.len() + 2 * step)
        .step_by(LEVEL_AT_STRIDE)
        .map(|i| {
            let level = component
                .radials
                .get(i / step)
                .and_then(|radial| radial.level_at(i % step, levels));
            (i, level)
        })
        .collect();
    // Past the last bin of a radial: no level (cell index outside every grid).
    level_at.push((
        usize::MAX,
        component
            .radials
            .first()
            .and_then(|radial| radial.level_at(cols, levels)),
    ));
    Some(Grid {
        rows: component.radials.len(),
        cols,
        bytes: grid_levels.iter().flat_map(|l| l.to_be_bytes()).collect(),
        levels: grid_levels,
        values: component.values(levels),
        level_at,
    })
}

/// `GenericRadialComponent::num_bins`/`values` against the per-radial
/// `GenericRadial::values`: same column count, rows equal to the radial values.
fn check_generic_value_accessors(
    component: &GenericRadialComponent,
    levels: &DataLevels,
    cols: usize,
    what: &str,
    problems: &mut Vec<String>,
) {
    check_eq!(
        problems,
        format!("{what} component num_bins"),
        component.num_bins(),
        cols
    );
    let joined: Vec<u32> = component
        .radials
        .iter()
        .flat_map(|radial| radial.values(levels))
        .map(f32::to_bits)
        .collect();
    let whole: Vec<u32> = component
        .values(levels)
        .into_iter()
        .map(f32::to_bits)
        .collect();
    if joined != whole {
        problems.push(format!(
            "{what}: component values differ from its radials' values"
        ));
    }
}

/// `values` and the sampled `level_at` of a grid agree with `DataLevels::level`
/// of its levels, and `level_at` is `None` past the grid.
fn check_values_match_levels(
    grid: &Grid,
    levels: &DataLevels,
    what: &str,
    problems: &mut Vec<String>,
) {
    if grid.values.len() != grid.levels.len() {
        problems.push(format!(
            "{what}: {} values for {} levels",
            grid.values.len(),
            grid.levels.len()
        ));
        return;
    }
    // One cell per distinct level: the value is the level's value as f32, or NaN.
    let mut seen = BTreeSet::new();
    for (&value, &level) in grid.values.iter().zip(&grid.levels) {
        if seen.insert(level) {
            let expected = levels.value(level).map_or(f32::NAN, |v| v as f32);
            if value.to_bits() != expected.to_bits() {
                problems.push(format!(
                    "{what}: level {level} value {value}, DataLevels gives {:?}",
                    levels.level(level)
                ));
            }
        }
    }
    for &(i, level) in &grid.level_at {
        let expected = grid.levels.get(i).map(|&n| levels.level(n));
        if level != expected {
            problems.push(format!(
                "{what}: level_at cell {i} is {level:?}, expected {expected:?}"
            ));
            break;
        }
    }
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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-4 * a.abs().max(b.abs()) + 1e-12
}

/// The number MetPy 1.7.1 `map_data` gives a class of a categorical product:
/// the class index `level // 10` for hydrometeor classes (165, 177), the level
/// itself for power removed control (113, through threshold halfwords that
/// hold the level), 0 for clutter filter control (34, whose threshold
/// halfwords are all zero). Flags have no number (NaN).
fn metpy_class_number(product_code: i16, level: Level) -> f64 {
    match (product_code, level) {
        (165 | 177, Level::Class(class)) => f64::from(class.code / 10),
        (113, Level::Class(class)) => f64::from(class.code),
        (34, Level::Class(_)) => 0.0,
        _ => f64::NAN,
    }
}

/// Product 138 values as MetPy 1.7.1 maps them, from the decoded (ICD) values:
/// levels 0 and 1 masked, level `N >= 2` two increments below the ICD value.
fn dsp_as_metpy(levels: &DataLevels, grid_levels: &[u16], values: &[f32]) -> PhysicalSummary {
    let LevelEncoding::Linear(linear) = levels.encoding() else {
        panic!("product 138 is not linear: {:?}", levels.encoding());
    };
    // In f32, like the values, so level 2 maps to exactly 0 as in MetPy.
    let shift = (2.0 * linear.increment) as f32;
    PhysicalSummary::of(grid_levels.iter().zip(values).map(|(&n, &v)| {
        if n < 2 {
            f64::NAN
        } else {
            f64::from(v - shift)
        }
    }))
}

/// Compares the public API's physical values with MetPy's `map_data` summary
/// and returns how they relate.
fn check_physical(
    product: &Level3Product,
    levels: &DataLevels,
    grid: &Grid,
    golden: &Json,
    what: &str,
    problems: &mut Vec<String>,
) -> MetpyRelation {
    let code = product.description.product_code;
    let relation = MetpyRelation::of(code);
    let decoded = match relation {
        MetpyRelation::Equal => PhysicalSummary::of_f32(&grid.values),
        MetpyRelation::Classes => {
            // Classes carry no physical value.
            if let Some(v) = grid.values.iter().find(|v| !v.is_nan()) {
                problems.push(format!("{what}: categorical product has value {v}"));
            }
            PhysicalSummary::of(
                levels
                    .levels(&grid.levels)
                    .map(|level| metpy_class_number(code, level)),
            )
        }
        MetpyRelation::Dsp => dsp_as_metpy(levels, &grid.levels, &grid.values),
    };
    for mismatch in decoded.mismatches(&PhysicalSummary::from_golden(golden)) {
        problems.push(format!("{what} physical ({relation:?}) {mismatch}"));
    }
    if !golden.get("topped").is_null() {
        let topped = levels
            .levels(&grid.levels)
            .filter(|level| matches!(level, Level::Topped(_)))
            .count();
        check_eq!(
            problems,
            format!("{what} topped"),
            topped as i64,
            golden.get("topped").int("topped")
        );
    }
    relation
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
        // Product 138 against the ICD and halfword 47:
        // `product_138_follows_the_icd_and_differs_from_metpy_as_documented`.
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

/// Product 138 (Digital Storm Total Precipitation), documented in
/// `src/levels.rs` ("Differences from MetPy") and `docs/level3/coverage.md`.
///
/// ICD 2620001AD Figure 3-6 sheet 6 Note 1: "data level code 0 corresponds to
/// no accumulation and data level codes 1 through 255 denote accumulation
/// values in units of hundredths-of-inches, in even data increments, with data
/// level code 1 being the first non-zero accumulation value"; halfword 31 is
/// the minimum (0), halfword 32 the increment in 0.01 in. The decoder follows
/// it. MetPy 1.7.1 (`DigitalStormPrecipMapper`, a `DigitalMapper` with
/// `_min_data = 2`) masks levels 0 and 1 and maps level `N >= 2` to
/// `(N - 2) * increment`.
///
/// Per corpus file, with halfwords and level counts read from the file: the
/// ICD values; MetPy's golden summary equal to exactly that documented shift of
/// the decoded values; and halfword 47 (maximum accumulation, 0.01 in) within
/// one increment of the ICD maximum but more than one increment away from
/// MetPy's (or, with no accumulation at all, 0 in where MetPy has no value).
#[test]
fn product_138_follows_the_icd_and_differs_from_metpy_as_documented() {
    // (id, halfword 32, bins at level 0, bins at level 1, highest level, halfword 47)
    const FILES: [(&str, u16, u64, u64, u16, u16); 3] = [
        ("l3-mci-dsp-20160526-2154", 2, 2395, 3304, 219, 438),
        ("l3-tlx-dsp-20130520-2016", 2, 33265, 2494, 145, 289),
        ("l3-tlx-dsp-20260629-173638", 1, 41760, 0, 0, 0),
    ];
    let manifest = common::level3_manifest();
    for (id, hw32, level0, level1, highest, hw47) in FILES {
        let entry = manifest.iter().find(|e| e.id == id).unwrap();
        let golden = entry.golden();
        let product = decode_product(&entry.bytes()).unwrap();
        let d = &product.description;
        assert_eq!(d.product_code, 138, "{id}");
        assert_eq!(
            [31, 32, 33, 47].map(|n| d.halfword(n).unwrap()),
            [0, hw32, 256, hw47],
            "{id}: halfwords 31, 32, 33, 47"
        );
        let Packet::Radial(radial) = &product.symbology.as_ref().unwrap().layers[0][0] else {
            panic!("{id}: layer 0 is not a radial packet");
        };
        let present = histogram(
            &radial
                .levels
                .iter()
                .map(|&l| u16::from(l))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            (
                present.get(&0).copied().unwrap_or(0),
                present.get(&1).copied().unwrap_or(0),
                *present.keys().next_back().unwrap()
            ),
            (level0, level1, highest),
            "{id}: level counts"
        );
        let bins = radial.levels.len() as u64;

        // ICD: level 0 is 0 in, level N is N increments.
        let levels = DataLevels::for_packet(d, radial.code).unwrap();
        let increment = f64::from(hw32) / 100.0;
        assert_eq!(levels.units(), Some("in"), "{id}");
        assert_eq!(levels.level(0), Level::Value(0.0), "{id}");
        for n in [1u16, 2, 145, 255] {
            let value = levels.value(n).unwrap();
            assert!(
                close(value, f64::from(n) * increment),
                "{id}: level {n} is {value}"
            );
        }
        let values = radial.values(&levels);
        let icd = PhysicalSummary::of_f32(&values);
        assert_eq!(
            (icd.finite, icd.masked),
            (bins, 0),
            "{id}: every bin has a value"
        );
        assert_eq!(icd.min, Some(0.0), "{id}");
        let icd_max = icd.max.unwrap();
        assert!(
            close(icd_max, f64::from(highest) * increment),
            "{id}: {icd_max}"
        );
        let max_accumulation = f64::from(hw47) / 100.0;
        assert!(
            (icd_max - max_accumulation).abs() <= increment + 1e-6,
            "{id}: ICD maximum {icd_max} in, halfword 47 {max_accumulation} in"
        );

        // MetPy: the documented shift of the same values.
        let metpy = PhysicalSummary::from_golden(golden.get("data").items()[0].get("physical"));
        assert_eq!(
            (metpy.finite, metpy.masked),
            (bins - level0 - level1, level0 + level1),
            "{id}: MetPy masks levels 0 and 1"
        );
        let grid_levels: Vec<u16> = radial.levels.iter().map(|&l| u16::from(l)).collect();
        let mismatches = dsp_as_metpy(&levels, &grid_levels, &values).mismatches(&metpy);
        assert!(mismatches.is_empty(), "{id}: {mismatches:?}");
        match metpy.max {
            Some(metpy_max) => {
                assert!(close(metpy_max, f64::from(highest - 2) * increment), "{id}");
                assert!(
                    (metpy_max - max_accumulation).abs() > increment,
                    "{id}: MetPy maximum {metpy_max} in is within one increment of halfword 47"
                );
            }
            None => assert_eq!((highest, icd_max), (0, max_accumulation), "{id}"),
        }
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
