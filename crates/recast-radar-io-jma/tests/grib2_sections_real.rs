//! Every GRIB2 section value of the JMA polar radar format reaches the model
//! and the FM301 view.
//!
//! The two committed single-station tars (RS47773 Osaka, N5 reflectivity
//! and N6 radial velocity, 2019-10-12 09:00Z) are decoded, and every value
//! is read twice: from the model (root attributes, the WMO originating
//! centre, `radar_parameters`, each sweep's attributes and `prt`, each
//! field's attributes) and from the FM301 view with every passthrough item
//! (`Passthrough::All`). Both must equal what this test reads from the file
//! with its own tar and GRIB2 section walker and the octet table of the JMA
//! format document ("レーダー毎極座標レーダーエコー強度 GPV フォーマット",
//! GRIB2 Ver.2.00): section 0 octet 7, section 1 octets 6-12, 20 and 21,
//! grid definition template 3.50120, product definition template 4.51022
//! with its per-radial table, data representation template 5.200 and the
//! section 6 bitmap indicator. The observation start and end of each sweep
//! (template 4.51022 octets 51-54) are also the FM301 ray times and the
//! volume's time coverage, and a missing per-radial elevation is kept as the
//! per-ray `jma_pdt_radial_elevation_deg`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use recast_radar_core::fm301::{
    self, FirstDim, Flavor, Passthrough, Values, ViewOptions, VolumeView,
};
use recast_radar_core::model::{ArrayBuf, AttrValue, Scalar, Sweep, Volume};
use recast_radar_io_jma::read_jma_tar_volumes;

const ALL: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Sign and magnitude, all ones missing (format note on negative values).
fn sm16(raw: u16) -> Option<i16> {
    (raw != u16::MAX).then(|| {
        let magnitude = (raw & 0x7fff) as i16;
        if raw & 0x8000 != 0 {
            -magnitude
        } else {
            magnitude
        }
    })
}

fn sm32(raw: u32) -> Option<i32> {
    (raw != u32::MAX).then(|| {
        let magnitude = (raw & 0x7fff_ffff) as i32;
        if raw & 0x8000_0000 != 0 {
            -magnitude
        } else {
            magnitude
        }
    })
}

/// Section 1 octets 13-19: the GRIB2 reference time.
fn reference_time(identification: &[u8]) -> DateTime<Utc> {
    let [month, day, hour, minute, second] =
        [14, 15, 16, 17, 18].map(|at| u32::from(identification[at]));
    Utc.with_ymd_and_hms(
        i32::from(be16(identification, 12)),
        month,
        day,
        hour,
        minute,
        second,
    )
    .unwrap()
}

/// A text value of the FM301 view: a root variable such as
/// `time_coverage_start`.
fn view_text(view: &VolumeView<'_>, name: &str) -> String {
    match &view.root.variable(name).unwrap().values {
        Values::Text(text) => text.to_string(),
        _ => panic!("{name} is not text"),
    }
}

/// The GRIB2 message of the first member of the real tar `id`: the message
/// and its sections as `(number, bytes)`.
fn sections(id: &str) -> (Vec<u8>, Vec<(u8, Vec<u8>)>) {
    let tar = recast_radar_testdata::bytes(id).unwrap();
    let size = usize::from_str_radix(
        std::str::from_utf8(&tar[124..136])
            .unwrap()
            .trim_matches(|c: char| c == '\0' || c == ' '),
        8,
    )
    .unwrap();
    let message = tar[512..512 + size].to_vec();
    let mut out = Vec::new();
    let mut pos = 16;
    while &message[pos..pos + 4] != b"7777" {
        let length = be32(&message, pos) as usize;
        out.push((message[pos + 4], message[pos..pos + length].to_vec()));
        pos += length;
    }
    (message, out)
}

type Expected = Vec<(String, AttrValue)>;

