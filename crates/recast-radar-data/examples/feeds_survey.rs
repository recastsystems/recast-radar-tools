//! Real-feed survey: decode radar files through the shared format router and
//! print one JSON object per decoded volume on stdout.
//!
//! Every file goes through `recast_radar_io::read_supported_volume_with_metadata`
//! (JMA tars through `recast_radar_io_jma::read_jma_tar_volumes`, which
//! decodes every station, or one with `--jma-site`). Each line carries the
//! volume summary (site, location, times, sweeps, fields with their value
//! ranges) or the decode error. For NEXRAD Level II input the line also
//! carries the archive structure read with `recast_radar_io_nexrad::messages`:
//! volume header, compression layout (LDM record sizes, frames per record),
//! message types present, and per-cut Message 31 details (moments, gate
//! geometry, block presence, radial status codes). `docs/testdata/feeds-survey.md`
//! was built from this output.
//!
//! An ODIM_H5 Cartesian `IMAGE` (IMGW's POLRAD CMAX products) is not a volume,
//! so the router refuses it; for such a file the line also carries the grid
//! read with `recast_radar_io_odim::odim_cartesian::decode_odim_h5_cartesian_max`
//! (`odim_image`: site, product, quantity, time, grid size and spacing, and
//! the valid-cell count and value range).
//!
//! The program reads local files only; it never touches the network.
//!
//! Usage:
//!   cargo run --release -p recast-radar-data --example feeds_survey -- [--jma-site ID] FILE...

use std::collections::BTreeMap;
use std::path::Path;

use recast_radar_core::model::{Field, RangeCoord, Sweep, Volume};
use recast_radar_io::{FormatMetadata, SupportedVolumeFormat};
use recast_radar_io_nexrad::NexradMetadata;
use recast_radar_io_nexrad::messages::rda_status::RdaStatus;
use recast_radar_io_nexrad::messages::{self, MessageBody, RawMessages};
use serde_json::{Value, json};

/// Largest input the survey reads, and the largest expansion it allows.
const MAX_BYTES: usize = 512 * 1024 * 1024;

fn main() {
    let mut jma_site: Option<String> = None;
    let mut merge_label: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--jma-site" {
            jma_site = args.next();
        } else if arg == "--merge" {
            merge_label = args.next();
        } else {
            files.push(arg);
        }
    }
    if files.is_empty() {
        eprintln!("usage: feeds_survey [--jma-site ID] [--merge LABEL] FILE...");
        std::process::exit(2);
    }
    if let Some(label) = merge_label {
        println!("{}", survey_merged(&label, &files, jma_site.as_deref()));
        return;
    }
    for file in &files {
        for line in survey_file(Path::new(file), jma_site.as_deref()) {
            println!("{line}");
        }
    }
}

/// Decode every file (one frame's parts, in plan order), merge them the way
/// the international poller does (`merge_volumes`, first part as base) and
/// summarize the merged volume.
fn survey_merged(label: &str, files: &[String], jma_site: Option<&str>) -> Value {
    let mut volumes: Vec<Volume> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for file in files {
        let raw = match std::fs::read(file) {
            Ok(raw) => raw,
            Err(err) => {
                errors.push(format!("{file}: read: {err}"));
                continue;
            }
        };
        let decoded = if recast_radar_io_jma::looks_like_jma_tar_bytes(&raw) {
            recast_radar_io_jma::read_jma_tar_volumes(&raw, jma_site)
                .map_err(|err| err.to_string())
                .and_then(|mut volumes| {
                    if volumes.is_empty() {
                        Err("no station decoded".to_owned())
                    } else {
                        Ok(volumes.swap_remove(0))
                    }
                })
        } else {
            recast_radar_io::read_supported_volume_bytes(&raw).map_err(|err| err.to_string())
        };
        match decoded {
            Ok(volume) => volumes.push(volume),
            Err(err) => errors.push(format!("{file}: {err}")),
        }
    }
    let parts = volumes.len();
    if volumes.is_empty() {
        return json!({"label": label, "files": files.len(), "ok": false, "errors": errors});
    }
    match recast_radar_core::model::merge_volumes(volumes) {
        Ok((volume, report)) => json!({
            "label": label,
            "files": files.len(),
            "decoded_parts": parts,
            "ok": errors.is_empty(),
            "errors": errors,
            "merge": {
                "merged_fields": report.merged_fields,
                "skipped_geometry": report.skipped_geometry,
                "field_collisions": report.field_collisions,
            },
            "volume": volume_summary(&volume),
        }),
        Err(err) => json!({
            "label": label,
            "files": files.len(),
            "decoded_parts": parts,
            "ok": false,
            "errors": errors,
            "merge_error": err.to_string(),
        }),
    }
}

