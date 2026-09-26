//! Radial, raster and generic products as FM301 volumes
//! (`recast_radar_io_level3::volume`), against the real Level III corpus
//! (`testdata/level3/manifest.toml`) and its golden JSON
//! (`testdata/level3/golden/<id>.json`, `tools/level3_golden.py`).
//!
//! For every file whose golden `data` holds data packets (radial 16 and
//! 0xAF1F, raster 0xBA07, digital precipitation 17, precipitation rate 18,
//! generic 28 radial components), in file order:
//!
//! - the volume has one sweep per data array, each with one field of the
//!   golden dimensions;
//! - every field keeps its levels packed and hashes to the golden raw array
//!   (MetPy 1.7.1's array, `u8` or big-endian `u16`);
//! - every gate resolves to what the crate's level mapping says
//!   ([`DataLevels::level`], which `tests/radial_generic.rs` and
//!   `tests/raster.rs` pin to MetPy): values within one `f32` rounding step,
//!   below threshold as `Undetect`, range folded as `RangeFolded`, every
//!   other level without a value as `Missing`; and the summary equals
//!   MetPy's `map_data` for the products MetPy maps exactly (the same set as
//!   `tests/radial_generic.rs`);
//! - the geometry follows the ICD: bin size per product, first centre half a
//!   bin out, elevation from halfword 30 for elevation products (MetPy's
//!   `el_angle`) and NaN otherwise, ray time from the halfword 50 delay,
//!   volume time, location and VCP from the Product Description Block, and
//!   the HRAP arrays on their polar stereographic grid.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use common::{Entry, Json, PhysicalSummary};
use recast_radar_core::fm301::{Values, ViewOptions, volume_view};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, FieldData, FieldName, Gate, LevelTable, LinearTransform, RangeCoord,
    Scalar, SourceFormat, Sweep, SweepMode, Volume,
};
use recast_radar_io_level3::levels::{DataLevels, Level, LevelFlag};
use recast_radar_io_level3::volume::{
    elevation_deg, elevation_delay_s, field_name, range_bin_size_m, raster_cell_size_m,
};
use recast_radar_io_level3::{
    DataArray, Level3Error, Level3Product, decode_product, read_level3_volume,
};

/// Products MetPy 1.7.1 maps to the same physical values (see
/// `tests/radial_generic.rs`).
const METPY_EQUAL: [i16; 34] = [
    19, 20, 25, 27, 28, 30, 32, 56, 78, 79, 80, 94, 99, 134, 135, 153, 154, 155, 159, 161, 163,
    167, 169, 170, 171, 172, 173, 174, 175, 176, 180, 181, 182, 186,
];

/// Expected range bin size (radial) or cell size (raster) in metres of every
/// product in the corpus, from ICD Table III; HRAP arrays in projection
/// metres (4762.5 m boxes, 10 of them for the rate arrays).
fn expected_spacing_m(product_code: i16, packet: u16, num_bins: u16) -> f64 {
    match packet {
        17 => return 4762.5,
        18 => return 47_625.0,
        0xBA07 => {
            return match product_code {
                // Cross sections 50 and 51: 0.54 nmi columns (1 km).
                35 | 37 | 50 | 51 => 1000.0,
                78 | 80 => 2000.0,
                36 | 38 | 41 | 57 | 63..=67 | 90 => 4000.0,
                // Combined shear (halfword 50 of the corpus product) and the
                // Weak Echo Region window: 0.54 nmi.
                53 | 87 => 0.54 * 1852.0,
                other => panic!("no expected cell size for raster product {other}"),
            };
        }
        _ => {}
    }
    match product_code {
        16 | 19 | 24 | 27 | 30 | 32 | 43 | 56 | 94 | 134 | 135 => 1000.0,
        17 | 20 | 78 | 79 | 80 | 138 | 169 | 171 => 2000.0,
        18 | 21 => 4000.0,
        26 | 29 | 46 | 55 => 500.0,
        22 | 25 | 28 | 44 | 45 | 99 | 113 | 153 | 154 | 155 | 159 | 161 | 163 | 165 | 167 | 170
        | 172 | 173 | 174 | 175 | 176 | 177 | 197 => 250.0,
        180..=182 => 150.0,
        186 => 300.0,
        // The unverified reading `range_bin_size_m` documents.
        34 => 230_000.0 / f64::from(num_bins),
        other => panic!("no expected bin size for radial product {other}"),
    }
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .with_timezone(&Utc)
}

/// The window of a Weak Echo Region raster: raster row `r` shifted left by
/// `rows - 1 - r` columns, window columns `d` from -1 to `rows` (`rows + 2`
/// of them), read here from the decoded packet (whose raster
/// `tests/raster.rs` compares with MetPy's). Panics unless every nonzero
/// raster cell lands in the window, so that a window that drops a cell
/// cannot pass.
fn deskewed(array: &DataArray<'_>) -> Vec<u8> {
    let DataArray::Raster { packet, .. } = *array else {
        panic!("not a raster");
    };
    let grid = &packet.grid;
    let n = grid.rows();
    let mut out = Vec::with_capacity(n * (n + 2));
    for row in 0..n {
        for d in -1..=n as i64 {
            let column = d + (n - 1 - row) as i64;
            out.push(
                usize::try_from(column)
                    .ok()
                    .and_then(|c| grid.get(row, c))
                    .unwrap_or(0),
            );
        }
    }
    let nonzero = |levels: &[u8]| levels.iter().filter(|&&l| l != 0).count();
    assert_eq!(
        nonzero(&out),
        nonzero(grid.levels()),
        "a nonzero raster cell outside the Weak Echo Region window"
    );
    out
}

/// Raw levels of a data array as the packet stores them (generic radials
/// padded with 0 like the golden array).
fn array_levels(array: &DataArray<'_>) -> Vec<u16> {
    match *array {
        DataArray::Radial { packet, .. } => packet.levels.iter().map(|&l| u16::from(l)).collect(),
        DataArray::Raster { packet, .. } => {
            packet.grid.levels().iter().map(|&l| u16::from(l)).collect()
        }
        DataArray::DigitalPrecip { packet, .. } => {
            packet.grid.levels().iter().map(|&l| u16::from(l)).collect()
        }
        DataArray::GenericRadial { component, .. } => {
            let columns = component.num_bins();
            let mut out = Vec::new();
            for radial in &component.radials {
                out.extend(radial.bins().iter().map(|&v| u16::try_from(v).unwrap()));
                out.resize(out.len() + columns - radial.bins().len(), 0);
            }
            out
        }
        _ => unreachable!(),
    }
}

#[derive(Default)]
struct Counts {
    files: usize,
    sweeps: usize,
    packed_hashes: usize,
    metpy_summaries: usize,
    per_packet: BTreeMap<u16, usize>,
}

#[test]
fn every_data_array_converts_to_a_sweep() {
    let mut counts = Counts::default();
    let mut failures = Vec::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let data = golden.get("data").items();
        if data.is_empty() {
            continue;
        }
        counts.files += 1;
        let problems = check_file(&entry, &golden, data, &mut counts);
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
    // Files MetPy reads: digital radial, run-length radial, raster (eight
    // in each Weak Echo Region), DPA (packet 17 and 40 rate arrays of packet
    // 18) and generic products (reference.md section 7). Product 82's rate
    // array is checked in `tests/raster.rs` (MetPy cannot read the file).
    assert_eq!(counts.files, 194);
    assert_eq!(counts.per_packet.get(&16), Some(&93));
    assert_eq!(counts.per_packet.get(&0xAF1F), Some(&58));
    assert_eq!(counts.per_packet.get(&0xBA07), Some(&51));
    assert_eq!(counts.per_packet.get(&17), Some(&4));
    assert_eq!(counts.per_packet.get(&18), Some(&40));
    assert_eq!(counts.per_packet.get(&28), Some(&2));
    assert_eq!(counts.sweeps, 248);
    assert_eq!(counts.packed_hashes, 248);
    eprintln!(
        "{} files, {} sweeps: {} packed fields hashed against MetPy, {} summaries matched MetPy",
        counts.files, counts.sweeps, counts.packed_hashes, counts.metpy_summaries
    );
}

