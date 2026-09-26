//! Inputs the volume decoders must not turn into a volume: bytes without an
//! Archive II volume header, and non-radial messages inside a volume stream.
//!
//! Real files:
//!
//! - `l2-klix-20210829-175748-mdm`: the KLIX 2021-08-29 model-data (`_MDM`)
//!   file. No volume header; one bzip2 LDM record holding a single Message 29
//!   of 809 229 bytes with the 0xFFFF extended-size convention. Py-ART 2.2.5
//!   raises `OSError: unknown compression record`; MetPy 1.7.1 logs "Unable
//!   to read volume header" and returns a `Level2File` with 0 sweeps and no
//!   `stid` or `dt`. Before wave 3, `read_volume_from_bytes` parsed the
//!   compressed bytes as a header and uncompressed records, and failed only
//!   when a random "message 1" tripped the gate limit.
//! - The committed KIWA 2026 real-time chunks: the start chunk (volume header
//!   and metadata record) and intermediate chunk 002 (120 message 31 radials,
//!   no header).
//!
//! A `Volume` needs the site and volume time of the volume header, so
//! headerless input is [`NexradError::MissingVolumeHeader`] from every
//! decoder. Inside a volume, a Message 29 record is read whole: its bytes
//! are never read as message headers or radials, and the volume carries the
//! message verbatim (`nexrad_unparsed_message_*`) with its header in the
//! `nexrad_metadata_message_*` table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use recast_radar_core::model::{ArrayBuf, Volume};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::{
    NexradError, NexradMetadata, normalize_archive_bytes, read_bzip_block_preview_from_bytes,
    read_volume_from_bytes, read_volume_from_bytes_with_bzip_preview, read_volume_with_metadata,
};

use common::load;

const MDM: &str = "l2-klix-20210829-175748-mdm";
const START_CHUNK: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
const CHUNK_002: &str = "l2chunk-kiwa-307-20260917-003629-002-i";

/// Length of the Message 29 in the model-data file (manifest description).
const MDM_MESSAGE_LEN: usize = 809_229;

/// `found` is what the error must report as the start of the input: the
/// LDM control word and bzip2 magic of a raw file, or the zeroed CTM header
/// of decompressed records.
fn assert_missing_volume_header(what: &str, found: &str, result: Result<Volume, NexradError>) {
    let error = result
        .err()
        .unwrap_or_else(|| panic!("{what}: decoded into a volume"));
    assert!(
        matches!(error, NexradError::MissingVolumeHeader { .. }),
        "{what}: {error}"
    );
    let text = error.to_string();
    assert!(
        text.starts_with("no Archive II volume header: the input starts with `"),
        "{what}: {text}"
    );
    assert!(
        text.contains(&format!("starts with `{found}`")),
        "{what}: {text}"
    );
}

/// The first 8 bytes of an LDM-compressed file: the record's byte count and
/// `BZh9`.
const LDM_BZIP2_START: &str = r"\x00\x0cd\xb1BZh9";

/// The model-data file: every volume decoder rejects it with the same
/// error, while the walker still reads its Message 29 and the metadata
/// reader finds no metadata message in it.
#[test]
fn model_data_file_is_rejected_with_a_clear_error() {
    let Some(raw) = load(MDM) else { return };
    assert_eq!(messages::volume_header_len(&raw), 0);

    assert_missing_volume_header(
        "read_volume_from_bytes",
        LDM_BZIP2_START,
        read_volume_from_bytes(&raw),
    );
    assert_missing_volume_header(
        "read_volume_with_metadata",
        LDM_BZIP2_START,
        read_volume_with_metadata(&raw).map(|decoded| decoded.volume),
    );
    assert_missing_volume_header(
        "read_volume_from_bytes_with_bzip_preview",
        LDM_BZIP2_START,
        read_volume_from_bytes_with_bzip_preview(&raw, 1, |_| {}),
    );
    // The block-bzip preview looks for LDM records after a 24-byte header,
    // finds none, and reports "not block-bzip" rather than a volume.
    assert_eq!(read_bzip_block_preview_from_bytes(&raw, 1).unwrap(), None);
    // Uncompressed record bytes without a header are rejected the same way.
    let records = messages::record_bytes(&raw).unwrap();
    assert_eq!(records.len(), MDM_MESSAGE_LEN + 12);
    assert_missing_volume_header(
        "read_volume_from_bytes on the records",
        r"\x00\x00\x00\x00\x00\x00\x00\x00",
        read_volume_from_bytes(&records),
    );

    let mut walked = Vec::new();
    for item in MessageWalker::new(&records) {
        let (header, body) = item.unwrap();
        assert!(matches!(body, MessageBody::Unparsed(_)), "{header:?}");
        walked.push((header.message_type, header.message_len()));
    }
    assert_eq!(walked, [(29, MDM_MESSAGE_LEN)]);

    let metadata = NexradMetadata::from_metadata_record(&raw);
    assert_eq!(metadata, NexradMetadata::default(), "no metadata message");
}

