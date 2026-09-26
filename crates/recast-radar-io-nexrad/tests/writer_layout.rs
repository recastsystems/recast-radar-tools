//! Byte layout of the Level II writer's output, checked on files written
//! from real volumes: the volume header, the LDM records, the 134-frame
//! metadata record (Messages 18, 5 and 2 where real files carry them), the
//! Message 31 radials, the real-time chunks, the polling-directory
//! publisher, and the typed refusals.

mod common;

use std::collections::BTreeSet;

use recast_radar_core::model::{FieldData, SweepMode, Volume};
use recast_radar_io_nexrad::messages::msg31_blocks::AzimuthResolution;
use recast_radar_io_nexrad::messages::rda_status::RdaStatus;
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::write::polling::{PollingDirectory, format_dir_list, parse_dir_list};
use recast_radar_io_nexrad::write::realtime::{
    Chunk, ChunkKind, ChunkWriter, write_realtime_chunks,
};
use recast_radar_io_nexrad::write::{
    Compression, SourceMetadata, WriteError, WriteOptions, WrittenRays, write_volume,
    write_volume_to, write_volume_with_source,
};
use recast_radar_io_nexrad::{
    MessageHeader, NexradMetadata, parse_message_header, read_volume_from_bytes,
};

use common::load;

const KTLX_2024: &str = "l2-ktlx-20240315-000217-trim";
const KDVN_2020: &str = "l2-kdvn-20200810-180401-trim";
const FRAME: usize = 2432;

fn decoded(id: &str) -> Option<Volume> {
    let bytes = load(id)?;
    Some(read_volume_from_bytes(&bytes).unwrap_or_else(|e| panic!("{id}: {e}")))
}

fn be_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// The LDM records after the volume header: (control word, record bytes
/// with the control word).
fn ldm_records(file: &[u8]) -> Vec<(i32, &[u8])> {
    let mut records = Vec::new();
    let mut cursor = 24;
    while cursor < file.len() {
        let control = be_i32(file, cursor);
        let end = cursor + 4 + control.unsigned_abs() as usize;
        records.push((control, &file[cursor..end]));
        cursor = end;
    }
    assert_eq!(cursor, file.len());
    records
}

/// Frames of decompressed record bytes: (message header, frame length).
fn frames(records: &[u8]) -> Vec<(MessageHeader, usize)> {
    let mut frames = Vec::new();
    let mut cursor = 0;
    while cursor < records.len() {
        let header = parse_message_header(records, cursor + 12).unwrap_or_else(|e| panic!("{e}"));
        let len = if header.message_type == 31 {
            12 + header.message_len()
        } else {
            FRAME
        };
        frames.push((header, len));
        cursor += len;
    }
    assert_eq!(cursor, records.len(), "frames end with the records");
    frames
}

/// Decompressed bytes of one LDM record.
fn inflate(record: &[u8]) -> Vec<u8> {
    messages::record_bytes(record)
        .unwrap_or_else(|e| panic!("{e}"))
        .into_owned()
}

