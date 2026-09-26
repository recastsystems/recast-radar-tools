//! `convert` and `publish`: decode, then hand the volume to a backend.
//!
//! Both check that the backend exists before reading any input, so a build
//! without the writer fails at once with exit status 3.

use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::backend::{
    BackendError, Backends, ChunkedOutput, Level2Compression, PublishRequest, WriteInput,
    WriteOptions,
};
use crate::open::{self, OpenOptions};
use crate::output::{AtomicFile, human_bytes, write_file_atomically};
use crate::{CliError, ConvertArgs, PublishArgs};

fn site_override(site: &Option<String>) -> Result<Option<String>, CliError> {
    match site {
        Some(site) if site.trim().is_empty() || !site.is_ascii() => Err(CliError::Usage(format!(
            "--site `{site}` must be a non-empty ASCII identifier"
        ))),
        other => Ok(other.clone()),
    }
}

pub(crate) fn run(
    args: &ConvertArgs,
    backends: &Backends,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    if args.chunks && args.level2_compression != Level2Compression::Bzip2 {
        return Err(CliError::Usage(
            "real-time chunks are bzip2 LDM records: --chunks takes no --level2-compression none"
                .to_owned(),
        ));
    }
    let writer = backends.writer(args.to)?;
    if args.chunks && !writer.supports_chunks() {
        return Err(BackendError::ChunksUnavailable(args.to).into());
    }
    let options = WriteOptions::new(args.level2_compression, site_override(&args.site)?);
    let loaded = open::load_one(
        &args.inputs,
        &OpenOptions::from_args(&args.input, true),
        args.merge,
        args.volume,
    )?;
    let source_name = args
        .inputs
        .first()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let input = WriteInput {
        volume: &loaded.volume,
        metadata: &loaded.metadata,
        source_name: source_name.as_deref(),
    };
    if args.chunks {
        let chunked = writer.write_chunks(&input, &options)?;
        let saved = save_chunks(&chunked, &args.output, args.force)?;
        writeln!(
            out,
            "wrote {} chunk(s) in {} ({})",
            saved.count,
            saved.directory.display(),
            human_bytes(saved.bytes)
        )?;
        return Ok(());
    }

    let mut file = AtomicFile::create(&args.output, args.force)?;
    if args.gzip {
        let mut encoder = GzEncoder::new(file.writer(), Compression::default());
        writer.write(&input, &options, &mut encoder)?;
        encoder
            .finish()
            .map_err(|err| CliError::io(&args.output, err))?;
    } else {
        writer.write(&input, &options, file.writer())?;
    }
    let path = file.commit()?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    writeln!(
        out,
        "wrote {} ({}, {} bytes, {} sweeps)",
        path.display(),
        args.to.label(),
        size,
        loaded.volume.sweeps.len()
    )?;
    Ok(())
}

/// What [`save_chunks`] wrote.
struct SavedChunks {
    /// Chunk files written.
    count: usize,
    /// Their total size.
    bytes: u64,
    /// The volume's directory, `output/SITE/VOLUME`.
    directory: PathBuf,
}

/// `convert --chunks`: every chunk as its own file under `output`, named by
/// its key in the chunks bucket. Nothing is written when a chunk file
/// exists and `replace` is false, or when the site would not stay one
/// directory name.
fn save_chunks(
    chunked: &ChunkedOutput,
    output: &Path,
    replace: bool,
) -> Result<SavedChunks, CliError> {
    if chunked.chunks.is_empty() {
        return Err(CliError::Failed("the writer wrote no chunks".to_owned()));
    }
    // The site names a directory: it comes from the input file or --site.
    if chunked.site.is_empty()
        || !chunked
            .site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CliError::Failed(format!(
            "chunk site `{}` is not letters, digits, `_` or `-`",
            chunked.site
        )));
    }
    let paths: Vec<PathBuf> = chunked
        .chunks
        .iter()
        .map(|chunk| output.join(chunked.chunk_key(chunk)))
        .collect();
    if !replace && let Some(existing) = paths.iter().find(|path| path.exists()) {
        return Err(CliError::Usage(format!(
            "{} exists (pass --force to replace it)",
            existing.display()
        )));
    }
    let mut bytes = 0u64;
    for (chunk, path) in chunked.chunks.iter().zip(&paths) {
        write_file_atomically(path, &chunk.bytes)?;
        bytes += chunk.bytes.len() as u64;
    }
    Ok(SavedChunks {
        count: paths.len(),
        bytes,
        directory: output
            .join(&chunked.site)
            .join(chunked.volume_number.to_string()),
    })
}