/// An intermediate real-time chunk on its own (committed, runs offline).
#[test]
fn intermediate_chunk_alone_is_rejected() {
    let Some(raw) = load(CHUNK_002) else { return };
    assert_eq!(messages::volume_header_len(&raw), 0);
    let found = r"\x00\x02yyBZh9";
    assert_missing_volume_header("chunk 002", found, read_volume_from_bytes(&raw));
    assert_missing_volume_header(
        "chunk 002 with metadata",
        found,
        read_volume_with_metadata(&raw).map(|decoded| decoded.volume),
    );
    // The same chunk after the start chunk decodes.
    let mut bytes = load(START_CHUNK).unwrap();
    bytes.extend_from_slice(&raw);
    let volume = read_volume_from_bytes(&bytes).unwrap();
    assert_eq!(volume.attrs.instrument_name, "KIWA");
    assert_eq!(volume.provenance.decode.decoded_ray_count, 120);
}

/// Everything but the message counters, which count the skipped Message 29.
fn without_message_counters(mut volume: Volume) -> Volume {
    volume.provenance.decode.message_count = 0;
    volume.provenance.decode.skipped_message_count = 0;
    volume
}

/// The Message 29 of the model-data file as its record holds it: the
/// message header and body after the 12-byte CTM header, as long as the
/// extended size in header bytes 12-15 says.
fn model_data_message(mdm: &[u8]) -> Vec<u8> {
    let record = messages::record_bytes(mdm).unwrap();
    let header = &record[12..28];
    assert_eq!(header[3], 29);
    assert_eq!(u16::from_be_bytes([header[0], header[1]]), 0xFFFF);
    let len = u32::from_be_bytes([header[12], header[13], header[14], header[15]]) as usize;
    assert_eq!(len, MDM_MESSAGE_LEN);
    record[12..12 + len].to_vec()
}

fn variable<'v>(volume: &'v Volume, name: &str) -> &'v ArrayBuf {
    &volume
        .extra_vars
        .iter()
        .find(|variable| &*variable.name == name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .values
}

/// `volume` with the Message 29 it carries taken out: the
/// `nexrad_unparsed_message_*` variables (which must hold exactly
/// `message`) and its entry in the `nexrad_metadata_message_*` header table
/// (which must hold its header as stored), and without the message
/// counters.
fn without_model_data(mut volume: Volume, message: &[u8]) -> Volume {
    assert_eq!(
        variable(&volume, "nexrad_unparsed_message_type"),
        &ArrayBuf::U8(vec![29])
    );
    assert_eq!(
        variable(&volume, "nexrad_unparsed_message_length"),
        &ArrayBuf::U32(vec![message.len() as u32])
    );
    assert_eq!(
        variable(&volume, "nexrad_unparsed_message_frames"),
        &ArrayBuf::U8(message.to_vec())
    );
    volume
        .extra_vars
        .retain(|variable| !variable.name.starts_with("nexrad_unparsed_message"));
    let ArrayBuf::U8(types) = variable(&volume, "nexrad_metadata_message_type") else {
        panic!("message types are bytes");
    };
    let entry = types.iter().position(|kind| *kind == 29).unwrap();
    assert_eq!(types.iter().filter(|kind| **kind == 29).count(), 1);
    let be16 = |at: usize| u16::from_be_bytes([message[at], message[at + 1]]);
    let expected: [(&str, u64); 8] = [
        ("nexrad_metadata_message_channels", u64::from(message[2])),
        ("nexrad_metadata_message_size", u64::from(be16(0))),
        (
            "nexrad_metadata_message_sequence_number",
            u64::from(be16(4)),
        ),
        ("nexrad_metadata_message_date", u64::from(be16(6))),
        (
            "nexrad_metadata_message_milliseconds",
            u64::from(u32::from_be_bytes([
                message[8],
                message[9],
                message[10],
                message[11],
            ])),
        ),
        ("nexrad_metadata_message_segments", u64::from(be16(12))),
        (
            "nexrad_metadata_message_segment_number",
            u64::from(be16(14)),
        ),
        ("nexrad_metadata_message_type", 29),
    ];
    for variable in volume
        .extra_vars
        .iter_mut()
        .filter(|variable| variable.name.starts_with("nexrad_metadata_message_"))
    {
        let (_, value) = expected
            .iter()
            .find(|(name, _)| *name == &*variable.name)
            .unwrap_or_else(|| panic!("unexpected {}", variable.name));
        let removed = match &mut variable.values {
            ArrayBuf::U8(values) => u64::from(values.remove(entry)),
            ArrayBuf::U16(values) => u64::from(values.remove(entry)),
            ArrayBuf::U32(values) => u64::from(values.remove(entry)),
            other => panic!("{}: {other:?}", variable.name),
        };
        assert_eq!(removed, *value, "{}", variable.name);
        variable.shape[0] -= 1;
    }
    without_message_counters(volume)
}

