//! Cross-library decode benchmark: the recast-radar-tools side.
//!
//! Decodes one radar file of any format this repository reads, from bytes
//! read before any timing, `--warmup` + `--iters` times, and prints one JSON
//! line with every timed sample and a content summary. The driver in
//! `tools/xlib-bench/run.py` runs this binary and the other libraries'
//! harnesses under the same pinning, rounds and peak-RSS accounting
//! (`docs/perf/cross-library.md`).
//!
//! ```text
//! decode_bench --format l2|l2-meta|l3|l3-product|odim|odim-cart|cfrad1|dorade|jma|jma-all|auto FILE
//!              [--iters N] [--warmup N] [--threads 0|N] [--physical]
//!              [--order-rays] [--view] [--from-path] [--debug-hash]
//!              [--wait-stdin]
//! ```
//!
//! - `l3-product` decodes a Level III file without converting it to a
//!   volume (`recast_radar_io_level3::decode_message`: every block and
//!   packet of a product, a General Status Message or a text message), for
//!   the graphic and tabular products that have no data array for a volume
//!   (storm tracking, mesocyclone, storm structure), as MetPy's `Level3File`
//!   parses them; `--physical`, `--order-rays` and `--view` do nothing for
//!   it, and its hash is that of the decoded message's `Debug` text.
//! - `odim-cart` is an ODIM_H5 Cartesian product (`IMAGE`, the `MAX`
//!   composite) through `decode_odim_h5_cartesian_max`, a grid of physical
//!   `f32` values rather than a volume; `--physical`, `--order-rays` and
//!   `--view` do nothing for it.
//! - `--threads 1` builds the global rayon pool with one thread that is the
//!   calling thread (`use_current_thread`), so the whole decode runs on it;
//!   `--threads N` uses N pool threads; `0` (default) keeps rayon's default.
//! - `--physical` also expands every field to float32 physical values
//!   ([`Field::to_physical`](recast_radar_core::model::Field::to_physical))
//!   inside the timed region, the work Py-ART, xradar and MetPy's masked
//!   arrays do.
//! - `--order-rays` puts every sweep's rays in the xradar-default view order
//!   in place (`fm301::order_rays_for_view`) before `--view`.
//! - `--view` also builds the FM301 view with the xradar defaults and
//!   materializes every variable (what a binding hands to xarray).
//! - `--from-path` reads the file inside every timed sample (path to
//!   decoded arrays, as the other libraries' harnesses time it); by default
//!   the bytes are read once before any timing.
//! - `--debug-hash` adds an FNV hash of every volume's `Debug` text (the
//!   whole model), for comparing builds; it costs memory, so peak-RSS runs
//!   leave it off.
//! - `--wait-stdin` reads one line from stdin before any work so the driver
//!   can set CPU affinity on Windows first.

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use recast_radar_core::fm301::{ViewOptions, order_rays_for_view, volume_view};
use recast_radar_core::model::{FieldData, Volume};

const USAGE: &str = "usage: decode_bench --format l2|l2-meta|l3|l3-product|odim|odim-cart|cfrad1|dorade|jma|jma-all|auto FILE \
[--iters N] [--warmup N] [--threads 0|N] [--physical] [--order-rays] [--view] [--from-path] [--debug-hash] [--wait-stdin]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Level2,
    /// Level II volume and metadata (`read_volume_with_metadata`).
    Level2Metadata,
    Level3,
    /// A Level III file decoded as a message, not converted to a volume.
    Level3Product,
    Odim,
    /// An ODIM_H5 Cartesian `MAX` product: a grid, not a volume.
    OdimCartesian,
    CfRadial1,
    Dorade,
    /// First station of a JMA tar (what the router decodes).
    Jma,
    /// Every station of a JMA tar.
    JmaAll,
    /// The byte router (`recast_radar_io::read_supported_volume_bytes`).
    Auto,
}

