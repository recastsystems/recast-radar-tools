//! Level II writer round trips on real Level II files: decode, write, decode
//! again, and compare.
//!
//! - Message 31 volumes (2008 to 2026, WSR-88D and TDWR): with the source's metadata ([`SourceMetadata`]) every gate
//!   code, every ray and sweep value and the volume-level model come back
//!   identical, for uncompressed records, LDM bzip2 records and a gzip
//!   wrapper; so does the NEXRAD metadata (messages 2, 3, 5, 13, 15, 18 and
//!   32, and the per-sweep VOL, ELV and RAD blocks) whenever the source's
//!   metadata record could be carried over. Without the source metadata the
//!   volume still comes back identical apart from its provenance.
//! - Message 1 volumes (1991 to 2005): Message 31 cannot hold the Doppler
//!   gates before the radar (from -375 m), so they are written with
//!   [`WriteOptions::drop_negative_range_gates`]; every gate written comes
//!   back identical, at the same range.

mod common;

use recast_radar_core::model::{
    AttrValue, Field, FieldData, Location, RowRef, SourceFormat, Sweep, Volume,
};
use recast_radar_io_nexrad::messages;
use recast_radar_io_nexrad::write::{
    Compression, DataMessage, SourceMetadata, WriteOptions, WriteSummary, data_messages,
    rewrite_level2, write_volume_with_source,
};
use recast_radar_io_nexrad::{NexradMetadata, NexradVolume, read_volume_with_metadata};
use recast_radar_testdata::Format;

use common::{assert_checked_every_available, load};

/// Level II manifest entries with radials, and whether they are Message 1
/// volumes.
fn sources() -> Vec<(&'static str, bool)> {
    recast_radar_testdata::manifest()
        .files
        .iter()
        .filter(|entry| entry.format == Format::NexradLevel2)
        .filter(|entry| {
            !entry.tags.iter().any(|tag| {
                tag == "edge:status-only" || tag == "file:mdm" || tag == "fuzz-regression"
            })
        })
        .map(|entry| {
            let message_1 = entry.tags.iter().any(|tag| tag == "msg:1");
            (entry.id.as_str(), message_1)
        })
        .collect()
}