fn survey_file(path: &Path, jma_site: Option<&str>) -> Vec<Value> {
    let name = path.display().to_string();
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) => {
            return vec![json!({"file": name, "ok": false, "error": format!("read: {err}")})];
        }
    };
    if raw.len() > MAX_BYTES {
        return vec![
            json!({"file": name, "ok": false, "error": "file larger than the survey limit"}),
        ];
    }
    let gunzipped = if raw.starts_with(&[0x1f, 0x8b]) {
        match recast_radar_io_nexrad::gzip::inflate_gzip_members_limited(
            &raw,
            MAX_BYTES,
            "survey gzip",
        ) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                return vec![json!({"file": name, "ok": false, "error": format!("gzip: {err}")})];
            }
        }
    } else {
        None
    };
    let inner = gunzipped.as_deref().unwrap_or(&raw);
    let format = recast_radar_io::sniff_supported_volume_format(inner);
    let base = json!({
        "file": name,
        "size": raw.len(),
        "gzip_wrapped": gunzipped.is_some(),
        "zip_wrapped": raw.starts_with(&[0x50, 0x4b, 0x03, 0x04]),
        "inner_size": inner.len(),
        "sniffed": format!("{format:?}"),
        "magic": escape_prefix(inner, 8),
    });

    if format == SupportedVolumeFormat::JmaGrib2Tar {
        return match recast_radar_io_jma::read_jma_tar_volumes(inner, jma_site) {
            Ok(volumes) => volumes
                .iter()
                .map(|volume| merge(&base, json!({"ok": true, "volume": volume_summary(volume)})))
                .collect(),
            Err(err) => vec![merge(&base, json!({"ok": false, "error": err.to_string()}))],
        };
    }

    let mut level2_source = format == SupportedVolumeFormat::NexradLevel2;
    let mut line = match recast_radar_io::read_supported_volume_with_metadata(&raw) {
        Ok(decoded) => {
            // A ZIP-wrapped file sniffs as Level II here (the survey does not
            // unwrap ZIP); the decoded source format says what it was.
            level2_source = decoded.volume.provenance.source_format
                == recast_radar_core::model::SourceFormat::NexradLevel2;
            let mut line = merge(
                &base,
                json!({"ok": true, "volume": volume_summary(&decoded.volume)}),
            );
            if let FormatMetadata::Nexrad(metadata) = &decoded.metadata {
                line["nexrad_metadata"] = nexrad_metadata_summary(metadata);
            }
            line
        }
        Err(err) => {
            let mut line = merge(&base, json!({"ok": false, "error": err.to_string()}));
            if inner.starts_with(HDF5_MAGIC) {
                line["odim_image"] = odim_image_summary(inner);
            }
            line
        }
    };
    if level2_source {
        line["level2"] = level2_structure(inner);
    }
    vec![line]
}

const HDF5_MAGIC: &[u8] = &[0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a];

/// An ODIM_H5 Cartesian `MAX` image, which the volume router does not read.
fn odim_image_summary(bytes: &[u8]) -> Value {
    match recast_radar_io_odim::odim_cartesian::decode_odim_h5_cartesian_max(bytes) {
        Ok(grid) => {
            let values = grid.values();
            let valid: Vec<f64> = values
                .iter()
                .filter(|value| value.is_finite())
                .map(|&value| f64::from(value))
                .collect();
            let range = finite_range(valid.iter().copied());
            json!({
                "ok": true,
                "site": grid.site.id,
                "source": grid.site.source,
                "latitude_deg": grid.site.latitude_deg,
                "longitude_deg": grid.site.longitude_deg,
                "odim_version": grid.odim_version,
                "product": grid.product,
                "quantity": grid.quantity_code,
                "units": grid.units,
                "start": grid.start_time.to_rfc3339(),
                "end": grid.end_time.map(|time| time.to_rfc3339()),
                "width": grid.geometry.width,
                "height": grid.geometry.height,
                "x_spacing_m": grid.geometry.x_spacing_m,
                "y_spacing_m": grid.geometry.y_spacing_m,
                "valid": valid.len(),
                "total": values.len(),
                "min": range.map(|(low, _)| low),
                "max": range.map(|(_, high)| high),
            })
        }
        Err(err) => json!({"ok": false, "error": err.to_string()}),
    }
}