#[test]
fn written_files_follow_the_archive_ii_layout() {
    let Some(volume) = decoded(KTLX_2024) else {
        return;
    };
    let (bytes, summary) =
        write_volume_with_source(&volume, SourceMetadata::default(), &WriteOptions::default())
            .unwrap();

    // Volume header: tape name and extension of the source, the volume time,
    // the ICAO.
    assert_eq!(&bytes[..12], b"AR2V0006.626");
    assert_eq!(&bytes[20..24], b"KTLX");
    let header_time = summary.volume_time.unwrap();
    let days = be_u32(&bytes, 12);
    let ms = be_u32(&bytes, 16);
    assert_eq!(
        i64::from(days - 1) * 86_400_000 + i64::from(ms),
        header_time.timestamp_millis()
    );

    // LDM records: the metadata record, then radials; only the last control
    // word is negative.
    let records = ldm_records(&bytes);
    assert_eq!(records.len(), summary.records);
    for (index, (control, _)) in records.iter().enumerate() {
        assert_eq!(*control < 0, index + 1 == records.len(), "record {index}");
    }

    // Metadata record: 134 fixed frames; Message 18 in frames 127 to 130,
    // Message 5 in 133, Message 2 in 134, as real files place them.
    let metadata = inflate(records[0].1);
    assert_eq!(metadata.len(), 134 * FRAME);
    let layout: Vec<(usize, u8, u16, u16, u16)> = frames(&metadata)
        .iter()
        .enumerate()
        .filter(|(_, (header, _))| header.size_halfwords != 0)
        .map(|(index, (header, _))| {
            (
                index + 1,
                header.message_type,
                header.size_halfwords,
                header.segment_number,
                header.segments,
            )
        })
        .collect();
    assert_eq!(
        layout,
        [
            (127, 18, 1208, 1, 4),
            (128, 18, 1208, 2, 4),
            (129, 18, 1208, 3, 4),
            (130, 18, 1142, 4, 4),
            (133, 5, 8 + 11 + 23 * volume.sweeps.len() as u16, 1, 1),
            (134, 2, 68, 1, 1),
        ]
    );
    let meta = NexradMetadata::from_metadata_record(&bytes);
    assert!(meta.errors.is_empty(), "{:?}", meta.errors);
    let Some(RdaStatus::Orda(status)) = &meta.rda_status else {
        panic!("no Open RDA message 2");
    };
    assert_eq!(status.volume_coverage_pattern.pattern(), Some(212));
    assert_eq!(
        meta.build.map(|build| build.to_string()).as_deref(),
        Some("20.0")
    );
    let vcp = meta.vcp.as_ref().unwrap();
    assert_eq!(vcp.pattern_number, 212);
    assert_eq!(vcp.cuts.len(), volume.sweeps.len());
    for (cut, sweep) in vcp.cuts.iter().zip(&volume.sweeps) {
        assert_eq!(cut.elevation_angle_deg, sweep.fixed_angle_deg);
    }
    let adaptation = meta.adaptation.as_ref().unwrap();
    assert_eq!(adaptation.site_name, "KTLX");
    assert!((adaptation.latitude() - volume.location.latitude_deg.unwrap()).abs() < 1e-5);
    assert!((adaptation.longitude() - volume.location.longitude_deg.unwrap()).abs() < 1e-5);
    assert_eq!(
        f64::from(adaptation.tfreq_mhz) * 1e6,
        volume.radar_parameters.frequency_hz[0]
    );

    // Radial records: 120 radials (the last one the rest); statuses open and
    // close each cut and the volume; azimuth numbers count from 1; sequence
    // numbers increase; message times are the source radials' recorded
    // generation times (the model's nexrad_message_date and _milliseconds).
    let generated = |cut: usize, ray: usize| {
        let sweep = &volume.sweeps[cut];
        let find = |name: &str| {
            sweep
                .extra_vars
                .iter()
                .find(|variable| &*variable.name == name)
                .map(|variable| &variable.values)
        };
        match (
            find("nexrad_message_date"),
            find("nexrad_message_milliseconds"),
        ) {
            (
                Some(recast_radar_core::model::ArrayBuf::U16(dates)),
                Some(recast_radar_core::model::ArrayBuf::U32(milliseconds)),
            ) => (dates[ray], milliseconds[ray]),
            other => panic!("sweep {cut}: no message times: {other:?}"),
        }
    };
    let mut previous_sequence = None;
    let mut radials = 0;
    let mut statuses: Vec<Vec<u8>> = vec![Vec::new(); volume.sweeps.len()];
    for (_, record) in &records[1..] {
        let bytes = inflate(record);
        let frames = frames(&bytes);
        assert!(!frames.is_empty() && frames.len() <= 120);
        for item in MessageWalker::new(&bytes) {
            let (header, body) = item.unwrap();
            let MessageBody::DigitalRadarDataGeneric(radial) = body else {
                panic!("a data record holds message {}", header.message_type);
            };
            let data = &radial.header;
            assert_eq!(data.radar_identifier_str(), "KTLX");
            assert_eq!(data.block_pointers.len(), 3 + radial.moments.len());
            assert_eq!(data.blocks_offset(), 72, "10 pointer slots");
            assert_eq!(radial.volume.unwrap().block_size, 52);
            assert_eq!(radial.elevation.unwrap().block_size, 12);
            assert_eq!(radial.radial.unwrap().block_size, 28);
            assert_eq!(data.azimuth_resolution, AzimuthResolution::HalfDegree);
            let cut = usize::from(data.elevation_number) - 1;
            assert_eq!(
                (header.date, header.milliseconds),
                generated(cut, statuses[cut].len())
            );
            if let Some(previous) = previous_sequence {
                assert_eq!(header.sequence_id, (previous + 1) % 0x8000);
            }
            previous_sequence = Some(header.sequence_id);
            let cut = usize::from(data.elevation_number) - 1;
            statuses[cut].push(data.radial_status_code);
            assert_eq!(usize::from(data.azimuth_number), statuses[cut].len());
            radials += 1;
        }
    }
    let sizes: Vec<usize> = records[1..]
        .iter()
        .map(|(_, record)| frames(&inflate(record)).len())
        .collect();
    let (last, full) = sizes.split_last().unwrap();
    assert!(
        full.iter().all(|size| *size == 120) && *last <= 120,
        "{sizes:?}"
    );
    assert_eq!(radials, summary.radials);
    let last = statuses.len() - 1;
    for (cut, codes) in statuses.iter().enumerate() {
        let open = match cut {
            0 => 3,
            _ if cut == last => 5,
            _ => 0,
        };
        let close = if cut == last { 4 } else { 2 };
        assert_eq!(codes[0], open, "cut {cut}");
        assert_eq!(*codes.last().unwrap(), close, "cut {cut}");
        assert!(
            codes[1..codes.len() - 1].iter().all(|code| *code == 1),
            "cut {cut}"
        );
    }

    // Uncompressed: the same frames after the volume header.
    let mut options = WriteOptions::default();
    options.compression = Compression::None;
    let plain = write_volume(&volume, &options).unwrap();
    assert_eq!(plain[..24], bytes[..24]);
    let mut expected = Vec::new();
    for (_, record) in &records {
        expected.extend_from_slice(&inflate(record));
    }
    assert!(plain[24..] == expected[..], "uncompressed records");

    // gzip wrapper: the same file inside.
    options.compression = Compression::Bzip2LdmRecords;
    options.gzip = true;
    let wrapped = write_volume(&volume, &options).unwrap();
    assert_eq!(&wrapped[..2], &[0x1f, 0x8b]);
    let mut inner = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&wrapped[..]), &mut inner)
        .unwrap();
    assert!(inner == bytes, "gzip wraps the bzip2 file");
}

