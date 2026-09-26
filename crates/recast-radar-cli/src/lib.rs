//! The `recast-radar` command-line tool.
//!
//! One binary over the recast-radar-tools crates:
//!
//! | Command | What it does |
//! |---|---|
//! | `info` | One-screen summary of radar files (format, site, time, scan, sweeps, fields) |
//! | `dump` | Every decoded value: metadata, sweeps, fields, optionally gate data; text or JSON; the FM301 group tree |
//! | `render` | One field of one sweep to PNG |
//! | `fetch` | AWS Level II archive and real-time chunks, AWS Level III, international feeds, GR2Analyst polling servers |
//! | `validate` | Decode and check files; non-zero exit on failure |
//! | `bench` | Decode timing |
//! | `convert` | Write another radar format (through [`backend::VolumeWriter`]) |
//! | `publish` | Place volumes in a GR2Analyst polling directory (through [`backend::PollingPublisher`]) |
//! | `serve` | Serve a directory, such as a polling directory, over HTTP |
//!
//! The user guide is `docs/guide/cli.md`. [`run`] is the whole program; the
//! binary calls [`main_with_args`].
//!
//! # Exit status
//!
//! 0 on success, 1 when a command fails (a file does not decode, `validate`
//! finds a problem, a download fails), 2 for invalid arguments, and 3 when
//! the command needs something this build does not have (a writer, the
//! publisher, or the `net` feature for `fetch`).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![deny(missing_docs)]

pub mod backend;
mod bench;
mod convert;
mod debug_json;
pub mod dump;
#[cfg(feature = "net")]
mod fetch;
pub mod frames;
mod info;
pub mod open;
mod output;
pub mod records;
mod render;
pub mod serve;
mod summary;
mod validate;
pub mod writers;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use thiserror::Error;

use backend::{
    BackendError, Backends, Level2Compression, Level2Quantization, OutputFormat, SitePosition,
};

/// Exit status of a failed command.
pub const EXIT_FAILURE: u8 = 1;
/// Exit status for invalid arguments.
pub const EXIT_USAGE: u8 = 2;
/// Exit status when this build lacks what the command needs.
pub const EXIT_UNAVAILABLE: u8 = 3;

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "recast-radar",
    version,
    about = "Inspect, validate, render, fetch, convert and publish weather radar files",
    long_about = "Inspect, validate, render, fetch, convert and publish weather radar files.\n\n\
        Reads NEXRAD Level II (uncompressed, gzip, bzip2, LDM records), NEXRAD and TDWR Level III, \
        ODIM_H5, CfRadial 1 and 2 (classic netCDF and netCDF-4), DORADE sweep files and mobile \
        archives, and JMA radar GRIB2 tars. The format is detected from the file contents. \
        Writes NEXRAD Level II, CfRadial 1, ODIM_H5 and FM301.",
    after_help = "Exit status: 0 success, 1 failure, 2 invalid arguments, 3 not available in this build.\n\
        Guide: docs/guide/cli.md"
)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// A `recast-radar` command.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum Command {
    /// Summarize radar files: format, site, time, scan, sweeps and fields.
    Info(InfoArgs),
    /// Print every decoded value of a file: metadata, sweeps, fields, and optionally gate data.
    Dump(DumpArgs),
    /// Render one field of one sweep to a PNG image.
    Render(RenderArgs),
    /// Download radar data: AWS Level II, real-time chunks, Level III, international feeds, polling servers.
    Fetch(FetchArgs),
    /// Decode files and check them; exits 1 when any file fails.
    Validate(ValidateArgs),
    /// Time the decoding of files.
    Bench(BenchArgs),
    /// Convert a file to NEXRAD Level II, CfRadial 1, ODIM_H5 or FM301.
    Convert(ConvertArgs),
    /// Write volumes into a GR2Analyst polling directory (site folders with dir.list).
    Publish(PublishArgs),
    /// Serve a directory, such as a polling directory, over HTTP.
    Serve(ServeArgs),
}