/// The model-data record inside a volume stream: the start chunk, then the
/// MDM file's LDM record, then chunk 002. The Message 29 is read whole by
/// its extended size, so the volume is the one decoded without it plus the
/// message, carried verbatim with its header. Before the wave 3 fix, the
/// decoder advanced one 2432-byte frame into the 809 229-byte message and
/// read its bytes as message headers: an `Ok` volume with VCP 52942, a
/// volume time in 2104 and a cut at 339 degrees.
#[test]
fn message_29_record_inside_a_volume_is_carried_whole() {
    let Some(mdm) = load(MDM) else { return };
    let start = load(START_CHUNK).unwrap();
    let chunk = load(CHUNK_002).unwrap();
    let message = model_data_message(&mdm);

    let mut reference = start.clone();
    reference.extend_from_slice(&chunk);
    let reference = read_volume_from_bytes(&reference).unwrap();
    assert_eq!(reference.provenance.decode.decoded_ray_count, 120);
    let reference_site_and_sweeps = (
        reference.attrs.clone(),
        reference.location,
        reference.sweeps.clone(),
    );

    let mut with_model_data = start.clone();
    with_model_data.extend_from_slice(&mdm);
    with_model_data.extend_from_slice(&chunk);
    let decoded = read_volume_from_bytes(&with_model_data).unwrap();
    assert_eq!(
        decoded.provenance.decode.message_count,
        reference.provenance.decode.message_count + 1
    );
    assert_eq!(
        decoded.provenance.decode.skipped_message_count,
        reference.provenance.decode.skipped_message_count + 1
    );
    assert_eq!(
        without_model_data(decoded, &message),
        without_message_counters(reference.clone())
    );

    // The same through the metadata decoder and the pipelined block decoder
    // with a preview: the Message 29 adds itself and changes the counters.
    let with_metadata = read_volume_with_metadata(&with_model_data).unwrap();
    assert_eq!(
        without_model_data(with_metadata.volume, &message),
        without_message_counters(reference.clone())
    );
    assert_eq!(with_metadata.metadata.errors, Vec::<String>::new());
    assert_eq!(
        with_metadata
            .metadata
            .per_sweep_elevation_data
            .map(|sweeps| sweeps.len()),
        Some(1)
    );
    let mut previews = 0;
    let previewed =
        read_volume_from_bytes_with_bzip_preview(&with_model_data, 1, |_| previews += 1).unwrap();
    assert_eq!(
        without_model_data(previewed, &message),
        without_message_counters(reference)
    );
    assert_eq!(previews, 0, "the volume has no complete sweep to preview");

    // The start chunk followed by the model-data record alone: the empty
    // volume the start chunk decodes to, one skipped message more, and the
    // message.
    let mut start_and_model_data = start.clone();
    start_and_model_data.extend_from_slice(&mdm);
    let empty = read_volume_from_bytes(&start_and_model_data).unwrap();
    let start_only = read_volume_from_bytes(&start).unwrap();
    assert_eq!(start_only.sweeps.len(), 0);
    assert_eq!(
        empty.provenance.decode.message_count,
        start_only.provenance.decode.message_count + 1
    );
    assert_eq!(
        without_model_data(empty, &message),
        without_message_counters(start_only)
    );

    // Uncompressed: the decoders' fixed-frame path with the same records.
    let (mut records, _) = normalize_archive_bytes(&start).unwrap();
    records.extend_from_slice(&messages::record_bytes(&mdm).unwrap());
    records.extend_from_slice(&messages::record_bytes(&chunk).unwrap());
    let uncompressed = read_volume_from_bytes(&records).unwrap();
    assert_eq!(uncompressed.provenance.decode.decoded_ray_count, 120);
    assert_eq!(
        uncompressed.provenance.compression.as_deref(),
        Some("uncompressed")
    );
    assert_eq!(uncompressed.attrs, reference_site_and_sweeps.0);
    assert_eq!(uncompressed.location, reference_site_and_sweeps.1);
    assert_eq!(uncompressed.sweeps, reference_site_and_sweeps.2);
    assert_eq!(
        variable(&uncompressed, "nexrad_unparsed_message_frames"),
        &ArrayBuf::U8(message)
    );
}
