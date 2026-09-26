//! The format writers and the polling-directory publisher that
//! [`Backends::builtin`](crate::backend::Backends::builtin) registers.
//!
//! Each one hands the decoded volume to the writer crate for its format and
//! maps that crate's refusals onto [`BackendError`]:
//!
//! | Format | Writer |
//! |---|---|
//! | NEXRAD Level II | `recast_radar_io_nexrad::write` (Archive II, real-time chunks) |
//! | CfRadial 1 | `recast_radar_io_cfradial::write_cfradial1` |
//! | ODIM_H5 | `recast_radar_io_odim::write_odim_h5_volume` |
//! | FM301 (CfRadial 2) | `recast_radar_io_cfradial::write_cfradial2` |
//! | polling directory | `recast_radar_io_nexrad::write::polling::PollingDirectory` |
//!
//! A Level II input's decoded metadata ([`WriteInput::metadata`]) goes to
//! the Level II writer as its source context, so the VCP, the per-sweep
//! constant blocks and the volume header time carry over. The Level II
//! writer's summary becomes the [`WriteReport`]: the fields and sweeps left
//! out, the codings coarser than their source, the radials reordered or left
//! out, and the writer's notes.

use std::borrow::Cow;
use std::io::Write;

use recast_radar_core::model::Volume;
use recast_radar_io::FormatMetadata;
use recast_radar_io_cfradial::{CfWriteError, Cfradial1Options, Cfradial2Options};
use recast_radar_io_nexrad::write::{
    self as level2, Compression, Quantization, SourceMetadata, WriteError as Level2Error,
    WriteSummary,
    polling::{PollingDirectory, PublishError},
    realtime,
};
use recast_radar_io_odim::{OdimWriteError, OdimWriteOptions};

use crate::backend::{
    BackendError, ChunkKind, ChunkedOutput, Level2Compression, Level2Quantization, OutputChunk,
    OutputFormat, PollingPublisher, PublishRequest, Published, VolumeWriter, WriteInput,
    WriteOptions, WriteReport,
};

/// The volume with `site` as its instrument name, borrowed when there is no
/// override.
fn with_site<'v>(volume: &'v Volume, site: Option<&str>) -> Cow<'v, Volume> {
    match site {
        Some(site) if site != volume.attrs.instrument_name => {
            let mut volume = volume.clone();
            volume.attrs.instrument_name = site.to_owned();
            Cow::Owned(volume)
        }
        _ => Cow::Borrowed(volume),
    }
}

/// The Level II writer's options for the front end's.
fn level2_options(options: &WriteOptions) -> level2::WriteOptions {
    let mut out = level2::WriteOptions::default();
    out.compression = match options.level2_compression {
        Level2Compression::None => Compression::None,
        _ => Compression::Bzip2LdmRecords,
    };
    out.icao = options.site_id.clone();
    out.quantization = match options.level2_quantization {
        Level2Quantization::Compatible => Quantization::Compatible,
        Level2Quantization::Standard => Quantization::Standard,
        _ => Quantization::Precise,
    };
    out.nyquist_velocity_mps = options.nyquist_velocity_mps;
    out.unambiguous_range_m = options.unambiguous_range_m;
    out.drop_negative_range_gates = options.drop_negative_range_gates;
    out
}

/// Sweep indices as ranges: `0-3, 5, 7-8`.
fn index_ranges(mut indices: Vec<usize>) -> String {
    indices.sort_unstable();
    indices.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    for index in indices {
        run = match run {
            Some((first, last)) if index == last + 1 => Some((first, index)),
            Some((first, last)) => {
                parts.push(range_text(first, last));
                Some((index, index))
            }
            None => Some((index, index)),
        };
    }
    if let Some((first, last)) = run {
        parts.push(range_text(first, last));
    }
    parts.join(", ")
}

fn range_text(first: usize, last: usize) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    }
}