/// Options that choose what to decode from a file.
#[derive(Clone, Debug, Default, Args)]
pub struct InputArgs {
    /// JMA tars: decode this station (JMA id such as ITOK, or station number) instead of the first.
    #[arg(long, value_name = "ID")]
    pub station: Option<String>,
    /// JMA tars: decode every station.
    #[arg(long, conflicts_with = "station")]
    pub all_stations: bool,
}

/// `info` arguments.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Radar files.
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
    /// Merge the files into one volume first (parts of one scan) and summarize that.
    #[arg(long)]
    pub merge: bool,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// Which FM301 conventions `dump --fm301` follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[non_exhaustive]
pub enum Fm301Flavor {
    /// xradar 0.12 names and attributes, azimuth-sorted rays (what `xradar.open_*` returns).
    #[default]
    Xradar,
    /// The FM301-2022 text, rays in acquisition order.
    Wmo,
}

/// `dump` arguments.
#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Radar file.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
    /// Only this sweep (0-based index).
    #[arg(long, value_name = "N")]
    pub sweep: Option<usize>,
    /// Only these fields (repeatable; names such as DBZH, case-insensitive).
    #[arg(long, value_name = "NAME")]
    pub field: Vec<String>,
    /// Include every gate value (physical units; missing gates are null) and the bins of Level III data packets.
    #[arg(long)]
    pub data: bool,
    /// Include every ray's time, azimuth, elevation and per-ray variables.
    #[arg(long)]
    pub rays: bool,
    /// Print the FM301 (CfRadial 2) group tree instead: groups, dimensions, variables, attributes.
    #[arg(long, conflicts_with_all = ["data", "rays"])]
    pub fm301: bool,
    /// Conventions for --fm301.
    #[arg(long, value_enum, default_value_t, requires = "fm301")]
    pub flavor: Fm301Flavor,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// `render` arguments.
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// Radar file.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Output PNG. With --all-sweeps, a directory.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,
    /// Sweep index (0-based). Default: the first sweep that has the field.
    #[arg(long, value_name = "N")]
    pub sweep: Option<usize>,
    /// Field name (DBZH, VRADH, ...) or quantity (reflectivity, velocity, width, zdr, rhohv, phidp, kdp).
    /// Default: reflectivity, else the sweep's first field.
    #[arg(long, value_name = "NAME")]
    pub field: Option<String>,
    /// Image width and height in pixels.
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(64..=8192))]
    pub size: u32,
    /// Percent of the half-width the last gate reaches.
    #[arg(long, default_value_t = 94, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub range_fraction: u8,
    /// Dealias radial velocity before drawing it.
    #[arg(long)]
    pub dealias: bool,
    /// GR2Analyst .pal color table to draw with.
    #[arg(long, value_name = "FILE")]
    pub palette: Option<PathBuf>,
    /// Render the field of every sweep that has it, into the --output directory.
    #[arg(long, conflicts_with = "sweep")]
    pub all_sweeps: bool,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// `validate` arguments.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Files or directories.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Descend into subdirectories.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
    /// Treat warnings as failures.
    #[arg(long)]
    pub strict: bool,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// `bench` arguments.
#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Radar files, each read into memory once and decoded repeatedly.
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// Timed decodes per file.
    #[arg(short = 'n', long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..))]
    pub iterations: u32,
    /// Untimed decodes per file before timing.
    #[arg(long, default_value_t = 1)]
    pub warmup: u32,
    /// Decoder worker threads (1 for single-core timing). Default: one per core.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
    /// Also decode the format metadata (NEXRAD metadata messages), as `info` does.
    #[arg(long)]
    pub metadata: bool,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// Sweep indices given on the command line: `0,2,5-9` (0-based, in the
/// order given).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepList(pub Vec<usize>);

