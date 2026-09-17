//! Radial, raster and generic products as one-sweep FM301 volumes
//! (`recast_radar_io_level3::volume`), against the real Level III corpus
//! (`testdata/level3/manifest.toml`) and its golden JSON
//! (`testdata/level3/golden/<id>.json`, `tools/level3_golden.py`).
//!
//! For every file whose golden `data` holds a radial (16, 0xAF1F), raster
//! (0xBA07), digital precipitation (17) or generic (28) array:
//!
//! - the volume has one sweep with one field of the golden dimensions;
//! - fields kept packed hash to the golden raw levels (MetPy 1.7.1's array,
//!   `u8` or big-endian `u16`);
//! - every gate's physical value equals the crate's own level mapping
//!   ([`DataLevels::values`]), which `tests/radial_generic.rs` pins to MetPy,
//!   and the summary equals MetPy's `map_data` for the products MetPy maps
//!   exactly (the same set as that test);
//! - the geometry follows the ICD: bin size per product, first centre half a
//!   bin out, elevation from halfword 30 for elevation products (MetPy's
//!   `el_angle`) and NaN otherwise, ray time from the halfword 50 delay,
//!   volume time, location and VCP from the Product Description Block.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use common::{Entry, Json, PhysicalSummary};
use recast_radar_core::model::{
    AttrValue, FieldData, FieldName, Gate, RangeCoord, Scalar, SourceFormat, SweepMode,
};
use recast_radar_io_level3::levels::{DataLevels, Level};
use recast_radar_io_level3::volume::{
    elevation_deg, elevation_delay_s, field_name, range_bin_size_m, raster_cell_size_m,
};
use recast_radar_io_level3::{Level3Error, Packet, decode_product, read_level3_volume};

/// Products MetPy 1.7.1 maps to the same physical values (see
/// `tests/radial_generic.rs`).
const METPY_EQUAL: [i16; 34] = [
    19, 20, 25, 27, 28, 30, 32, 56, 78, 79, 80, 94, 99, 134, 135, 153, 154, 155, 159, 161, 163,
    167, 169, 170, 171, 172, 173, 174, 175, 176, 180, 181, 182, 186,
];

/// Expected range bin size (radial) or cell size (raster) in metres of every
/// product in the corpus, from ICD Table III.
fn expected_spacing_m(product_code: i16, packet: u16, num_bins: u16) -> f64 {
    if packet == 0xBA07 || packet == 17 {
        return match product_code {
            37 => 1000.0,
            78 | 80 => 2000.0,
            36 | 38 | 41 | 57 | 65 | 66 | 67 | 90 => 4000.0,
            81 => 4762.5,
            other => panic!("no expected cell size for raster product {other}"),
        };
    }
    match product_code {
        19 | 27 | 30 | 32 | 56 | 94 | 134 | 135 => 1000.0,
        20 | 78 | 79 | 80 | 138 | 169 | 171 => 2000.0,
        25 | 28 | 99 | 113 | 153 | 154 | 155 | 159 | 161 | 163 | 165 | 167 | 170 | 172 | 173
        | 174 | 175 | 176 | 177 | 197 => 250.0,
        180..=182 => 150.0,
        186 => 300.0,
        34 => 230_000.0 / f64::from(num_bins),
        other => panic!("no expected bin size for radial product {other}"),
    }
}

/// The golden `data` entry the conversion uses: the first data packet in
/// symbology order other than packet 18.
fn first_data_entry(golden: &Json) -> Option<&Json> {
    golden
        .get("data")
        .items()
        .iter()
        .filter(|e| e.get("packet").int("packet") != 18)
        .min_by_key(|e| (e.get("layer").int("layer"), e.get("index").int("index")))
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .with_timezone(&Utc)
}

