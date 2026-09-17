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
//!   `stid` or `dt`. Before wave 3, `decode_volume_from_bytes` parsed the
//!   compressed bytes as a header and uncompressed records, and failed only
//!   when a random "message 1" tripped the gate limit.
//! - The committed KIWA 2026 real-time chunks: the start chunk (volume header
//!   and metadata record) and intermediate chunk 002 (120 message 31 radials,
//!   no header).
//!
//! A `RadarVolume` needs the site and volume time of the volume header, so
//! headerless input is [`NexradError::MissingVolumeHeader`] from every
//! decoder. Inside a volume, a Message 29 record is skipped whole: its bytes
//! are never read as message headers or radials.

mod common;

use recast_radar_core::RadarVolume;
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::{
    NexradError, NexradMetadata, decode_bzip_block_preview_from_bytes, decode_volume_from_bytes,
    decode_volume_from_bytes_with_bzip_preview, decode_volume_with_metadata,
    normalize_archive_bytes,
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
fn assert_missing_volume_header(what: &str, found: &str, result: Result<RadarVolume, NexradError>) {
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
        "decode_volume_from_bytes",
        LDM_BZIP2_START,
        decode_volume_from_bytes(&raw),
    );
    assert_missing_volume_header(
        "decode_volume_with_metadata",
        LDM_BZIP2_START,
        decode_volume_with_metadata(&raw).map(|decoded| decoded.volume),
    );
    assert_missing_volume_header(
        "decode_volume_from_bytes_with_bzip_preview",
        LDM_BZIP2_START,
        decode_volume_from_bytes_with_bzip_preview(&raw, 1, |_| {}),
    );
    // The block-bzip preview looks for LDM records after a 24-byte header,
    // finds none, and reports "not block-bzip" rather than a volume.
    assert_eq!(decode_bzip_block_preview_from_bytes(&raw, 1).unwrap(), None);
    // Uncompressed record bytes without a header are rejected the same way.
    let records = messages::record_bytes(&raw).unwrap();
    assert_eq!(records.len(), MDM_MESSAGE_LEN + 12);
    assert_missing_volume_header(
        "decode_volume_from_bytes on the records",
        r"\x00\x00\x00\x00\x00\x00\x00\x00",
        decode_volume_from_bytes(&records),
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
    assert_missing_volume_header("chunk 002", found, decode_volume_from_bytes(&raw));
    assert_missing_volume_header(
        "chunk 002 with metadata",
        found,
        decode_volume_with_metadata(&raw).map(|decoded| decoded.volume),
    );
    // The same chunk after the start chunk decodes.
    let mut bytes = load(START_CHUNK).unwrap();
    bytes.extend_from_slice(&raw);
    let volume = decode_volume_from_bytes(&bytes).unwrap();
    assert_eq!(volume.site.id, "KIWA");
    assert_eq!(volume.metadata.decoded_radial_count, 120);
}

/// Everything but the message counters, which count the skipped Message 29.
fn without_message_counters(mut volume: RadarVolume) -> RadarVolume {
    volume.metadata.message_count = 0;
    volume.metadata.skipped_message_count = 0;
    volume
}

/// The model-data record inside a volume stream: the start chunk, then the
/// MDM file's LDM record, then chunk 002. The Message 29 is skipped by its
/// extended size, so the volume is the one decoded without it. Before the
/// fix, the decoder advanced one 2432-byte frame into the 809 229-byte
/// message and read its bytes as message headers: an `Ok` volume with VCP
/// 52942, a volume time in 2104 and a cut at 339 degrees.
#[test]
fn message_29_record_inside_a_volume_is_skipped_whole() {
    let Some(mdm) = load(MDM) else { return };
    let start = load(START_CHUNK).unwrap();
    let chunk = load(CHUNK_002).unwrap();

    let mut reference = start.clone();
    reference.extend_from_slice(&chunk);
    let reference = decode_volume_from_bytes(&reference).unwrap();
    assert_eq!(reference.metadata.decoded_radial_count, 120);
    let reference_site_and_cuts = (reference.site.clone(), reference.cuts.clone());

    let mut with_model_data = start.clone();
    with_model_data.extend_from_slice(&mdm);
    with_model_data.extend_from_slice(&chunk);
    let decoded = decode_volume_from_bytes(&with_model_data).unwrap();
    assert_eq!(
        decoded.metadata.message_count,
        reference.metadata.message_count + 1
    );
    assert_eq!(
        decoded.metadata.skipped_message_count,
        reference.metadata.skipped_message_count + 1
    );
    assert_eq!(
        without_message_counters(decoded),
        without_message_counters(reference.clone())
    );

    // The same through the metadata decoder and the pipelined block decoder
    // with a preview: the Message 29 changes nothing but the counters.
    let with_metadata = decode_volume_with_metadata(&with_model_data).unwrap();
    assert_eq!(
        without_message_counters(with_metadata.volume),
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
        decode_volume_from_bytes_with_bzip_preview(&with_model_data, 1, |_| previews += 1).unwrap();
    assert_eq!(
        without_message_counters(previewed),
        without_message_counters(reference)
    );
    assert_eq!(previews, 0, "the volume has no complete cut to preview");

    // The start chunk followed by the model-data record alone: the empty
    // volume the start chunk decodes to, one skipped message more.
    let mut start_and_model_data = start.clone();
    start_and_model_data.extend_from_slice(&mdm);
    let empty = decode_volume_from_bytes(&start_and_model_data).unwrap();
    let start_only = decode_volume_from_bytes(&start).unwrap();
    assert_eq!(start_only.cuts.len(), 0);
    assert_eq!(
        empty.metadata.message_count,
        start_only.metadata.message_count + 1
    );
    assert_eq!(
        without_message_counters(empty),
        without_message_counters(start_only)
    );

    // Uncompressed: the decoders' fixed-frame path with the same records.
    let (mut records, _) = normalize_archive_bytes(&start).unwrap();
    records.extend_from_slice(&messages::record_bytes(&mdm).unwrap());
    records.extend_from_slice(&messages::record_bytes(&chunk).unwrap());
    let uncompressed = decode_volume_from_bytes(&records).unwrap();
    assert_eq!(uncompressed.metadata.decoded_radial_count, 120);
    assert_eq!(
        uncompressed.metadata.compression.as_deref(),
        Some("uncompressed")
    );
    assert_eq!(uncompressed.site, reference_site_and_cuts.0);
    assert_eq!(uncompressed.cuts, reference_site_and_cuts.1);
}
