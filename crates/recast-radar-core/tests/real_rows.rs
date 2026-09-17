//! Field row layouts that no corpus file has, built by mutating real Level II
//! records (plan task C.2: edge and corruption cases mutate real file bytes).
//!
//! Input: `l2-ktlx-20130520-201643-trim`, decompressed to its uncompressed
//! record stream. Its surveillance cut (elevation number 1) carries REF, ZDR,
//! RHO and 16-bit PHI; its Doppler cut (elevation number 2) REF, VEL and SW.
//! Each test edits a few bytes of real Message 31 radials (a data block
//! pointer, a gate count or a word size, ICD 2620002 Tables XVII-A and
//! XVII-B) and compares the decoded volume with the unmodified one.

mod common;

use common::{field, level2_bytes, raw_code};
use recast_radar_core::model::FieldError;
use recast_radar_core::{Field, FieldData, FieldName, Gate, GateMapping, IntCoding, Volume};
use recast_radar_io_nexrad::messages::{RawMessages, volume_header_len};
use recast_radar_io_nexrad::{normalize_archive_bytes, parse_message_31_header};

/// Length of the Archive II message header before a message body.
const MESSAGE_HEADER_LEN: usize = 16;

/// The uncompressed record stream of the KTLX 2013 trim and the offset of
/// every Message 31 body in it, in file order.
fn records() -> (Vec<u8>, Vec<usize>) {
    // A committed fixture: available offline.
    let path = recast_radar_testdata::path("l2-ktlx-20130520-201643-trim")
        .unwrap_or_else(|error| panic!("{error}"));
    let raw = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let (bytes, _) = normalize_archive_bytes(&raw).expect("decompress");
    let header = volume_header_len(&bytes);
    let bodies = RawMessages::new(&bytes[header..])
        .filter_map(Result::ok)
        .filter(|message| message.header.message_type == 31)
        .map(|message| header + message.offset + MESSAGE_HEADER_LEN)
        .collect::<Vec<_>>();
    for body in &bodies {
        let radial = parse_message_31_header(&bytes, *body).expect("message 31 header");
        assert_eq!(&radial.radar_identifier, b"KTLX");
    }
    (bytes, bodies)
}

/// The radials of one elevation number, in file order.
fn radials(bytes: &[u8], bodies: &[usize], elevation_number: u8) -> Vec<usize> {
    bodies
        .iter()
        .copied()
        .filter(|body| {
            parse_message_31_header(bytes, *body)
                .expect("message 31 header")
                .elevation_number
                == elevation_number
        })
        .collect()
}

/// Offset of the block pointer that points at data block `name` of a radial,
/// and the block's offset.
fn block(bytes: &[u8], body: usize, name: &[u8; 3]) -> (usize, usize) {
    let radial = parse_message_31_header(bytes, body).expect("message 31 header");
    radial
        .block_pointers
        .iter()
        .enumerate()
        .find(|(_, pointer)| {
            **pointer != 0
                && bytes[body + **pointer] == b'D'
                && &bytes[body + **pointer + 1..body + **pointer + 4] == name
        })
        .map(|(slot, pointer)| (body + 32 + slot * 4, body + *pointer))
        .unwrap_or_else(|| panic!("radial at {body} has no {name:?} block"))
}

/// Every raw code of `row`.
fn row_codes(field: &Field, row: usize) -> Vec<u16> {
    (0..field.ngates as usize)
        .map(|gate| raw_code(field, row, gate))
        .collect()
}

/// Every field of `mutated` other than `name` in sweep `sweep` equals the
/// field of the same name in `original` (fields are listed in the order the
/// radials first carry them, which a mutation can change).
fn assert_same_fields_except(original: &Volume, mutated: &Volume, sweep: usize, name: &FieldName) {
    for (a, b) in original.sweeps.iter().zip(&mutated.sweeps) {
        assert_eq!(a.nrays(), b.nrays());
        assert_eq!(a.fields.len(), b.fields.len());
        for fa in &a.fields {
            if a.sweep_number as usize == sweep && &fa.name == name {
                continue;
            }
            let fb = field(b, &fa.name);
            assert_eq!(fa.ngates, fb.ngates, "{}", fa.name);
            assert_eq!(fa.absent_rows, fb.absent_rows, "{}", fa.name);
            for row in 0..fa.nrays as usize {
                assert_eq!(
                    row_codes(fa, row),
                    row_codes(fb, row),
                    "{} row {row}",
                    fa.name
                );
            }
        }
    }
}