fn check_file(entry: &Entry, golden: &Json, data: &[Json], counts: &mut Counts) -> Vec<String> {
    let mut problems = Vec::new();
    let bytes = entry.bytes();
    let product = decode_product(&bytes).unwrap();
    let desc = &product.description;
    let code = desc.product_code;
    let volume = match read_level3_volume(&bytes) {
        Ok(volume) => volume,
        Err(e) => return vec![format!("read_level3_volume failed: {e}")],
    };
    // `to_volume` on the decoded product gives the same volume (NaN angles
    // and values keep `PartialEq` from saying so; compare through `Debug`).
    let from_product = product.to_volume().unwrap();
    if format!("{from_product:?}") != format!("{volume:?}") {
        problems.push("to_volume differs from read_level3_volume".into());
    }
    let arrays = product.data_arrays();
    if volume.sweeps.len() != data.len() || arrays.len() != data.len() {
        return vec![format!(
            "{} sweeps and {} arrays for {} golden data packets",
            volume.sweeps.len(),
            arrays.len(),
            data.len()
        )];
    }
    let mut rays = 0;
    for (index, ((sweep, item), array)) in volume.sweeps.iter().zip(data).zip(&arrays).enumerate() {
        let packet = u16::try_from(item.get("packet").int("packet")).unwrap();
        *counts.per_packet.entry(packet).or_default() += 1;
        counts.sweeps += 1;
        rays += sweep.nrays();
        for problem in check_sweep(&product, golden, sweep, item, array, packet, counts) {
            problems.push(format!("sweep {index} (packet {packet}): {problem}"));
        }
    }

    // Time: volume scan start, plus the elevation delay for the products that carry it.
    let metadata = golden.get("metpy_detail").get("metadata");
    let vol_time = parse_time(metadata.get("vol_time").as_str().unwrap());
    if volume.time_reference != vol_time {
        problems.push(format!(
            "time_reference {} != {vol_time}",
            volume.time_reference
        ));
    }
    let delay = f64::from(elevation_delay_s(desc).unwrap_or(0));
    let coverage = volume.time_coverage.expect("time coverage");
    if coverage.start != vol_time + chrono::Duration::seconds(delay as i64) {
        problems.push(format!("time_coverage start {}", coverage.start));
    }

    // Location, VCP, product attributes.
    let prod_desc = golden.get("metpy_detail").get("prod_desc");
    let lat = prod_desc.get("lat").int("lat") as f64 * 0.001;
    let lon = prod_desc.get("lon").int("lon") as f64 * 0.001;
    let alt = prod_desc.get("height").int("height") as f64 * 0.3048;
    if volume.location.latitude_deg != Some(lat)
        || volume.location.longitude_deg != Some(lon)
        || volume
            .location
            .altitude_m
            .is_none_or(|a| (a - alt).abs() > 1e-9)
    {
        problems.push(format!(
            "location {:?} != ({lat}, {lon}, {alt})",
            volume.location
        ));
    }
    let vcp = u16::try_from(prod_desc.get("vcp").int("vcp")).unwrap();
    if volume.scan.vcp_pattern != Some(vcp)
        || volume.scan.name.as_deref() != Some(&format!("VCP-{vcp}"))
    {
        problems.push(format!("scan {:?} != VCP {vcp}", volume.scan));
    }
    if volume.volume_number != Some(i32::from(desc.volume_scan_number)) {
        problems.push("volume_number".into());
    }
    if volume.provenance.source_format != SourceFormat::NexradLevel3 {
        problems.push("source_format".into());
    }
    if volume.provenance.decode.decoded_ray_count != rays {
        problems.push("decoded_ray_count".into());
    }
    let expected_code = Some(AttrValue::Scalar(Scalar::I16(code)));
    if attr(&volume.attrs.other, "product_code") != expected_code {
        problems.push("product_code attribute".into());
    }
    // All 60 raw halfwords, as MetPy's golden `halfwords`.
    let halfwords: Vec<u16> = golden
        .get("halfwords")
        .items()
        .iter()
        .map(|h| u16::try_from(h.int("halfword")).unwrap())
        .collect();
    if attr(&volume.attrs.other, "level3_halfwords")
        != Some(AttrValue::Array(ArrayBuf::U16(halfwords)))
    {
        problems.push("level3_halfwords".into());
    }
    let generic = arrays
        .iter()
        .any(|a| matches!(a, DataArray::GenericRadial { .. }));
    if let Some(awips) = golden.get("framing").get("awips_id").as_str()
        && !generic
        && volume.attrs.instrument_name != awips[3..]
    {
        problems.push(format!(
            "instrument_name {} != {awips}",
            volume.attrs.instrument_name
        ));
    }
    let expected_compression = match (
        golden.get("framing").get("zlib_frames").int("zlib_frames") > 0,
        common::is_bzip2(golden),
    ) {
        (true, true) => "zlib+bzip2",
        (true, false) => "zlib",
        (false, true) => "bzip2",
        (false, false) => "uncompressed",
    };
    if volume.provenance.compression.as_deref() != Some(expected_compression) {
        problems.push(format!("compression {:?}", volume.provenance.compression));
    }
    problems
}

fn attr(other: &[(Box<str>, AttrValue)], name: &str) -> Option<AttrValue> {
    other
        .iter()
        .find(|(k, _)| &**k == name)
        .map(|(_, v)| v.clone())
}