fn merge(base: &Value, extra: Value) -> Value {
    let mut out = base.clone();
    if let (Some(out_map), Value::Object(extra_map)) = (out.as_object_mut(), extra) {
        for (key, value) in extra_map {
            out_map.insert(key, value);
        }
    }
    out
}

fn escape_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .take(len)
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}

// ---------------------------------------------------------------------------
// Volume summary (any format)
// ---------------------------------------------------------------------------

fn volume_summary(volume: &Volume) -> Value {
    let sweeps: Vec<Value> = volume
        .sweeps
        .iter()
        .map(|sweep| sweep_summary(volume, sweep))
        .collect();
    let mut field_names: Vec<String> = Vec::new();
    for sweep in &volume.sweeps {
        for field in &sweep.fields {
            let name = field.name.to_string();
            if !field_names.contains(&name) {
                field_names.push(name);
            }
        }
    }
    json!({
        "instrument_name": volume.attrs.instrument_name,
        "site_name": volume.attrs.site_name,
        "source": volume.attrs.source,
        "wmo_id": volume.attrs.wmo.id,
        "latitude_deg": volume.location.latitude_deg,
        "longitude_deg": volume.location.longitude_deg,
        "altitude_m": volume.location.altitude_m,
        "time_reference": volume.time_reference.to_rfc3339(),
        "time_coverage_start": volume.time_coverage.as_ref().map(|t| t.start.to_rfc3339()),
        "time_coverage_end": volume.time_coverage.as_ref().map(|t| t.end.to_rfc3339()),
        "scan_name": volume.scan.name,
        "scan_id": volume.scan.id,
        "vcp_pattern": volume.scan.vcp_pattern,
        "source_format": format!("{:?}", volume.provenance.source_format),
        "source_version": volume.provenance.source_version,
        "compression": volume.provenance.compression,
        "frequency_hz": volume.radar_parameters.frequency_hz,
        "beam_width_h_deg": volume.radar_parameters.beam_width_h_deg,
        "decode": {
            "messages": volume.provenance.decode.message_count,
            "rays": volume.provenance.decode.decoded_ray_count,
            "skipped_messages": volume.provenance.decode.skipped_message_count,
        },
        "nsweeps": volume.sweeps.len(),
        "fields": field_names,
        "sweeps": sweeps,
    })
}

fn sweep_summary(volume: &Volume, sweep: &Sweep) -> Value {
    let times = finite_range(sweep.rays.time_s.iter().copied());
    let elevations = finite_range(sweep.rays.elevation_deg.iter().map(|v| f64::from(*v)));
    let (first_center_m, spacing_m) = match &sweep.range {
        RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ..
        } => (Some(*first_center_m), Some(*spacing_m)),
        RangeCoord::Explicit { centers_m } => {
            (centers_m.first().map(|c| f64::from(*c)), None::<f64>)
        }
    };
    let fields: Vec<Value> = sweep.fields.iter().map(field_summary).collect();
    json!({
        "number": sweep.sweep_number,
        "mode": sweep.sweep_mode.as_str(),
        "fixed_angle_deg": sweep.fixed_angle_deg,
        "nrays": sweep.nrays(),
        "ngates": sweep.range.ngates(),
        "first_gate_center_m": first_center_m,
        "gate_spacing_m": spacing_m,
        "elevation_min_deg": elevations.map(|r| r.0),
        "elevation_max_deg": elevations.map(|r| r.1),
        "first_azimuth_deg": sweep.rays.azimuth_deg.first(),
        "start": times.and_then(|r| volume.instant(r.0)).map(|t| t.to_rfc3339()),
        "end": times.and_then(|r| volume.instant(r.1)).map(|t| t.to_rfc3339()),
        "nyquist_mps": sweep.ray_vars.nyquist_velocity_mps.as_ref().and_then(|v| v.first().copied()),
        "unambiguous_range_m": sweep.ray_vars.unambiguous_range_m.as_ref().and_then(|v| v.first().copied()),
        "fields": fields,
    })
}