impl std::str::FromStr for SweepList {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        /// Most indices one list may name.
        const MAX_INDICES: usize = 4096;
        let bad = || format!("`{text}` is not a list of sweep indices such as 0,2,5-9");
        let mut indices = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            let (first, last) = match part.split_once('-') {
                Some((first, last)) => (first.trim(), last.trim()),
                None => (part, part),
            };
            let first: usize = first.parse().map_err(|_| bad())?;
            let last: usize = last.parse().map_err(|_| bad())?;
            if last < first || indices.len() + (last - first) >= MAX_INDICES {
                return Err(bad());
            }
            indices.extend(first..=last);
        }
        Ok(Self(indices))
    }
}

/// Options of the Level II writer, for `convert --to level2` and `publish`
/// (`docs/level2/writer.md`).
#[derive(Clone, Debug, Default, Args)]
pub struct Level2Args {
    /// Level II record packing.
    #[arg(long, value_enum, default_value_t)]
    pub level2_compression: Level2Compression,
    /// Level II value coding: precise never codes a value more coarsely than its source; compatible
    /// keeps NEXRAD's word sizes (what xradar 0.12 reads); standard writes NOAA's codings where they
    /// hold every value. No policy clips a value.
    #[arg(long, value_enum, default_value_t)]
    pub quantization: Level2Quantization,
    /// The radar's Nyquist velocity (m/s), written in every radial whose source has none (JMA
    /// volumes have none). Without it such radials carry 0, which readers take as unknown.
    #[arg(long, value_name = "M/S")]
    pub nyquist: Option<f32>,
    /// The radar's unambiguous range (m), written in every radial whose source has none.
    #[arg(long, value_name = "M")]
    pub unambiguous_range: Option<f32>,
    /// Leave out gates centred before the radar instead of refusing the field (Message 1 volumes
    /// place their Doppler gates from -375 m).
    #[arg(long)]
    pub drop_negative_range_gates: bool,
    /// Fail, writing nothing, when the output would leave out a field or a sweep of the volume.
    #[arg(long)]
    pub strict: bool,
}

/// Changes made to the decoded volume before it is written.
#[derive(Clone, Debug, Default, Args)]
pub struct EditArgs {
    /// Keep only these sweeps, in this order: 0-based indices and ranges such as 0,2,5-9 (the
    /// numbers `info` lists). Level II holds at most 32.
    #[arg(long, value_name = "LIST")]
    pub sweeps: Option<SweepList>,
    /// Put the sweeps in the order their first rays were collected.
    #[arg(long)]
    pub sweeps_in_time_order: bool,
    /// Write each scan cycle of the input as its own volume, its sweeps in the order they were
    /// collected (a JMA 10-minute tar holds two 5-minute cycles; a Level II file holds one):
    /// convert writes cycle N to --output with _N before its extension (out_1.ar2v), publish
    /// publishes each.
    #[arg(long)]
    pub split_scan_cycles: bool,
    /// Site position to write, as LAT,LON,HEIGHT: degrees north, degrees east, metres above sea
    /// level (Message 1 volumes carry none).
    #[arg(
        long,
        value_name = "LAT,LON,HEIGHT",
        allow_hyphen_values = true,
        conflicts_with = "position_from"
    )]
    pub position: Option<SitePosition>,
    /// Take the site position from another file of the same radar (a Message 31 file for a
    /// Message 1 volume, for example).
    #[arg(long, value_name = "FILE")]
    pub position_from: Option<PathBuf>,
}