fn check_sweep(
    product: &Level3Product,
    golden: &Json,
    sweep: &Sweep,
    item: &Json,
    array: &DataArray<'_>,
    packet: u16,
    counts: &mut Counts,
) -> Vec<String> {
    let mut problems = Vec::new();
    let desc = &product.description;
    let code = desc.product_code;
    if array.packet_code() != packet {
        return vec![format!("array packet {}", array.packet_code())];
    }
    let rows = usize::try_from(item.get("rows").int("rows")).unwrap();
    // A Weak Echo Region slice is a window of `rows + 2` columns taken from
    // its raster's shifted rows (`deskewed`).
    let cols = if code == 53 {
        rows + 2
    } else {
        usize::try_from(item.get("cols").int("cols")).unwrap()
    };
    if sweep.fields.len() != 1 {
        return vec![format!("{} fields", sweep.fields.len())];
    }
    let field = &sweep.fields[0];
    if sweep.nrays() != rows || field.shape() != (rows, cols) || sweep.range.ngates() != cols {
        problems.push(format!(
            "shape: {} rays, field {:?}, range {} gates; golden {rows} x {cols}",
            sweep.nrays(),
            field.shape(),
            sweep.range.ngates()
        ));
    }
    let expected_name = if packet == 18 {
        FieldName::Rr
    } else {
        field_name(code)
    };
    if field.name != expected_name {
        problems.push(format!("name {} != {expected_name}", field.name));
    }
    if attr(&field.attrs.other, "product_code") != Some(AttrValue::Scalar(Scalar::I16(code))) {
        problems.push("field product_code attribute".into());
    }

    // Packed levels hash to MetPy's raw array (for product 53, whose raster
    // `tests/raster.rs` compares with MetPy's, equal its window).
    let golden_hash = if code == 53 {
        common::sha256_hex(&deskewed(array))
    } else {
        item.get("raw_sha256").as_str().unwrap().to_owned()
    };
    let golden_hash = golden_hash.as_str();
    match &field.data {
        FieldData::U8 { values, .. } => {
            if common::sha256_hex(values) != golden_hash {
                problems.push("u8 levels differ from the golden raw array".into());
            }
            counts.packed_hashes += 1;
        }
        FieldData::U16 { values, .. } => {
            let be: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
            if common::sha256_hex(&be) != golden_hash {
                problems.push("u16 levels differ from the golden raw array".into());
            }
            counts.packed_hashes += 1;
        }
        other => problems.push(format!("unexpected storage {other:?}")),
    }

    // Every gate resolves as the level mapping says.
    let levels = if code == 53 {
        deskewed(array).into_iter().map(u16::from).collect()
    } else {
        array_levels(array)
    };
    let discrete = field.attrs.is_discrete == Some(true);
    let Some(mapping) = DataLevels::for_packet(desc, packet) else {
        problems.push("no level mapping".into());
        return problems;
    };
    let expected = mapping.values(&levels);
    let mut mismatches = 0usize;
    let mut first = None;
    for (index, (&level, &expected)) in levels.iter().zip(&expected).enumerate() {
        let (ray, gate) = (index / cols, index % cols);
        let gate_value = field.gate(ray, gate).unwrap();
        let ok = match (gate_value, mapping.level(level)) {
            (Gate::Value(v), Level::Class(class)) if discrete => {
                f64::from(v) == f64::from(class.code)
            }
            // `REAL*4` scale / offset products evaluate in f32, one rounding
            // step from the f64 level table.
            (Gate::Value(v), _) => {
                !discrete
                    && expected.is_finite()
                    && (v - expected).abs() <= 2.0 * f32::EPSILON * expected.abs().max(1.0)
            }
            (Gate::Undetect, level) => matches!(
                level,
                Level::Flag(LevelFlag::BelowThreshold | LevelFlag::NoAccumulation)
            ),
            (Gate::RangeFolded, level) => matches!(level, Level::Flag(LevelFlag::RangeFolded)),
            (Gate::Missing, level) => {
                expected.is_nan() && !matches!(level, Level::Flag(LevelFlag::RangeFolded))
            }
            (other, _) => panic!("gate class {other:?} is not handled here"),
        };
        if !ok {
            mismatches += 1;
            first.get_or_insert((ray, gate, level, gate_value, expected));
        }
    }
    if mismatches > 0 {
        problems.push(format!(
            "{mismatches} gates differ from DataLevels; first {first:?}"
        ));
    }
    // Discrete fields list every class as a flag value.
    if discrete {
        let classes: Vec<i64> = (0..256u16)
            .filter(|&n| matches!(mapping.level(n), Level::Class(_)))
            .map(i64::from)
            .collect();
        if field.attrs.flag_values[..classes.len()] != classes[..] {
            problems.push(format!(
                "flag_values {:?} do not start with classes {classes:?}",
                field.attrs.flag_values
            ));
        }
    }
    if field.attrs.flag_meanings.len() != field.attrs.flag_values.len() {
        problems.push("flag_meanings length".into());
    }

    // MetPy's physical summary (MetPy maps no packet 18).
    if METPY_EQUAL.contains(&code) && packet != 18 {
        let values: Vec<f32> = (0..rows)
            .flat_map(|ray| (0..cols).map(move |gate| (ray, gate)))
            .map(|(ray, gate)| field.value(ray, gate).unwrap_or(f32::NAN))
            .collect();
        let summary = PhysicalSummary::of_f32(&values);
        let golden_summary = PhysicalSummary::from_golden(item.get("physical"));
        for mismatch in summary.mismatches(&golden_summary) {
            problems.push(format!("physical vs MetPy: {mismatch}"));
        }
        counts.metpy_summaries += 1;
    }

    // Geometry.
    let num_bins = u16::try_from(cols).unwrap();
    let expected_spacing = expected_spacing_m(code, packet, num_bins);
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ..
    } = &sweep.range
    else {
        return vec!["range is not uniform".into()];
    };
    if (spacing_m - expected_spacing).abs() > 1e-9 {
        problems.push(format!("spacing {spacing_m} != {expected_spacing}"));
    }
    let metadata = golden.get("metpy_detail").get("metadata");
    let is_raster = matches!(packet, 0xBA07 | 17 | 18);
    let delay = f64::from(elevation_delay_s(desc).unwrap_or(0));
    let windowed = matches!(code, 43..=46 | 55);
    if code == 53 {
        // Weak Echo Region: a raster sweep of the window, its columns 0 to
        // rows - 1 (all but the first and last) centred at halfwords 27-28
        // (`weak_echo_region_window_matches_base_reflectivity`).
        let azimuth = (f64::from(desc.halfword(27).unwrap()) * 0.1).to_radians();
        let range = f64::from(desc.halfword(28).unwrap()) * 0.1 * 1852.0;
        let expected_first = range * azimuth.sin() + (0.5 - cols as f64 / 2.0) * expected_spacing;
        if attr(&sweep.other, "level3_window_first_column")
            != Some(AttrValue::Scalar(Scalar::I32(-1)))
        {
            problems.push("weak echo region window does not start at column -1".into());
        }
        if sweep.sweep_mode != SweepMode::Other("raster".into())
            || (first_center_m - expected_first).abs() > 1e-6
        {
            problems.push(format!(
                "weak echo region {:?} first column {first_center_m} != {expected_first}",
                sweep.sweep_mode
            ));
        }
        if sweep.fixed_angle_deg.is_nan() {
            problems.push("weak echo region slice without elevation".into());
        }
    } else if matches!(code, 50 | 51) {
        // Cross section: columns along the section from its first end point,
        // rows of 500 m from the top (checked against the product's own axes in
        // `cross_section_geometry_matches_its_axes`).
        if sweep.sweep_mode != SweepMode::Other("vertical_cross_section".into()) {
            problems.push(format!("cross section sweep_mode {:?}", sweep.sweep_mode));
        }
        if (first_center_m - 500.0).abs() > 1e-9 {
            problems.push(format!("cross section first column {first_center_m}"));
        }
        let y = sweep.extra_vars.iter().find(|v| &*v.name == "y").unwrap();
        let expected: Vec<f32> = (0..rows)
            .map(|row| ((rows - row) as f64 * 500.0 - 250.0) as f32)
            .collect();
        if y.values != ArrayBuf::F32(expected) {
            problems.push("cross section row heights".into());
        }
    } else if is_raster {
        if sweep.sweep_mode != SweepMode::Other("raster".into()) {
            problems.push(format!("raster sweep_mode {:?}", sweep.sweep_mode));
        }
        let y = sweep
            .extra_vars
            .iter()
            .find(|v| &*v.name == "y")
            .expect("raster y variable");
        if y.values.len() != rows || !y.is_per_ray() {
            problems.push("raster y variable shape".into());
        }
        if !sweep.rays.azimuth_deg.iter().all(|a| a.is_nan())
            || !sweep.rays.elevation_deg.iter().all(|e| e.is_nan())
        {
            problems.push("raster rays carry angles".into());
        }
        if packet == 0xBA07 {
            let expected_first = (0.5 - cols as f64 / 2.0) * expected_spacing;
            if (first_center_m - expected_first).abs() > 1e-6 {
                problems.push(format!(
                    "raster first column {first_center_m} != {expected_first}"
                ));
            }
            let cell = attr(&sweep.other, "raster_cell_m");
            if cell != Some(AttrValue::Scalar(Scalar::F64(expected_spacing))) {
                problems.push(format!("raster_cell_m {cell:?}"));
            }
        } else if !sweep.extra_vars.iter().any(|v| &*v.name == "latitude") {
            problems.push("HRAP array without latitude".into());
        }
    } else {
        if sweep.sweep_mode != SweepMode::AzimuthSurveillance {
            problems.push(format!("sweep_mode {:?}", sweep.sweep_mode));
        }
        // Bin 0 starts at the radar (the windowed products 46 and 55 start
        // at their first bin); generic components state the centre.
        let expected_first = match packet {
            28 => item
                .get("generic_component")
                .get("first_gate")
                .as_f64()
                .unwrap(),
            _ => (item.get("header").get("first_bin").as_f64().unwrap() + 0.5) * expected_spacing,
        };
        if (first_center_m - expected_first).abs() > 1e-6 {
            problems.push(format!("first centre {first_center_m} != {expected_first}"));
        }
        // The array does not reach past the product's maximum range (for the
        // windowed products 43-46 and 55 MetPy's range is the window's or a
        // default).
        let max_range_m = golden
            .get("metpy_detail")
            .get("max_range")
            .as_f64()
            .unwrap()
            * 1000.0;
        let extent = first_center_m + spacing_m * (cols as f64 - 0.5);
        // MetPy rounds 225 nm to 416 km; 1390 x 300 m is 417 km.
        if !windowed && extent > max_range_m * 1.005 + spacing_m {
            problems.push(format!(
                "extent {extent} m past MetPy max range {max_range_m}"
            ));
        }
        // Azimuth: the radial centre; the raw angles are kept.
        let (start, width) = match *array {
            DataArray::Radial { packet, .. } => (
                packet.radials[0].start_angle_deg(),
                packet.radials[0].delta_angle_deg(),
            ),
            DataArray::GenericRadial { component, .. } => {
                (component.radials[0].azimuth, component.radials[0].width)
            }
            _ => unreachable!(),
        };
        let expected_azimuth = (start + 0.5 * width).rem_euclid(360.0);
        if (sweep.rays.azimuth_deg[0] - expected_azimuth).abs() > 1e-4 {
            problems.push(format!(
                "azimuth {} != centre {expected_azimuth}",
                sweep.rays.azimuth_deg[0]
            ));
        }
        if packet != 28 {
            let starts = sweep
                .extra_vars
                .iter()
                .find(|v| &*v.name == "level3_start_angle")
                .map(|v| v.values.clone());
            if !matches!(&starts, Some(ArrayBuf::F32(s)) if s[0] == start && s.len() == rows) {
                problems.push("level3_start_angle".into());
            }
        }
        // Elevation: MetPy's el_angle for elevation products, else NaN.
        // MetPy reads 43-46 with default metadata: their Table V elevation is
        // halfword 30, compared directly.
        let metpy_elevation = if (43..=46).contains(&code) {
            Some(f64::from(desc.halfword(30).unwrap() as i16) * 0.1)
        } else {
            metadata.get("el_angle").as_f64()
        };
        match (elevation_deg(desc), metpy_elevation) {
            (Some(elevation), Some(metpy)) => {
                if (f64::from(elevation) - metpy).abs() > 1e-5
                    || (f64::from(sweep.fixed_angle_deg) - metpy).abs() > 1e-5
                    || sweep.rays.elevation_deg.iter().any(|e| *e != elevation)
                {
                    problems.push(format!(
                        "elevation {elevation} / fixed {} != MetPy {metpy}",
                        sweep.fixed_angle_deg
                    ));
                }
            }
            (Some(elevation), None) => {
                problems.push(format!("elevation {elevation} but MetPy has none"));
            }
            (None, _) if packet != 28 => {
                if !sweep.fixed_angle_deg.is_nan()
                    || !sweep.rays.elevation_deg.iter().all(|e| e.is_nan())
                {
                    problems.push(format!(
                        "volume product has elevation {}",
                        sweep.fixed_angle_deg
                    ));
                }
            }
            (None, _) => {}
        }
        // Elevation number.
        let el_num = u16::try_from(
            golden
                .get("metpy_detail")
                .get("prod_desc")
                .get("el_num")
                .int("el_num"),
        )
        .unwrap();
        if sweep.elevation_number != (el_num != 0).then_some(el_num) {
            problems.push(format!(
                "elevation_number {:?} != {el_num}",
                sweep.elevation_number
            ));
        }
        if sweep.rays.time_s.iter().any(|t| *t != delay) {
            problems.push(format!("ray times differ from delay {delay}"));
        }
    }
    problems
}