/// The Level II writer's summary as a report. Sweep indices are those of
/// the volume given to the writer.
pub(crate) fn level2_report(summary: &WriteSummary) -> WriteReport {
    // One line per field and reason, with the sweeps it applies to.
    let mut skipped: Vec<(String, &str, Vec<usize>)> = Vec::new();
    for field in &summary.skipped_fields {
        let name = field.field.to_string();
        match skipped
            .iter_mut()
            .find(|(known, reason, _)| *known == name && *reason == field.reason)
        {
            Some((.., sweeps)) => sweeps.push(field.sweep),
            None => skipped.push((name, &field.reason, vec![field.sweep])),
        }
    }
    let mut left_out: Vec<String> = skipped
        .into_iter()
        .map(|(field, reason, sweeps)| {
            format!("field {field} (sweeps {}): {reason}", index_ranges(sweeps))
        })
        .collect();
    if !summary.skipped_sweeps.is_empty() {
        left_out.push(format!(
            "sweeps left out (no field with a Message 31 moment, or no ray with data): {}",
            index_ranges(summary.skipped_sweeps.clone())
        ));
    }
    let mut notes = Vec::new();
    // Codings that do not give back every source value, one line per
    // moment, field and coding.
    let mut inexact: Vec<(String, Vec<usize>, f32)> = Vec::new();
    for report in summary.moments.iter().filter(|report| !report.exact) {
        let key = format!(
            "{} from {}: {}-bit, scale {}, offset {}",
            report.moment, report.field, report.word_size, report.scale, report.offset
        );
        match inexact.iter_mut().find(|(known, ..)| *known == key) {
            Some((_, sweeps, error)) => {
                sweeps.push(report.sweep);
                *error = error.max(report.max_abs_error);
            }
            None => inexact.push((key, vec![report.sweep], report.max_abs_error)),
        }
    }
    for (key, sweeps, error) in inexact {
        notes.push(format!(
            "{key}: values within {error} of the source (sweeps {})",
            index_ranges(sweeps)
        ));
    }
    let mut dropped: Vec<(String, Vec<usize>)> = Vec::new();
    for report in summary
        .moments
        .iter()
        .filter(|report| report.dropped_gates > 0)
    {
        let key = format!(
            "{} from {}: the first {} gates of every ray, which lie before the radar, left out",
            report.moment, report.field, report.dropped_gates
        );
        match dropped.iter_mut().find(|(known, _)| *known == key) {
            Some((_, sweeps)) => sweeps.push(report.sweep),
            None => dropped.push((key, vec![report.sweep])),
        }
    }
    for (key, sweeps) in dropped {
        notes.push(format!("{key} (sweeps {})", index_ranges(sweeps)));
    }
    if !summary.written_rays.is_empty() {
        notes.push(format!(
            "radials not in the source's storage order (written from the earliest ray \
             collected, or rays without data left out): sweeps {}",
            index_ranges(summary.written_rays.iter().map(|rays| rays.sweep).collect())
        ));
    }
    notes.extend(summary.notes.iter().cloned());
    WriteReport::new(left_out, notes)
}

/// Under [`WriteOptions::strict`], refuse a write that leaves something
/// out.
fn check_strict(options: &WriteOptions, report: &WriteReport) -> Result<(), BackendError> {
    if options.strict && !report.left_out.is_empty() {
        return Err(BackendError::Unrepresentable {
            format: OutputFormat::Level2,
            reason: format!(
                "refused under --strict (Python: strict=True): the output would leave out {}",
                report.left_out.join("; ")
            ),
        });
    }
    Ok(())
}

/// A Level II input's metadata as the Level II writer's source context.
fn level2_source(metadata: &FormatMetadata) -> SourceMetadata<'_> {
    let mut source = SourceMetadata::default();
    if let FormatMetadata::Nexrad(metadata) = metadata {
        source.metadata = Some(metadata.as_ref());
    }
    source
}

/// What a front-end user can do about a Level II refusal: the option of
/// the command and the keyword of the Python package.
fn level2_hint(err: &Level2Error) -> Option<&'static str> {
    match err {
        Level2Error::MissingLocation(_) => Some(
            "give the radar's position (recast-radar: --position LAT,LON,HEIGHT or \
             --position-from FILE, a file of the same radar; Python: position=(lat, lon, height_m))",
        ),
        Level2Error::TooManySweeps { .. } => Some(
            "write one scan's sweeps at a time (recast-radar: --sweeps LIST; Python: \
             sweeps=[...])",
        ),
        Level2Error::Geometry { reason, .. } if reason.contains("before the radar") => Some(
            "leave out the gates before the radar (recast-radar: --drop-negative-range-gates; \
             Python: drop_negative_range_gates=True)",
        ),
        _ => None,
    }
}

fn level2_error(err: Level2Error) -> BackendError {
    let hint = level2_hint(&err);
    match err {
        Level2Error::Io(err) => BackendError::Io(err),
        err @ (Level2Error::LimitExceeded(_) | Level2Error::Compression(_)) => {
            BackendError::Other(Box::new(err))
        }
        err => {
            let reason = match &err {
                // The library's advice names the model field; say what is
                // missing and leave the advice to the hint.
                Level2Error::MissingLocation(what) => {
                    format!("the volume has no site {what} (Message 1 volumes carry none)")
                }
                err => err.to_string(),
            };
            BackendError::Unrepresentable {
                format: OutputFormat::Level2,
                reason: match hint {
                    Some(hint) => format!("{reason}; {hint}"),
                    None => reason,
                },
            }
        }
    }
}

fn cfradial_error(format: OutputFormat, err: CfWriteError) -> BackendError {
    match err {
        CfWriteError::Unrepresentable(reason) | CfWriteError::TooLarge(reason) => {
            BackendError::Unrepresentable { format, reason }
        }
        err => BackendError::Other(Box::new(err)),
    }
}

fn odim_error(err: OdimWriteError) -> BackendError {
    match err {
        OdimWriteError::Unrepresentable { what } | OdimWriteError::TooLarge { what } => {
            BackendError::Unrepresentable {
                format: OutputFormat::OdimH5,
                reason: what,
            }
        }
        err => BackendError::Other(Box::new(err)),
    }
}