fn u8a(name: &str, value: u8) -> (String, AttrValue) {
    (name.to_owned(), AttrValue::Scalar(Scalar::U8(value)))
}

/// The expected sweep attributes of one section 3 / section 4 pair.
fn sweep_expected(grid: &[u8], product: &[u8]) -> Expected {
    let mut out = vec![
        u8a("jma_gdt_source_of_grid_definition", grid[5]),
        u8a("jma_gdt_optional_list_octets", grid[10]),
        u8a("jma_gdt_optional_list_interpretation", grid[11]),
    ];
    for (name, at) in [
        ("jma_gdt_center_latitude_deg", 22),
        ("jma_gdt_center_longitude_deg", 26),
    ] {
        if let Some(value) = sm32(be32(grid, at)) {
            out.push((
                name.into(),
                AttrValue::Scalar(Scalar::F64(f64::from(value) / 1e6)),
            ));
        }
    }
    out.extend([
        (
            "jma_gdt_bin_spacing_mm".into(),
            AttrValue::Scalar(Scalar::U32(be32(grid, 30))),
        ),
        (
            "jma_gdt_first_bin_offset_mm".into(),
            AttrValue::Scalar(Scalar::U32(be32(grid, 34))),
        ),
        u8a("jma_gdt_scanning_mode", grid[38]),
        (
            "jma_gdt_start_azimuth_deg".into(),
            AttrValue::Scalar(Scalar::F32(f32::from(be16(grid, 39)) / 100.0)),
        ),
        u8a("jma_pdt_type_of_generating_process", product[11]),
        u8a("jma_pdt_number_of_radars", product[12]),
        u8a("jma_pdt_time_range_unit", product[13]),
    ]);
    if let Some(value) = sm16(be16(product, 30)) {
        out.push((
            "jma_pdt_magnetic_declination_deg".into(),
            AttrValue::Scalar(Scalar::F32(f32::from(value) / 100.0)),
        ));
    }
    if be32(product, 32) != u32::MAX {
        out.push((
            "jma_pdt_transmitted_frequency_khz".into(),
            AttrValue::Scalar(Scalar::U32(be32(product, 32))),
        ));
    }
    out.extend([
        u8a("jma_pdt_polarization", product[36]),
        u8a("jma_pdt_operation_mode", product[37]),
    ]);
    if product[38] != 255 {
        let magnitude = f32::from(product[38] & 0x7f) / 10.0;
        let value = if product[38] & 0x80 != 0 {
            -magnitude
        } else {
            magnitude
        };
        out.push((
            "jma_pdt_reflectivity_correction_db".into(),
            AttrValue::Scalar(Scalar::F32(value)),
        ));
    }
    out.extend([
        u8a("jma_pdt_quality_control_indicator", product[39]),
        u8a("jma_pdt_clutter_filter_indicator", product[40]),
    ]);
    if let Some(value) = sm16(be16(product, 41)) {
        out.push((
            "jma_pdt_antenna_elevation_setting_deg".into(),
            AttrValue::Scalar(Scalar::F32(f32::from(value) / 100.0)),
        ));
    }
    out.push(u8a("jma_pdt_number_of_prfs", product[43]));
    let prfs: Vec<f32> = [44, 46, 48]
        .into_iter()
        .map(|at| be16(product, at))
        .filter(|raw| *raw != u16::MAX)
        .map(|raw| f32::from(raw) / 10.0)
        .collect();
    if !prfs.is_empty() {
        out.push((
            "jma_pdt_representative_prf_hz".into(),
            AttrValue::Array(ArrayBuf::F32(prfs)),
        ));
    }
    for (name, at) in [
        ("jma_pdt_observation_start_offset_s", 50),
        ("jma_pdt_observation_end_offset_s", 52),
    ] {
        if let Some(value) = sm16(be16(product, at)) {
            out.push((name.into(), AttrValue::Scalar(Scalar::I16(value))));
        }
    }
    if product[54] != 255 {
        out.push(u8a(
            "jma_pdt_echo_top_reference_reflectivity_db",
            product[54],
        ));
    }
    let bins = u32::from_be_bytes([0, product[55], product[56], product[57]]);
    if bins != 0x00ff_ffff {
        out.push((
            "jma_pdt_range_bin_spacing_m".into(),
            AttrValue::Scalar(Scalar::U32(bins)),
        ));
    }
    if be16(product, 58) != u16::MAX {
        out.push((
            "jma_pdt_radial_spacing_deg".into(),
            AttrValue::Scalar(Scalar::F32(f32::from(be16(product, 58)) / 10.0)),
        ));
    }
    out
}

