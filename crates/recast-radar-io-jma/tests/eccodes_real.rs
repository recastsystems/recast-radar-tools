//! The JMA decoder against ecCodes, an independent GRIB2 reader.
//!
//! `testdata/golden/jma/eccodes.json` (`tools/jma_eccodes_golden.py`, ecCodes
//! 2.48.0) holds what ecCodes decodes from every field of the two
//! single-station tars (RS47773 Osaka, N5 reflectivity with 26 sweeps and N6
//! radial velocity with 13; not redistributed, so each is checked only when
//! it is in the testdata cache): the section 0 and 1 keys, the section 3 header
//! keys, the data representation template 5.200 keys with the level values,
//! the bitmap indicator, and a summary of the decoded values. ecCodes has no
//! definition for JMA's local templates 3.50120 and 4.51022; the script gives
//! it a placeholder that reads nothing, so none of those template bodies is
//! compared here (`grib2_sections_real.rs` reads them from the bytes).
//!
//! Each ecCodes field must match one sweep of the decoded volume, each sweep
//! at most once: its data (point count, missing points, the histogram of
//! stored level values and their index-weighted sum) and every compared key.
//! The decoder's value is turned back into the stored level value as GRIB2
//! stores it: round(value * 10^D), a negative one as its magnitude with the
//! top bit set, which is how ecCodes reads an unsigned level value.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use chrono::{Datelike, Timelike};
use recast_radar_core::model::{ArrayBuf, AttrValue, FieldData, Scalar, Sweep, Volume};
use recast_radar_io_jma::read_jma_tar_volumes;
use serde_json::Value;