/// Decode `bytes` with metadata, the raw metadata record and the data
/// records' non-radial messages.
fn decode(id: &str, bytes: &[u8]) -> (NexradVolume, Vec<u8>, Vec<DataMessage>) {
    let decoded = read_volume_with_metadata(bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
    let record = messages::metadata_record(bytes)
        .unwrap_or_else(|e| panic!("{id}: metadata record: {e}"))
        .into_owned();
    let messages = data_messages(bytes).unwrap_or_else(|e| panic!("{id}: data messages: {e}"));
    (decoded, record, messages)
}

fn options(compression: Compression, gzip: bool) -> WriteOptions {
    let mut options = WriteOptions::default();
    options.compression = compression;
    options.gzip = gzip;
    options
}

/// f32 slices equal bit for bit (NaN equals NaN).
fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Half of one Message 5 angle code (Table III-A: 360/65536 degrees).
const HALF_ANGLE_CODE_DEG: f32 = 180.0 / 65_536.0;

/// Every item of two sweeps except the fields, which the callers compare.
/// `angle_tolerance` is 0, or half an angle code when the source has no
/// Message 5 and the written one quantises the first ray's elevation.
fn assert_same_sweep_items(label: &str, index: usize, a: &Sweep, b: &Sweep, angle_tolerance: f32) {
    let Sweep {
        sweep_number,
        sweep_mode,
        follow_mode,
        prt_mode,
        polarization_mode,
        polarization_sequence,
        fixed_angle_deg,
        target_scan_rate_deg_per_s,
        rays_are_indexed,
        rays_angle_resolution_deg,
        qc_procedures,
        rays,
        range,
        ray_vars,
        monitoring,
        platform_track,
        extra_vars,
        other,
        fields: _,
        elevation_number,
        complete,
    } = a;
    let at = format!("{label}: sweep {index}");
    assert_eq!(*sweep_number, b.sweep_number, "{at}: sweep_number");
    assert_eq!(*sweep_mode, b.sweep_mode, "{at}: sweep_mode");
    assert_eq!(*follow_mode, b.follow_mode, "{at}: follow_mode");
    assert_eq!(*prt_mode, b.prt_mode, "{at}: prt_mode");
    assert_eq!(
        *polarization_mode, b.polarization_mode,
        "{at}: polarization_mode"
    );
    assert_eq!(*polarization_sequence, b.polarization_sequence, "{at}");
    if angle_tolerance == 0.0 {
        assert_eq!(
            fixed_angle_deg.to_bits(),
            b.fixed_angle_deg.to_bits(),
            "{at}: fixed angle"
        );
    } else {
        assert!(
            (fixed_angle_deg - b.fixed_angle_deg).abs() <= angle_tolerance,
            "{at}: fixed angle {fixed_angle_deg} vs {}",
            b.fixed_angle_deg
        );
    }
    assert_eq!(
        *target_scan_rate_deg_per_s, b.target_scan_rate_deg_per_s,
        "{at}"
    );
    assert_eq!(*rays_are_indexed, b.rays_are_indexed, "{at}");
    assert_eq!(
        *rays_angle_resolution_deg, b.rays_angle_resolution_deg,
        "{at}"
    );
    assert_eq!(*qc_procedures, b.qc_procedures, "{at}");
    assert_eq!(rays.time_s, b.rays.time_s, "{at}: ray times");
    assert!(
        same_bits(&rays.azimuth_deg, &b.rays.azimuth_deg),
        "{at}: azimuths"
    );
    assert!(
        same_bits(&rays.elevation_deg, &b.rays.elevation_deg),
        "{at}: elevations"
    );
    assert_eq!(*range, b.range, "{at}: range");
    assert_eq!(
        format!("{ray_vars:?}"),
        format!("{:?}", b.ray_vars),
        "{at}: ray variables"
    );
    assert_eq!(*monitoring, b.monitoring, "{at}");
    assert_eq!(*platform_track, b.platform_track, "{at}");
    assert_eq!(*extra_vars, b.extra_vars, "{at}");
    assert_eq!(*other, b.other, "{at}");
    assert_eq!(
        *elevation_number, b.elevation_number,
        "{at}: elevation number"
    );
    assert_eq!(*complete, b.complete, "{at}");
}

/// The header version the writer gives a source's: its own, except that
/// `AR2V0001` (the Message 1 version, which KVWX 2008 carries over Message
/// 31 radials) becomes `AR2V0006`, keeping the volume number.
fn written_version(source: &str) -> String {
    match source.strip_prefix("AR2V0001") {
        Some(extension) => format!("AR2V0006{extension}"),
        None => source.to_owned(),
    }
}

/// The metadata the writer gives back for `source`: the same, except the
/// radial status codes that place a radial in the written volume, which
/// follow the written sweeps (a trimmed source has no end of volume): 3
/// opens the volume, 2 and 4 end cuts and the volume, a cut start is the
/// source's 0 or 5, and a mid-cut radial keeps any code but a start or end.
fn with_written_statuses(source: &NexradMetadata, sweeps: usize) -> NexradMetadata {
    let mut expected = source.clone();
    for entry in expected.per_sweep_elevation_data.iter_mut().flatten() {
        let index = entry.sweep_index;
        let rays = entry.radials.len();
        for (ray, radial) in entry.radials.iter_mut().enumerate() {
            let code = radial.radial_status_code;
            let last_sweep = index + 1 == sweeps;
            radial.radial_status_code = match (ray == 0, index == 0, ray + 1 == rays) {
                (true, true, _) => 3,
                (true, false, _) if matches!(code, 0 | 5) => code,
                (true, false, _) => {
                    if last_sweep {
                        5
                    } else {
                        0
                    }
                }
                (false, _, true) => {
                    if last_sweep {
                        4
                    } else {
                        2
                    }
                }
                _ if matches!(code, 0 | 2 | 3 | 4 | 5) => 1,
                _ => code,
            };
        }
    }
    expected
}

/// Equal `Debug` text (NaN fields compare equal to themselves); on a
/// mismatch, the text around the first difference.
fn assert_same_debug<T: std::fmt::Debug>(a: &T, b: &T, what: &str) {
    let (a, b) = (format!("{a:?}"), format!("{b:?}"));
    if a != b {
        let at = a
            .bytes()
            .zip(b.bytes())
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        let window = |text: &str| {
            let start = text.floor_char_boundary(at.saturating_sub(300));
            let end = text.ceil_char_boundary((at + 120).min(text.len()));
            text[start..end].to_owned()
        };
        panic!(
            "{what}: differs at byte {at}
  source: ...{}...
 written: ...{}...",
            window(&a),
            window(&b)
        );
    }
}

/// The summary's notes on the metadata record (replaced or synthesised
/// messages), the note on a relabelled header version aside.
fn record_notes(summary: &WriteSummary) -> Vec<&String> {
    summary
        .notes
        .iter()
        .filter(|note| !note.contains("header version"))
        .collect()
}

/// The volume-level model of two volumes, sweeps and provenance aside.
fn assert_same_volume_items(label: &str, a: &Volume, b: &Volume) {
    let Volume {
        attrs,
        volume_number,
        time_reference,
        time_coverage,
        location,
        platform_type,
        instrument_type,
        primary_axis,
        status_str,
        scan,
        radar_parameters,
        radar_calibration,
        georeferencing_correction,
        extra_vars,
        provenance: _,
        simulation,
        sweeps: _,
    } = a;
    assert_eq!(*attrs, b.attrs, "{label}: global attributes");
    assert_eq!(*volume_number, b.volume_number, "{label}");
    assert_eq!(*time_reference, b.time_reference, "{label}: time reference");
    assert_eq!(*time_coverage, b.time_coverage, "{label}: time coverage");
    assert_eq!(*location, b.location, "{label}: location");
    assert_eq!(*platform_type, b.platform_type, "{label}");
    assert_eq!(*instrument_type, b.instrument_type, "{label}");
    assert_eq!(*primary_axis, b.primary_axis, "{label}");
    assert_eq!(*status_str, b.status_str, "{label}");
    assert_eq!(*scan, b.scan, "{label}: scan");
    assert_eq!(
        *radar_parameters, b.radar_parameters,
        "{label}: radar parameters"
    );
    assert_eq!(*radar_calibration, b.radar_calibration, "{label}");
    assert_eq!(
        *georeferencing_correction, b.georeferencing_correction,
        "{label}"
    );
    assert_eq!(*extra_vars, b.extra_vars, "{label}");
    assert_eq!(*simulation, b.simulation, "{label}");
    assert_eq!(a.sweeps.len(), b.sweeps.len(), "{label}: sweeps");
}

/// Message 31 source: every item of the decoded volume comes back.
fn assert_identical(label: &str, a: &Volume, b: &Volume, angle_tolerance: f32) {
    assert_same_volume_items(label, a, b);
    for (index, (sa, sb)) in a.sweeps.iter().zip(&b.sweeps).enumerate() {
        assert_same_sweep_items(label, index, sa, sb, angle_tolerance);
        assert_eq!(
            sa.fields.len(),
            sb.fields.len(),
            "{label}: sweep {index} fields"
        );
        for (fa, fb) in sa.fields.iter().zip(&sb.fields) {
            assert!(
                fa == fb,
                "{label}: sweep {index} field {} differs (data equal: {}, gates {:?} / {:?}, absent rows {} / {})",
                fa.name,
                fa.data == fb.data,
                fa.gates,
                fb.gates,
                fa.absent_rows.len(),
                fb.absent_rows.len()
            );
        }
    }
    let (pa, pb) = (&a.provenance, &b.provenance);
    assert_eq!(pa.source_format, pb.source_format, "{label}");
    assert_eq!(
        pa.source_version.as_deref().map(written_version),
        pb.source_version,
        "{label}: header version"
    );
    // Message counts can differ: without the source's data messages, the
    // non-radial messages inside its data records (mid-volume Message 2
    // updates) are not written again.
    assert_eq!(
        pa.decode.decoded_ray_count, pb.decode.decoded_ray_count,
        "{label}"
    );
}

/// The written and re-decoded pair of one write.
fn roundtrip(
    id: &str,
    source: &NexradVolume,
    record: Option<&[u8]>,
    with_metadata: bool,
    messages: &[DataMessage],
    options: &WriteOptions,
) -> (NexradVolume, WriteSummary) {
    let context = SourceMetadata {
        metadata: with_metadata.then_some(&source.metadata),
        metadata_record: record,
        data_messages: messages,
    };
    let (bytes, summary) = write_volume_with_source(&source.volume, context, options)
        .unwrap_or_else(|e| panic!("{id}: write: {e}"));
    assert_eq!(summary.bytes, bytes.len(), "{id}");
    let again = read_volume_with_metadata(&bytes).unwrap_or_else(|e| panic!("{id}: reread: {e}"));
    // The data records' non-radial messages come back where they were
    // (checked on the LDM variant: every variant holds the same records).
    if options.compression == Compression::Bzip2LdmRecords && !options.gzip {
        let written = data_messages(&bytes).unwrap_or_else(|e| panic!("{id}: data messages: {e}"));
        assert!(written == messages, "{id}: data messages differ");
    }
    let expected = match (options.gzip, options.compression) {
        (true, _) => "gzip",
        (false, Compression::None) => "uncompressed",
        (false, _) => "bzip2-blocks",
    };
    assert_eq!(
        again.volume.provenance.compression.as_deref(),
        Some(expected),
        "{id}: written compression"
    );
    (again, summary)
}

#[test]
fn message_31_volumes_come_back_identical() {
    let mut checked = 0;
    let mut applied = Vec::new();
    let mut records_carried = 0;
    for (id, message_1) in sources() {
        if message_1 {
            continue;
        }
        applied.push(vec![id]);
        let Some(bytes) = load(id) else { continue };
        checked += 1;
        let (source, record, messages) = decode(id, &bytes);
        assert_eq!(
            source.volume.provenance.source_format,
            SourceFormat::NexradLevel2
        );

        let small = bytes.len() < 1_500_000;
        // Without a Message 5 that decodes, fixed angles are the first
        // rays' elevations, which the synthesised Message 5 quantises.
        let angle_tolerance = if source.metadata.vcp.is_some() {
            0.0
        } else {
            HALF_ANGLE_CODE_DEG
        };
        let mut variants = vec![
            (Compression::Bzip2LdmRecords, false),
            (Compression::None, false),
        ];
        if small {
            variants.push((Compression::Bzip2LdmRecords, true));
            variants.push((Compression::None, true));
        }
        for (compression, gzip) in variants {
            let label = format!("{id} ({compression:?}, gzip {gzip})");
            let options = options(compression, gzip);
            let (again, summary) = roundtrip(id, &source, Some(&record), true, &messages, &options);
            assert!(
                summary.skipped_fields.is_empty(),
                "{label}: {:?}",
                summary.skipped_fields
            );
            assert!(
                summary
                    .moments
                    .iter()
                    .all(|m| m.exact && m.max_abs_error == 0.0)
            );
            let same_record = record_notes(&summary).is_empty();
            let tolerance = if same_record { 0.0 } else { angle_tolerance };
            assert_identical(&label, &source.volume, &again.volume, tolerance);
            if same_record {
                // NaN fields (message 3) make `==` false on equal bytes.
                assert_same_debug(
                    &with_written_statuses(&source.metadata, source.volume.sweeps.len()),
                    &again.metadata,
                    &format!("{label}: NEXRAD metadata"),
                );
                if compression == Compression::Bzip2LdmRecords && !gzip {
                    records_carried += 1;
                }
            } else {
                assert_same_debug(
                    &with_written_statuses(&source.metadata, source.volume.sweeps.len())
                        .per_sweep_elevation_data,
                    &again.metadata.per_sweep_elevation_data,
                    &format!("{label}: per-sweep constant blocks"),
                );
            }
        }

        // Without the source's metadata: the same volume, apart from the
        // provenance of the synthesised metadata record.
        let bare = options(Compression::Bzip2LdmRecords, false);
        let (again, summary) = roundtrip(id, &source, None, false, &[], &bare);
        assert!(
            record_notes(&summary).is_empty(),
            "{id}: {:?}",
            summary.notes
        );
        assert_identical(
            &format!("{id} (bare)"),
            &source.volume,
            &again.volume,
            angle_tolerance,
        );

        // The convenience entry point does the same as the steps above.
        if small {
            let (rewritten, _) = rewrite_level2(&bytes, &WriteOptions::default())
                .unwrap_or_else(|e| panic!("{id}: rewrite: {e}"));
            let (direct, _) = write_volume_with_source(
                &source.volume,
                SourceMetadata {
                    metadata: Some(&source.metadata),
                    metadata_record: Some(&record),
                    data_messages: &messages,
                },
                &WriteOptions::default(),
            )
            .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(rewritten == direct, "{id}: rewrite_level2 differs");
        }
    }
    assert_checked_every_available("message 31 round trips", checked, &applied);
    if checked > 0 {
        assert!(records_carried > 0, "no metadata record was carried over");
    }
}

/// Codes of one row as `u16`.
fn codes(field: &Field, ray: usize) -> Vec<u16> {
    match field.row(ray) {
        Some(RowRef::U8(values)) => values.iter().map(|v| u16::from(*v)).collect(),
        Some(RowRef::U16(values)) => values.to_vec(),
        other => panic!("{}: row {ray} is {other:?}", field.name),
    }
}

/// The radar's position for a Message 1 volume, which carries none: the
/// VOL block of a Message 31 file of the same radar (the committed KTLX
/// 2024 and KLIX 2021 trims). `None` for other sites.
fn site_location(id: &str) -> Option<Location> {
    let same_radar = if id.starts_with("l2-ktlx-") {
        "l2-ktlx-20240315-000217-trim"
    } else if id.starts_with("l2-klix-") {
        "l2-klix-20210829-180425-trim"
    } else {
        return None;
    };
    let bytes = load(same_radar)?;
    let decoded = read_volume_with_metadata(&bytes).unwrap_or_else(|e| panic!("{same_radar}: {e}"));
    Some(decoded.volume.location)
}

#[test]
fn message_1_volumes_keep_every_written_gate() {
    let mut checked = 0;
    let mut applied = Vec::new();
    for (id, message_1) in sources() {
        if !message_1 {
            continue;
        }
        applied.push(vec![id]);
        let Some(bytes) = load(id) else { continue };
        checked += 1;
        let (mut source, record, messages) = decode(id, &bytes);
        let mut options = options(Compression::Bzip2LdmRecords, false);
        // Message 1 carries no site position, which Message 31 needs: the
        // writer refuses the volume until the radar's position is given.
        assert!(
            matches!(
                write_volume_with_source(&source.volume, SourceMetadata::default(), &options),
                Err(recast_radar_io_nexrad::WriteError::MissingLocation(_))
            ),
            "{id}: a volume without a site position must be refused"
        );
        let Some(location) = site_location(id) else {
            panic!("{id}: no Message 31 file of the same radar for its position")
        };
        source.volume.location = location;
        // ARCHIVE2 headers from before 2004 may carry no ICAO.
        if source.volume.attrs.instrument_name.trim().is_empty() {
            let refused =
                write_volume_with_source(&source.volume, SourceMetadata::default(), &options);
            assert!(
                matches!(
                    refused,
                    Err(recast_radar_io_nexrad::WriteError::InvalidSiteId(_))
                ),
                "{id}: a blank site identifier must be refused"
            );
            options.icao = Some("KTLX".to_owned());
        }
        // The Doppler gates from -375 m are refused unless dropped.
        let before_radar = source.volume.sweeps.iter().any(|sweep| {
            sweep.fields.iter().any(|field| {
                field
                    .native_geometry(&sweep.range)
                    .is_some_and(|(first, _)| first < -0.5)
            })
        });
        let refused = write_volume_with_source(&source.volume, SourceMetadata::default(), &options);
        assert_eq!(
            matches!(
                refused,
                Err(recast_radar_io_nexrad::WriteError::Geometry { .. })
            ),
            before_radar,
            "{id}: negative-range gates must be refused by default"
        );
        options.drop_negative_range_gates = true;
        let (again, summary) = roundtrip(id, &source, Some(&record), true, &messages, &options);
        let volume = &source.volume;
        assert_eq!(volume.sweeps.len(), again.volume.sweeps.len(), "{id}");
        assert_eq!(volume.time_reference, again.volume.time_reference, "{id}");
        let mut dropped_moments = 0;
        for (index, (sa, sb)) in volume.sweeps.iter().zip(&again.volume.sweeps).enumerate() {
            assert_eq!(sa.rays.time_s, sb.rays.time_s, "{id}: sweep {index} times");
            assert!(
                same_bits(&sa.rays.azimuth_deg, &sb.rays.azimuth_deg),
                "{id}"
            );
            assert!(
                same_bits(&sa.rays.elevation_deg, &sb.rays.elevation_deg),
                "{id}"
            );
            assert_eq!(sa.elevation_number, sb.elevation_number, "{id}");
            assert_eq!(
                sa.fixed_angle_deg.to_bits(),
                sb.fixed_angle_deg.to_bits(),
                "{id}"
            );
            for fa in &sa.fields {
                let fb = sb
                    .field(&fa.name)
                    .unwrap_or_else(|| panic!("{id}: sweep {index} lost {}", fa.name));
                let report = summary
                    .moments
                    .iter()
                    .find(|m| m.sweep == index && m.field == fa.name)
                    .unwrap_or_else(|| panic!("{id}: no report for {}", fa.name));
                let skip = report.dropped_gates;
                dropped_moments += usize::from(skip > 0);
                let (first_a, spacing_a) = fa.native_geometry(&sa.range).unwrap();
                let (first_b, spacing_b) = fb.native_geometry(&sb.range).unwrap();
                assert_eq!(spacing_a, spacing_b, "{id}: {} spacing", fa.name);
                assert_eq!(
                    first_a + skip as f64 * spacing_a,
                    first_b,
                    "{id}: {} first gate",
                    fa.name
                );
                assert!(first_b >= 0.0, "{id}");
                assert_eq!(
                    fa.ngates as usize - skip,
                    fb.ngates as usize,
                    "{id}: {}",
                    fa.name
                );
                assert_eq!(
                    fa.absent_rows, fb.absent_rows,
                    "{id}: {} absent rows",
                    fa.name
                );
                assert_eq!(
                    fa.data.coding(),
                    fb.data.coding(),
                    "{id}: {} coding",
                    fa.name
                );
                for ray in 0..sa.nrays() {
                    assert_eq!(
                        codes(fa, ray)[skip..],
                        codes(fb, ray)[..],
                        "{id}: sweep {index} {} ray {ray}",
                        fa.name
                    );
                }
                // Message 1 has no TOVER, SNR threshold or recombination;
                // Message 31 always does, written as zero.
                assert!(fa.attrs.other.is_empty(), "{id}");
                let extras: Vec<(&str, f64)> = fb
                    .attrs
                    .other
                    .iter()
                    .map(|(key, value)| match value {
                        AttrValue::Scalar(scalar) => (key.as_ref(), scalar.as_f64()),
                        other => panic!("{id}: {key} = {other:?}"),
                    })
                    .collect();
                assert_eq!(
                    extras,
                    [
                        ("nexrad_tover_db", 0.0),
                        ("nexrad_snr_threshold_db", 0.0),
                        ("nexrad_recombination", 0.0)
                    ],
                    "{id}"
                );
            }
        }
        assert_eq!(
            dropped_moments > 0,
            before_radar,
            "{id}: gates before the radar"
        );
    }
    assert_checked_every_available("message 1 round trips", checked, &applied);
}

/// Raw field codes are copied: the writer never re-quantises NEXRAD codes,
/// whatever the policy.
#[test]
fn standard_quantisation_copies_nexrad_codes() {
    let id = "l2-ktlx-20240315-000217-trim";
    let Some(bytes) = load(id) else { return };
    let (source, _, _) = decode(id, &bytes);
    let mut options = WriteOptions::default();
    options.quantization = recast_radar_io_nexrad::write::Quantization::Standard;
    let (again, summary) = roundtrip(id, &source, None, false, &[], &options);
    assert_identical(id, &source.volume, &again.volume, 0.0);
    for report in &summary.moments {
        let field = source.volume.sweeps[report.sweep]
            .field(&report.field)
            .unwrap();
        let (scale, offset) = match &field.data {
            FieldData::U8 { coding, .. } => (coding.transform, 8),
            FieldData::U16 { coding, .. } => (coding.transform, 16),
            other => panic!("{id}: {}", other.dtype()),
        };
        assert_eq!(report.word_size, offset, "{id}: {}", report.field);
        assert_eq!(
            recast_radar_core::model::LinearTransform::IcdScaleOffset {
                scale: report.scale,
                offset: report.offset
            },
            scale,
            "{id}: {}",
            report.field
        );
    }
}

/// A volume that leaves out or reorders its source's sweeps, written with
/// the source's metadata: readers number the cuts 1 to n in the written
/// order and index Message 5 by that number, so each written sweep must
/// keep its source cut's Message 5 entry (and with it its fixed angle) and
/// its source sweep's constant blocks.
#[test]
fn sweep_subsets_keep_their_cuts_and_constant_blocks() {
    let mut cases = 0;
    for (id, selections) in [
        ("l2-ktlx-20240315-000217-trim", vec![vec![1], vec![1, 0]]),
        (
            "l2-ktlx-20240315-000217",
            vec![vec![0, 2, 3, 4], vec![5, 1]],
        ),
    ] {
        let Some(bytes) = load(id) else { continue };
        let (source, record, _) = decode(id, &bytes);
        let Some(source_vcp) = source.metadata.vcp.as_ref() else {
            panic!("{id}: no Message 5");
        };
        let blocks = source
            .metadata
            .per_sweep_elevation_data
            .as_deref()
            .unwrap_or_else(|| panic!("{id}: no constant blocks"));
        for selection in selections {
            cases += 1;
            let label = format!("{id} sweeps {selection:?}");
            let mut volume = source.volume.clone();
            volume.sweeps = selection
                .iter()
                .map(|&index| source.volume.sweeps[index].clone())
                .collect();
            let context = SourceMetadata {
                metadata: Some(&source.metadata),
                metadata_record: Some(&record),
                data_messages: &[],
            };
            let (bytes, summary) =
                write_volume_with_source(&volume, context, &WriteOptions::default())
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(
                summary
                    .notes
                    .iter()
                    .any(|note| note.contains("listed again in the order of the written sweeps")),
                "{label}: {:?}",
                summary.notes
            );
            let again =
                read_volume_with_metadata(&bytes).unwrap_or_else(|e| panic!("{label}: {e}"));
            let vcp = again
                .metadata
                .vcp
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: Message 5"));
            assert_eq!(vcp.pattern_number, source_vcp.pattern_number, "{label}");
            assert_eq!(usize::from(vcp.number_of_cuts), selection.len(), "{label}");
            let again_blocks = again
                .metadata
                .per_sweep_elevation_data
                .as_deref()
                .unwrap_or_default();
            for (out, &index) in selection.iter().enumerate() {
                let (a, b) = (&source.volume.sweeps[index], &again.volume.sweeps[out]);
                let number = usize::from(a.elevation_number.unwrap_or_else(|| panic!("{label}")));
                assert_eq!(
                    b.elevation_number,
                    Some(out as u16 + 1),
                    "{label}: sweep {out}"
                );
                assert_eq!(
                    format!("{:?}", vcp.cuts[out]),
                    format!("{:?}", source_vcp.cuts[number - 1]),
                    "{label}: Message 5 cut of sweep {out}"
                );
                assert_eq!(
                    a.fixed_angle_deg.to_bits(),
                    b.fixed_angle_deg.to_bits(),
                    "{label}: fixed angle of sweep {out}"
                );
                let source_blocks = blocks
                    .iter()
                    .find(|blocks| blocks.sweep_index == index)
                    .unwrap_or_else(|| panic!("{label}: source blocks of sweep {index}"));
                let written = again_blocks
                    .iter()
                    .find(|blocks| blocks.sweep_index == out)
                    .unwrap_or_else(|| panic!("{label}: written blocks of sweep {out}"));
                assert_eq!(
                    format!(
                        "{:?}",
                        (
                            &source_blocks.volume,
                            &source_blocks.elevation,
                            &source_blocks.radial
                        )
                    ),
                    format!(
                        "{:?}",
                        (&written.volume, &written.elevation, &written.radial)
                    ),
                    "{label}: constant blocks of sweep {out}"
                );
                for field in &a.fields {
                    assert!(
                        b.field(&field.name).is_some_and(|f| f.data == field.data),
                        "{label}: sweep {out} {}",
                        field.name
                    );
                }
            }
        }
    }
    assert!(cases >= 2, "the committed trim applies");
}

/// Option overrides reach a carried-over metadata record: the VCP of
/// Messages 2 and 5 and the Message 18 site name follow the written VCP and
/// site, each change reported.
#[test]
fn option_overrides_reach_the_carried_metadata_record() {
    let id = "l2-ktlx-20240315-000217-trim";
    let Some(bytes) = load(id) else { return };
    let mut options = WriteOptions::default();
    options.vcp = Some(35);
    options.icao = Some("KXYZ".to_owned());
    let (written, summary) =
        rewrite_level2(&bytes, &options).unwrap_or_else(|e| panic!("{id}: {e}"));
    for message in ["Message 5", "Message 2", "Message 18"] {
        assert!(
            summary.notes.iter().any(|note| note.contains(message)),
            "{id}: no note on {message}: {:?}",
            summary.notes
        );
    }
    let again = read_volume_with_metadata(&written).unwrap_or_else(|e| panic!("{id}: {e}"));
    assert_eq!(&written[20..24], b"KXYZ");
    assert_eq!(again.volume.attrs.instrument_name, "KXYZ");
    assert_eq!(again.volume.scan.vcp_pattern, Some(35));
    let metadata = &again.metadata;
    assert_eq!(
        metadata.vcp.as_ref().map(|vcp| vcp.pattern_number),
        Some(35)
    );
    assert_eq!(
        metadata
            .rda_status
            .as_ref()
            .and_then(|status| status.volume_coverage_pattern().pattern()),
        Some(35)
    );
    assert_eq!(
        metadata
            .adaptation
            .as_ref()
            .map(|adaptation| adaptation.site_name.as_str()),
        Some("KXYZ")
    );
    // Without overrides nothing changes and nothing is reported.
    let (_, summary) =
        rewrite_level2(&bytes, &WriteOptions::default()).unwrap_or_else(|e| panic!("{id}: {e}"));
    assert!(summary.notes.is_empty(), "{id}: {:?}", summary.notes);
}

/// The radar identifier and azimuth resolution code of every radial as
/// recorded, in file order.
fn radial_ids_and_resolutions(id: &str, bytes: &[u8]) -> Vec<([u8; 4], u8)> {
    let decoded = read_volume_with_metadata(bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
    decoded
        .metadata
        .per_sweep_elevation_data
        .unwrap_or_default()
        .iter()
        .flat_map(|entry| entry.radials.iter())
        .map(|radial| (radial.radar_identifier, radial.azimuth_resolution_code))
        .collect()
}

/// KVWX 2008's Message 31 radials carry a blank radar identifier (its
/// volume header names KVWX). A re-encoding keeps every radial's own
/// identifier and azimuth resolution code; a site set in the options
/// (`WriteOptions::icao`) goes into every radial instead.
#[test]
fn radials_keep_their_identifier_and_resolution_code() {
    let id = "l2-kvwx-20080415-235337";
    let Some(bytes) = load(id) else { return };
    let source = radial_ids_and_resolutions(id, &bytes);
    assert_eq!(source.len(), 2500, "{id}");
    assert!(source.iter().all(|(site, _)| site == b"    "), "{id}");
    assert!(source.iter().all(|(_, code)| *code == 2), "{id}");
    let decoded = read_volume_with_metadata(&bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
    assert_eq!(decoded.volume.attrs.instrument_name, "KVWX");

    let (written, _) =
        rewrite_level2(&bytes, &WriteOptions::default()).unwrap_or_else(|e| panic!("{id}: {e}"));
    assert_eq!(&written[20..24], b"KVWX");
    assert_eq!(radial_ids_and_resolutions(id, &written), source);

    let mut options = WriteOptions::default();
    options.icao = Some("KXYZ".to_owned());
    let (written, _) = rewrite_level2(&bytes, &options).unwrap_or_else(|e| panic!("{id}: {e}"));
    let renamed = radial_ids_and_resolutions(id, &written);
    assert_eq!(renamed.len(), source.len());
    assert!(
        renamed
            .iter()
            .all(|(site, code)| site == b"KXYZ" && *code == 2)
    );
}