/// The expected field attributes of one section 4 / 5 / 6 group.
fn field_expected(product: &[u8], representation: &[u8], bitmap: &[u8]) -> Expected {
    let levels = be16(representation, 14);
    vec![
        u8a("jma_parameter_category", product[9]),
        u8a("jma_parameter_number", product[10]),
        u8a("jma_drt_bits_per_value", representation[11]),
        (
            "jma_drt_max_level_used".into(),
            AttrValue::Scalar(Scalar::U16(be16(representation, 12))),
        ),
        (
            "jma_drt_max_level".into(),
            AttrValue::Scalar(Scalar::U16(levels)),
        ),
        (
            "jma_drt_level_values".into(),
            AttrValue::Array(ArrayBuf::U16(
                (0..usize::from(levels))
                    .map(|i| be16(representation, 17 + 2 * i))
                    .collect(),
            )),
        ),
        u8a("jma_drt_decimal_scale_factor", representation[16]),
        u8a("jma_bitmap_indicator", bitmap[5]),
    ]
}

fn find<'a>(attrs: &'a [(Box<str>, AttrValue)], name: &str) -> Option<&'a AttrValue> {
    attrs
        .iter()
        .find(|(key, _)| &**key == name)
        .map(|(_, value)| value)
}

fn check_attrs(
    what: &str,
    model: &[(Box<str>, AttrValue)],
    viewed: impl Fn(&str) -> Option<AttrValue>,
    expected: &Expected,
    prefixes: &[&str],
) {
    for (name, value) in expected {
        assert_eq!(find(model, name), Some(value), "{what} {name}");
        assert_eq!(viewed(name).as_ref(), Some(value), "{what} {name} view");
    }
    // Nothing else under these prefixes: every carried value was checked.
    for (name, _) in model {
        if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            assert!(
                expected.iter().any(|(known, _)| known == &**name),
                "{what}: unchecked {name}"
            );
        }
    }
}

/// Model sweeps are sorted lowest elevation first; a section group is found
/// by its observation start offset, antenna elevation and field.
fn matching_sweep<'v>(
    volume: &'v Volume,
    expected: &Expected,
    parameter: u8,
) -> (usize, &'v Sweep) {
    volume
        .sweeps
        .iter()
        .enumerate()
        .find(|(_, sweep)| {
            expected.iter().all(|(name, value)| {
                !matches!(
                    name.as_str(),
                    "jma_pdt_observation_start_offset_s"
                        | "jma_pdt_observation_end_offset_s"
                        | "jma_pdt_antenna_elevation_setting_deg"
                        | "jma_gdt_start_azimuth_deg"
                ) || find(&sweep.other, name) == Some(value)
            }) && sweep.fields.iter().any(|field| {
                find(&field.attrs.other, "jma_parameter_number")
                    == Some(&AttrValue::Scalar(Scalar::U8(parameter)))
            })
        })
        .expect("a sweep for the section group")
}