/// The field names of the corpus products: FM301 moment names for base
/// moments, REC / RR for classification and rate, mnemonics otherwise.
#[test]
fn field_names_follow_the_design_note() {
    let cases = [
        (19, FieldName::Dbzh),
        (94, FieldName::Dbzh),
        (153, FieldName::Dbzh),
        (181, FieldName::Dbzh),
        (186, FieldName::Dbzh),
        (32, FieldName::Dbzh),
        (27, FieldName::Vradh),
        (99, FieldName::Vradh),
        (182, FieldName::Vradh),
        (30, FieldName::Wradh),
        (155, FieldName::Wradh),
        (184, FieldName::Wradh),
        (159, FieldName::Zdr),
        (161, FieldName::Rhohv),
        (163, FieldName::Kdp),
        (165, FieldName::Rec),
        (177, FieldName::Rec),
        (176, FieldName::Rr),
        (37, FieldName::Other("CR".into())),
        (41, FieldName::Other("ET".into())),
        (57, FieldName::Other("VIL".into())),
        (134, FieldName::Other("DVL".into())),
        (135, FieldName::Other("EET".into())),
        (56, FieldName::Other("SRM".into())),
        (170, FieldName::Other("DAA".into())),
        (34, FieldName::Other("P34".into())),
        (113, FieldName::Other("PRC".into())),
        (197, FieldName::Other("RRC".into())),
    ];
    for (code, expected) in cases {
        assert_eq!(field_name(code), expected, "product {code}");
    }
}

/// Bin and cell sizes from the ICD tables, including the products without a
/// corpus sample (2620001H Table III for 43-46 and 55, 2620001P for 93, 156
/// and 157), and `None` where the ICD gives none. Product 34's sizes pin the
/// reading `range_bin_size_m` documents (230 km over the bin count); they
/// restate it and do not verify it, which no real evidence found does.
#[test]
fn geometry_tables() {
    assert_eq!(range_bin_size_m(94, 460), Some(1000.0));
    assert_eq!(range_bin_size_m(153, 1840), Some(250.0));
    assert_eq!(range_bin_size_m(180, 592), Some(150.0));
    assert_eq!(range_bin_size_m(184, 600), Some(150.0));
    assert_eq!(range_bin_size_m(186, 1390), Some(300.0));
    assert_eq!(range_bin_size_m(34, 230), Some(1000.0));
    assert_eq!(range_bin_size_m(34, 460), Some(500.0));
    assert_eq!(range_bin_size_m(34, 0), None);
    // Severe weather analysis windows: 0.54, 0.13, 0.13, 0.27 nmi.
    assert_eq!(range_bin_size_m(43, 50), Some(1000.0));
    assert_eq!(range_bin_size_m(44, 200), Some(250.0));
    assert_eq!(range_bin_size_m(45, 200), Some(250.0));
    assert_eq!(range_bin_size_m(46, 100), Some(500.0));
    assert_eq!(range_bin_size_m(55, 100), Some(500.0));
    assert_eq!(range_bin_size_m(93, 115), Some(1000.0));
    assert_eq!(range_bin_size_m(156, 115), Some(2000.0));
    assert_eq!(range_bin_size_m(157, 115), Some(2000.0));
    assert_eq!(range_bin_size_m(48, 0), None);
    assert_eq!(raster_cell_size_m(37), Some(1000.0));
    assert_eq!(raster_cell_size_m(38), Some(4000.0));
    assert_eq!(raster_cell_size_m(81), Some(4762.5));
    assert_eq!(raster_cell_size_m(189), None);
    assert_eq!(raster_cell_size_m(87), None);
}

/// Products without a data array are `NoDataArray`; the General Status
/// Message is not a product.
#[test]
fn products_without_a_data_array_are_rejected() {
    let vwp = common::entry("l3-tlx-nvw-20130520-2016");
    assert!(matches!(
        read_level3_volume(&vwp.bytes()),
        Err(Level3Error::NoDataArray { code: 48 })
    ));
    let sti = common::entry("l3-tlx-nst-20130520-2016");
    assert!(matches!(
        read_level3_volume(&sti.bytes()),
        Err(Level3Error::NoDataArray { code: 58 })
    ));
    let gsm = common::entry("l3-ddc-gsm-20200817-1000");
    assert!(matches!(
        read_level3_volume(&gsm.bytes()),
        Err(Level3Error::NotAProduct { code: 2 })
    ));
}

/// KTLX N0Q 2022-05-03: a 1 km reflectivity cut with a 16 s elevation delay
/// (halfword 50 = 512): the sweep's coordinates and coding in detail.
#[test]
fn ktlx_n0q_2022_sweep_in_detail() {
    let entry = common::entry("l3-tlx-n0q-20220503-005231");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    assert_eq!(volume.attrs.instrument_name, "TLX");
    assert_eq!(volume.attrs.source.as_deref(), Some("NEXRAD Level III"));
    assert_eq!(volume.scan.vcp_pattern, Some(212));
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 360);
    assert_eq!(sweep.fixed_angle_deg, 0.5);
    assert_eq!(sweep.elevation_number, Some(1));
    assert_eq!(sweep.rays.time_s[0], 16.0);
    assert_eq!(
        volume.ray_time(0, 0).unwrap(),
        volume.time_reference + chrono::Duration::seconds(16)
    );
    // 1 degree radials whose boundaries follow the rounded Level II
    // azimuths: 22 radials are 0.9 or 1.1 degrees wide, so the starts are
    // not all on whole degrees.
    assert_eq!(sweep.rays_angle_resolution_deg, Some(1.0));
    assert_eq!(sweep.rays_are_indexed, Some(false));
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 500.0,
            spacing_m: 1000.0,
            ngates: 460
        }
    );
    let field = &sweep.fields[0];
    assert_eq!(field.name, FieldName::Dbzh);
    assert_eq!(field.attrs.units.as_deref(), Some("dBZ"));
    assert_eq!(
        field.attrs.long_name.as_deref(),
        Some("Base Reflectivity Data Array")
    );
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("N0Q is u8");
    };
    // Level 0 is below threshold, level 1 missing (ND).
    assert_eq!(coding.fill_value, Some(1));
    assert_eq!(coding.undetect, Some(0));
    assert_eq!(coding.range_folded, None);
    assert_eq!(coding.valid_range, Some([2, 255]));
    // Level 2 is the minimum (hw31 / 10 dBZ), each level hw32 / 10 more.
    let desc = &decode_product(&entry.bytes()).unwrap().description;
    let minimum = f64::from(desc.halfword(31).unwrap() as i16) / 10.0;
    let increment = f64::from(desc.halfword(32).unwrap() as i16) / 10.0;
    assert_eq!(
        coding.resolve(2).value().map(f64::from),
        Some(minimum),
        "level 2 is the minimum"
    );
    assert!((f64::from(coding.resolve(3).value().unwrap()) - (minimum + increment)).abs() < 1e-6);
    assert_eq!(coding.resolve(1), Gate::Missing);
    assert_eq!(coding.resolve(0), Gate::Undetect);
    // Table V: MetPy's `max` (47) and elevation (30); every raw halfword.
    let other = &volume.attrs.other;
    assert_eq!(
        attr(other, "level3_max_reflectivity"),
        Some(AttrValue::Scalar(Scalar::I64(i64::from(
            desc.halfword(47).unwrap() as i16
        ))))
    );
    assert_eq!(
        attr(other, "level3_elevation_delta_time"),
        Some(AttrValue::Scalar(Scalar::I64(16)))
    );
    assert_eq!(
        attr(other, "level3_supplemental_scan"),
        Some(AttrValue::text("none"))
    );
    assert_eq!(
        attr(other, "level3_compression_method"),
        Some(AttrValue::Scalar(Scalar::I64(1)))
    );
}

/// KTLX N0U 2013: velocity with range folding (level 1) as the flag.
#[test]
fn ktlx_n0u_range_folded_flag() {
    let entry = common::entry("l3-tlx-n0u-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    assert_eq!(field.name, FieldName::Vradh);
    assert_eq!(field.attrs.units.as_deref(), Some("m s-1"));
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("N0U is u8");
    };
    assert_eq!(coding.range_folded, Some(1));
    assert_eq!(coding.undetect, Some(0));
    assert_eq!(volume.sweeps[0].range.ngates(), 1200);
}

/// KTLX N0H 2013: hydrometeor classes kept as discrete levels.
#[test]
fn ktlx_n0h_classes_are_discrete() {
    let entry = common::entry("l3-tlx-n0h-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    assert_eq!(field.name, FieldName::Rec);
    assert_eq!(field.attrs.is_discrete, Some(true));
    assert_eq!(field.attrs.units, None);
    assert_eq!(
        field.attrs.flag_values,
        vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 140]
    );
    assert_eq!(&*field.attrs.flag_meanings[0], "biological");
    assert_eq!(
        &*field.attrs.flag_meanings[1],
        "anomalous_propagation_ground_clutter"
    );
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("N0H is u8");
    };
    assert_eq!(coding.range_folded, Some(150));
    assert_eq!(coding.undetect, Some(0));
    assert_eq!(coding.resolve(60), Gate::Value(60.0));
}

/// KTLX N0R 2013: a 16-level product keeps its levels with a 16-entry level
/// table: 5 dBZ steps from level 1, level 0 ND as the fill value.
#[test]
fn ktlx_n0r_thresholds_keep_their_levels() {
    let entry = common::entry("l3-tlx-n0r-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    assert_eq!(field.name, FieldName::Dbzh);
    assert_eq!(field.attrs.units.as_deref(), Some("dBZ"));
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("N0R is u8");
    };
    let LinearTransform::Levels(LevelTable::Sixteen(table)) = coding.transform else {
        panic!("N0R has a 16-level table, not {:?}", coding.transform);
    };
    assert!(table[0].is_nan());
    assert_eq!(
        table[1..],
        [
            5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0
        ]
    );
    assert_eq!(coding.fill_value, Some(0));
    assert_eq!(coding.valid_range, Some([1, 15]));
    let labels = attr(&field.attrs.other, "level3_threshold_labels").unwrap();
    let AttrValue::Array(ArrayBuf::Text(labels)) = labels else {
        panic!("labels are text");
    };
    assert_eq!(&*labels[0], "ND");
    assert_eq!(&*labels[15], "75");
    let values: Vec<f32> = (0..360)
        .flat_map(|ray| (0..230).map(move |gate| (ray, gate)))
        .filter_map(|(ray, gate)| field.value(ray, gate))
        .collect();
    assert!(
        values
            .iter()
            .all(|v| v.rem_euclid(5.0) == 0.0 && (5.0..=75.0).contains(v))
    );
}