fn field_summary(field: &Field) -> Value {
    let (nrays, ngates) = field.shape();
    let mut valid = 0usize;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for ray in 0..nrays {
        for gate in 0..ngates {
            if let Some(value) = field.value(ray, gate)
                && value.is_finite()
            {
                valid += 1;
                min = min.min(value);
                max = max.max(value);
            }
        }
    }
    json!({
        "name": field.name.to_string(),
        "units": field.attrs.units,
        "dtype": field.data.dtype(),
        "ngates": ngates,
        "gate_start": field.gates.start,
        "gate_stride": field.gates.stride,
        "valid": valid,
        "total": nrays.saturating_mul(ngates),
        "min": (valid > 0).then_some(min),
        "max": (valid > 0).then_some(max),
    })
}

fn finite_range(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    values
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

// ---------------------------------------------------------------------------
// NEXRAD metadata (from read_volume_with_metadata)
// ---------------------------------------------------------------------------

fn nexrad_metadata_summary(metadata: &NexradMetadata) -> Value {
    let status = metadata.rda_status.as_ref().map(|status| match status {
        RdaStatus::Orda(orda) => json!({
            "layout": "orda",
            "rda_build": orda.rda_build.0,
            "vcp": format!("{:?}", orda.volume_coverage_pattern),
            "rda_state": format!("{:?}", orda.rda_state),
            "operability": format!("{:?}", orda.operability),
            "operational_mode": format!("{:?}", orda.operational_mode),
            "status_version": orda.status_version,
        }),
        RdaStatus::Legacy(_) => json!({"layout": "legacy"}),
        other => json!({"layout": format!("{other:?}")}),
    });
    let vcp = metadata.vcp.as_ref().map(|vcp| {
        json!({
            "pattern_number": vcp.pattern_number,
            "number_of_cuts": vcp.number_of_cuts,
            "version": vcp.version,
            "pulse_width": format!("{:?}", vcp.pulse_width),
            "doppler_velocity_resolution": format!("{:?}", vcp.doppler_velocity_resolution),
            "cuts": vcp.cuts.iter().map(|cut| json!({
                "elevation_deg": cut.elevation_angle_deg,
                "waveform": format!("{:?}", cut.waveform),
                "channel": format!("{:?}", cut.channel_configuration),
                "super_resolution": format!("{:?}", cut.super_resolution),
                "azimuth_rate_deg_per_s": cut.azimuth_rate_deg_per_s,
                "surveillance_prf_number": cut.surveillance_prf_number,
                "surveillance_pulse_count": cut.surveillance_pulse_count,
            })).collect::<Vec<_>>(),
        })
    });
    let adaptation = metadata.adaptation.as_ref().map(|adaptation| {
        json!({
            "site_name": adaptation.site_name,
            "adap_file_name": adaptation.adap_file_name,
            "adap_format": adaptation.adap_format,
            "adap_revision": adaptation.adap_revision,
            "adap_date": adaptation.adap_date,
            "tfreq_mhz": adaptation.tfreq_mhz,
            "slat": format!("{} {} {} {}", adaptation.slatdeg, adaptation.slatmin, adaptation.slatsec, adaptation.slatdir),
            "slon": format!("{} {} {} {}", adaptation.slondeg, adaptation.slonmin, adaptation.slonsec, adaptation.slondir),
        })
    });
    json!({
        "rda_status": status,
        "performance": metadata.performance.is_some(),
        "vcp": vcp,
        "adaptation": adaptation,
        "clutter_filter_map": metadata.clutter_filter_map.is_some(),
        "bypass_map": metadata.bypass_map.is_some(),
        "clutter_censor_zones": metadata.clutter_censor_zones.is_some(),
        "prf": metadata.prf.is_some(),
        "per_sweep_elevation_data": metadata.per_sweep_elevation_data.as_ref().map(Vec::len),
        "errors": metadata.errors,
    })
}

// ---------------------------------------------------------------------------
// Level II archive structure
// ---------------------------------------------------------------------------

/// Bytes of one fixed Archive II frame (CTM header + message).
const FRAME_BYTES: usize = 2432;
/// Frames of the uncompressed metadata record.
const METADATA_FRAMES: usize = messages::METADATA_RECORD_FRAMES;

fn level2_structure(bytes: &[u8]) -> Value {
    let header_len = messages::volume_header_len(bytes);
    let header = (header_len == 24).then(|| volume_header(&bytes[..24]));
    let body = &bytes[header_len..];

    let (layout, records) = split_records(body);
    let mut record_rows: Vec<Value> = Vec::new();
    let mut all_records: Vec<u8> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    match &records {
        Some(records) => {
            for (index, record) in records.iter().enumerate() {
                match messages::record_bytes(record.bytes) {
                    Ok(decoded) => {
                        let summary = record_summary(&decoded);
                        record_rows.push(json!({
                            "index": index,
                            "control_word": record.control_word,
                            "compressed": record.bytes.len() - 4,
                            "decompressed": decoded.len(),
                            "frames_are_whole": decoded.len() % FRAME_BYTES == 0,
                            "messages": summary,
                        }));
                        if all_records.len() + decoded.len() <= MAX_BYTES {
                            all_records.extend_from_slice(&decoded);
                        } else {
                            errors.push("records expand beyond the survey limit".to_owned());
                            break;
                        }
                    }
                    Err(err) => errors.push(format!("record {index}: {err}")),
                }
            }
        }
        None => all_records.extend_from_slice(body),
    }

    let metadata_record: &[u8] = match &records {
        Some(_) => match record_rows.first() {
            Some(first) => {
                let len = first["decompressed"].as_u64().unwrap_or(0) as usize;
                &all_records[..len.min(all_records.len())]
            }
            None => &[],
        },
        None => &all_records[..all_records.len().min(METADATA_FRAMES * FRAME_BYTES)],
    };

    let (cuts, decoded_counts, walk_errors) = walk_messages(&all_records);
    errors.extend(walk_errors);
    let (message_counts, message_headers) = raw_message_inventory(&all_records);
    let metadata_sequence = type_runs(metadata_record);

    json!({
        "volume_header": header,
        "layout": layout,
        "record_count": records.as_ref().map(Vec::len),
        "records": compact_records(&record_rows),
        "uncompressed_body_bytes": all_records.len(),
        "uncompressed_body_holds_134_frames": records
            .is_none()
            .then_some(all_records.len() >= METADATA_FRAMES * FRAME_BYTES),
        "metadata_record_bytes": metadata_record.len(),
        "metadata_record_sequence": metadata_sequence,
        "message_counts": message_counts,
        "decoded_message_counts": decoded_counts,
        "message_headers": message_headers,
        "cuts": cuts,
        "errors": errors,
    })
}

/// The 24-byte volume header (Archive II ICD 2620010 Table I): tape name,
/// extension, date (4-byte NEXRAD Julian day), milliseconds, ICAO. The date
/// is also read from its high halfword, where GR2-style exports put it.
fn volume_header(header: &[u8]) -> Value {
    let tape = escape_prefix(&header[..9], 9);
    let extension = escape_prefix(&header[9..12], 3);
    let date = u32::from_be_bytes([header[12], header[13], header[14], header[15]]);
    let high_halfword = u16::from_be_bytes([header[12], header[13]]);
    let millis = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let icao = escape_prefix(&header[20..24], 4);
    let date_layout = if date <= u32::from(u16::MAX) {
        "icd-u32"
    } else if header[14] == 0 && header[15] == 0 {
        "high-halfword"
    } else {
        "other"
    };
    let day = if date <= u32::from(u16::MAX) {
        date
    } else {
        u32::from(high_halfword)
    };
    json!({
        "tape": tape,
        "extension": extension,
        "date_bytes": format!("{:02x}{:02x}{:02x}{:02x}", header[12], header[13], header[14], header[15]),
        "date_layout": date_layout,
        "julian_date": day,
        "milliseconds": millis,
        "time": day_time(day, millis),
        "icao": icao,
    })
}

/// NEXRAD Julian day (day 1 = 1970-01-01) and milliseconds as RFC 3339.
fn day_time(day: u32, millis: u32) -> Option<String> {
    chrono::DateTime::from_timestamp(
        (i64::from(day) - 1) * 86_400 + i64::from(millis / 1000),
        (millis % 1000) * 1_000_000,
    )
    .map(|t| t.to_rfc3339())
}

struct Record<'a> {
    control_word: i32,
    /// Control word plus the compressed payload.
    bytes: &'a [u8],
}