impl Format {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "l2" => Self::Level2,
            "l2-meta" => Self::Level2Metadata,
            "l3" => Self::Level3,
            "l3-product" => Self::Level3Product,
            "odim" => Self::Odim,
            "odim-cart" => Self::OdimCartesian,
            "cfrad1" => Self::CfRadial1,
            "dorade" => Self::Dorade,
            "jma" => Self::Jma,
            "jma-all" => Self::JmaAll,
            "auto" => Self::Auto,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Level2 => "l2",
            Self::Level2Metadata => "l2-meta",
            Self::Level3 => "l3",
            Self::Level3Product => "l3-product",
            Self::Odim => "odim",
            Self::OdimCartesian => "odim-cart",
            Self::CfRadial1 => "cfrad1",
            Self::Dorade => "dorade",
            Self::Jma => "jma",
            Self::JmaAll => "jma-all",
            Self::Auto => "auto",
        }
    }
}

struct Args {
    format: Format,
    file: PathBuf,
    iters: usize,
    warmup: usize,
    threads: usize,
    physical: bool,
    view: bool,
    order_rays: bool,
    from_path: bool,
    debug_hash: bool,
    wait_stdin: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut format = None;
    let mut file = None;
    let mut iters = 10;
    let mut warmup = 1;
    let mut threads = 0;
    let mut physical = false;
    let mut view = false;
    let mut order_rays = false;
    let mut from_path = false;
    let mut debug_hash = false;
    let mut wait_stdin = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--format" => {
                let text = value("--format")?;
                format =
                    Some(Format::parse(&text).ok_or_else(|| format!("unknown format {text}"))?);
            }
            "--iters" => {
                iters = value("--iters")?
                    .parse()
                    .map_err(|e| format!("--iters: {e}"))?
            }
            "--warmup" => {
                warmup = value("--warmup")?
                    .parse()
                    .map_err(|e| format!("--warmup: {e}"))?;
            }
            "--threads" => {
                threads = value("--threads")?
                    .parse()
                    .map_err(|e| format!("--threads: {e}"))?;
            }
            "--physical" => physical = true,
            "--view" => view = true,
            "--order-rays" => order_rays = true,
            "--from-path" => from_path = true,
            "--debug-hash" => debug_hash = true,
            "--wait-stdin" => wait_stdin = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if file.replace(PathBuf::from(other)).is_some() {
                    return Err("only one FILE".to_owned());
                }
            }
        }
    }
    Ok(Args {
        format: format.ok_or("--format is required")?,
        file: file.ok_or("FILE is required")?,
        iters,
        warmup,
        threads,
        physical,
        view,
        order_rays,
        from_path,
        debug_hash,
        wait_stdin,
    })
}

fn decode(format: Format, bytes: &[u8]) -> Result<Vec<Volume>, String> {
    let one = |result: Result<Volume, String>| result.map(|volume| vec![volume]);
    match format {
        Format::Level2 => {
            one(recast_radar_io_nexrad::read_volume_from_bytes(bytes).map_err(|e| e.to_string()))
        }
        Format::Level2Metadata => one(recast_radar_io_nexrad::read_volume_with_metadata(bytes)
            .map(|decoded| decoded.volume)
            .map_err(|e| e.to_string())),
        Format::Level3 => {
            one(recast_radar_io_level3::read_level3_volume(bytes).map_err(|e| e.to_string()))
        }
        Format::Odim => {
            one(recast_radar_io_odim::read_odim_h5_volume(bytes).map_err(|e| e.to_string()))
        }
        Format::OdimCartesian => Err("an ODIM_H5 Cartesian product is a grid".to_owned()),
        Format::Level3Product => Err("a Level III message is not a volume".to_owned()),
        Format::CfRadial1 => {
            one(recast_radar_io_cfradial::read_cfradial1_volume(bytes).map_err(|e| e.to_string()))
        }
        Format::Dorade => {
            one(recast_radar_io_dorade::read_dorade_sweep_volume(bytes).map_err(|e| e.to_string()))
        }
        Format::Jma => {
            one(recast_radar_io_jma::read_jma_tar_first_station(bytes).map_err(|e| e.to_string()))
        }
        Format::JmaAll => {
            recast_radar_io_jma::read_jma_tar_volumes(bytes, None).map_err(|e| e.to_string())
        }
        Format::Auto => {
            one(recast_radar_io::read_supported_volume_bytes(bytes).map_err(|e| e.to_string()))
        }
    }
}