/// KTLX N0V 2013: a 16-level velocity product keeps range folding (level 15,
/// threshold code RF) as the range-folded flag instead of losing it to NaN:
/// as many range-folded gates as MetPy's histogram has level-15 bins.
#[test]
fn ktlx_n0v_thresholds_keep_range_folding() {
    let entry = common::entry("l3-tlx-n0v-20130520-2016");
    let golden = entry.golden();
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("N0V is u8");
    };
    assert_eq!(coding.range_folded, Some(15));
    assert_eq!(coding.fill_value, Some(0));
    assert!(matches!(
        coding.transform,
        LinearTransform::Levels(LevelTable::Sixteen(_))
    ));
    let histogram = golden.get("data").items()[0].get("histogram");
    let level15 = usize::try_from(histogram.get("15").int("level 15")).unwrap();
    assert_eq!(level15, 1457);
    let (rows, cols) = field.shape();
    let folded = (0..rows)
        .flat_map(|ray| (0..cols).map(move |gate| (ray, gate)))
        .filter(|&(ray, gate)| field.gate(ray, gate) == Some(Gate::RangeFolded))
        .count();
    assert_eq!(folded, level15);
    // Knots, from the ICD threshold table: -64 ... 64.
    assert_eq!(field.attrs.units.as_deref(), Some("kt"));
    assert_eq!(coding.resolve(1), Gate::Value(-64.0));
    assert_eq!(coding.resolve(14), Gate::Value(64.0));
}

/// KTLX EET 2013: enhanced echo tops keep the topped bit (0x80): the level
/// table masks it off for the value, `flag_masks` names it, and the number
/// of topped gates is MetPy's `topped` count.
#[test]
fn ktlx_eet_keeps_topped_echo_tops() {
    let entry = common::entry("l3-tlx-eet-20130520-2016");
    let golden = entry.golden();
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    let FieldData::U8 { values, coding } = &field.data else {
        panic!("EET is u8");
    };
    assert_eq!(
        coding.transform,
        LinearTransform::Levels(LevelTable::Masked {
            mask: 0x7F,
            scale: 1.0,
            offset: 2.0
        })
    );
    assert_eq!(field.attrs.flag_values, vec![1, 0x80]);
    assert_eq!(field.attrs.flag_masks, vec![0xFF, 0x80]);
    assert_eq!(
        field
            .attrs
            .flag_meanings
            .iter()
            .map(|m| &**m)
            .collect::<Vec<_>>(),
        ["bad_data", "topped"]
    );
    let physical = golden.get("data").items()[0].get("physical");
    let topped = values.iter().filter(|&&v| v & 0x80 != 0).count();
    assert_eq!(
        topped,
        usize::try_from(physical.get("topped").int("topped")).unwrap()
    );
    assert_eq!(topped, 5324);
    // Code 190 is topped at (190 & 0x7F) - 2 = 60 kft.
    assert_eq!(coding.resolve(190), Gate::Value(60.0));
    assert_eq!(coding.resolve(62), Gate::Value(60.0));
    assert_eq!(coding.resolve(1), Gate::Missing);
    assert_eq!(coding.resolve(0), Gate::Undetect);
}

/// KTLX DVL 2013: high resolution VIL keeps "flagged" (level 1) and
/// "reserved" (level 255) as named levels.
#[test]
fn ktlx_dvl_keeps_named_levels() {
    let entry = common::entry("l3-tlx-dvl-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("DVL is u8");
    };
    assert!(matches!(
        coding.transform,
        LinearTransform::Levels(LevelTable::LinearLog { log_start: 20, .. })
    ));
    assert_eq!(field.attrs.flag_values, vec![1, 255]);
    assert_eq!(
        field
            .attrs
            .flag_meanings
            .iter()
            .map(|m| &**m)
            .collect::<Vec<_>>(),
        ["flagged", "reserved"]
    );
    assert_eq!(coding.valid_range, Some([2, 254]));
    assert_eq!(field.attrs.units.as_deref(), Some("kg m-2"));
}

/// KTLX DPR 2013: the generic radial component as a `u16` field with the
/// component's range coordinate.
#[test]
fn ktlx_dpr_generic_component() {
    let entry = common::entry("l3-tlx-dpr-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    assert_eq!(volume.attrs.instrument_name, "KTLX");
    let sweep = &volume.sweeps[0];
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: 125.0,
            spacing_m: 250.0,
            ngates: 920
        }
    );
    let field = &sweep.fields[0];
    assert_eq!(field.name, FieldName::Rr);
    assert_eq!(field.attrs.units.as_deref(), Some("in h-1"));
    assert!(matches!(field.data, FieldData::U16 { .. }));
    assert_eq!(field.shape(), (360, 920));
    // The generic product description reaches the volume.
    let other = &volume.attrs.other;
    assert_eq!(
        attr(other, "level3_generic_radar_name"),
        Some(AttrValue::text("KTLX"))
    );
    assert!(attr(other, "level3_generic_description").is_some());
}

/// KTLX NCR 2013: a composite reflectivity raster as a 464-row sweep centred
/// on the radar, its 16 levels packed with a level table.
#[test]
fn ktlx_ncr_raster_geometry() {
    let entry = common::entry("l3-tlx-ncr-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.sweep_mode, SweepMode::Other("raster".into()));
    assert_eq!(sweep.nrays(), 464);
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: -231_500.0,
            spacing_m: 1000.0,
            ngates: 464
        }
    );
    let y = sweep.extra_vars.iter().find(|v| &*v.name == "y").unwrap();
    let ArrayBuf::F32(y) = &y.values else {
        panic!("y is f32");
    };
    assert_eq!(y[0], 231_500.0);
    assert_eq!(y[463], -231_500.0);
    let field = &sweep.fields[0];
    assert_eq!(field.name, FieldName::Other("CR".into()));
    assert!(matches!(field.data, FieldData::U8 { .. }));
    assert_eq!(field.attrs.units.as_deref(), Some("dBZ"));
    // The graphic alphanumeric block's storm table text is carried.
    let pages = attr(&volume.attrs.other, "level3_graphic_pages").unwrap();
    let AttrValue::Array(ArrayBuf::Text(pages)) = pages else {
        panic!("pages are text");
    };
    assert!(!pages.is_empty());
    assert!(pages[0].contains("STM ID"), "{}", pages[0]);
}

/// KFWS DPA 1995: the hourly accumulation (packet 17) and its twelve rate
/// arrays (packet 18) each become a sweep.
#[test]
fn kfws_dpa_every_array_is_a_sweep() {
    let entry = common::entry("l3-fws-dpa-19950517-2304");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    assert_eq!(volume.sweeps.len(), 13);
    assert_eq!(volume.sweeps[0].fields[0].name, field_name(81));
    assert_eq!(volume.sweeps[0].nrays(), 131);
    for sweep in &volume.sweeps[1..] {
        let field = &sweep.fields[0];
        assert_eq!(field.name, FieldName::Rr);
        assert_eq!(field.attrs.units.as_deref(), Some("in h-1"));
        assert_eq!(field.shape(), (13, 13));
    }
    // The rate levels are the ICD 2620003AE lower bounds; ND is missing.
    let field = &volume.sweeps[1].fields[0];
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("rate array is u8");
    };
    assert_eq!(coding.resolve(0), Gate::Value(0.0));
    assert_eq!(coding.resolve(4), Gate::Value(1.0));
    assert_eq!(coding.resolve(6), Gate::Value(4.0));
    assert_eq!(coding.fill_value, Some(7));
    assert_eq!(coding.resolve(7), Gate::Missing);
}