fn golden() -> Value {
    let path = recast_radar_testdata::testdata_dir().join("golden/jma/eccodes.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn find<'a>(attrs: &'a [(Box<str>, AttrValue)], name: &str) -> Option<&'a AttrValue> {
    attrs.iter().find(|(k, _)| &**k == name).map(|(_, v)| v)
}

fn number(value: Option<&AttrValue>) -> Option<i64> {
    match value? {
        AttrValue::Scalar(Scalar::U8(v)) => Some(i64::from(*v)),
        AttrValue::Scalar(Scalar::U16(v)) => Some(i64::from(*v)),
        AttrValue::Scalar(Scalar::U32(v)) => Some(i64::from(*v)),
        _ => None,
    }
}

fn int(value: &Value) -> i64 {
    value.as_i64().unwrap()
}

/// The data summary of a sweep's one field, in the golden's terms.
#[derive(Debug, PartialEq)]
struct Summary {
    count: usize,
    missing: usize,
    histogram: BTreeMap<u32, u64>,
    weighted_sum: u128,
}

fn summary(sweep: &Sweep, decimal_scale: i32) -> Summary {
    assert_eq!(sweep.fields.len(), 1);
    let FieldData::F32 { values, .. } = &sweep.fields[0].data else {
        panic!("JMA fields are f32")
    };
    let scale = 10f64.powi(decimal_scale);
    let mut histogram = BTreeMap::new();
    let mut weighted_sum = 0u128;
    let mut missing = 0;
    for (index, value) in values.iter().enumerate() {
        if value.is_nan() {
            missing += 1;
            continue;
        }
        let scaled = (f64::from(*value) * scale).round() as i64;
        let stored = if scaled < 0 {
            0x8000 | u32::try_from(-scaled).unwrap()
        } else {
            u32::try_from(scaled).unwrap()
        };
        *histogram.entry(stored).or_insert(0) += 1;
        weighted_sum += (index as u128 + 1) * u128::from(stored);
    }
    Summary {
        count: values.len(),
        missing,
        histogram,
        weighted_sum,
    }
}

fn golden_summary(values: &Value) -> Summary {
    Summary {
        count: values["count"].as_u64().unwrap() as usize,
        missing: values["missing"].as_u64().unwrap() as usize,
        histogram: values["histogram"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| (pair[0].as_u64().unwrap() as u32, pair[1].as_u64().unwrap()))
            .collect(),
        weighted_sum: values["weighted_sum"].as_str().unwrap().parse().unwrap(),
    }
}

/// Every compared key of one ecCodes field against one sweep; `Err` names
/// the first difference.
fn matches(sweep: &Sweep, field: &Value) -> Result<(), String> {
    let keys = &field["field"];
    let attrs = &sweep.fields[0].attrs.other;
    let sweep_keys = [
        (
            "jma_gdt_source_of_grid_definition",
            "sourceOfGridDefinition",
        ),
        (
            "jma_gdt_optional_list_octets",
            "numberOfOctectsForNumberOfPoints",
        ),
        (
            "jma_gdt_optional_list_interpretation",
            "interpretationOfNumberOfPoints",
        ),
    ];
    for (name, key) in sweep_keys {
        if number(find(&sweep.other, name)) != Some(int(&keys[key])) {
            return Err(format!("{name} != {key}"));
        }
    }
    let field_keys = [
        ("jma_drt_bits_per_value", "bitsPerValue"),
        ("jma_drt_max_level_used", "maxLevelValue"),
        ("jma_drt_max_level", "numberOfLevelValues"),
        ("jma_drt_decimal_scale_factor", "decimalScaleFactor"),
        ("jma_bitmap_indicator", "bitMapIndicator"),
    ];
    for (name, key) in field_keys {
        if number(find(attrs, name)) != Some(int(&keys[key])) {
            return Err(format!("{name} != {key}"));
        }
    }
    let levels: Vec<u16> = keys["levelValues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| u16::try_from(int(v)).unwrap())
        .collect();
    if find(attrs, "jma_drt_level_values") != Some(&AttrValue::Array(ArrayBuf::U16(levels))) {
        return Err("jma_drt_level_values != levelValues".into());
    }
    let points = sweep.nrays() * sweep.fields[0].ngates as usize;
    if points as i64 != int(&keys["numberOfDataPoints"])
        || points as i64 != int(&keys["numberOfValues"])
    {
        return Err("point count".into());
    }
    let scale = i32::try_from(int(&keys["decimalScaleFactor"])).unwrap();
    let expected = golden_summary(&field["values"]);
    let actual = summary(sweep, scale);
    if actual != expected {
        return Err(format!(
            "data: {} points {} missing, golden {} points {} missing",
            actual.count, actual.missing, expected.count, expected.missing
        ));
    }
    Ok(())
}

fn check_root(id: &str, volume: &Volume, message: &Value) {
    let other = &volume.attrs.other;
    for (name, key) in [
        ("jma_grib2_discipline", "discipline"),
        ("jma_grib2_edition", "editionNumber"),
        ("jma_grib2_master_tables_version", "tablesVersion"),
        ("jma_grib2_local_tables_version", "localTablesVersion"),
        (
            "jma_grib2_significance_of_reference_time",
            "significanceOfReferenceTime",
        ),
        (
            "jma_grib2_production_status",
            "productionStatusOfProcessedData",
        ),
        ("jma_grib2_type_of_data", "typeOfProcessedData"),
    ] {
        assert_eq!(
            number(find(other, name)),
            Some(int(&message[key])),
            "{id}: {name} vs ecCodes {key}"
        );
    }
    let wmo = &volume.attrs.wmo;
    assert_eq!(
        wmo.originating_centre.map(i64::from),
        Some(int(&message["centre"])),
        "{id}"
    );
    assert_eq!(
        wmo.originating_sub_centre.map(i64::from),
        Some(int(&message["subCentre"])),
        "{id}"
    );
    // The GRIB2 reference time is kept as the root attribute
    // `jma_grib2_reference_time`; the volume time reference is the earliest
    // sweep observation start before it (grib2_sections_real.rs).
    let Some(AttrValue::Text(text)) = find(&volume.attrs.other, "jma_grib2_reference_time") else {
        panic!("{id}: no jma_grib2_reference_time");
    };
    let time = chrono::DateTime::parse_from_rfc3339(text).unwrap();
    assert_eq!(
        [
            i64::from(time.year()),
            i64::from(time.month()),
            i64::from(time.day()),
            i64::from(time.hour()),
            i64::from(time.minute()),
            i64::from(time.second()),
        ],
        ["year", "month", "day", "hour", "minute", "second"].map(|key| int(&message[key])),
        "{id}: reference time"
    );
    assert!(volume.time_reference <= time, "{id}: time reference");
    // No section 2 in either message, and none in the model.
    assert_eq!(int(&message["grib2LocalSectionPresent"]), 0);
    assert!(
        !volume
            .extra_vars
            .iter()
            .any(|v| v.name.starts_with("jma_grib2_local_use")),
        "{id}"
    );
}

#[test]
fn every_field_matches_eccodes() {
    let golden = golden();
    assert_eq!(golden["eccodes"], "2.48.0");
    let files = golden["files"].as_object().unwrap();
    let mut fields_checked = 0;
    let mut skipped = 0;
    for (id, file) in files {
        let Some(tar) = recast_radar_testdata::bytes_if_available(id) else {
            skipped += 1;
            continue;
        };
        let volumes = read_jma_tar_volumes(&tar, None).unwrap();
        assert_eq!(volumes.len(), 1, "{id}");
        let volume = &volumes[0];
        let fields = file["fields"].as_array().unwrap();
        assert_eq!(volume.sweeps.len(), fields.len(), "{id}: sweeps");
        for field in fields {
            check_root(id, volume, &field["message"]);
            assert_eq!(int(&field["field"]["gridDefinitionTemplateNumber"]), 50120);
            assert_eq!(
                int(&field["field"]["productDefinitionTemplateNumber"]),
                51022
            );
            assert_eq!(
                int(&field["field"]["dataRepresentationTemplateNumber"]),
                200
            );
            assert_eq!(int(&field["field"]["NV"]), 0);
        }
        // Each ecCodes field (in message order) is one sweep (sorted by
        // elevation in the model), each sweep at most once.
        let mut used = vec![false; volume.sweeps.len()];
        for (number, field) in fields.iter().enumerate() {
            let mut reasons = Vec::new();
            let found = volume.sweeps.iter().enumerate().find(|(index, sweep)| {
                if used[*index] {
                    return false;
                }
                match matches(sweep, field) {
                    Ok(()) => true,
                    Err(reason) => {
                        reasons.push(format!("sweep {index}: {reason}"));
                        false
                    }
                }
            });
            let (index, _) = found.unwrap_or_else(|| {
                panic!("{id}: ecCodes field {number} matches no sweep: {reasons:?}")
            });
            used[index] = true;
            fields_checked += 1;
        }
    }
    // The total is that of both tars.
    if skipped == 0 {
        assert_eq!(fields_checked, 26 + 13);
    }
}

/// JMA's local grid template 3.50120 follows the WMO azimuth-range template
/// 3.120 octet for octet up to the scanning mode, and the decoder reads the
/// octets ecCodes' own definitions name here: bins along radials (15),
/// radials (19), bin spacing (31) and, at octets 35-38, the offset from the
/// origin to the inner bound of the first bin, so the decoder places the
/// first gate's centre half a spacing beyond it (`grib2_sections_real.rs`
/// checks every sweep's range against the octets).
#[test]
fn grid_octets_are_those_of_wmo_template_3_120() {
    let golden = golden();
    let octets = golden["wmo_grid_template_3_120_octets"]
        .as_object()
        .unwrap();
    let expected = [
        ("15", "numberOfDataBinsAlongRadials"),
        ("19", "numberOfRadials"),
        ("23", "latitudeOfCentrePoint"),
        ("27", "longitudeOfCentrePoint"),
        ("31", "spacingOfBinsAlongRadials"),
        ("35", "offsetFromOriginToInnerBound"),
        ("39", "scanningMode"),
    ];
    assert_eq!(octets.len(), expected.len());
    for (octet, key) in expected {
        assert_eq!(octets[octet].as_str(), Some(key), "octet {octet}");
    }
}