/// `convert` arguments.
#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input files. Several files need --merge (split scans such as DWD sweeps or ODIM per-moment files).
    #[arg(required = true, value_name = "FILE")]
    pub inputs: Vec<PathBuf>,
    /// Output format.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub to: OutputFormat,
    /// Output file (with --chunks, a directory).
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,
    /// Write NEXRAD real-time chunks instead of one file: the S, I and E files of the
    /// unidata-nexrad-level2-chunks bucket, as --output/SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-K.
    #[arg(long, conflicts_with = "gzip")]
    pub chunks: bool,
    /// Merge the inputs into one volume (parts of one scan).
    #[arg(long)]
    pub merge: bool,
    /// Which volume of a multi-volume input (mobile archive, --all-stations tar), 0-based.
    #[arg(long, value_name = "N")]
    pub volume: Option<usize>,
    /// Level II writer options.
    #[command(flatten)]
    pub level2: Level2Args,
    /// Sweep selection and site position.
    #[command(flatten)]
    pub edit: EditArgs,
    /// Worker threads for decoding and compressing (default: one per core). Each Level II
    /// compressing thread holds about 18 MB.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=1024))]
    pub threads: Option<u32>,
    /// Wrap the output in gzip.
    #[arg(long)]
    pub gzip: bool,
    /// Radar identifier to write instead of the volume's (Level II: 4 characters).
    #[arg(long, value_name = "ID")]
    pub site: Option<String>,
    /// Replace an existing output file (or chunk files).
    #[arg(short, long)]
    pub force: bool,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// `publish` arguments.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Input files; each volume is published as one Level II file.
    #[arg(required = true, value_name = "FILE")]
    pub inputs: Vec<PathBuf>,
    /// Root of the polling directory.
    #[arg(long, value_name = "DIR")]
    pub dir: PathBuf,
    /// Site folder and file-name prefix (default: the volume's radar id).
    #[arg(long, value_name = "ID")]
    pub site: Option<String>,
    /// Volumes to keep listed in each site's dir.list.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    pub keep: u32,
    /// Merge all inputs into one volume first (parts of one scan).
    #[arg(long)]
    pub merge: bool,
    /// Level II writer options.
    #[command(flatten)]
    pub level2: Level2Args,
    /// Sweep selection and site position.
    #[command(flatten)]
    pub edit: EditArgs,
    /// Worker threads for decoding and compressing (default: one per core). Each Level II
    /// compressing thread holds about 18 MB.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=1024))]
    pub threads: Option<u32>,
    /// Do not add the site to config.cfg and grlevel2.cfg.
    #[arg(long)]
    pub no_site_config: bool,
    /// What to decode from each input.
    #[command(flatten)]
    pub input: InputArgs,
}

/// `serve` arguments.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Directory to serve.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
    /// Address and port to listen on (port 0 picks a free port).
    #[arg(long, default_value = "127.0.0.1:8080", value_name = "ADDR")]
    pub bind: String,
    /// Connections served at once; more wait.
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..=1024))]
    pub max_connections: u32,
    /// Stop after this many requests (for scripts and tests).
    #[arg(long, value_name = "N", hide = true)]
    pub max_requests: Option<u64>,
}

/// `fetch` arguments.
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// The data source.
    #[command(subcommand)]
    pub source: FetchSource,
}

/// A `fetch` data source.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum FetchSource {
    /// NEXRAD Level II volumes from the unidata-nexrad-level2 bucket on AWS.
    Level2(Level2FetchArgs),
    /// The newest NEXRAD Level II real-time volume, assembled from the unidata-nexrad-level2-chunks bucket.
    Chunks(ChunksFetchArgs),
    /// NEXRAD Level III products from the unidata-nexrad-level3 bucket on AWS.
    Level3(Level3FetchArgs),
    /// International radar feeds (ODIM_H5, JMA, ...) through the recast-radar-data providers.
    Intl(IntlFetchArgs),
    /// Volumes from a GR2Analyst polling server (site folders with dir.list).
    Polling(PollingFetchArgs),
    /// List NEXRAD sites (embedded table; no network).
    Sites(SitesFetchArgs),
}

/// `fetch level2` arguments.
#[derive(Debug, Args)]
pub struct Level2FetchArgs {
    /// Radar id, such as KTLX.
    pub site: String,
    /// UTC date (YYYY-MM-DD). Default: the newest volumes.
    #[arg(long, value_name = "DATE")]
    pub date: Option<chrono::NaiveDate>,
    /// UTC time (HH:MM or HH:MM:SS) to pick the nearest volumes of --date.
    #[arg(long, value_name = "TIME", requires = "date", value_parser = parse_time)]
    pub time: Option<chrono::NaiveTime>,
    /// Number of volumes.
    #[arg(short = 'n', long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub count: u32,
    /// List the matching volumes without downloading.
    #[arg(long)]
    pub list: bool,
    /// Output directory.
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    pub output: PathBuf,
}