/// The FM301 view writes a level-table field decoded (float32, NaN fill)
/// with its codes beside it as `<name>_level`, which carries the coding and
/// flag attributes; linear fields are unchanged.
#[test]
fn view_writes_level_table_fields_decoded_with_their_codes() {
    let entry = common::entry("l3-tlx-n0v-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let view = volume_view(&volume, ViewOptions::XRADAR, None).unwrap();
    let group = view.group("sweep_0").unwrap();
    let decoded = group.variable("VRADH").unwrap();
    let Values::Owned(ArrayBuf::F32(values)) = &decoded.values else {
        panic!("VRADH is decoded: {:?}", decoded.values);
    };
    assert_eq!(values.len(), 360 * 230);
    assert!(decoded.attr("scale_factor").is_none());
    assert_eq!(
        decoded.attr("ancillary_variables"),
        Some(&AttrValue::text("VRADH_level"))
    );
    let codes = group.variable("VRADH_level").unwrap();
    assert!(codes.attr("scale_factor").is_none());
    assert_eq!(
        codes.attr("flag_meanings"),
        Some(&AttrValue::text("range_folded"))
    );
    assert_eq!(
        codes.attr("flag_values"),
        Some(&AttrValue::Array(ArrayBuf::U8(vec![15])))
    );
    assert_eq!(
        codes.attr("_FillValue"),
        Some(&AttrValue::Scalar(Scalar::U8(0)))
    );
    // Decoded values equal the model's gates in the view's ray order.
    let materialized = codes.values.materialize().unwrap();
    let ArrayBuf::U8(levels) = materialized else {
        panic!("codes are u8");
    };
    let field = &volume.sweeps[0].fields[0];
    let FieldData::U8 { coding, .. } = &field.data else {
        panic!("u8");
    };
    for (level, value) in levels.iter().zip(values) {
        match coding.resolve(*level) {
            Gate::Value(v) => assert_eq!(v, *value),
            _ => assert!(value.is_nan()),
        }
    }

    // N0U (linear) is written packed as before, with no `_level` variable.
    let entry = common::entry("l3-tlx-n0u-20130520-2016");
    let volume: Volume = read_level3_volume(&entry.bytes()).unwrap();
    let view = volume_view(&volume, ViewOptions::XRADAR, None).unwrap();
    let group = view.group("sweep_0").unwrap();
    assert!(
        group
            .variable("VRADH")
            .unwrap()
            .attr("scale_factor")
            .is_some()
    );
    assert!(group.variable("VRADH_level").is_none());
}

/// DPA products on the HRAP grid: the array's corner and the latitude and
/// longitude of sample box centres equal pyproj's
/// (`testdata/level3/golden-dpa.json`, `tools/level3_dpa_golden.py`), for
/// the accumulation array and the rate arrays.
#[test]
fn dpa_boxes_match_pyproj() {
    let path = common::testdata_dir().join("level3/golden-dpa.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let files = golden.get("files").items();
    assert_eq!(files.len(), 4);
    let mut boxes = 0;
    for file in files {
        let id = file.get("id").as_str().unwrap();
        let volume = read_level3_volume(&common::entry(id).bytes()).unwrap();
        for item in file.get("boxes").items() {
            let size = item.get("box_size").as_f64().unwrap();
            let sweep = volume.sweeps.iter().find(|s| {
                attr(&s.other, "hrap_box_size") == Some(AttrValue::Scalar(Scalar::F64(size)))
            });
            let Some(sweep) = sweep else {
                // 2026 products carry no rate arrays.
                assert_eq!(size, 10.0, "{id}");
                continue;
            };
            let (west, north) = if size == 10.0 {
                ("rate_west", "rate_north")
            } else {
                ("west", "north")
            };
            assert_eq!(
                attr(&sweep.other, "hrap_west"),
                Some(AttrValue::Scalar(Scalar::F64(
                    file.get(west).as_f64().unwrap()
                ))),
                "{id}"
            );
            assert_eq!(
                attr(&sweep.other, "hrap_north"),
                Some(AttrValue::Scalar(Scalar::F64(
                    file.get(north).as_f64().unwrap()
                ))),
                "{id}"
            );
            let hx = attr(&sweep.other, "hrap_radar_x")
                .and_then(|v| v.as_f64())
                .unwrap();
            assert!(
                (hx - file.get("hrap_x").as_f64().unwrap()).abs() < 1e-6,
                "{id} {hx}"
            );
            let row = usize::try_from(item.get("row").int("row")).unwrap();
            let column = usize::try_from(item.get("column").int("column")).unwrap();
            let columns = sweep.range.ngates();
            for (name, key) in [("latitude", "latitude"), ("longitude", "longitude")] {
                let var = sweep.extra_vars.iter().find(|v| &*v.name == name).unwrap();
                let ArrayBuf::F64(values) = &var.values else {
                    panic!("{name} is f64");
                };
                let ours = values[row * columns + column];
                let theirs = item.get(key).as_f64().unwrap();
                assert!(
                    (ours - theirs).abs() < 1e-9,
                    "{id} box {row},{column} x{size}: {name} {ours} != pyproj {theirs}"
                );
            }
            boxes += 1;
        }
    }
    // 4 files x 5 accumulation boxes, and 3 rate boxes in the 3 files with
    // rate arrays.
    assert_eq!(boxes, 29);

    // The placement evidence: the rule's offsets are the best (or within 2
    // boxes of the best) at every one of the 47 sites.
    let placement = golden.get("placement").items();
    assert_eq!(placement.len(), 47);
    for site in placement {
        let rule = site.get("rule_mismatches").int("rule");
        let best = site.get("best_mismatches").int("best");
        assert!(rule - best <= 2, "{:?}", site.get("file").as_str());
    }

    // The rate array placement evidence: the "ND" boxes of 28 real rate
    // arrays against the boxes wholly beyond 230 km, under the national
    // 1/4 LFM grid (at most 5 boxes of 169 off per array, 55 in all) and
    // under the corner of the 131 x 131 array (268).
    let rate = golden.get("rate_placement").items();
    assert_eq!(rate.len(), 28);
    let (mut rule_total, mut corner_total) = (0, 0);
    for array in rate {
        let rule = array.get("rule_mismatches").int("rule");
        let best = array.get("best_mismatches").int("best");
        assert!(
            rule <= 5 && rule - best <= 3,
            "{:?}",
            array.get("file").as_str()
        );
        rule_total += rule;
        corner_total += array.get("corner_rule_mismatches").int("corner");
    }
    assert_eq!((rule_total, corner_total), (55, 268));
}

/// The Radar Coded Message's intensity grid as a sweep on the national 1/16
/// LFM grid: levels equal the separate (same-author) Appendix B reading
/// (`golden-rcm.json` grid SHA-256), the grid's corner and the latitude and
/// longitude of sample fine boxes equal pyproj's (`golden-lfm.json`,
/// `tools/level3_lfm_golden.py`), and the recorded evidence for the grid
/// rule holds (see `recast_radar_io_level3::hrap`).
#[test]
fn radar_coded_message_grid_matches_pyproj() {
    let dir = common::testdata_dir().join("level3");
    let lfm = Json::parse(&std::fs::read_to_string(dir.join("golden-lfm.json")).unwrap()).unwrap();
    let rcm = Json::parse(&std::fs::read_to_string(dir.join("golden-rcm.json")).unwrap()).unwrap();
    // Four product 74 files and three products 83 (whose packet 32 grid is
    // the Part A grid of the message they hold; the KLOT 1994 one holds no
    // message and its grid is compared with the separate reading of its
    // packet 32).
    let files = lfm.get("files").items();
    assert_eq!(files.len(), 7);
    for file in files {
        let id = file.get("id").as_str().unwrap();
        let volume = read_level3_volume(&common::entry(id).bytes()).unwrap();
        assert_eq!(volume.sweeps.len(), 1, "{id}");
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.nrays(), 100, "{id}");
        assert_eq!(sweep.range.ngates(), 100, "{id}");
        for (name, key) in [("hrap_west", "west"), ("hrap_north", "north")] {
            assert_eq!(
                attr(&sweep.other, name),
                Some(AttrValue::Scalar(Scalar::F64(
                    file.get(key).as_f64().unwrap()
                ))),
                "{id} {name}"
            );
        }
        assert_eq!(
            attr(&sweep.other, "hrap_box_size"),
            Some(AttrValue::Scalar(Scalar::F64(2.5)))
        );
        for item in file.get("boxes").items() {
            let row = usize::try_from(item.get("row").int("row")).unwrap();
            let column = usize::try_from(item.get("column").int("column")).unwrap();
            for name in ["latitude", "longitude"] {
                let var = sweep.extra_vars.iter().find(|v| &*v.name == name).unwrap();
                let ArrayBuf::F64(values) = &var.values else {
                    panic!("{name} is f64");
                };
                let ours = values[row * 100 + column];
                let theirs = item.get(name).as_f64().unwrap();
                assert!(
                    (ours - theirs).abs() < 1e-9,
                    "{id} box {row},{column}: {name} {ours} != pyproj {theirs}"
                );
            }
        }
        let field = &sweep.fields[0];
        let FieldData::U8 { values, .. } = &field.data else {
            panic!("{id}: RCM levels are u8");
        };
        let golden = rcm
            .get("files")
            .items()
            .iter()
            .find(|f| f.get("id").as_str() == Some(id))
            .unwrap();
        let expected = if golden.get("no_message").as_bool() == Some(true) {
            golden.get("irm_grid_sha256").as_str().unwrap()
        } else {
            golden.get("part_a").get("grid_sha256").as_str().unwrap()
        };
        assert_eq!(common::sha256_hex(values), expected, "{id}");
        assert_eq!(field.attrs.is_discrete, Some(true), "{id}");
    }

    // The evidence: in each of the six volumes every named centroid and TVS
    // lies within 0.5 km of its box under the rule, only the rule fits all
    // six, and the intensity groups agree best (within 3 boxes) under it.
    let cases = lfm.get("cases").items();
    assert_eq!(cases.len(), 6);
    let mut common_offsets: Option<Vec<String>> = None;
    let mut features = 0;
    for case in cases {
        let name = case.get("name").as_str().unwrap();
        assert!(
            case.get("max_outside_km").as_f64().unwrap() <= 0.5,
            "{name}"
        );
        features += case.get("features").items().len();
        let offsets: Vec<String> = case
            .get("alignments_within_tolerance")
            .items()
            .iter()
            .map(|o| format!("{:?}", (o.items()[0].as_f64(), o.items()[1].as_f64())))
            .collect();
        assert!(
            offsets.contains(&format!("{:?}", (Some(0.0), Some(0.0)))),
            "{name}"
        );
        common_offsets = Some(match common_offsets {
            None => offsets,
            Some(previous) => previous
                .into_iter()
                .filter(|o| offsets.contains(o))
                .collect(),
        });
        let intensity = case.get("intensity");
        let rule = intensity.get("agree_rule").int("rule");
        let best = intensity.get("agree_best").int("best");
        assert!(best - rule <= 3, "{name}");
    }
    assert_eq!(features, 72);
    assert_eq!(
        common_offsets.unwrap(),
        vec![format!("{:?}", (Some(0.0), Some(0.0)))]
    );
    // Beyond 124 nmi, level 7 is the stronger echo.
    let levels = lfm.get("levels").get("levels");
    assert!(
        levels.get("7").get("median_dbz").as_f64().unwrap()
            > levels.get("8").get("median_dbz").as_f64().unwrap()
    );
}

/// KTLX N0B 2026-06-22 08:08:06: a mid-volume SAILS cut of VCP 212
/// (elevation number 3, 118 s after the volume start). Halfword 50 carries
/// supplemental scan code 2, which real products use for SAILS (Table V Note
/// 24 says MRLE; every extra 0.5 degree cut of the KTLX volumes that hour
/// carries 2).
#[test]
fn ktlx_sails_cut_is_labelled_sails() {
    let bytes =
        std::fs::read(recast_radar_testdata::path("l3-ktlx-20260622-080806-n0b-sails").unwrap())
            .unwrap();
    let product = decode_product(&bytes).unwrap();
    let desc = &product.description;
    assert_eq!(desc.product_code, 153);
    assert_eq!(desc.elevation_number, 3);
    assert_eq!(desc.halfword(50), Some((118 << 5) | 2));
    assert_eq!(
        recast_radar_io_level3::volume::supplemental_scan(desc).as_deref(),
        Some("sails")
    );
    let volume = product.to_volume().unwrap();
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.fixed_angle_deg, 0.5);
    assert_eq!(sweep.elevation_number, Some(3));
    assert_eq!(sweep.rays.time_s[0], 118.0);
    assert_eq!(
        attr(&sweep.other, "level3_supplemental_scan"),
        Some(AttrValue::text("sails"))
    );
    assert_eq!(
        attr(&volume.attrs.other, "level3_supplemental_scan"),
        Some(AttrValue::text("sails"))
    );
}

