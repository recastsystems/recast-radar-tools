//! `convert` and `publish`: decode, edit (sweep selection, site position),
//! then hand the volume to a backend.
//!
//! Both check that the backend exists before reading any input, so a build
//! without the writer fails at once with exit status 3. What the writer
//! leaves out or changes ([`WriteReport`]) goes to standard error.

use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::backend::{
    BackendError, Backends, ChunkedOutput, Level2Compression, PublishRequest, SitePosition,
    VolumeEdits, VolumeWriter, WriteInput, WriteOptions, WriteReport,
};
use crate::open::{self, OpenOptions};
use crate::output::{AtomicFile, human_bytes, write_file_atomically};
use crate::{CliError, ConvertArgs, EditArgs, InputArgs, Level2Args, PublishArgs};

fn site_override(site: &Option<String>) -> Result<Option<String>, CliError> {
    match site {
        Some(site) if site.trim().is_empty() || !site.is_ascii() => Err(CliError::Usage(format!(
            "--site `{site}` must be a non-empty ASCII identifier"
        ))),
        other => Ok(other.clone()),
    }
}

/// The writer options of the command line.
fn write_options(level2: &Level2Args, site: Option<String>) -> WriteOptions {
    let mut options = WriteOptions::new(level2.level2_compression, site);
    options.level2_quantization = level2.quantization;
    options.nyquist_velocity_mps = level2.nyquist;
    options.unambiguous_range_m = level2.unambiguous_range;
    options.drop_negative_range_gates = level2.drop_negative_range_gates;
    options.strict = level2.strict;
    options
}

/// The volume edits of the command line; `--position-from` decodes its file
/// for the position.
fn volume_edits(edit: &EditArgs, input: &InputArgs) -> Result<VolumeEdits, CliError> {
    let mut edits = VolumeEdits {
        sweeps: edit.sweeps.as_ref().map(|list| list.0.clone()),
        sweeps_in_time_order: edit.sweeps_in_time_order,
        split_scan_cycles: edit.split_scan_cycles,
        position: edit.position,
    };
    if let Some(path) = &edit.position_from {
        let loaded = open::load_one(
            std::slice::from_ref(path),
            &OpenOptions::from_args(input, false),
            false,
            None,
        )?;
        let position = SitePosition::of(&loaded.volume).ok_or_else(|| CliError::Decode {
            path: path.clone(),
            message: "has no site position (latitude, longitude and height) for --position-from"
                .to_owned(),
        })?;
        edits.position = Some(position);
    }
    Ok(edits)
}

/// Run `f` on a pool of `threads` workers, or on the global pool.
fn with_threads<T>(
    threads: Option<u32>,
    f: impl FnOnce() -> Result<T, CliError> + Send,
) -> Result<T, CliError>
where
    T: Send,
{
    match threads {
        None => f(),
        Some(threads) => rayon::ThreadPoolBuilder::new()
            .num_threads(threads as usize)
            .build()
            .map_err(|err| CliError::Failed(format!("--threads {threads}: {err}")))?
            .install(f),
    }
}

/// What the writer left out or changed, on standard error.
fn print_report(report: &WriteReport) {
    for line in &report.left_out {
        eprintln!("recast-radar: left out: {line}");
    }
    for line in &report.notes {
        eprintln!("recast-radar: note: {line}");
    }
}

pub(crate) fn run(
    args: &ConvertArgs,
    backends: &Backends,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let mut written = Vec::new();
    with_threads(args.threads, || convert(args, backends, &mut written))?;
    out.write_all(&written)?;
    Ok(())
}