/// This process's peak resident set since exec in KiB (`VmHWM` in
/// `/proc/self/status`), where the kernel reports it: the cross-check of the
/// driver's GNU time figure.
fn self_hwm_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Float expansion of every field, summed so the work cannot be elided.
fn expand_physical(volumes: &[Volume]) -> f64 {
    let mut total = 0.0f64;
    for field in volumes
        .iter()
        .flat_map(|volume| volume.sweeps.iter())
        .flat_map(|sweep| sweep.fields.iter())
    {
        let values = field.to_physical();
        total += values.len() as f64;
        total += f64::from(values.first().copied().unwrap_or(0.0).is_nan() as u8);
        std::hint::black_box(&values);
    }
    total
}

/// Build the xradar-default FM301 view and materialize every variable.
fn materialize_view(volumes: &[Volume]) -> usize {
    let mut elements = 0;
    for volume in volumes {
        let view = volume_view(volume, ViewOptions::XRADAR, None).expect("FM301 view");
        let mut stack = vec![&view.root];
        while let Some(group) = stack.pop() {
            for variable in &group.variables {
                if let Some(array) = variable.values.materialize() {
                    elements += array.len();
                    std::hint::black_box(&array);
                }
            }
            stack.extend(group.children.iter());
        }
    }
    elements
}

/// FNV hash of every field's `to_physical` output (with `physical`) and of
/// every materialized xradar-default view variable (with `view`), computed
/// after the timed samples so builds can be compared on derived output too.
fn derived_hash(volumes: &[Volume], physical: bool, view: bool) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    if physical {
        for field in volumes
            .iter()
            .flat_map(|volume| volume.sweeps.iter())
            .flat_map(|sweep| sweep.fields.iter())
        {
            for value in field.to_physical() {
                hash = fnv1a(hash, &value.to_bits().to_le_bytes());
            }
        }
    }
    if view {
        for volume in volumes {
            let view = volume_view(volume, ViewOptions::XRADAR, None).expect("FM301 view");
            let mut stack = vec![&view.root];
            while let Some(group) = stack.pop() {
                for variable in &group.variables {
                    hash = fnv1a(hash, variable.name.as_bytes());
                    if let Some(array) = variable.values.materialize() {
                        for index in 0..array.len() {
                            let value = array.get_f64(index).unwrap_or(f64::NAN);
                            hash = fnv1a(hash, &value.to_bits().to_le_bytes());
                        }
                    }
                }
                stack.extend(group.children.iter());
            }
        }
    }
    hash
}

#[derive(Default)]
struct Summary {
    volumes: usize,
    sweeps: usize,
    rays: usize,
    fields: usize,
    gates: usize,
    /// Bytes of field values, and the bytes their buffers have allocated.
    field_bytes: usize,
    field_capacity_bytes: usize,
    hash: u64,
}

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The content of an ODIM Cartesian grid: one field of width x height
/// cells, its rows counted as rays.
fn summarize_grid(grid: &recast_radar_io_odim::OdimCartesianGrid) -> Summary {
    // The grid keeps its packed plane; the physical values (what the hash
    // has always covered) are expanded here.
    let values = grid.values();
    Summary {
        volumes: 1,
        sweeps: 0,
        rays: grid.geometry.height,
        fields: 1,
        gates: values.len(),
        field_bytes: values.len() * 4,
        field_capacity_bytes: values.capacity() * 4,
        hash: values.iter().fold(0xcbf2_9ce4_8422_2325, |h, v| {
            fnv1a(h, &v.to_bits().to_le_bytes())
        }),
    }
}