/// Split the bytes after the volume header into LDM records when they start
/// with a control word followed by `BZh`; `None` for uncompressed frames.
fn split_records(body: &[u8]) -> (String, Option<Vec<Record<'_>>>) {
    if body.starts_with(b"BZh") {
        return ("bzip2-whole-file".to_owned(), None);
    }
    if body.len() < 8 || &body[4..7] != b"BZh" {
        return ("uncompressed-frames".to_owned(), None);
    }
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= body.len() {
        let control_word = i32::from_be_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]);
        let len = control_word.unsigned_abs() as usize;
        let end = offset + 4 + len;
        if len == 0 || end > body.len() {
            break;
        }
        records.push(Record {
            control_word,
            bytes: &body[offset..end],
        });
        offset = end;
    }
    let trailing = body.len() - offset;
    let layout = if trailing == 0 {
        "ldm-bzip2-records".to_owned()
    } else {
        format!("ldm-bzip2-records (+{trailing} trailing bytes)")
    };
    (layout, Some(records))
}

/// Message types of one record, as run-length `type x count` pairs, plus
/// the elevation numbers of its Message 31 radials.
fn record_summary(decoded: &[u8]) -> Value {
    let runs = type_runs(decoded);
    let mut elevations: Vec<u8> = Vec::new();
    let mut radials = 0usize;
    for message in RawMessages::new(decoded).flatten() {
        if message.header.message_type == 31 && message.body.len() > 22 {
            radials += 1;
            let elevation = message.body[22];
            if !elevations.contains(&elevation) {
                elevations.push(elevation);
            }
        }
    }
    json!({"runs": runs, "radials": radials, "elevations": elevations})
}