/// Cross sections (products 50, KLOT 1994-10-31 13:58Z, and 51, KMLB
/// 1994-11-16 03:35Z) against their own annotation: the unlinked vectors
/// (packet 7) draw the 0 kft axis, height lines labelled `10`-`60` (kft,
/// packet 1 text) and range ticks labelled in nm along the section, in screen
/// pixels, and the raster header gives the pixels per cell. The sweep's 500 m
/// rows and 1000 m columns (0.27 x 0.54 nmi, ICD 2620003AE 14.2.3) agree with
/// that scale within 1 %, its top row with the raster's top above the axis,
/// its end points (Table V halfwords 47-50) with the section's end labels, and
/// its length with the distance between them.
#[test]
fn cross_section_geometry_matches_its_axes() {
    for id in ["l3-lot-050-19941031-1358", "l3-mlb-051-19941116-0335"] {
        check_cross_section_axes(id);
    }
}

fn check_cross_section_axes(id: &str) {
    use recast_radar_io_level3::packets::raster::RasterHeader;
    use recast_radar_io_level3::packets::vectors::Vectors;
    use recast_radar_io_level3::{Packet, ParameterValue};

    let bytes = common::entry(id).bytes();
    let product = decode_product(&bytes).unwrap();
    let packets: Vec<&Packet> = product
        .symbology
        .as_ref()
        .unwrap()
        .layers
        .iter()
        .flatten()
        .collect();
    let (x_scale, y_scale, j_start) = packets
        .iter()
        .find_map(|p| match p {
            Packet::Raster(r) => match r.header {
                RasterHeader::RasterData {
                    x_scale,
                    y_scale,
                    j_start,
                    ..
                } => Some((f64::from(x_scale), f64::from(y_scale), f64::from(j_start))),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    let segments: Vec<_> = packets
        .iter()
        .filter_map(|p| match p {
            Packet::Vectors(v) if v.code == 7 => match &v.vectors {
                Vectors::Unlinked(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .collect();
    // Height lines start on the vertical axis (i = 50) and run to the right
    // edge; the 0 kft axis is the lowest of the full-width lines.
    let axis_i = 50;
    let mut height_lines: Vec<f64> = segments
        .iter()
        .filter(|s| s.begin.i == axis_i && s.begin.j == s.end.j && s.end.i > 500)
        .map(|s| f64::from(s.begin.j))
        .collect();
    height_lines.sort_by(|a, b| b.total_cmp(a));
    let axis_j = segments
        .iter()
        .filter(|s| s.begin.j == s.end.j && s.begin.i < axis_i && s.end.i > 500)
        .map(|s| f64::from(s.begin.j))
        .fold(f64::MIN, f64::max);
    // Range ticks hang up from the axis.
    let mut ticks: Vec<f64> = segments
        .iter()
        .filter(|s| f64::from(s.begin.j) == axis_j && s.begin.i == s.end.i && s.end.j < s.begin.j)
        .map(|s| f64::from(s.begin.i))
        .collect();
    ticks.sort_by(f64::total_cmp);
    let numbers = |keep: &dyn Fn(i16, i16) -> bool| -> Vec<f64> {
        packets
            .iter()
            .filter_map(|p| match p {
                Packet::Text(t) if t.code == 1 && keep(t.i, t.j) => t.text.trim().parse().ok(),
                _ => None,
            })
            .collect()
    };
    // Height labels sit left of the axis, range labels just below it.
    let kft = numbers(&|i, _| i < axis_i);
    let nm = numbers(&|_, j| f64::from(j) > axis_j && j < 480);
    assert_eq!(kft, [10.0, 20.0, 30.0, 40.0, 50.0, 60.0], "{id}");
    assert_eq!(height_lines.len(), 6, "{id}");
    assert_eq!(ticks.len(), nm.len(), "{id}");
    assert!(nm.len() >= 5, "{id}: {nm:?}");
    // Pixels per kft and per nm from the last line and tick.
    let px_per_kft = (axis_j - height_lines[5]) / 60.0;
    let px_per_nm = (ticks[ticks.len() - 1] - f64::from(axis_i)) / nm[nm.len() - 1];
    let row_m = y_scale / px_per_kft * 304.8;
    let column_m = x_scale / px_per_nm * 1852.0;
    assert!((row_m / 500.0 - 1.0).abs() < 0.01, "{id}: row {row_m} m");
    assert!(
        (column_m / 1000.0 - 1.0).abs() < 0.01,
        "{id}: column {column_m} m"
    );

    let volume = read_level3_volume(&bytes).unwrap();
    let sweep = &volume.sweeps[0];
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = sweep.range
    else {
        panic!("range");
    };
    assert_eq!((first_center_m, spacing_m), (500.0, 1000.0), "{id}");
    let y = sweep.extra_vars.iter().find(|v| &*v.name == "y").unwrap();
    let ArrayBuf::F32(heights) = &y.values else {
        panic!("y");
    };
    // The raster's top edge above the axis, from the annotation's scale.
    let top_m = (axis_j - j_start) / px_per_kft * 304.8;
    assert!(
        (f64::from(heights[0]) + 250.0 - top_m).abs() < 250.0,
        "{id}: top {top_m} m"
    );
    // The end points of Table V halfwords 47-50 are the section's end labels
    // (azimuth degrees / nm) printed below it, e.g. `(305/ 59)`.
    let mut end_labels: Vec<(i16, (f64, f64))> = packets
        .iter()
        .filter_map(|p| match p {
            Packet::Text(t) if t.code == 1 && f64::from(t.j) > axis_j + 8.0 => {
                let inner = t.text.trim().strip_prefix('(')?.strip_suffix(')')?;
                let (az, range) = inner.split_once('/')?;
                Some((t.i, (az.trim().parse().ok()?, range.trim().parse().ok()?)))
            }
            _ => None,
        })
        .collect();
    end_labels.sort_by_key(|(i, _)| *i);
    let parameter = |name: &str| {
        product
            .description
            .parameters()
            .into_iter()
            .find(|p| p.name == name)
            .and_then(|p| match p.value {
                ParameterValue::Float(v) => Some(v),
                _ => None,
            })
            .unwrap()
    };
    let point1 = (parameter("point1_azimuth"), parameter("point1_range"));
    let point2 = (parameter("point2_azimuth"), parameter("point2_range"));
    assert_eq!(end_labels.first().map(|(_, v)| *v), Some(point1), "{id}");
    assert_eq!(end_labels.last().map(|(_, v)| *v), Some(point2), "{id}");
    // The section's length from those end points.
    let xy = |(az, range): (f64, f64)| {
        let a = az.to_radians();
        (range * 1852.0 * a.sin(), range * 1852.0 * a.cos())
    };
    let (p1, p2) = (xy(point1), xy(point2));
    let length_m = (p1.0 - p2.0).hypot(p1.1 - p2.1);
    assert!(
        (f64::from(ngates) * spacing_m - length_m).abs() < spacing_m,
        "{id}: {ngates} columns for {length_m} m"
    );
}

/// Combined shear (product 87, KTLX 1994-03-08 19:39Z): a raster of cells of
/// halfword 50 (0.54 nmi) centred on the radar. Table V halfwords 48-49 give
/// the azimuth and range of the maximum shear (232.7 degrees, 39.7 nmi); the
/// sweep puts a cell of the highest level there, within half a cell.
#[test]
fn combined_shear_maximum_is_at_its_table_v_position() {
    let bytes = common::entry("l3-tlx-087-19940308-1939").bytes();
    let product = decode_product(&bytes).unwrap();
    let d = &product.description;
    let azimuth = f64::from(d.halfword(48).unwrap()) * 0.1;
    let range_m = f64::from(d.halfword(49).unwrap()) * 0.1 * 1852.0;
    let (east, north) = (
        range_m * azimuth.to_radians().sin(),
        range_m * azimuth.to_radians().cos(),
    );
    let volume = read_level3_volume(&bytes).unwrap();
    let sweep = &volume.sweeps[0];
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = sweep.range
    else {
        panic!("range");
    };
    assert!((spacing_m - 0.54 * 1852.0).abs() < 1e-6, "{spacing_m}");
    let y = sweep.extra_vars.iter().find(|v| &*v.name == "y").unwrap();
    let ArrayBuf::F32(rows) = &y.values else {
        panic!("y");
    };
    let field = &sweep.fields[0];
    let FieldData::U8 { values, .. } = &field.data else {
        panic!("storage");
    };
    let max = *values.iter().max().unwrap();
    let columns = ngates as usize;
    let nearest = values
        .iter()
        .enumerate()
        .filter(|(_, v)| **v == max)
        .map(|(index, _)| {
            let x = first_center_m + spacing_m * (index % columns) as f64;
            let y = f64::from(rows[index / columns]);
            (x - east).hypot(y - north)
        })
        .fold(f64::MAX, f64::min);
    assert!(
        nearest < 0.5 * spacing_m,
        "nearest maximum cell {nearest} m away"
    );
}

/// The national 1/16 LFM grid the radar coded message boxes lie on, as NWS
/// published it for its RCM reflectivity mosaic (Kitzmiller, Samplatsky and
/// Keller 2002, NOAA Techniques Development Laboratory, section 3: polar
/// stereographic, 105W, true at 60N, 11 906.25 m mesh, 460 columns by 360
/// rows, extreme lower-left corner 119.036W 23.097N and upper-right 58.025W
/// 45.317N, aligned with the 47 625 m and 4 762.5 m grids, the 1/4 LFM and
/// HRAP). On the HRAP
/// grid (`hrap`) those corners fall on the box edges `hrap` puts the national
/// boxes on, `1 (mod 2.5)` (and `1 (mod 10)` for the 1/4 LFM boxes): the
/// lower-left at (1, 1) and the upper-right 460 x 2.5 and 360 x 2.5 HRAP
/// units away, within the corners' three-decimal rounding.
#[test]
fn national_rcm_grid_corners_are_on_the_national_box_edges() {
    use recast_radar_io_level3::hrap;
    let (x0, y0) = hrap::to_grid(23.097, -119.036);
    let (x1, y1) = hrap::to_grid(45.317, -58.025);
    for (value, expected) in [(x0, 1.0), (y0, 1.0), (x1, 1151.0), (y1, 901.0)] {
        assert!((value - expected).abs() < 0.03, "{value} vs {expected}");
    }
    // The pole, where the national boxes have a corner, is 160 x 2.5 and 640
    // x 2.5 HRAP units from that lower-left corner.
    assert_eq!(hrap::POLE, (401.0, 1601.0));
    assert_eq!(((401.0 - 1.0) / 2.5, (1601.0 - 1.0) / 2.5), (160.0, 640.0));
}

/// Weak Echo Region (product 53): no ICD obtained places its window, so the
/// placement was fitted (module documentation). The lowest slice, placed as
/// the volume places it (north up, centred at halfwords 27-28), agrees with
/// the 0.5 degree Base Reflectivity of the same volume (read with the same
/// decoder and mapped to the slice's eight levels) better than the same
/// window moved by one or two cells in any direction or turned by 90, 180
/// or 270 degrees about its centre: KCAE 1994-06-29 19:06Z (window at
/// 185.5 degrees and 59.5 nmi, on storm 53; correlation above 0.85) and KLOT
/// 1994-11-06 02:46Z (halfwords 27-28 zero: the window at the radar;
/// correlation above 0.65, and a search of centres over 100 nmi around the
/// radar in 1 nmi steps found none better).
#[test]
fn weak_echo_region_window_matches_base_reflectivity() {
    check_weak_echo_region("l3-cae-053-19940629-1906", "l3-cae-n0r-19940629-1906", 0.85);
    check_weak_echo_region("l3-lot-053-19941106-0246", "l3-lot-n0r-19941106-0246", 0.65);
}

fn check_weak_echo_region(wer_id: &str, base_id: &str, min_correlation: f64) {
    use recast_radar_io_level3::levels::Level;

    let wer_bytes = common::entry(wer_id).bytes();
    let wer = read_level3_volume(&wer_bytes).unwrap();
    let wer_product = decode_product(&wer_bytes).unwrap();
    let base = read_level3_volume(&common::entry(base_id).bytes()).unwrap();
    assert_eq!(wer.time_reference, base.time_reference);
    assert_eq!(wer.sweeps.len(), 8);
    let slice = &wer.sweeps[0];
    assert_eq!(slice.fixed_angle_deg, 0.5);
    let reflectivity = &base.sweeps[0];
    assert_eq!(reflectivity.fixed_angle_deg, 0.5);

    // The slice's cells: centre positions and levels.
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    } = slice.range
    else {
        panic!("range");
    };
    let n = ngates as usize;
    let y = slice.extra_vars.iter().find(|v| &*v.name == "y").unwrap();
    let nrows = slice.nrays();
    let ArrayBuf::F32(rows) = &y.values else {
        panic!("y");
    };
    let FieldData::U8 { values: levels, .. } = &slice.fields[0].data else {
        panic!("storage");
    };
    // The slice's thresholds (levels 1-7), from its own description.
    let mapping = DataLevels::from_description(&wer_product.description).unwrap();
    let thresholds: Vec<f64> = (1..8)
        .map(|n| match mapping.level(n) {
            Level::Value(v) => v,
            other => panic!("level {n}: {other:?}"),
        })
        .collect();

    // Base reflectivity at a point: the ray whose start/width holds its
    // azimuth, the 1 km gate holding its range.
    let starts = reflectivity
        .extra_vars
        .iter()
        .find(|v| &*v.name == "level3_start_angle")
        .unwrap();
    let widths = reflectivity
        .extra_vars
        .iter()
        .find(|v| &*v.name == "level3_delta_angle")
        .unwrap();
    let (ArrayBuf::F32(starts), ArrayBuf::F32(widths)) = (&starts.values, &widths.values) else {
        panic!("angles");
    };
    let dbz_at = |east: f64, north: f64| -> Option<f64> {
        let azimuth = east.atan2(north).to_degrees().rem_euclid(360.0);
        let ray = starts
            .iter()
            .zip(widths)
            .position(|(&s, &w)| (azimuth - f64::from(s)).rem_euclid(360.0) < f64::from(w))?;
        let gate = (east.hypot(north) / 1000.0).floor() as usize;
        reflectivity.fields[0]
            .value(ray, gate)
            .map(f64::from)
            .filter(|v| v.is_finite())
    };
    let level_of = |dbz: Option<f64>| -> f64 {
        dbz.map_or(0.0, |v| {
            thresholds.iter().filter(|&&t| v >= t).count() as f64
        })
    };
    let centre = (
        first_center_m + spacing_m * (n as f64 - 1.0) / 2.0,
        f64::from(rows[0] + rows[nrows - 1]) / 2.0,
    );
    // Correlation of the slice with the base reflectivity when the window is
    // turned by `quarter` quarter turns and moved by `(dx, dy)` cells.
    let score = |quarter: u32, dx: f64, dy: f64| -> f64 {
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        for row in 0..nrows {
            for column in 0..n {
                let (mut x, mut y) = (
                    first_center_m + spacing_m * column as f64 - centre.0,
                    f64::from(rows[row]) - centre.1,
                );
                for _ in 0..quarter {
                    (x, y) = (y, -x);
                }
                let east = centre.0 + x + dx * spacing_m;
                let north = centre.1 + y + dy * spacing_m;
                ours.push(f64::from(levels[row * n + column]));
                theirs.push(level_of(dbz_at(east, north)));
            }
        }
        correlation(&ours, &theirs)
    };
    let placed = score(0, 0.0, 0.0);
    assert!(placed > min_correlation, "{wer_id}: correlation {placed}");
    for quarter in 0..4 {
        for dx in [-2.0, -1.0, 0.0, 1.0, 2.0] {
            for dy in [-2.0, -1.0, 0.0, 1.0, 2.0] {
                if (quarter, dx, dy) == (0, 0.0, 0.0) {
                    continue;
                }
                let other = score(quarter, dx, dy);
                assert!(
                    other < placed,
                    "{wer_id}: turned {quarter} quarters, moved ({dx}, {dy}) cells: {other} >= {placed}"
                );
            }
        }
    }
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt()
}