/// The sweep count and the sweeps with a missing radial elevation.
fn check_tar(id: &str) -> (usize, usize) {
    let tar = recast_radar_testdata::bytes(id).unwrap();
    let volumes = read_jma_tar_volumes(&tar, None).unwrap();
    assert_eq!(volumes.len(), 1, "{id}");
    let volume = &volumes[0];
    let view: VolumeView<'_> = fm301::volume_view(volume, ALL, None).unwrap();
    let (message, sections) = sections(id);
    let identification = &sections.iter().find(|(n, _)| *n == 1).unwrap().1;

    // Sections 0 and 1.
    let root = vec![
        u8a("jma_grib2_discipline", message[6]),
        u8a("jma_grib2_edition", message[7]),
        u8a("jma_grib2_master_tables_version", identification[9]),
        u8a("jma_grib2_local_tables_version", identification[10]),
        u8a(
            "jma_grib2_significance_of_reference_time",
            identification[11],
        ),
        u8a("jma_grib2_production_status", identification[19]),
        u8a("jma_grib2_type_of_data", identification[20]),
        (
            "jma_grib2_reference_time".to_owned(),
            AttrValue::text(
                reference_time(identification)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string(),
            ),
        ),
    ];
    check_attrs(
        id,
        &volume.attrs.other,
        |name| view.root.attr(name).cloned(),
        &root,
        &["jma_"],
    );
    assert_eq!(
        volume.attrs.wmo.originating_centre,
        Some(be16(identification, 5))
    );
    assert_eq!(
        volume.attrs.wmo.originating_sub_centre,
        Some(be16(identification, 7))
    );
    assert_eq!(
        view.root.attr("wmo__originating_centre"),
        Some(&AttrValue::Scalar(Scalar::U16(be16(identification, 5))))
    );
    assert!(
        sections.iter().all(|(n, _)| *n != 2),
        "{id}: the format has no section 2"
    );
    assert!(
        volume.extra_vars.is_empty(),
        "{id}: no local use section to carry"
    );

    // Sections 3 to 7, one group per sweep.
    let mut grid: Option<&[u8]> = None;
    let mut product: Option<&[u8]> = None;
    let mut representation: Option<&[u8]> = None;
    let mut bitmap: Option<&[u8]> = None;
    let mut frequency: Option<u32> = None;
    let mut groups = 0;
    let mut with_prt = 0;
    let reference = reference_time(identification);
    // Observation start and end of every section group, from the reference.
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut missing_elevations = 0;
    for (number, bytes) in &sections {
        match number {
            3 => grid = Some(bytes),
            4 => product = Some(bytes),
            5 => representation = Some(bytes),
            6 => bitmap = Some(bytes),
            7 => {
                let (g, p) = (grid.unwrap(), product.unwrap());
                frequency.get_or_insert(be32(p, 32));
                let expected = sweep_expected(g, p);
                let (index, sweep) = matching_sweep(volume, &expected, p[10]);
                let group = view.group(&format!("sweep_{index}")).unwrap();
                let what = format!("{id} sweep {index}");
                check_attrs(
                    &what,
                    &sweep.other,
                    |name| group.attr(name).cloned(),
                    &expected,
                    &["jma_"],
                );
                let field = &sweep.fields[0];
                let expected_field = field_expected(p, representation.unwrap(), bitmap.unwrap());
                let variable = group.variable(field.name.as_str()).unwrap();
                check_attrs(
                    &what,
                    &field.attrs.other,
                    |name| variable.attr(name).cloned(),
                    &expected_field,
                    &["jma_"],
                );
                // Per-radial PRF (octets 63-64 + 4 (X - 1), 0.1 Hz) as the
                // FM301 `prt` in seconds.
                let radials = be32(g, 18) as usize;
                let expected_prt: Vec<f32> = (0..radials)
                    .map(|ray| match be16(p, 62 + 4 * ray) {
                        u16::MAX => f32::NAN,
                        raw => 1.0 / (f32::from(raw) / 10.0),
                    })
                    .collect();
                match sweep.ray_vars.prt_s.as_ref() {
                    Some(model) => {
                        assert_eq!(model.len(), radials, "{what}");
                        for (a, b) in model.iter().zip(&expected_prt) {
                            assert!(a.to_bits() == b.to_bits(), "{what}: prt {a} != {b}");
                        }
                        let viewed = group.variable("prt").unwrap().values.materialize().unwrap();
                        assert_eq!(viewed, ArrayBuf::F32(model.clone()), "{what}: view prt");
                        with_prt += 1;
                    }
                    // Every radial's PRF missing (all ones): no `prt`.
                    None => assert!(
                        expected_prt.iter().all(|prt| prt.is_nan()),
                        "{what}: prt dropped"
                    ),
                }
                // Octets 51-52 and 53-54: the sweep's observation start and
                // end, seconds from the reference time. Every ray carries
                // the start, in the model and in the view.
                let start = sm16(be16(p, 50)).map_or(0, i64::from);
                starts.push(reference + TimeDelta::seconds(start));
                if let Some(end) = sm16(be16(p, 52)) {
                    ends.push(reference + TimeDelta::seconds(i64::from(end)));
                }
                let expected_time = (reference + TimeDelta::seconds(start) - volume.time_reference)
                    .num_seconds() as f64;
                assert!(
                    sweep.rays.time_s.iter().all(|t| *t == expected_time),
                    "{what}: ray times {:?}, expected {expected_time}",
                    sweep.rays.time_s.first()
                );
                assert_eq!(
                    group
                        .variable("time")
                        .unwrap()
                        .values
                        .materialize()
                        .unwrap(),
                    ArrayBuf::F64(vec![expected_time; radials]),
                    "{what}: view time"
                );
                // Octets 61-62 + 4 (X - 1): each radial's elevation; where
                // it is missing the ray takes the antenna elevation (octets
                // 42-43) and the table is kept, NaN for the missing ones.
                let table: Vec<Option<f32>> = (0..radials)
                    .map(|ray| sm16(be16(p, 60 + 4 * ray)).map(|v| f32::from(v) / 100.0))
                    .collect();
                let antenna = sm16(be16(p, 41)).map_or(0.0, |v| f32::from(v) / 100.0);
                let elevations: Vec<f32> = table.iter().map(|e| e.unwrap_or(antenna)).collect();
                assert_eq!(sweep.rays.elevation_deg, elevations, "{what}: elevation");
                let carried = sweep
                    .extra_vars
                    .iter()
                    .find(|variable| &*variable.name == "jma_pdt_radial_elevation_deg");
                if table.iter().any(Option::is_none) {
                    let carried = carried.expect("the per-radial elevation table");
                    let expected: Vec<u32> = table
                        .iter()
                        .map(|e| e.unwrap_or(f32::NAN).to_bits())
                        .collect();
                    let ArrayBuf::F32(model) = &carried.values else {
                        panic!("{what}: {:?}", carried.values);
                    };
                    assert_eq!(
                        model.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                        expected,
                        "{what}: radial elevation table"
                    );
                    let viewed = group
                        .variable("jma_pdt_radial_elevation_deg")
                        .unwrap()
                        .values
                        .materialize()
                        .unwrap();
                    let ArrayBuf::F32(viewed) = viewed else {
                        panic!("{what}: view {viewed:?}");
                    };
                    assert_eq!(
                        viewed.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                        expected,
                        "{what}: view radial elevation table"
                    );
                    missing_elevations += 1;
                } else {
                    assert!(carried.is_none(), "{what}: table without a missing radial");
                }
                groups += 1;
            }
            _ => {}
        }
    }
    assert_eq!(groups, volume.sweeps.len(), "{id}: every sweep matched");
    // The time reference is the earliest observation start and the coverage
    // runs to the latest observation end.
    let first = *starts.iter().min().unwrap();
    let last = *ends.iter().chain(&starts).max().unwrap();
    assert_eq!(volume.time_reference, first, "{id}: time reference");
    let coverage = volume.time_coverage.unwrap();
    assert_eq!((coverage.start, coverage.end), (first, last), "{id}");
    let text = |time: DateTime<Utc>| time.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    assert_eq!(view_text(&view, "time_coverage_start"), text(first), "{id}");
    assert_eq!(view_text(&view, "time_coverage_end"), text(last), "{id}");
    assert!(
        first < reference && last < reference,
        "{id}: observed before the reference time"
    );

    assert!(
        with_prt * 2 > groups,
        "{id}: {with_prt} sweeps with a PRF table"
    );
    // Section 4 octets 33-36 (kHz) as the FM301 frequency.
    let khz = frequency.unwrap();
    assert_eq!(
        volume.radar_parameters.frequency_hz,
        vec![f64::from(khz) * 1e3],
        "{id}"
    );
    (groups, missing_elevations)
}