/// The content of a decoded Level III message: no volume; the hash of
/// its `Debug` text (every block and packet).
fn summarize_message(message: &recast_radar_io_level3::Level3Message) -> Summary {
    Summary {
        hash: fnv1a(0xcbf2_9ce4_8422_2325, format!("{message:?}").as_bytes()),
        ..Summary::default()
    }
}

/// What one timed sample decoded: volumes, a Cartesian grid, or a Level III
/// message.
#[derive(Default)]
struct Decoded {
    volumes: Vec<Volume>,
    grid: Option<recast_radar_io_odim::OdimCartesianGrid>,
    message: Option<recast_radar_io_level3::Level3Message>,
}

fn summarize(volumes: &[Volume]) -> Summary {
    let mut summary = Summary {
        volumes: volumes.len(),
        hash: 0xcbf2_9ce4_8422_2325,
        ..Summary::default()
    };
    for sweep in volumes.iter().flat_map(|volume| volume.sweeps.iter()) {
        summary.sweeps += 1;
        summary.rays += sweep.nrays();
        for value in &sweep.rays.azimuth_deg {
            summary.hash = fnv1a(summary.hash, &value.to_le_bytes());
        }
        for field in &sweep.fields {
            summary.fields += 1;
            summary.gates += field.nrays as usize * field.ngates as usize;
            let (len, capacity, width) = match &field.data {
                FieldData::U8 { values, .. } => (values.len(), values.capacity(), 1),
                FieldData::I8 { values, .. } => (values.len(), values.capacity(), 1),
                FieldData::U16 { values, .. } => (values.len(), values.capacity(), 2),
                FieldData::I16 { values, .. } => (values.len(), values.capacity(), 2),
                FieldData::I32 { values, .. } => (values.len(), values.capacity(), 4),
                FieldData::F32 { values, .. } => (values.len(), values.capacity(), 4),
                FieldData::F64 { values, .. } => (values.len(), values.capacity(), 8),
            };
            summary.field_bytes += len * width;
            summary.field_capacity_bytes += capacity * width;
            summary.hash = fnv1a(summary.hash, field.name.as_str().as_bytes());
            summary.hash = fnv1a(summary.hash, &field.nrays.to_le_bytes());
            summary.hash = fnv1a(summary.hash, &field.ngates.to_le_bytes());
            summary.hash = match &field.data {
                FieldData::U8 { values, .. } => fnv1a(summary.hash, values),
                FieldData::U16 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
                FieldData::I8 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
                FieldData::I16 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
                FieldData::I32 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
                FieldData::F32 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
                FieldData::F64 { values, .. } => values
                    .iter()
                    .fold(summary.hash, |h, v| fnv1a(h, &v.to_le_bytes())),
            };
        }
    }
    summary
}

/// Everything inside one timed sample. Not inlined, so a profiler can
/// restrict collection to it (`valgrind --toggle-collect='*timed_work*'`).
#[inline(never)]
fn timed_work(
    args: &Args,
    read: &dyn Fn() -> Result<Vec<u8>, String>,
    preloaded: &[u8],
) -> Result<Decoded, String> {
    if args.format == Format::OdimCartesian {
        let grid = |bytes: &[u8]| {
            recast_radar_io_odim::decode_odim_h5_cartesian_max(bytes).map_err(|e| e.to_string())
        };
        let grid = if args.from_path {
            grid(&read()?)?
        } else {
            grid(preloaded)?
        };
        return Ok(Decoded {
            grid: Some(grid),
            ..Decoded::default()
        });
    }
    if args.format == Format::Level3Product {
        let message =
            |bytes: &[u8]| recast_radar_io_level3::decode_message(bytes).map_err(|e| e.to_string());
        let message = if args.from_path {
            message(&read()?)?
        } else {
            message(preloaded)?
        };
        return Ok(Decoded {
            message: Some(message),
            ..Decoded::default()
        });
    }
    let mut volumes = if args.from_path {
        decode(args.format, &read()?)?
    } else {
        decode(args.format, preloaded)?
    };
    if args.order_rays {
        for volume in &mut volumes {
            order_rays_for_view(volume, ViewOptions::XRADAR).map_err(|e| e.to_string())?;
        }
    }
    if args.physical {
        std::hint::black_box(expand_physical(&volumes));
    }
    if args.view {
        std::hint::black_box(materialize_view(&volumes));
    }
    Ok(Decoded {
        volumes,
        ..Decoded::default()
    })
}