pub(crate) fn publish(
    args: &PublishArgs,
    backends: &Backends,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let publisher = backends.publisher()?;
    let mut request = PublishRequest::new(args.dir.clone());
    request.site = site_override(&args.site)?;
    request.keep = args.keep as usize;
    request.options = WriteOptions::new(args.level2_compression, request.site.clone());
    request.update_site_config = !args.no_site_config;
    let options = OpenOptions::from_args(&args.input, true);

    let batches: Vec<Vec<open::Loaded>> = if args.merge {
        vec![vec![open::load_one(&args.inputs, &options, true, None)?]]
    } else {
        let mut batches = Vec::new();
        for path in &args.inputs {
            batches.push(open::open_path(path, &options)?.into_volumes());
        }
        batches
    };
    for (path, volumes) in args.inputs.iter().zip(batches) {
        if volumes.is_empty() {
            return Err(CliError::Decode {
                path: path.clone(),
                message: "holds no radar volume".to_owned(),
            });
        }
        for loaded in &volumes {
            let source_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let input = WriteInput {
                volume: &loaded.volume,
                metadata: &loaded.metadata,
                source_name: source_name.as_deref(),
            };
            let published = publisher.publish(&input, &request)?;
            writeln!(
                out,
                "published {} ({}; dir.list {})",
                published.path.display(),
                published.site,
                published.dir_list.display()
            )?;
            for removed in &published.removed {
                writeln!(out, "removed {}", removed.display())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use clap::Parser;

    use super::*;
    use crate::backend::{ChunkKind, OutputChunk, OutputFormat, VolumeWriter};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "recast-radar-convert-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The first three real-time chunks of KIWA volume 307 as captured from
    /// the chunks bucket (`KIWA/307/20260917-003629-00N-K`).
    fn kiwa_chunks() -> ChunkedOutput {
        let chunk = |id: &str, kind, number| {
            let bytes = recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"));
            OutputChunk::new(kind, number, bytes)
        };
        ChunkedOutput::new(
            "KIWA".to_owned(),
            307,
            DateTime::from_timestamp(1_789_605_389, 0).unwrap_or_default(),
            vec![
                chunk(
                    "l2chunk-kiwa-307-20260917-003629-001-s",
                    ChunkKind::Start,
                    1,
                ),
                chunk(
                    "l2chunk-kiwa-307-20260917-003629-002-i",
                    ChunkKind::Intermediate,
                    2,
                ),
                chunk(
                    "l2chunk-kiwa-307-20260917-003629-003-i",
                    ChunkKind::Intermediate,
                    3,
                ),
            ],
        )
    }

    #[test]
    fn chunks_are_saved_under_their_bucket_keys() {
        let chunked = kiwa_chunks();
        let dir = scratch("chunks");
        let saved = save_chunks(&chunked, &dir, false).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(saved.count, 3);
        assert_eq!(saved.directory, dir.join("KIWA").join("307"));
        let mut volume = Vec::new();
        for (chunk, name) in chunked.chunks.iter().zip(["001-S", "002-I", "003-I"]) {
            let path = saved.directory.join(format!("20260917-003629-{name}"));
            let bytes = std::fs::read(&path).unwrap_or_default();
            assert_eq!(bytes, chunk.bytes, "{}", path.display());
            volume.extend_from_slice(&bytes);
        }
        assert_eq!(saved.bytes, volume.len() as u64);
        // The saved chunks, concatenated, are the start of the volume: two
        // records of 120 radials of the first elevation (manifest).
        let opened = crate::open::open_bytes(&volume, &crate::open::OpenOptions::default());
        let Ok(crate::open::Contents::Volumes(volumes)) = opened else {
            panic!("the saved chunks do not decode as a volume: {opened:?}");
        };
        let rays: usize = volumes
            .iter()
            .flat_map(|loaded| &loaded.volume.sweeps)
            .map(|sweep| sweep.nrays())
            .sum();
        assert_eq!(rays, 240);

        // Existing chunks are replaced only when asked.
        assert!(matches!(
            save_chunks(&chunked, &dir, false),
            Err(CliError::Usage(_))
        ));
        assert!(save_chunks(&chunked, &dir, true).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_site_that_would_leave_the_output_directory_is_refused() {
        let mut chunked = kiwa_chunks();
        chunked.site = "../x".to_owned();
        let dir = scratch("site");
        assert!(matches!(
            save_chunks(&chunked, &dir, true),
            Err(CliError::Failed(_))
        ));
        assert!(!dir.exists());
    }

    fn convert(backends: &Backends, input: &Path, extra: &[&str]) -> Result<(), CliError> {
        let out_dir = scratch("unused");
        let mut args = vec![
            "recast-radar".to_owned(),
            "convert".to_owned(),
            input.display().to_string(),
            "--to".to_owned(),
            "level2".to_owned(),
            "--chunks".to_owned(),
            "-o".to_owned(),
            out_dir.display().to_string(),
        ];
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        let cli =
            crate::Cli::try_parse_from(args).map_err(|err| CliError::Usage(err.to_string()))?;
        let result = crate::run(cli, backends, &mut Vec::new());
        assert!(!out_dir.exists());
        result
    }

    #[test]
    fn writers_without_chunks_and_stubs_refuse_before_reading() {
        /// A Level II writer without chunk output.
        struct WholeFiles;
        impl VolumeWriter for WholeFiles {
            fn format(&self) -> OutputFormat {
                OutputFormat::Level2
            }
            fn write(
                &self,
                _input: &WriteInput<'_>,
                _options: &WriteOptions,
                _out: &mut dyn Write,
            ) -> Result<(), BackendError> {
                Ok(())
            }
        }
        // The input does not exist: each refusal comes before reading it.
        let missing = scratch("no-input").join("missing.ar2v");
        let backends = Backends::stubs().with_writer(Box::new(WholeFiles));
        let err = convert(&backends, &missing, &[]).err();
        assert!(
            matches!(
                err,
                Some(CliError::Backend(BackendError::ChunksUnavailable(
                    OutputFormat::Level2
                )))
            ),
            "{err:?}"
        );
        let err = convert(&Backends::stubs(), &missing, &[]).err();
        assert!(
            matches!(
                err,
                Some(CliError::Backend(BackendError::WriterUnavailable(
                    OutputFormat::Level2
                )))
            ),
            "{err:?}"
        );
        let err = convert(&backends, &missing, &["--level2-compression", "none"]).err();
        assert!(matches!(err, Some(CliError::Usage(_))), "{err:?}");
    }
}