/// Radials whose VEL block pointer is zeroed carry no velocity: their rows are
/// absent (`Gate::Missing`), including a trailing one, and every other row and
/// field decodes as before.
#[test]
fn radials_without_a_moment_block_become_absent_rows() {
    let (mut bytes, bodies) = records();
    let original = level2_bytes(&bytes);
    let doppler = radials(&bytes, &bodies, 2);
    let dropped = [0, 5, doppler.len() - 1];
    for row in dropped {
        let (pointer, _) = block(&bytes, doppler[row], b"VEL");
        bytes[pointer..pointer + 4].fill(0);
    }
    let mutated = level2_bytes(&bytes);

    let before = field(&original.sweeps[1], &FieldName::Vradh);
    let after = field(&mutated.sweeps[1], &FieldName::Vradh);
    assert!(before.absent_rows.is_empty());
    assert_eq!(after.absent_rows, dropped.map(|row| row as u32).to_vec());
    assert_eq!(after.nrays, before.nrays);
    assert_eq!(after.ngates, before.ngates);
    for row in 0..before.nrays as usize {
        if dropped.contains(&row) {
            assert_eq!(after.gate(row, 0), Some(Gate::Missing), "row {row}");
        } else {
            assert_eq!(row_codes(after, row), row_codes(before, row), "row {row}");
        }
    }
    assert_same_fields_except(&original, &mutated, 1, &FieldName::Vradh);
}

/// A first radial whose REF block declares fewer gates than the later ones:
/// the field widens to the longer rows, the short row is padded with the fill
/// code, and its native gates keep their codes.
#[test]
fn a_short_first_row_is_padded_when_later_rows_are_longer() {
    let (mut bytes, bodies) = records();
    let original = level2_bytes(&bytes);
    let surveillance = radials(&bytes, &bodies, 1);
    let (_, reflectivity) = block(&bytes, surveillance[0], b"REF");
    let short = 100u16;
    bytes[reflectivity + 8..reflectivity + 10].copy_from_slice(&short.to_be_bytes());
    let mutated = level2_bytes(&bytes);

    let before = field(&original.sweeps[0], &FieldName::Dbzh);
    let after = field(&mutated.sweeps[0], &FieldName::Dbzh);
    assert!(before.ngates > u32::from(short));
    assert_eq!(after.ngates, before.ngates);
    assert_eq!(after.gates, before.gates);
    assert!(after.absent_rows.is_empty());
    let (original_row, padded_row) = (row_codes(before, 0), row_codes(after, 0));
    let short = usize::from(short);
    assert_eq!(padded_row[..short], original_row[..short]);
    assert!(padded_row[short..].iter().all(|code| *code == 0));
    assert!(original_row[short..].iter().any(|code| *code != 0));
    for row in 1..before.nrays as usize {
        assert_eq!(row_codes(after, row), row_codes(before, row), "row {row}");
    }
    assert_same_fields_except(&original, &mutated, 0, &FieldName::Dbzh);
}

/// A radial whose 16-bit PHI block is relabelled 8-bit no longer fits the
/// field's storage: the decode fails and names the field.
#[test]
fn a_moment_that_changes_word_size_is_rejected() {
    let (mut bytes, bodies) = records();
    let surveillance = radials(&bytes, &bodies, 1);
    let (_, phase) = block(&bytes, surveillance[3], b"PHI");
    assert_eq!(bytes[phase + 19], 16);
    bytes[phase + 19] = 8;
    let error = recast_radar_io_nexrad::read_volume_from_bytes(&bytes)
        .expect_err("a u8 row cannot join a u16 field")
        .to_string();
    let expected = FieldError::StorageMismatch {
        expected: "uint16",
        actual: "u8",
    }
    .to_string();
    assert!(
        error.contains("PHIDP") && error.contains(&expected),
        "{error}"
    );
}

/// The big-endian row API on a real PHI block: the block's gate bytes decode
/// to the codes the reader stored, the same bytes cut by one byte are refused,
/// and a second row for the same ray is refused.
#[test]
fn u16_rows_decode_real_big_endian_bytes_and_reject_odd_lengths() {
    let (bytes, bodies) = records();
    let volume = level2_bytes(&bytes);
    let decoded = field(&volume.sweeps[0], &FieldName::Phidp);
    let surveillance = radials(&bytes, &bodies, 1);
    let (_, phase) = block(&bytes, surveillance[0], b"PHI");
    let gates = usize::from(u16::from_be_bytes([bytes[phase + 8], bytes[phase + 9]]));
    let scale = f32::from_be_bytes(bytes[phase + 20..phase + 24].try_into().unwrap());
    let offset = f32::from_be_bytes(bytes[phase + 24..phase + 28].try_into().unwrap());
    // The generic data moment block header is 28 bytes; the gates follow.
    let payload = &bytes[phase + 28..phase + 28 + 2 * gates];

    let mut row = Field::new(
        FieldName::Phidp,
        GateMapping::IDENTITY,
        u32::try_from(gates).unwrap(),
        FieldData::U16 {
            values: Vec::new(),
            coding: IntCoding::nexrad(scale, offset),
        },
    );
    row.push_row_u16_be(0, payload).unwrap();
    assert_eq!(row_codes(&row, 0), row_codes(decoded, 0));
    assert_eq!(
        row.push_row_u16_be(1, &payload[..payload.len() - 1]),
        Err(FieldError::InvalidRowByteLength {
            byte_len: payload.len() - 1
        })
    );
    assert_eq!(
        row.push_row_u16_be(0, payload),
        Err(FieldError::RowOrder { ray: 0, rows: 1 })
    );
    assert_eq!(row.nrays, 1);
}