/// NEXRAD Level II: an Archive II file, or real-time chunks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Level2Writer;

impl VolumeWriter for Level2Writer {
    fn format(&self) -> OutputFormat {
        OutputFormat::Level2
    }

    fn write(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
        mut out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError> {
        let summary = level2::write_volume_with_source_to(
            input.volume,
            level2_source(input.metadata),
            &level2_options(options),
            &mut out,
        )
        .map_err(level2_error)?;
        let report = level2_report(&summary);
        check_strict(options, &report)?;
        Ok(report)
    }

    fn supports_chunks(&self) -> bool {
        true
    }

    fn write_chunks(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
    ) -> Result<ChunkedOutput, BackendError> {
        let chunked = realtime::write_realtime_chunks_with_source(
            input.volume,
            level2_source(input.metadata),
            &level2_options(options),
        )
        .map_err(level2_error)?;
        let report = level2_report(&chunked.summary);
        check_strict(options, &report)?;
        let chunks = chunked
            .chunks
            .into_iter()
            .map(|chunk| {
                let kind = match chunk.kind {
                    realtime::ChunkKind::Start => ChunkKind::Start,
                    realtime::ChunkKind::End => ChunkKind::End,
                    _ => ChunkKind::Intermediate,
                };
                OutputChunk::new(kind, chunk.number, chunk.bytes)
            })
            .collect();
        Ok(ChunkedOutput::new(
            chunked.icao,
            chunked.volume_number,
            chunked.volume_time,
            chunks,
        )
        .with_report(report))
    }
}

/// CfRadial 1.4 in classic netCDF (CDF-2), with the writer's default gate
/// layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CfRadial1Writer;

impl VolumeWriter for CfRadial1Writer {
    fn format(&self) -> OutputFormat {
        OutputFormat::CfRadial1
    }

    fn write(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
        out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_cfradial::write_cfradial1(&volume, &Cfradial1Options::default())
                .map_err(|err| cfradial_error(OutputFormat::CfRadial1, err))?;
        out.write_all(&bytes)?;
        Ok(WriteReport::default())
    }
}

/// An ODIM_H5 polar volume (PVOL).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OdimWriter;

impl VolumeWriter for OdimWriter {
    fn format(&self) -> OutputFormat {
        OutputFormat::OdimH5
    }

    fn write(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
        out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_odim::write_odim_h5_volume(&volume, &OdimWriteOptions::default())
                .map_err(odim_error)?;
        out.write_all(&bytes)?;
        Ok(WriteReport::default())
    }
}

/// WMO FM301 (CfRadial 2) in netCDF-4.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fm301Writer;

impl VolumeWriter for Fm301Writer {
    fn format(&self) -> OutputFormat {
        OutputFormat::Fm301
    }

    fn write(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
        out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_cfradial::write_cfradial2(&volume, &Cfradial2Options::default())
                .map_err(|err| cfradial_error(OutputFormat::Fm301, err))?;
        out.write_all(&bytes)?;
        Ok(WriteReport::default())
    }
}

/// A GR2Analyst polling directory, following the GRLevelX polling
/// conventions (`recast_radar_io_nexrad::write::polling`): one folder per
/// site with `SITEYYYYMMDD_HHMMSS_V06.ar2v` files (`.ar2v.gz` for gzip
/// bytes) and a `dir.list`, and the site in the root's `config.cfg` and
/// `grlevel2.cfg`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollingDirectoryPublisher;

impl PollingPublisher for PollingDirectoryPublisher {
    fn publish(
        &self,
        input: &WriteInput<'_>,
        request: &PublishRequest,
    ) -> Result<Published, BackendError> {
        let mut options = level2_options(&request.options);
        if let Some(site) = &request.site {
            options.icao = Some(site.clone());
        }
        let directory = PollingDirectory::new(&request.root)
            .with_max_files(request.keep)
            .with_site_lists(request.update_site_config);
        let mut bytes = Vec::new();
        let summary = level2::write_volume_with_source_to(
            input.volume,
            level2_source(input.metadata),
            &options,
            &mut bytes,
        )
        .map_err(level2_error)?;
        let report = level2_report(&summary);
        check_strict(&request.options, &report)?;
        let time = summary
            .volume_time
            .ok_or_else(|| BackendError::Unrepresentable {
                format: OutputFormat::Level2,
                reason: "the volume has no time to name its file by".to_owned(),
            })?;
        let published = directory
            .publish_bytes(&summary.icao, time, &bytes)
            .map_err(|err| match err {
                PublishError::Write(err) => level2_error(err),
                // A file operation's error keeps its path.
                err => BackendError::Other(Box::new(err)),
            })?;
        let site_dir = request.root.join(&summary.icao);
        Ok(Published::new(
            summary.icao.clone(),
            published.path,
            site_dir.join("dir.list"),
            published
                .removed
                .iter()
                .map(|name| site_dir.join(name))
                .collect(),
        )
        .with_report(report))
    }
}
