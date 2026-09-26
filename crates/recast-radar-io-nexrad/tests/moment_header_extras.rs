//! The Message 31 moment-header values FM301 has no field for reach the
//! model as source attributes of the field.
//!
//! TOVER (Table XVII-B bytes 14-15, 0.1 dB), the SNR threshold (bytes 16-17,
//! 0.125 dB) and the recombination code (byte 18) are read by
//! `messages::msg31_blocks` and were dropped when a volume was built. They
//! are now written into `FieldAttrs::other` by the radial that creates the
//! field. The expected values are not constants here: the test walks the same
//! bytes with the message decoder and compares the attributes against the
//! moment headers those radials carry.

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use recast_radar_core::model::{AttrValue, FieldName};
use recast_radar_io_nexrad::messages::msg31_blocks::DigitalRadarDataGeneric;
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::read_volume_from_bytes;

/// The three attributes as a comparable triple: `(TOVER dB, SNR dB,
/// recombination code)`, each scaled to thousandths so floats compare exactly.
type Triple = (i64, i64, i64);

fn triple(tover_db: f32, snr_db: f32, recombination: u8) -> Triple {
    (
        (f64::from(tover_db) * 1000.0).round() as i64,
        (f64::from(snr_db) * 1000.0).round() as i64,
        i64::from(recombination),
    )
}

/// A field's three attributes, or `None` when it carries none of them.
fn field_triple(other: &[(Box<str>, AttrValue)]) -> Option<Triple> {
    let value = |key: &str| -> Option<f64> {
        other
            .iter()
            .find(|(name, _)| &**name == key)
            .map(|(_, value)| match value {
                AttrValue::Scalar(scalar) => scalar.as_f64(),
                other => panic!("{key} is not a scalar: {other:?}"),
            })
    };
    let tover = value("nexrad_tover_db");
    let snr = value("nexrad_snr_threshold_db");
    let recombination = value("nexrad_recombination");
    match (tover, snr, recombination) {
        (Some(tover), Some(snr), Some(recombination)) => Some((
            (tover * 1000.0).round() as i64,
            (snr * 1000.0).round() as i64,
            recombination as i64,
        )),
        (None, None, None) => None,
        _ => panic!("a field carries only some of the moment-header attributes"),
    }
}

/// The first radial's triple per `(elevation number, field name)`.
type FirstTriples = BTreeMap<(u8, FieldName), Triple>;
/// Every triple of each field.
type AllTriples = BTreeMap<FieldName, BTreeSet<Triple>>;

/// From the message stream: the first radial of each elevation number gives
/// `(elevation number, field name) -> triple`, and every radial contributes to
/// the per-field set of triples the file contains.
fn headers_from_messages(bytes: &[u8]) -> (FirstTriples, AllTriples) {
    let mut first = FirstTriples::new();
    let mut all = AllTriples::new();
    let records = messages::record_bytes(bytes).expect("decompress the records");
    for item in MessageWalker::new(&records) {
        let Ok((_, MessageBody::DigitalRadarDataGeneric(radial))) = item else {
            continue;
        };
        let radial: DigitalRadarDataGeneric<'_> = *radial;
        let elevation = radial.header.elevation_number;
        for block in &radial.moments {
            let value = triple(
                block.tover_db(),
                block.snr_threshold_db(),
                block.control_flags.code(),
            );
            let name = block.field_name();
            first.entry((elevation, name.clone())).or_insert(value);
            all.entry(name).or_default().insert(value);
        }
    }
    (first, all)
}

/// The committed real-time chunks of the KIWA 2026-09-17 00:36Z volume, so
/// the test runs offline. Build 24.1, VCP 215 with SAILS, CFP present.
fn kiwa_chunk_ids() -> Vec<String> {
    (1..=70)
        .map(|n| {
            let suffix = match n {
                1 => "s",
                70 => "e",
                _ => "i",
            };
            format!("l2chunk-kiwa-307-20260917-003629-{n:03}-{suffix}")
        })
        .collect()
}

#[test]
fn message_31_moment_header_extras_reach_the_field_attributes() {
    let ids = kiwa_chunk_ids();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let Some(bytes) = common::load_all(&refs) else {
        return;
    };

    let volume = read_volume_from_bytes(&bytes).expect("decode the volume");
    let (first, all) = headers_from_messages(&bytes);
    assert!(!first.is_empty(), "no message 31 radials in the file");

    // The first sweep is elevation number 1, so its fields compare against
    // exactly the headers that opened that cut.
    let opening = volume.sweeps.first().expect("at least one sweep");
    let mut exact = 0usize;
    for field in &opening.fields {
        let expected = first
            .get(&(1, field.name.clone()))
            .unwrap_or_else(|| panic!("{:?} has no moment header in cut 1", field.name));
        assert_eq!(
            field_triple(&field.attrs.other).as_ref(),
            Some(expected),
            "{:?} in cut 1",
            field.name
        );
        exact += 1;
    }
    assert!(exact >= 4, "cut 1 has only {exact} fields");

    // Every other sweep's values must be ones this file actually contains
    // for that moment.
    let mut checked = 0usize;
    for sweep in &volume.sweeps {
        for field in &sweep.fields {
            let value = field_triple(&field.attrs.other)
                .unwrap_or_else(|| panic!("{:?} carries no moment-header attributes", field.name));
            let seen = all
                .get(&field.name)
                .unwrap_or_else(|| panic!("{:?} never appears in the message stream", field.name));
            assert!(
                seen.contains(&value),
                "{:?}: {value:?} is not a header value in this file",
                field.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} fields checked");

    // Byte 18 is a Table XVII-B code; anything else would mean the attribute
    // is reading the wrong byte.
    for values in all.values() {
        for (_, _, code) in values {
            assert!((0..=3).contains(code), "unknown recombination code {code}");
        }
    }
}

#[test]
fn message_1_volumes_carry_no_moment_header_extras() {
    // The legacy Message 1 moment header has no TOVER, SNR threshold or
    // recombination code, so the attributes must be absent rather than zero.
    let Some(bytes) = common::load("l2-ktlx-20030508-221041") else {
        return;
    };
    let volume = read_volume_from_bytes(&bytes).expect("decode the volume");
    let mut fields = 0usize;
    for sweep in &volume.sweeps {
        for field in &sweep.fields {
            assert_eq!(
                field_triple(&field.attrs.other),
                None,
                "{:?} has message 31 attributes on a message 1 volume",
                field.name
            );
            fields += 1;
        }
    }
    assert!(fields > 0, "no fields decoded");
}