#[test]
fn every_grib2_section_value_reaches_the_model_and_the_view() {
    let n5 = check_tar("jma-n5-20191012-090000-rs47773");
    let n6 = check_tar("jma-n6-20191012-090000-rs47773");
    assert_eq!((n5.0, n6.0), (26, 13));
    // The velocity member's lowest sweep has no per-radial elevations.
    assert!(n5.1 + n6.1 > 0, "no sweep lacks a radial elevation");
}

/// The tar header and GRIB2 message of the single member of the committed
/// tar `id`.
fn member(id: &str) -> (Vec<u8>, Vec<u8>) {
    let tar = recast_radar_testdata::bytes(id).unwrap();
    let (message, _) = sections(id);
    (tar[..512].to_vec(), message)
}

/// `message` with a section 2 (local use) holding `payload` inserted after
/// section 1, and section 0's total length updated.
fn with_local_use(message: &[u8], payload: &[u8]) -> Vec<u8> {
    let section_1_end = 16 + be32(message, 16) as usize;
    let length = u32::try_from(5 + payload.len()).unwrap();
    let mut out = message[..section_1_end].to_vec();
    out.extend_from_slice(&length.to_be_bytes());
    out.push(2);
    out.extend_from_slice(payload);
    out.extend_from_slice(&message[section_1_end..]);
    let total = u64::try_from(out.len()).unwrap();
    out[8..16].copy_from_slice(&total.to_be_bytes());
    out
}