/// Run-length encoding of the message types in `records` (empty frames are
/// type 0).
fn type_runs(records: &[u8]) -> Vec<String> {
    let mut runs: Vec<(u8, usize)> = Vec::new();
    for message in RawMessages::new(records) {
        let kind = match message {
            Ok(message) => message.header.message_type,
            Err(_) => 255,
        };
        match runs.last_mut() {
            Some((last, count)) if *last == kind => *count += 1,
            _ => runs.push((kind, 1)),
        }
    }
    runs.iter()
        .map(|(kind, count)| {
            if *count == 1 {
                kind.to_string()
            } else {
                format!("{kind}x{count}")
            }
        })
        .collect()
}

/// Keep the first three and last two record rows; summarize the middle.
fn compact_records(rows: &[Value]) -> Value {
    if rows.len() <= 6 {
        return Value::Array(rows.to_vec());
    }
    let mut out: Vec<Value> = rows[..3].to_vec();
    let middle = &rows[3..rows.len() - 2];
    let radials: Vec<u64> = middle
        .iter()
        .filter_map(|row| row["messages"]["radials"].as_u64())
        .collect();
    out.push(json!({
        "elided_records": middle.len(),
        "radials_min": radials.iter().min(),
        "radials_max": radials.iter().max(),
    }));
    out.extend_from_slice(&rows[rows.len() - 2..]);
    Value::Array(out)
}