/// Raw levels of the data array as the packet stores them.
fn packet_levels(product: &recast_radar_io_level3::Level3Product) -> Vec<u16> {
    let packet = product
        .symbology
        .iter()
        .flat_map(|s| s.layers.iter().flatten())
        .find(|p| {
            matches!(
                p,
                Packet::Radial(_) | Packet::Generic(_) | Packet::DigitalPrecip(_)
            ) || matches!(p, Packet::Raster(r) if r.code != 18)
        })
        .expect("data packet");
    match packet {
        Packet::Radial(r) => r.levels.iter().map(|&l| u16::from(l)).collect(),
        Packet::Raster(r) => r.grid.levels().iter().map(|&l| u16::from(l)).collect(),
        Packet::DigitalPrecip(p) => p.grid.levels().iter().map(|&l| u16::from(l)).collect(),
        Packet::Generic(g) => {
            let component = g.radial_components().next().expect("radial component");
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
    packed_hashes: usize,
    metpy_summaries: usize,
    per_packet: BTreeMap<u16, usize>,
}

#[test]
fn every_data_array_product_converts_to_a_volume() {
    let mut counts = Counts::default();
    let mut failures = Vec::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(data) = first_data_entry(&golden) else {
            continue;
        };
        counts.files += 1;
        let packet = u16::try_from(data.get("packet").int("packet")).unwrap();
        *counts.per_packet.entry(packet).or_default() += 1;
        let problems = check_file(&entry, &golden, data, packet, &mut counts);
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
    // 93 digital radial, 43 run-length radial, 26 raster, 4 digital
    // precipitation and 2 generic products (reference.md section 7).
    assert_eq!(counts.files, 168);
    assert_eq!(counts.per_packet.get(&16), Some(&93));
    assert_eq!(counts.per_packet.get(&0xAF1F), Some(&43));
    assert_eq!(counts.per_packet.get(&0xBA07), Some(&26));
    assert_eq!(counts.per_packet.get(&17), Some(&4));
    assert_eq!(counts.per_packet.get(&28), Some(&2));
    eprintln!(
        "{} files converted: {} packed fields hashed against MetPy, {} summaries matched MetPy",
        counts.files, counts.packed_hashes, counts.metpy_summaries
    );
}

fn check_file(
    entry: &Entry,
    golden: &Json,
    data: &Json,
    packet: u16,
    counts: &mut Counts,
) -> Vec<String> {
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

    // Shape.
    let rows = usize::try_from(data.get("rows").int("rows")).unwrap();
    let cols = usize::try_from(data.get("cols").int("cols")).unwrap();
    if volume.sweeps.len() != 1 {
        return vec![format!("{} sweeps", volume.sweeps.len())];
    }
    let sweep = &volume.sweeps[0];
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
    if field.name != field_name(code) {
        problems.push(format!("name {} != field_name({code})", field.name));
    }

    // Packed levels hash to MetPy's raw array; every gate's value equals the
    // crate's level mapping.
    let levels = packet_levels(&product);
    let mapping = DataLevels::from_description(desc);
    let golden_hash = data.get("raw_sha256").as_str().unwrap();
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
        FieldData::F32 { .. } => {}
        other => problems.push(format!("unexpected storage {other:?}")),
    }
    let discrete = field.attrs.is_discrete == Some(true);
    if let Some(mapping) = &mapping {
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
                // `REAL*4` scale / offset products evaluate in f32, one
                // rounding step from the f64 level table.
                (Gate::Value(v), _) => {
                    !discrete
                        && expected.is_finite()
                        && (v - expected).abs() <= 2.0 * f32::EPSILON * expected.abs().max(1.0)
                }
                (Gate::Undetect, level) => {
                    matches!(
                        level,
                        Level::Flag(
                            recast_radar_io_level3::levels::LevelFlag::BelowThreshold
                                | recast_radar_io_level3::levels::LevelFlag::NoAccumulation
                        )
                    )
                }
                (Gate::RangeFolded, level) => {
                    matches!(
                        level,
                        Level::Flag(recast_radar_io_level3::levels::LevelFlag::RangeFolded)
                    )
                }
                (Gate::Missing, _) => expected.is_nan(),
            };
            if !ok {
                mismatches += 1;
                first.get_or_insert((ray, gate, level, gate_value, expected));
            }
        }
        if mismatches > 0 {
            problems.push(format!(
                "{mismatches} gates differ from DataLevels::values; first {first:?}"
            ));
        }
        // Discrete fields list every class as a flag value.
        if discrete {
            let classes: Vec<i64> = (0..256u16)
                .filter(|&n| matches!(mapping.level(n), Level::Class(_)))
                .map(i64::from)
                .collect();
            if field.attrs.flag_values != classes {
                problems.push(format!(
                    "flag_values {:?} != classes {classes:?}",
                    field.attrs.flag_values
                ));
            }
            if field.attrs.flag_meanings.len() != classes.len() {
                problems.push("flag_meanings length".into());
            }
        }
    } else if !matches!(field.data, FieldData::U8 { .. }) {
        problems.push("product without a level mapping is not kept as u8".into());
    }

    // MetPy's physical summary.
    if METPY_EQUAL.contains(&code) {
        let values: Vec<f32> = (0..rows)
            .flat_map(|ray| (0..cols).map(move |gate| (ray, gate)))
            .map(|(ray, gate)| field.value(ray, gate).unwrap_or(f32::NAN))
            .collect();
        let summary = PhysicalSummary::of_f32(&values);
        let golden_summary = PhysicalSummary::from_golden(data.get("physical"));
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
    let is_raster = packet == 0xBA07 || packet == 17;
    if is_raster {
        if sweep.sweep_mode != SweepMode::Other("raster".into()) {
            problems.push(format!("raster sweep_mode {:?}", sweep.sweep_mode));
        }
        let expected_first = (0.5 - cols as f64 / 2.0) * expected_spacing;
        if (first_center_m - expected_first).abs() > 1e-6 {
            problems.push(format!(
                "raster first column {first_center_m} != {expected_first}"
            ));
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
        let cell = sweep.other.iter().find(|(k, _)| &**k == "raster_cell_m");
        if cell.map(|(_, v)| v.clone()) != Some(AttrValue::Scalar(Scalar::F64(expected_spacing))) {
            problems.push(format!("raster_cell_m {cell:?}"));
        }
    } else {
        if sweep.sweep_mode != SweepMode::AzimuthSurveillance {
            problems.push(format!("sweep_mode {:?}", sweep.sweep_mode));
        }
        // Bin 0 starts at the radar; generic components state the centre.
        let expected_first = match packet {
            28 => data
                .get("generic_component")
                .get("first_gate")
                .as_f64()
                .unwrap(),
            _ => 0.5 * expected_spacing,
        };
        if (first_center_m - expected_first).abs() > 1e-6 {
            problems.push(format!("first centre {first_center_m} != {expected_first}"));
        }
        // The array does not reach past the product's maximum range.
        let max_range_m = golden
            .get("metpy_detail")
            .get("max_range")
            .as_f64()
            .unwrap()
            * 1000.0;
        let extent = first_center_m + spacing_m * (cols as f64 - 0.5);
        // MetPy rounds 225 nm to 416 km; 1390 x 300 m is 417 km.
        if extent > max_range_m * 1.005 + spacing_m {
            problems.push(format!(
                "extent {extent} m past MetPy max range {max_range_m}"
            ));
        }
        // Azimuth: the radial centre.
        let first_radial = product
            .symbology
            .iter()
            .flat_map(|s| s.layers.iter().flatten())
            .find_map(|p| match p {
                Packet::Radial(r) => Some((
                    r.radials[0].start_angle_deg(),
                    r.radials[0].delta_angle_deg(),
                )),
                Packet::Generic(g) => g
                    .radial_components()
                    .next()
                    .map(|c| (c.radials[0].azimuth, c.radials[0].width)),
                _ => None,
            })
            .unwrap();
        let expected_azimuth = (first_radial.0 + 0.5 * first_radial.1).rem_euclid(360.0);
        if (sweep.rays.azimuth_deg[0] - expected_azimuth).abs() > 1e-4 {
            problems.push(format!(
                "azimuth {} != centre {expected_azimuth}",
                sweep.rays.azimuth_deg[0]
            ));
        }
        // Elevation: MetPy's el_angle for elevation products, else NaN.
        match (elevation_deg(desc), metadata.get("el_angle").as_f64()) {
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
    }

    // Time: volume scan start, plus the elevation delay for the products that carry it.
    let vol_time = parse_time(metadata.get("vol_time").as_str().unwrap());
    if volume.time_reference != vol_time {
        problems.push(format!(
            "time_reference {} != {vol_time}",
            volume.time_reference
        ));
    }
    let delay = f64::from(elevation_delay_s(desc).unwrap_or(0));
    if sweep.rays.time_s.iter().any(|t| *t != delay) {
        problems.push(format!("ray times differ from delay {delay}"));
    }
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
    if volume.provenance.decode.decoded_ray_count != rows {
        problems.push("decoded_ray_count".into());
    }
    let product_code = |other: &[(Box<str>, AttrValue)]| {
        other
            .iter()
            .find(|(k, _)| &**k == "product_code")
            .map(|(_, v)| v.clone())
    };
    let expected_code = Some(AttrValue::Scalar(Scalar::I16(code)));
    if product_code(&volume.attrs.other) != expected_code
        || product_code(&field.attrs.other) != expected_code
    {
        problems.push("product_code attribute".into());
    }
    if let Some(awips) = golden.get("framing").get("awips_id").as_str()
        && packet != 28
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

/// Bin and cell sizes of the products with a corpus sample, and `None` for
/// products the ICD leaves without one.
#[test]
fn geometry_tables() {
    assert_eq!(range_bin_size_m(94, 460), Some(1000.0));
    assert_eq!(range_bin_size_m(153, 1840), Some(250.0));
    assert_eq!(range_bin_size_m(180, 592), Some(150.0));
    assert_eq!(range_bin_size_m(186, 1390), Some(300.0));
    assert_eq!(range_bin_size_m(34, 230), Some(1000.0));
    assert_eq!(range_bin_size_m(34, 460), Some(500.0));
    assert_eq!(range_bin_size_m(34, 0), None);
    assert_eq!(range_bin_size_m(48, 0), None);
    assert_eq!(raster_cell_size_m(37), Some(1000.0));
    assert_eq!(raster_cell_size_m(38), Some(4000.0));
    assert_eq!(raster_cell_size_m(189), None);
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

/// KTLX N0R 2013: a 16-level product expands to physical dBZ values.
#[test]
fn ktlx_n0r_thresholds_expand_to_f32() {
    let entry = common::entry("l3-tlx-n0r-20130520-2016");
    let volume = read_level3_volume(&entry.bytes()).unwrap();
    let field = &volume.sweeps[0].fields[0];
    assert_eq!(field.name, FieldName::Dbzh);
    assert_eq!(field.attrs.units.as_deref(), Some("dBZ"));
    assert!(matches!(field.data, FieldData::F32 { .. }));
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
}

/// KTLX NCR 2013: a composite reflectivity raster as a 464-row sweep centred
/// on the radar.
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
    let recast_radar_core::model::ArrayBuf::F32(y) = &y.values else {
        panic!("y is f32");
    };
    assert_eq!(y[0], 231_500.0);
    assert_eq!(y[463], -231_500.0);
    let field = &sweep.fields[0];
    assert_eq!(field.name, FieldName::Other("CR".into()));
    assert!(matches!(field.data, FieldData::F32 { .. }));
    assert_eq!(field.attrs.units.as_deref(), Some("dBZ"));
}