fn run(args: &Args) -> Result<String, String> {
    if args.wait_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
    }
    if args.threads > 0 {
        let mut builder = rayon::ThreadPoolBuilder::new().num_threads(args.threads);
        if args.threads == 1 {
            builder = builder.use_current_thread();
        }
        builder.build_global().map_err(|e| e.to_string())?;
    }
    let read = || std::fs::read(&args.file).map_err(|e| format!("{}: {e}", args.file.display()));
    let preloaded = if args.from_path { Vec::new() } else { read()? };

    let mut samples = Vec::with_capacity(args.iters);
    let mut last = None;
    for iteration in 0..args.warmup + args.iters {
        let started = Instant::now();
        let decoded = timed_work(args, &read, &preloaded)?;
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if iteration >= args.warmup {
            samples.push(elapsed);
        }
        // Drop the previous volume outside the timed region.
        drop(last.replace(decoded));
    }
    let Decoded {
        volumes,
        grid,
        message,
    } = last.unwrap_or_default();
    let summary = match (&grid, &message) {
        (Some(grid), _) => summarize_grid(grid),
        (None, Some(message)) => summarize_message(message),
        (None, None) => summarize(&volumes),
    };
    // Everything the model holds, provenance and decode statistics included.
    // Opt-in: the Debug text of a volume is several times its size, which
    // would dominate the process's peak RSS.
    let debug_hash = if args.debug_hash {
        let hash = volumes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, volume| {
            fnv1a(hash, format!("{volume:?}").as_bytes())
        });
        format!("{hash:016x}")
    } else {
        String::new()
    };
    let derived = if args.physical || args.view {
        format!("{:016x}", derived_hash(&volumes, args.physical, args.view))
    } else {
        String::new()
    };
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN);
    let min = sorted.first().copied().unwrap_or(f64::NAN);
    let samples_text = samples
        .iter()
        .map(|value| format!("{value:.3}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "{{\"lib\":\"recast\",\"format\":\"{}\",\"threads\":{},\"physical\":{},\"view\":{},\
\"from_path\":{},\"iters\":{},\"median_ms\":{median:.3},\"min_ms\":{min:.3},\"samples_ms\":[{samples_text}],\
\"volumes\":{},\"sweeps\":{},\"rays\":{},\"fields\":{},\"gates\":{},\"field_bytes\":{},\"field_capacity_bytes\":{},\"hash\":\"{:016x}\",\
\"derived_hash\":\"{derived}\",\"debug_hash\":\"{debug_hash}\",\"paired_bzip2\":{},\"self_hwm_kb\":{}}}",
        args.format.name(),
        args.threads,
        args.physical,
        args.view,
        args.from_path,
        args.iters,
        summary.volumes,
        summary.sweeps,
        summary.rays,
        summary.fields,
        summary.gates,
        summary.field_bytes,
        summary.field_capacity_bytes,
        summary.hash,
        cfg!(feature = "paired-bzip2"),
        self_hwm_kb().map_or("null".to_owned(), |kb| kb.to_string()),
    ))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("decode_bench: {message}");
            ExitCode::FAILURE
        }
    }
}