#[test]
fn realtime_chunks_concatenate_to_the_archive() {
    let Some(volume) = decoded(KDVN_2020) else {
        return;
    };
    let options = WriteOptions::default();
    let chunked = write_realtime_chunks(&volume, &options).unwrap();
    let file = write_volume(&volume, &options).unwrap();
    assert!(chunked.concatenated() == file);
    assert_eq!(chunked.icao, "KDVN");
    assert_eq!(chunked.volume_number, 157);

    // Like the chunks bucket (the committed KIWA chunks): the start chunk is
    // the volume header and one LDM record of metadata, every other chunk
    // one LDM record of 120 radials (the last one the rest and the end).
    let kinds: Vec<ChunkKind> = chunked.chunks.iter().map(|chunk| chunk.kind).collect();
    assert_eq!(kinds[0], ChunkKind::Start);
    assert_eq!(*kinds.last().unwrap(), ChunkKind::End);
    assert!(
        kinds[1..kinds.len() - 1]
            .iter()
            .all(|kind| *kind == ChunkKind::Intermediate)
    );
    let first = &chunked.chunks[0];
    assert_eq!(
        chunked.chunk_key(first),
        format!(
            "KDVN/157/{}-001-S",
            chunked.volume_time.format("%Y%m%d-%H%M%S")
        )
    );
    assert_eq!(
        NexradMetadata::from_metadata_record(&first.bytes),
        NexradMetadata::from_metadata_record(&file)
    );
    for chunk in &chunked.chunks[1..] {
        assert_eq!(ldm_records_without_header(&chunk.bytes), 1);
        let records = messages::record_bytes(&chunk.bytes).unwrap();
        let mut radials = 0;
        for item in MessageWalker::new(&records) {
            let (_, body) = item.unwrap();
            assert!(
                matches!(body, MessageBody::DigitalRadarDataGeneric(_)),
                "chunk {} holds a non-radial message",
                chunk.number
            );
            radials += 1;
        }
        assert!(
            radials == 120 || chunk.kind == ChunkKind::End,
            "chunk {}: {radials} radials",
            chunk.number
        );
    }

    // The real start chunk has the same shape.
    if let Some(real) = load("l2chunk-kiwa-307-20260917-003629-001-s") {
        assert_eq!(&real[..4], b"AR2V");
        assert_eq!(ldm_records_without_header(&real[24..]), 1);
        assert_eq!(messages::metadata_record(&real).unwrap().len(), 134 * FRAME);
    }

    // Chunks are LDM records: other layouts are refused.
    let mut plain = WriteOptions::default();
    plain.compression = Compression::None;
    assert!(matches!(
        write_realtime_chunks(&volume, &plain),
        Err(WriteError::InvalidOption(_))
    ));
}

/// The radial statuses of the Message 31 radials in `chunk`.
fn chunk_statuses(chunk: &Chunk) -> Vec<u8> {
    let records = messages::record_bytes(&chunk.bytes).unwrap_or_else(|e| panic!("{e}"));
    MessageWalker::new(&records)
        .filter_map(|item| match item.unwrap_or_else(|e| panic!("{e}")).1 {
            MessageBody::DigitalRadarDataGeneric(radial) => Some(radial.header.radial_status_code),
            _ => None,
        })
        .collect()
}