#[derive(Default)]
struct CutStats {
    elevation_number: u8,
    radials: usize,
    first_elevation_deg: f32,
    elevation_min_deg: f32,
    elevation_max_deg: f32,
    first_azimuth_deg: f32,
    first_azimuth_number: u16,
    last_azimuth_number: u16,
    statuses: BTreeMap<u8, usize>,
    azimuth_resolutions: Vec<String>,
    compressions: Vec<String>,
    radar_identifiers: Vec<String>,
    first_time: Option<(u16, u32)>,
    last_time: Option<(u16, u32)>,
    radial_length_min: u16,
    radial_length_max: u16,
    cut_sectors: Vec<u8>,
    blocks: Vec<String>,
    moments: BTreeMap<String, Value>,
    volume_block: Option<Value>,
    elevation_block: Option<Value>,
    radial_block: Option<Value>,
    spot_blanking: Vec<u8>,
    azimuth_indexing: Vec<u8>,
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn walk_messages(records: &[u8]) -> (Vec<Value>, BTreeMap<String, usize>, Vec<String>) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors: Vec<String> = Vec::new();
    let mut cuts: Vec<CutStats> = Vec::new();
    for item in messages::MessageWalker::new(records) {
        let (header, body) = match item {
            Ok(pair) => pair,
            Err(err) => {
                if errors.len() < 10 {
                    errors.push(err.to_string());
                }
                continue;
            }
        };
        *counts
            .entry(format!("{:02}", header.message_type))
            .or_default() += 1;
        let MessageBody::DigitalRadarDataGeneric(radial) = body else {
            continue;
        };
        let data = &radial.header;
        let index = match cuts
            .iter()
            .position(|cut| cut.elevation_number == data.elevation_number)
        {
            Some(index) => index,
            None => {
                cuts.push(CutStats {
                    elevation_number: data.elevation_number,
                    first_elevation_deg: data.elevation_angle_deg,
                    elevation_min_deg: data.elevation_angle_deg,
                    elevation_max_deg: data.elevation_angle_deg,
                    first_azimuth_deg: data.azimuth_angle_deg,
                    first_azimuth_number: data.azimuth_number,
                    radial_length_min: data.radial_length,
                    radial_length_max: data.radial_length,
                    ..CutStats::default()
                });
                cuts.len() - 1
            }
        };
        let cut = &mut cuts[index];
        cut.radials += 1;
        cut.elevation_min_deg = cut.elevation_min_deg.min(data.elevation_angle_deg);
        cut.elevation_max_deg = cut.elevation_max_deg.max(data.elevation_angle_deg);
        cut.last_azimuth_number = data.azimuth_number;
        *cut.statuses.entry(data.radial_status_code).or_default() += 1;
        push_unique(
            &mut cut.azimuth_resolutions,
            format!("{:?}", data.azimuth_resolution),
        );
        push_unique(&mut cut.compressions, format!("{:?}", data.compression));
        push_unique(
            &mut cut.radar_identifiers,
            escape_prefix(&data.radar_identifier, 4),
        );
        let time = (data.modified_julian_date, data.collection_time_ms);
        cut.first_time.get_or_insert(time);
        cut.last_time = Some(time);
        cut.radial_length_min = cut.radial_length_min.min(data.radial_length);
        cut.radial_length_max = cut.radial_length_max.max(data.radial_length);
        push_unique(&mut cut.cut_sectors, data.cut_sector_number);
        push_unique(&mut cut.spot_blanking, data.spot_blanking.0);
        push_unique(&mut cut.azimuth_indexing, data.azimuth_indexing_raw);
        let mut blocks: Vec<&str> = Vec::new();
        if radial.volume.is_some() {
            blocks.push("VOL");
        }
        if radial.elevation.is_some() {
            blocks.push("ELV");
        }
        if radial.radial.is_some() {
            blocks.push("RAD");
        }
        let moment_names: Vec<String> = radial
            .moments
            .iter()
            .map(|moment| moment.name.short_name().into_owned())
            .collect();
        let unknown: Vec<String> = radial
            .unknown_blocks
            .iter()
            .map(|block| format!("?{}", escape_prefix(&block.name, 3)))
            .collect();
        let signature = [blocks.join("+"), moment_names.join("+"), unknown.join("+")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");
        push_unique(&mut cut.blocks, signature);
        for moment in &radial.moments {
            let name = moment.name.short_name().into_owned();
            cut.moments.entry(name).or_insert_with(|| {
                json!({
                    "gates": moment.gate_count,
                    "first_gate_m": moment.first_gate_range_m,
                    "gate_spacing_m": moment.gate_spacing_m,
                    "word_bits": moment.data_word_size,
                    "scale": moment.scale,
                    "offset": moment.offset,
                    "tover_raw": moment.tover_raw,
                    "snr_threshold_raw": moment.snr_threshold_raw,
                    "control_flags": format!("{:?}", moment.control_flags),
                })
            });
        }
        if cut.volume_block.is_none()
            && let Some(volume) = &radial.volume
        {
            cut.volume_block = Some(json!({
                "block_size": volume.block_size,
                "version": format!("{}.{}", volume.version_major, volume.version_minor),
                "latitude_deg": volume.latitude_deg,
                "longitude_deg": volume.longitude_deg,
                "site_height_m": volume.site_height_m,
                "feedhorn_height_m": volume.feedhorn_height_m,
                "calibration_constant_db": volume.calibration_constant_db,
                "h_tx_power_kw": volume.horizontal_shv_tx_power_kw,
                "v_tx_power_kw": volume.vertical_shv_tx_power_kw,
                "system_zdr_db": volume.system_differential_reflectivity_db,
                "initial_phidp_deg": volume.initial_system_differential_phase_deg,
                "vcp_number": volume.vcp_number,
                "processing_status": volume.processing_status.0,
                "zdr_bias_estimate_raw": volume.zdr_bias_estimate_raw,
            }));
        }
        if cut.elevation_block.is_none()
            && let Some(elevation) = &radial.elevation
        {
            cut.elevation_block = Some(json!({
                "block_size": elevation.block_size,
                "atmospheric_attenuation_raw": elevation.atmospheric_attenuation_raw,
                "calibration_constant_db": elevation.calibration_constant_db,
            }));
        }
        if cut.radial_block.is_none()
            && let Some(radial_block) = &radial.radial
        {
            cut.radial_block = Some(json!({
                "block_size": radial_block.block_size,
                "unambiguous_range_raw": radial_block.unambiguous_range_raw,
                "nyquist_velocity_raw": radial_block.nyquist_velocity_raw,
                "h_noise_dbm": radial_block.horizontal_noise_level_dbm,
                "v_noise_dbm": radial_block.vertical_noise_level_dbm,
                "radial_flags": radial_block.radial_flags,
                "h_calibration_dbz": radial_block.horizontal_calibration_constant_dbz,
                "v_calibration_dbz": radial_block.vertical_calibration_constant_dbz,
            }));
        }
    }
    let cuts = cuts
        .into_iter()
        .map(|cut| {
            json!({
                "elevation_number": cut.elevation_number,
                "radials": cut.radials,
                "first_elevation_deg": cut.first_elevation_deg,
                "elevation_min_deg": cut.elevation_min_deg,
                "elevation_max_deg": cut.elevation_max_deg,
                "first_azimuth_deg": cut.first_azimuth_deg,
                "azimuth_numbers": [cut.first_azimuth_number, cut.last_azimuth_number],
                "radial_status_counts": cut.statuses,
                "azimuth_resolution": cut.azimuth_resolutions,
                "compression": cut.compressions,
                "radar_identifier": cut.radar_identifiers,
                "first_time": cut.first_time.map(|(d, ms)| mjd_time(d, ms)),
                "last_time": cut.last_time.map(|(d, ms)| mjd_time(d, ms)),
                "radial_length": [cut.radial_length_min, cut.radial_length_max],
                "cut_sector": cut.cut_sectors,
                "spot_blanking": cut.spot_blanking,
                "azimuth_indexing_raw": cut.azimuth_indexing,
                "block_signatures": cut.blocks,
                "moments": cut.moments,
                "vol": cut.volume_block,
                "elv": cut.elevation_block,
                "rad": cut.radial_block,
            })
        })
        .collect();
    (cuts, counts, errors)
}

/// Message 31 date (days since 1969-12-31, day 1 = 1970-01-01) and time.
fn mjd_time(date: u16, millis: u32) -> Option<String> {
    day_time(u32::from(date), millis)
}

/// Every message by type, counted with the raw walker (so a message whose
/// body fails its table decoder still counts), plus the header fields of
/// each non-radial message: size, channel byte, segments, and for Message 5
/// the declared size and cut count against the frame.
fn raw_message_inventory(records: &[u8]) -> (BTreeMap<String, usize>, Vec<Value>) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut headers: Vec<Value> = Vec::new();
    for message in RawMessages::new(records) {
        let Ok(message) = message else {
            *counts.entry("framing_error".to_owned()).or_default() += 1;
            continue;
        };
        let header = &message.header;
        *counts
            .entry(format!("{:02}", header.message_type))
            .or_default() += 1;
        if header.message_type == 31 || header.message_type == 1 || headers.len() >= 40 {
            continue;
        }
        let mut row = json!({
            "type": header.message_type,
            "size_halfwords": header.size_halfwords,
            "channels": header.channels,
            "sequence_id": header.sequence_id,
            "time": day_time(u32::from(header.date), header.milliseconds),
            "segments": header.segments,
            "frames": message.frames,
            "body_bytes": message.body.len(),
        });
        if header.message_type == 5 && message.body.len() >= 8 {
            let body = &message.body;
            row["vcp_message_size_halfwords"] = json!(u16::from_be_bytes([body[0], body[1]]));
            row["vcp_pattern_type"] = json!(u16::from_be_bytes([body[2], body[3]]));
            row["vcp_pattern_number"] = json!(u16::from_be_bytes([body[4], body[5]]));
            row["vcp_number_of_cuts"] = json!(u16::from_be_bytes([body[6], body[7]]));
        }
        headers.push(row);
    }
    (counts, headers)
}