/// `fetch chunks` arguments.
#[derive(Debug, Args)]
pub struct ChunksFetchArgs {
    /// Radar id, such as KTLX.
    pub site: String,
    /// Output directory.
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    pub output: PathBuf,
    /// Also keep each chunk as its own file.
    #[arg(long)]
    pub keep_chunks: bool,
    /// List the chunks without downloading.
    #[arg(long)]
    pub list: bool,
}

/// `fetch level3` arguments.
#[derive(Debug, Args)]
pub struct Level3FetchArgs {
    /// Radar id (TLX or KTLX).
    pub site: String,
    /// Product mnemonic, such as N0B, N0G, NST, DVL.
    pub product: String,
    /// UTC date (YYYY-MM-DD). Default: the newest products.
    #[arg(long, value_name = "DATE")]
    pub date: Option<chrono::NaiveDate>,
    /// UTC time (HH:MM or HH:MM:SS) to pick the nearest products of --date.
    #[arg(long, value_name = "TIME", requires = "date", value_parser = parse_time)]
    pub time: Option<chrono::NaiveTime>,
    /// Number of products.
    #[arg(short = 'n', long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub count: u32,
    /// List the matching products without downloading.
    #[arg(long)]
    pub list: bool,
    /// Output directory.
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    pub output: PathBuf,
}

/// `fetch intl` arguments.
#[derive(Debug, Args)]
pub struct IntlFetchArgs {
    /// Provider id (see `fetch intl` with no arguments).
    pub provider: Option<String>,
    /// Site id of the provider (see --list-sites).
    pub site: Option<String>,
    /// List the provider's sites.
    #[arg(long, requires = "provider", conflicts_with = "site")]
    pub list_sites: bool,
    /// With --list-sites: ask the provider's catalog instead of the embedded table.
    #[arg(long, requires = "list_sites")]
    pub online: bool,
    /// UTC date (YYYY-MM-DD) of archived frames, for providers with an archive
    /// (`archive yes` in `fetch intl`). Default: the newest frames.
    #[arg(long, value_name = "DATE", requires = "site")]
    pub date: Option<chrono::NaiveDate>,
    /// UTC time (HH:MM or HH:MM:SS) to pick the archived frames of --date nearest it.
    #[arg(long, value_name = "TIME", requires = "date", value_parser = parse_time)]
    pub time: Option<chrono::NaiveTime>,
    /// Number of frames: the newest (providers without a rolling catalog give 1),
    /// the first of --date, or the nearest --time.
    #[arg(short = 'n', long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub count: u32,
    /// List the frames and their files without downloading (with --date alone: every frame of the day).
    #[arg(long)]
    pub list: bool,
    /// Output directory.
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    pub output: PathBuf,
}

/// `fetch polling` arguments.
#[derive(Debug, Args)]
pub struct PollingFetchArgs {
    /// Base URL of the polling directory, such as `https://mesonet-nexrad.agron.iastate.edu/level2/raw/`.
    pub url: String,
    /// Site folder, such as KTLX. Without it, the sites of config.cfg are listed.
    pub site: Option<String>,
    /// Number of volumes, newest first.
    #[arg(short = 'n', long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub count: u32,
    /// List the site's dir.list without downloading.
    #[arg(long)]
    pub list: bool,
    /// Minimum time between requests to the server, in milliseconds (at least 250).
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(250..))]
    pub interval_ms: u64,
    /// Output directory.
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    pub output: PathBuf,
}