/// A volume written while its sweeps arrive: pushed one by one, the chunks
/// are those of the whole volume; the last record so far is held back until
/// the volume ends; a volume can end early; and no more sweeps than planned
/// are taken.
#[test]
fn chunk_writer_sends_chunks_as_sweeps_arrive() {
    let Some(volume) = decoded(KTLX_2024) else {
        return;
    };
    assert_eq!(volume.sweeps.len(), 2);
    let options = WriteOptions::default();
    let part = |index: usize| {
        let mut part = volume.clone();
        part.sweeps = vec![volume.sweeps[index].clone()];
        part
    };
    let whole = write_realtime_chunks(&volume, &options).unwrap();

    let mut writer = ChunkWriter::new(&volume, &options).unwrap();
    assert_eq!(writer.volume_time(), None);
    // 480 radials: the start chunk and three records; the fourth is held.
    let mut chunks = writer.push(&part(0)).unwrap();
    let kinds: Vec<ChunkKind> = chunks.iter().map(|chunk| chunk.kind).collect();
    assert_eq!(
        kinds,
        [
            ChunkKind::Start,
            ChunkKind::Intermediate,
            ChunkKind::Intermediate,
            ChunkKind::Intermediate
        ]
    );
    assert_eq!(writer.volume_time(), Some(whole.volume_time));
    assert_eq!(
        writer.chunk_key(&chunks[0]),
        Some(whole.chunk_key(&whole.chunks[0]))
    );
    chunks.extend(writer.push(&part(1)).unwrap());
    let (end, summary) = writer.finish().unwrap();
    chunks.push(end);
    assert!(
        chunks == whole.chunks,
        "streamed chunks differ from the whole volume's"
    );
    assert_eq!(
        format!("{:?}", summary.moments),
        format!("{:?}", whole.summary.moments)
    );
    assert_eq!(
        (
            summary.sweeps,
            summary.radials,
            summary.records,
            summary.bytes
        ),
        (
            whole.summary.sweeps,
            whole.summary.radials,
            whole.summary.records,
            whole.summary.bytes
        )
    );
    // Every record holds radials of one cut: the first cut ends the fourth
    // chunk (status 2), the volume the last (status 4).
    let statuses: Vec<Vec<u8>> = chunks[1..].iter().map(chunk_statuses).collect();
    assert!(statuses.iter().all(|s| s.len() == 120));
    assert_eq!(statuses[0][0], 3);
    assert_eq!(*statuses[3].last().unwrap(), 2);
    assert_eq!(statuses[4][0], 5);
    assert_eq!(*statuses[7].last().unwrap(), 4);

    // A volume ended after its first sweep.
    let mut writer = ChunkWriter::new(&volume, &options).unwrap();
    let mut early = writer.push(&part(0)).unwrap();
    let (end, summary) = writer.finish().unwrap();
    assert_eq!(*chunk_statuses(&end).last().unwrap(), 4);
    assert!(
        be_i32(&end.bytes, 0) < 0,
        "the end chunk's control word is negated"
    );
    early.push(end);
    let file: Vec<u8> = early.iter().flat_map(|chunk| chunk.bytes.clone()).collect();
    let again = read_volume_from_bytes(&file).unwrap();
    assert_eq!(again.sweeps.len(), 1);
    assert_eq!(summary.sweeps, 1);
    assert!(
        again.sweeps[0]
            .field(&volume.sweeps[0].fields[0].name)
            .unwrap()
            .data
            == volume.sweeps[0].fields[0].data
    );

    // A moment the planned volume lacks has no fixed coding: planned from
    // the surveillance cut (REF and the dual-polarization moments), the
    // Doppler cut's VEL is refused, with nothing sent.
    let planned = part(0);
    let planned_moments: BTreeSet<String> = planned.sweeps[0]
        .fields
        .iter()
        .map(|field| field.name.to_string())
        .collect();
    assert!(!planned_moments.contains("VEL"), "{planned_moments:?}");
    let mut writer = ChunkWriter::new(&planned, &options).unwrap();
    let sent = writer.push(&part(0)).unwrap();
    assert!(matches!(
        writer.push(&part(1)),
        Err(WriteError::UnplannedMoment {
            sweep: 0,
            moment: recast_radar_io_nexrad::write::Moment::Vel,
            ..
        })
    ));
    // The refused push sent nothing: the volume still ends after sweep 0.
    let (end, summary) = writer.finish().unwrap();
    assert_eq!(summary.sweeps, 1);
    let file: Vec<u8> = sent
        .iter()
        .chain([&end])
        .flat_map(|c| c.bytes.clone())
        .collect();
    assert_eq!(read_volume_from_bytes(&file).unwrap().sweeps.len(), 1);

    // No more sweeps than the planned volume has; nothing to end before a
    // push.
    let mut writer = ChunkWriter::new(&planned, &options).unwrap();
    writer.push(&part(0)).unwrap();
    assert!(matches!(
        writer.push(&part(0)),
        Err(WriteError::TooManySweeps { count: 2, max: 1 })
    ));
    let writer = ChunkWriter::new(&planned, &options).unwrap();
    assert!(matches!(writer.finish(), Err(WriteError::EmptyVolume)));
    let mut plain = WriteOptions::default();
    plain.gzip = true;
    assert!(matches!(
        ChunkWriter::new(&planned, &plain),
        Err(WriteError::InvalidOption(_))
    ));
}

/// Number of LDM records in `bytes` (which start at a control word).
fn ldm_records_without_header(bytes: &[u8]) -> usize {
    let mut cursor = 0;
    let mut count = 0;
    while cursor < bytes.len() {
        cursor += 4 + be_i32(bytes, cursor).unsigned_abs() as usize;
        count += 1;
    }
    assert_eq!(cursor, bytes.len());
    count
}