/// A ustar archive of `members` (a real member header with its size field
/// rewritten, the message, padding), then the two end-of-archive blocks.
fn tar_of(members: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (header, message) in members {
        let mut header = header.clone();
        header[124..136].copy_from_slice(format!("{:011o}\0", message.len()).as_bytes());
        tar.extend_from_slice(&header);
        tar.extend_from_slice(message);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    tar
}

/// The two committed RS47773 members in one archive merge into one TAKA
/// volume (26 reflectivity then 13 velocity sweeps). Their sections 0 and 1
/// hold the same values (read from the bytes here), so the volume's
/// attributes are them and no sweep carries its own.
///
/// No real JMA message has a section 2 and no real station's members
/// differ in sections 0 and 1, so the two real messages are then given
/// what the merge must keep apart: a section 2 each and, in the velocity
/// member, another production status (section 1 octet 20). The velocity
/// sweeps then carry that member's section 0 and 1 values, and its local
/// use section is `jma_grib2_local_use_0_member1`, on its own dimension.
#[test]
fn merged_members_keep_each_members_values() {
    let reflectivity = member("jma-n5-20191012-090000-rs47773");
    let velocity = member("jma-n6-20191012-090000-rs47773");
    let identification = |message: &[u8]| message[16..16 + be32(message, 16) as usize].to_vec();
    assert_eq!(reflectivity.1[6], velocity.1[6], "section 0 discipline");
    assert_eq!(identification(&reflectivity.1), identification(&velocity.1));

    let volumes =
        read_jma_tar_volumes(&tar_of(&[reflectivity.clone(), velocity.clone()]), None).unwrap();
    assert_eq!(volumes.len(), 1);
    let volume = &volumes[0];
    assert_eq!(volume.sweeps.len(), 26 + 13);
    let root_names = [
        "jma_grib2_discipline",
        "jma_grib2_edition",
        "jma_grib2_master_tables_version",
        "jma_grib2_local_tables_version",
        "jma_grib2_significance_of_reference_time",
        "jma_grib2_production_status",
        "jma_grib2_type_of_data",
    ];
    let section_1 = identification(&reflectivity.1);
    assert_eq!(
        find(&volume.attrs.other, "jma_grib2_production_status"),
        Some(&AttrValue::Scalar(Scalar::U8(section_1[19])))
    );
    for sweep in &volume.sweeps {
        for name in root_names {
            assert!(find(&sweep.other, name).is_none(), "{name} on a sweep");
        }
    }
    assert!(volume.extra_vars.is_empty());

    // Members that differ.
    let mut changed = with_local_use(&velocity.1, b"velocity member local use");
    let status_at = 16 + 19;
    let status = changed[status_at] ^ 1;
    changed[status_at] = status;
    let first = with_local_use(&reflectivity.1, b"reflectivity");
    let volumes = read_jma_tar_volumes(
        &tar_of(&[
            (reflectivity.0.clone(), first),
            (velocity.0.clone(), changed.clone()),
        ]),
        None,
    )
    .unwrap();
    assert_eq!(volumes.len(), 1);
    let volume = &volumes[0];
    let view = fm301::volume_view(volume, ALL, None).unwrap();
    assert_eq!(
        find(&volume.attrs.other, "jma_grib2_production_status"),
        Some(&AttrValue::Scalar(Scalar::U8(section_1[19])))
    );
    let velocity_sweeps: Vec<usize> = (0..volume.sweeps.len())
        .filter(|&index| {
            volume.sweeps[index]
                .fields
                .iter()
                .any(|field| field.name.as_str() == "VRADH")
        })
        .collect();
    assert_eq!(velocity_sweeps.len(), 13);
    let changed_section_1 = identification(&changed);
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        let group = view.group(&format!("sweep_{index}")).unwrap();
        let carried = find(&sweep.other, "jma_grib2_production_status");
        if velocity_sweeps.contains(&index) {
            let expected = AttrValue::Scalar(Scalar::U8(changed_section_1[19]));
            assert_eq!(carried, Some(&expected), "sweep {index}");
            assert_eq!(group.attr("jma_grib2_production_status"), Some(&expected));
            assert_eq!(
                find(&sweep.other, "jma_grib2_type_of_data"),
                Some(&AttrValue::Scalar(Scalar::U8(changed_section_1[20])))
            );
            assert_eq!(
                find(&sweep.other, "wmo__originating_centre"),
                Some(&AttrValue::Scalar(Scalar::U16(be16(&changed_section_1, 5))))
            );
        } else {
            assert_eq!(carried, None, "sweep {index}");
        }
    }
    let local_use = |name: &str| {
        let model = volume
            .extra_vars
            .iter()
            .find(|variable| &*variable.name == name)
            .unwrap_or_else(|| panic!("{name}"));
        let viewed = view.root.variable(name).unwrap();
        assert_eq!(
            viewed.values.materialize().unwrap(),
            model.values,
            "{name} view"
        );
        (model.dims.clone(), model.values.clone())
    };
    let (dims, values) = local_use("jma_grib2_local_use_0");
    assert_eq!(values, ArrayBuf::U8(b"reflectivity".to_vec()));
    let (member_dims, member_values) = local_use("jma_grib2_local_use_0_member1");
    assert_eq!(
        member_values,
        ArrayBuf::U8(b"velocity member local use".to_vec())
    );
    assert_ne!(
        dims, member_dims,
        "each local use section has its own dimension"
    );
}