fn convert(args: &ConvertArgs, backends: &Backends, out: &mut Vec<u8>) -> Result<(), CliError> {
    if args.chunks && args.level2.level2_compression != Level2Compression::Bzip2 {
        return Err(CliError::Usage(
            "real-time chunks are bzip2 LDM records: --chunks takes no --level2-compression none"
                .to_owned(),
        ));
    }
    if args.chunks && args.edit.split_scan_cycles {
        return Err(CliError::Usage(
            "--chunks writes one volume: select one scan cycle's sweeps with --sweeps instead of \
             --split-scan-cycles"
                .to_owned(),
        ));
    }
    let writer = backends.writer(args.to)?;
    if args.chunks && !writer.supports_chunks() {
        return Err(BackendError::ChunksUnavailable(args.to).into());
    }
    let options = write_options(&args.level2, site_override(&args.site)?);
    let edits = volume_edits(&args.edit, &args.input)?;
    let loaded = open::load_one(
        &args.inputs,
        &OpenOptions::from_args(&args.input, true),
        args.merge,
        args.volume,
    )?;
    let volumes = edits.apply_each(&loaded.volume).map_err(CliError::Usage)?;
    let source_name = args
        .inputs
        .first()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    if edits.split_scan_cycles {
        if volumes.len() > 1 {
            writeln!(out, "{} scan cycles", volumes.len())?;
        }
        for (number, volume) in volumes.iter().enumerate() {
            let input = WriteInput {
                volume,
                metadata: &loaded.metadata,
                source_name: source_name.as_deref(),
            };
            let output = cycle_path(&args.output, number + 1);
            write_file(args, writer, &options, &input, &output, out)?;
        }
        return Ok(());
    }
    let [volume] = &volumes[..] else {
        return Err(CliError::Failed(
            "the edits gave more than one volume".to_owned(),
        ));
    };
    let input = WriteInput {
        volume,
        metadata: &loaded.metadata,
        source_name: source_name.as_deref(),
    };
    if args.chunks {
        let chunked = writer.write_chunks(&input, &options)?;
        let saved = save_chunks(&chunked, &args.output, args.force)?;
        print_report(&chunked.report);
        writeln!(
            out,
            "wrote {} chunk(s) in {} ({})",
            saved.count,
            saved.directory.display(),
            human_bytes(saved.bytes)
        )?;
        return Ok(());
    }

    write_file(args, writer, &options, &input, &args.output, out)
}

/// Write one volume to `output` (gzip-wrapped with `--gzip`) and say what
/// was written.
fn write_file(
    args: &ConvertArgs,
    writer: &dyn VolumeWriter,
    options: &WriteOptions,
    input: &WriteInput<'_>,
    output: &Path,
    out: &mut Vec<u8>,
) -> Result<(), CliError> {
    let mut file = AtomicFile::create(output, args.force)?;
    let report = if args.gzip {
        let mut encoder = GzEncoder::new(file.writer(), Compression::default());
        let report = writer.write(input, options, &mut encoder)?;
        encoder.finish().map_err(|err| CliError::io(output, err))?;
        report
    } else {
        writer.write(input, options, file.writer())?
    };
    let path = file.commit()?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    print_report(&report);
    writeln!(
        out,
        "wrote {} ({}, {} bytes, {} sweeps{})",
        path.display(),
        args.to.label(),
        size,
        input.volume.sweeps.len(),
        if report.left_out.is_empty() {
            ""
        } else {
            "; some fields or sweeps left out, see above"
        }
    )?;
    Ok(())
}

/// `path` with `_number` before its extension (`.ar2v.gz` counts as one):
/// the file of scan cycle `number` under `--split-scan-cycles`.
fn cycle_path(path: &Path, number: usize) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dot = |text: &str| text.rfind('.').filter(|&at| at > 0);
    let split = match dot(&name) {
        Some(at) if name[at..].eq_ignore_ascii_case(".gz") => dot(&name[..at]).unwrap_or(at),
        Some(at) => at,
        None => name.len(),
    };
    path.with_file_name(format!("{}_{number}{}", &name[..split], &name[split..]))
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
    let mut written = Vec::new();
    with_threads(args.threads, || publish_all(args, backends, &mut written))?;
    out.write_all(&written)?;
    Ok(())
}

fn publish_all(args: &PublishArgs, backends: &Backends, out: &mut Vec<u8>) -> Result<(), CliError> {
    let publisher = backends.publisher()?;
    let mut request = PublishRequest::new(args.dir.clone());
    request.site = site_override(&args.site)?;
    request.keep = args.keep as usize;
    request.options = write_options(&args.level2, request.site.clone());
    request.update_site_config = !args.no_site_config;
    let edits = volume_edits(&args.edit, &args.input)?;
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
            for volume in edits.apply_each(&loaded.volume).map_err(CliError::Usage)? {
                let input = WriteInput {
                    volume: &volume,
                    metadata: &loaded.metadata,
                    source_name: source_name.as_deref(),
                };
                let published = publisher.publish(&input, &request)?;
                print_report(&published.report);
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
    fn each_scan_cycle_is_written_with_its_number_before_the_extension() {
        let cases = [
            ("out.ar2v", "out_2.ar2v"),
            ("out.ar2v.gz", "out_2.ar2v.gz"),
            ("OUT.GZ", "OUT_2.GZ"),
            ("volume", "volume_2"),
            (".hidden", ".hidden_2"),
            ("a.b.nc", "a.b_2.nc"),
        ];
        for (name, want) in cases {
            let path = Path::new("dir").join(name);
            assert_eq!(cycle_path(&path, 2), Path::new("dir").join(want), "{name}");
        }
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
            ) -> Result<WriteReport, BackendError> {
                Ok(WriteReport::default())
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