#[test]
fn polling_directory_follows_the_grlevelx_conventions() {
    let (Some(first), Some(second)) = (decoded(KTLX_2024), decoded(KDVN_2020)) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("recast-l2-polling-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let directory = PollingDirectory::new(&root).with_max_files(1);

    let published = directory
        .publish_volume(&first, &WriteOptions::default())
        .unwrap();
    let summary = published.summary.as_ref().unwrap();
    let time = summary.volume_time.unwrap();
    let name = format!("KTLX_{}.ar2v", time.format("%Y%m%d%H%M%S"));
    assert_eq!(published.entry.name, name);
    let written = std::fs::read(root.join("KTLX").join(&name)).unwrap();
    assert_eq!(published.entry.size, written.len() as u64);
    assert_eq!(
        read_volume_from_bytes(&written).unwrap().sweeps.len(),
        first.sweeps.len()
    );
    let listing = std::fs::read_to_string(root.join("KTLX").join("dir.list")).unwrap();
    assert_eq!(listing, format!("{} {name}\r\n", written.len()));
    assert_eq!(
        parse_dir_list(&listing),
        std::slice::from_ref(&published.entry)
    );
    assert_eq!(format_dir_list(&parse_dir_list(&listing)), listing);

    // A later volume replaces the older one under a one-file limit.
    let later = time + chrono::TimeDelta::minutes(5);
    let again = directory.publish_bytes("KTLX", later, &written).unwrap();
    assert_eq!(again.removed, std::slice::from_ref(&name));
    assert!(!root.join("KTLX").join(&name).exists());
    assert_eq!(
        directory.entries("KTLX").unwrap(),
        std::slice::from_ref(&again.entry)
    );

    // Every site is listed once in both site files, sorted without regard
    // to case.
    directory
        .publish_volume(&second, &WriteOptions::default())
        .unwrap();
    directory.list_site("dbor").unwrap();
    for file in ["config.cfg", "grlevel2.cfg"] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        assert_eq!(text, "Site: dbor\r\nSite: KDVN\r\nSite: KTLX\r\n", "{file}");
    }
    assert!(matches!(
        directory.publish_bytes("../x", later, &written),
        Err(recast_radar_io_nexrad::write::polling::PublishError::InvalidSite(_))
    ));

    // gzip bytes are named .ar2v.gz; a fixed suffix names every file so.
    let mut gzip = WriteOptions::default();
    gzip.gzip = true;
    let wrapped = directory.publish_volume(&first, &gzip).unwrap();
    assert!(
        wrapped.entry.name.ends_with(".ar2v.gz"),
        "{}",
        wrapped.entry.name
    );
    let fixed = PollingDirectory::new(root.join("fixed")).with_suffix(".ar2v.gz");
    let named = fixed.publish_bytes("BJAB", later, &written).unwrap();
    assert_eq!(
        named.entry.name,
        format!("BJAB_{}.ar2v.gz", later.format("%Y%m%d%H%M%S"))
    );

    // Other name formats: the date and time apart, a lower-case site
    // written without a separator (the ICAO in the file lower case too),
    // and two volumes a time under names the caller chooses.
    let time = chrono::DateTime::parse_from_rfc3339("2026-09-24T21:05:00Z")
        .unwrap()
        .to_utc();
    let france = PollingDirectory::new(root.join("mf"))
        .with_name_format("{site}_%Y%m%d_%H%M%S")
        .unwrap();
    assert_eq!(
        france.file_name("MF36", time, &written),
        "MF36_20260924_210500.ar2v"
    );
    let compact = PollingDirectory::new(root.join("dk"))
        .with_name_format("{site}%Y%m%d%H%M%S")
        .unwrap();
    let mut danish = WriteOptions::default();
    danish.icao = Some("dbor".to_owned());
    let published = compact.publish_volume(&first, &danish).unwrap();
    assert_eq!(
        published.entry.name,
        format!(
            "dbor{}.ar2v",
            summary.volume_time.unwrap().format("%Y%m%d%H%M%S")
        )
    );
    assert_eq!(&std::fs::read(&published.path).unwrap()[20..24], b"dbor");
    let jma = PollingDirectory::new(root.join("jma"))
        .with_name_format("{site}%Y%m%d_%H%M%S_1")
        .unwrap()
        .with_suffix(".msg31.gz");
    let one = jma.publish_bytes("ITOK", time, &written).unwrap();
    assert_eq!(one.entry.name, "ITOK20260924_210500_1.msg31.gz");
    let two = jma
        .publish_named("ITOK", "ITOK20260924_210500_2.msg31.gz", &written)
        .unwrap();
    assert_eq!(jma.entries("ITOK").unwrap(), [one.entry, two.entry]);

    // Formats and names that are not plain file names every platform can
    // hold are refused: path separators and control characters, and what
    // Windows refuses (a `:` would write an NTFS alternate data stream).
    use recast_radar_io_nexrad::write::polling::PublishError;
    for format in [
        "",
        "{site}%Q",
        "../{site}_%Y",
        "a\\b",
        "{site}_%H:%M",
        "{site}_%T",
        "{site}*",
        "CON",
        "nul",
    ] {
        assert!(
            matches!(
                PollingDirectory::new(&root).with_name_format(format),
                Err(PublishError::InvalidNameFormat { .. })
            ),
            "{format:?}"
        );
    }
    for name in [
        "",
        "..",
        "dir.list",
        "DIR.LIST",
        "dir.list.tmp",
        "a/b",
        "a\\b",
        "a\nb",
        "a:b",
        "KTLX.ar2v:stream",
        "a*b",
        "a?b",
        "a\"b",
        "a<b",
        "a>b",
        "a|b",
        "CON",
        "nul.ar2v",
        "Com1.txt",
        "LPT9",
        "AUX .ar2v",
        "x.",
        "x ",
    ] {
        assert!(
            matches!(
                jma.publish_named("ITOK", name, &written),
                Err(PublishError::InvalidFileName(_))
            ),
            "{name:?}"
        );
    }
    for site in ["CON", "nul", "COM1", "lpt3"] {
        assert!(
            matches!(
                directory.publish_bytes(site, later, &written),
                Err(PublishError::InvalidSite(_))
            ),
            "{site:?}"
        );
    }
    // Names that only start like a device name are plain names.
    for name in ["CONUS_1.ar2v", "COM10.ar2v", "NULL.ar2v"] {
        jma.publish_named("ITOK", name, &written)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    std::fs::remove_dir_all(&root).unwrap();
}

/// Moment blocks of every radial in Level II `bytes`, per radial.
fn radial_moment_counts(bytes: &[u8]) -> Vec<usize> {
    let records = messages::record_bytes(bytes).unwrap_or_else(|e| panic!("{e}"));
    MessageWalker::new(&records)
        .filter_map(|item| match item.unwrap_or_else(|e| panic!("{e}")).1 {
            MessageBody::DigitalRadarDataGeneric(radial) => Some(radial.moments.len()),
            _ => None,
        })
        .collect()
}

/// Every radial carries a moment (Py-ART and xradar take a cut's moments
/// from its first radial and fail on one without any): a sweep on whose
/// rays no written moment has data is left out, and so are single rays
/// without data; both are reported.
#[test]
fn sweeps_and_rays_without_moment_data_are_left_out() {
    let Some(volume) = decoded(KTLX_2024) else {
        return;
    };
    let options = WriteOptions::default();
    let rays0 = volume.sweeps[0].nrays();
    let rays1 = volume.sweeps[1].nrays() as u32;

    // A sweep without fields, and one whose every row is absent.
    let mut bare = volume.clone();
    bare.sweeps[1].fields.clear();
    let mut absent = volume.clone();
    for field in &mut absent.sweeps[1].fields {
        field.absent_rows = (0..rays1).collect();
    }
    for (what, changed) in [("no fields", bare), ("absent rows", absent)] {
        let (bytes, summary) =
            write_volume_with_source(&changed, SourceMetadata::default(), &options).unwrap();
        assert_eq!(summary.skipped_sweeps, [1], "{what}");
        assert_eq!((summary.sweeps, summary.radials), (1, rays0), "{what}");
        assert!(
            summary
                .notes
                .iter()
                .any(|note| note.starts_with("sweep 1: ")),
            "{what}: {:?}",
            summary.notes
        );
        let again = read_volume_from_bytes(&bytes).unwrap();
        assert_eq!(again.sweeps.len(), 1, "{what}");
        let counts = radial_moment_counts(&bytes);
        assert_eq!(counts.len(), rays0, "{what}");
        assert!(counts.iter().all(|count| *count > 0), "{what}");
    }

    // Rays without data are left out of their sweep; the others keep their
    // order, the first opening the volume.
    let mut holes = volume.clone();
    for field in &mut holes.sweeps[0].fields {
        field.absent_rows = vec![0, 1, 7];
    }
    let (bytes, summary) =
        write_volume_with_source(&holes, SourceMetadata::default(), &options).unwrap();
    let kept: Vec<usize> = (2..rays0).filter(|ray| *ray != 7).collect();
    assert_eq!(
        summary.written_rays,
        [WrittenRays {
            sweep: 0,
            rays: kept.clone()
        }]
    );
    assert_eq!(
        summary.radials,
        volume.sweeps.iter().map(|s| s.nrays()).sum::<usize>() - 3
    );
    assert!(
        summary
            .notes
            .iter()
            .any(|note| note.contains("left out of sweeps 0 (3 of")),
        "{:?}",
        summary.notes
    );
    let again = read_volume_from_bytes(&bytes).unwrap();
    let azimuths: Vec<f32> = kept
        .iter()
        .map(|ray| volume.sweeps[0].rays.azimuth_deg[*ray])
        .collect();
    assert_eq!(again.sweeps[0].rays.azimuth_deg, azimuths);
    // Ray times since 1970 are the source's (each volume's time reference
    // is its own earliest ray, which moves with ray 0 left out).
    let epoch_ms = |volume: &Volume, time_s: f64| {
        volume.time_reference.timestamp_millis() + (time_s * 1000.0).round() as i64
    };
    let times: Vec<i64> = kept
        .iter()
        .map(|ray| epoch_ms(&volume, volume.sweeps[0].rays.time_s[*ray]))
        .collect();
    let written: Vec<i64> = again.sweeps[0]
        .rays
        .time_s
        .iter()
        .map(|time_s| epoch_ms(&again, *time_s))
        .collect();
    assert_eq!(written, times);
    assert!(radial_moment_counts(&bytes).iter().all(|count| *count > 0));
    assert_eq!(chunk_statuses_of(&bytes)[0], 3);

    // No ray with data anywhere: refused, nothing written.
    let mut none = volume.clone();
    for sweep in &mut none.sweeps {
        sweep.fields.clear();
    }
    let mut sink = Vec::new();
    assert!(matches!(
        write_volume_to(&none, &options, &mut sink),
        Err(WriteError::NoMoments)
    ));
    assert!(sink.is_empty());
}

/// The radial statuses of every Message 31 radial in Level II `bytes`.
fn chunk_statuses_of(bytes: &[u8]) -> Vec<u8> {
    let records = messages::record_bytes(bytes).unwrap_or_else(|e| panic!("{e}"));
    MessageWalker::new(&records)
        .filter_map(|item| match item.unwrap_or_else(|e| panic!("{e}")).1 {
            MessageBody::DigitalRadarDataGeneric(radial) => Some(radial.header.radial_status_code),
            _ => None,
        })
        .collect()
}

#[test]
fn unwritable_volumes_are_refused_before_any_byte() {
    let Some(volume) = decoded(KTLX_2024) else {
        return;
    };
    let options = WriteOptions::default();
    let refused = |volume: &Volume, options: &WriteOptions| {
        let mut sink = Vec::new();
        let result = write_volume_to(volume, options, &mut sink);
        assert!(sink.is_empty(), "bytes written before a refusal");
        result.expect_err("refusal")
    };

    let mut rhi = volume.clone();
    rhi.sweeps[1].sweep_mode = SweepMode::Rhi;
    assert!(matches!(
        refused(&rhi, &options),
        WriteError::UnsupportedSweepMode { sweep: 1, ref mode } if mode == "rhi"
    ));

    let mut many = volume.clone();
    while many.sweeps.len() < 33 {
        let copy = many.sweeps[0].clone();
        many.sweeps.push(copy);
    }
    assert!(matches!(
        refused(&many, &options),
        WriteError::TooManySweeps { count: 33, max: 32 }
    ));

    let mut empty = volume.clone();
    empty.sweeps.clear();
    assert!(matches!(refused(&empty, &options), WriteError::EmptyVolume));

    let mut angle = volume.clone();
    angle.sweeps[0].rays.azimuth_deg[5] = f32::NAN;
    assert!(matches!(
        refused(&angle, &options),
        WriteError::Ray {
            sweep: 0,
            ray: 5,
            ..
        }
    ));

    let mut early = volume.clone();
    early.time_reference = chrono::DateTime::from_timestamp(-86_400, 0).unwrap();
    assert!(matches!(refused(&early, &options), WriteError::Ray { .. }));

    let mut far = volume.clone();
    if let recast_radar_core::model::RangeCoord::Uniform { first_center_m, .. } =
        &mut far.sweeps[0].range
    {
        *first_center_m += 40_000.0;
    }
    assert!(matches!(
        refused(&far, &options),
        WriteError::Geometry { sweep: 0, .. }
    ));

    // A spacing 0.3 m off accumulates past half a gate over 1832 gates...
    let mut stretched = volume.clone();
    if let recast_radar_core::model::RangeCoord::Uniform { spacing_m, .. } =
        &mut stretched.sweeps[0].range
    {
        *spacing_m += 0.3;
    }
    assert!(matches!(
        refused(&stretched, &options),
        WriteError::Geometry { .. }
    ));
    // ...unless the caller accepts the error.
    let mut loose = WriteOptions::default();
    loose.max_range_error_m = Some(1000.0);
    let (_, summary) =
        write_volume_with_source(&stretched, SourceMetadata::default(), &loose).unwrap();
    assert!(summary.max_range_error_m > 500.0);

    let mut bad = WriteOptions::default();
    bad.radials_per_record = 0;
    assert!(matches!(
        refused(&volume, &bad),
        WriteError::InvalidOption(_)
    ));
    let mut bad = WriteOptions::default();
    bad.volume_number = Some(1000);
    assert!(matches!(
        refused(&volume, &bad),
        WriteError::InvalidOption(_)
    ));
    let mut bad = WriteOptions::default();
    bad.icao = Some("KTLXX".to_owned());
    assert!(matches!(
        refused(&volume, &bad),
        WriteError::InvalidSiteId(_)
    ));

    // Model fields are public: a volume changed without Sweep::seal can
    // have per-ray arrays or field rows that do not match the ray count.
    // Each is refused, never indexed past its end.
    type Change = fn(&mut Volume);
    let changes: [(&str, Change); 9] = [
        ("time", |v| {
            v.sweeps[0].rays.time_s.pop();
        }),
        ("elevation", |v| {
            v.sweeps[0].rays.elevation_deg.pop();
        }),
        ("azimuth", |v| {
            v.sweeps[0].rays.azimuth_deg.pop();
        }),
        ("nyquist", |v| {
            if let Some(values) = &mut v.sweeps[0].ray_vars.nyquist_velocity_mps {
                values.pop();
            }
        }),
        ("unambiguous range", |v| {
            if let Some(values) = &mut v.sweeps[0].ray_vars.unambiguous_range_m {
                values.pop();
            }
        }),
        ("field rows", |v| v.sweeps[0].fields[0].nrays -= 1),
        ("field values", |v| {
            if let FieldData::U8 { values, .. } = &mut v.sweeps[0].fields[0].data {
                values.pop();
            }
        }),
        ("absent rows", |v| {
            v.sweeps[0].fields[0].absent_rows = vec![7, 3]
        }),
        ("gate stride", |v| v.sweeps[0].fields[0].gates.stride = 0),
    ];
    for (what, change) in changes {
        let mut changed = volume.clone();
        change(&mut changed);
        assert!(
            matches!(
                refused(&changed, &options),
                WriteError::Inconsistent { sweep: 0, .. }
            ),
            "{what}"
        );
    }
    assert!(
        volume.sweeps[0].ray_vars.nyquist_velocity_mps.is_some()
            && volume.sweeps[0].ray_vars.unambiguous_range_m.is_some()
            && matches!(volume.sweeps[0].fields[0].data, FieldData::U8 { .. }),
        "the changes above apply to this volume"
    );

    // A garbage metadata record is refused, not copied.
    let record = vec![0xAB; 1000];
    let source = SourceMetadata {
        metadata_record: Some(&record),
        ..SourceMetadata::default()
    };
    assert!(matches!(
        write_volume_with_source(&volume, source, &options),
        Err(WriteError::MetadataRecord(_))
    ));
}

/// `write_volume_to` streams the file a batch of records at a time (one
/// record per rayon thread): in a two-thread pool the KDVN trim's records go
/// out in many batches, and the bytes are those of the whole-file writers
/// under every compression, the real-time chunks' among them. A sink that
/// fails part way returns its error.
#[test]
fn streamed_files_are_the_whole_file_bytes() {
    let Some(volume) = decoded(KDVN_2020) else {
        return;
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    pool.install(|| {
        for (compression, gzip) in [
            (Compression::Bzip2LdmRecords, false),
            (Compression::None, false),
            (Compression::Bzip2LdmRecords, true),
            (Compression::None, true),
        ] {
            let mut options = WriteOptions::default();
            options.compression = compression;
            options.gzip = gzip;
            // Records of 10 radials: many batches of two.
            options.radials_per_record = 10;
            let (whole, whole_summary) =
                write_volume_with_source(&volume, SourceMetadata::default(), &options).unwrap();
            let mut sink = Vec::new();
            let summary = write_volume_to(&volume, &options, &mut sink).unwrap();
            assert!(sink == whole, "{compression:?} gzip {gzip}");
            assert_eq!(summary, whole_summary);
            assert_eq!(summary.bytes, sink.len());
            assert!(summary.records > 6, "{}", summary.records);
            if compression == Compression::Bzip2LdmRecords && !gzip {
                let records = ldm_records(&sink);
                assert_eq!(records.len(), summary.records);
                // Only the last control word is negated.
                let negative: Vec<usize> = records
                    .iter()
                    .enumerate()
                    .filter(|(_, (control, _))| *control < 0)
                    .map(|(index, _)| index)
                    .collect();
                assert_eq!(negative, vec![records.len() - 1]);
                let chunked = write_realtime_chunks(&volume, &options).unwrap();
                assert!(chunked.concatenated() == sink);
            }
            if gzip {
                // The gzip wrapper holds exactly the unwrapped file.
                let mut unwrapped = options.clone();
                unwrapped.gzip = false;
                let plain = write_volume(&volume, &unwrapped).unwrap();
                let mut inflated = Vec::new();
                std::io::Read::read_to_end(
                    &mut flate2::read::GzDecoder::new(sink.as_slice()),
                    &mut inflated,
                )
                .unwrap();
                assert!(inflated == plain, "{compression:?}");
            }
        }
    });

    /// A sink that takes `room` bytes, then fails.
    struct Failing {
        room: usize,
    }
    impl std::io::Write for Failing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.room == 0 {
                return Err(std::io::Error::other("disk full"));
            }
            let taken = buf.len().min(self.room);
            self.room -= taken;
            Ok(taken)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Failing { room: 100_000 };
    match write_volume_to(&volume, &WriteOptions::default(), &mut sink) {
        Err(WriteError::Io(err)) => assert_eq!(err.to_string(), "disk full"),
        other => panic!("{other:?}"),
    }
}
