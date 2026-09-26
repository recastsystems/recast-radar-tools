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
//! constant blocks and the volume header time carry over.

use std::borrow::Cow;
use std::io::Write;

use recast_radar_core::model::Volume;
use recast_radar_io::FormatMetadata;
use recast_radar_io_cfradial::{CfWriteError, Cfradial1Options, Cfradial2Options};
use recast_radar_io_nexrad::write::{
    self as level2, Compression, SourceMetadata, WriteError as Level2Error,
    polling::{PollingDirectory, PublishError},
    realtime,
};
use recast_radar_io_odim::{OdimWriteError, OdimWriteOptions};

use crate::backend::{
    BackendError, ChunkKind, ChunkedOutput, Level2Compression, OutputChunk, OutputFormat,
    PollingPublisher, PublishRequest, Published, VolumeWriter, WriteInput, WriteOptions,
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
    out
}

/// A Level II input's metadata as the Level II writer's source context.
fn level2_source(metadata: &FormatMetadata) -> SourceMetadata<'_> {
    let mut source = SourceMetadata::default();
    if let FormatMetadata::Nexrad(metadata) = metadata {
        source.metadata = Some(metadata.as_ref());
    }
    source
}

fn level2_error(err: Level2Error) -> BackendError {
    match err {
        Level2Error::Io(err) => BackendError::Io(err),
        err @ (Level2Error::LimitExceeded(_) | Level2Error::Compression(_)) => {
            BackendError::Other(Box::new(err))
        }
        err => BackendError::Unrepresentable {
            format: OutputFormat::Level2,
            reason: err.to_string(),
        },
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
    ) -> Result<(), BackendError> {
        level2::write_volume_with_source_to(
            input.volume,
            level2_source(input.metadata),
            &level2_options(options),
            &mut out,
        )
        .map(|_| ())
        .map_err(level2_error)
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
        ))
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
    ) -> Result<(), BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_cfradial::write_cfradial1(&volume, &Cfradial1Options::default())
                .map_err(|err| cfradial_error(OutputFormat::CfRadial1, err))?;
        out.write_all(&bytes)?;
        Ok(())
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
    ) -> Result<(), BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_odim::write_odim_h5_volume(&volume, &OdimWriteOptions::default())
                .map_err(odim_error)?;
        out.write_all(&bytes)?;
        Ok(())
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
    ) -> Result<(), BackendError> {
        let volume = with_site(input.volume, options.site_id.as_deref());
        let bytes =
            recast_radar_io_cfradial::write_cfradial2(&volume, &Cfradial2Options::default())
                .map_err(|err| cfradial_error(OutputFormat::Fm301, err))?;
        out.write_all(&bytes)?;
        Ok(())
    }
}

/// A GR2Analyst polling directory, following the GRLevelX polling
/// conventions (`recast_radar_io_nexrad::write::polling`): one folder per
/// site with `SITE_YYYYMMDDHHMMSS.ar2v` files (`.ar2v.gz` for gzip bytes)
/// and a `dir.list`, and the site in the root's `config.cfg` and
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
        ))
    }
}