/// `fetch sites` arguments.
#[derive(Debug, Args)]
pub struct SitesFetchArgs {
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

fn parse_time(text: &str) -> Result<chrono::NaiveTime, String> {
    chrono::NaiveTime::parse_from_str(text, "%H:%M:%S")
        .or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M"))
        .map_err(|_| format!("`{text}` is not a time (HH:MM or HH:MM:SS)"))
}

/// Error from a command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliError {
    /// A file could not be read or written.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
    /// A file did not decode.
    #[error("{}: {message}", .path.display())]
    Decode {
        /// The file.
        path: PathBuf,
        /// The decoder's message.
        message: String,
    },
    /// The arguments do not make sense together.
    #[error("{0}")]
    Usage(String),
    /// A writer or the publisher failed or is missing.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// This build does not have what the command needs.
    #[error("{0}")]
    Unavailable(String),
    /// The command ran and failed, for example `validate` found problems.
    #[error("{0}")]
    Failed(String),
    /// Writing the command's output failed.
    #[error("writing output: {0}")]
    Output(io::Error),
}

impl CliError {
    /// The process exit status for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Unavailable(_) => EXIT_UNAVAILABLE,
            Self::Backend(err) if err.is_unavailable() => EXIT_UNAVAILABLE,
            _ => EXIT_FAILURE,
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<io::Error> for CliError {
    fn from(err: io::Error) -> Self {
        Self::Output(err)
    }
}

/// Parse `args` (the first item is the program name), run the command with
/// the builtin backends, print errors, and return the exit status.
pub fn main_with_args<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = if err.use_stderr() { EXIT_USAGE } else { 0 };
            // Help and version go to stdout, errors to stderr.
            let _ = err.print();
            return ExitCode::from(code);
        }
    };
    let backends = Backends::builtin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run(cli, &backends, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (`recast-radar dump FILE | head`): stop quietly.
        Err(CliError::Output(err)) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            let _ = out.flush();
            eprintln!("recast-radar: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

/// Run a parsed command, writing its report to `out` and progress notes to
/// standard error.
pub fn run(cli: Cli, backends: &Backends, out: &mut dyn Write) -> Result<(), CliError> {
    match cli.command {
        Command::Info(args) => info::run(&args, out),
        Command::Dump(args) => dump::run(&args, out),
        Command::Render(args) => render::run(&args, out),
        Command::Fetch(args) => run_fetch(args, out),
        Command::Validate(args) => validate::run(&args, out),
        Command::Bench(args) => bench::run(&args, out),
        Command::Convert(args) => convert::run(&args, backends, out),
        Command::Publish(args) => convert::publish(&args, backends, out),
        Command::Serve(args) => serve::run_command(&args, out),
    }
}

#[cfg(feature = "net")]
fn run_fetch(args: FetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    fetch::run(args, out)
}

#[cfg(not(feature = "net"))]
fn run_fetch(args: FetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let _ = (args, out);
    Err(CliError::Unavailable(
        "fetch is not available: this build has no `net` feature".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn exit_codes_follow_the_documented_classes() {
        assert_eq!(CliError::Usage(String::new()).exit_code(), EXIT_USAGE);
        assert_eq!(
            CliError::Backend(BackendError::WriterUnavailable(OutputFormat::Level2)).exit_code(),
            EXIT_UNAVAILABLE
        );
        assert_eq!(
            CliError::Backend(BackendError::PublisherUnavailable).exit_code(),
            EXIT_UNAVAILABLE
        );
        assert_eq!(CliError::Failed(String::new()).exit_code(), EXIT_FAILURE);
    }

    #[test]
    fn times_parse_with_or_without_seconds() {
        assert_eq!(
            parse_time("21:04"),
            Ok(chrono::NaiveTime::from_hms_opt(21, 4, 0).unwrap_or_default())
        );
        assert_eq!(
            parse_time("21:04:05"),
            Ok(chrono::NaiveTime::from_hms_opt(21, 4, 5).unwrap_or_default())
        );
        assert!(parse_time("9pm").is_err());
    }
}
