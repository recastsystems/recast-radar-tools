//! BowEcho headless benchmark harness.
//!
//! Decodes one Level-II archive volume and rasterizes its lowest
//! reflectivity and velocity sweeps through the exact `recast_radar_io` /
//! `recast_radar_render` paths the app uses, with wall-clock timing and a pixel
//! checksum. See README.md for the three purposes this serves (LTO A/B
//! referee, x86-64-v3 validation, PGO training workload).

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

mod dealias_eval;

use recast_radar_core::{FieldName, Quantity, Sweep, Volume};
use recast_radar_render::{
    ColorTableSet, ViewportFieldCache, ViewportRasterOptions, color_family_for_name,
    viewport_rgba_buffer_len,
};

const USAGE: &str =
    "usage: recast-radar-bench <path-to-level2-file> [--iters N] [--json] [--sweeps R,V]

Decode the volume and raster its lowest reflectivity + velocity sweeps at
three viewport shapes, once as warmup and then N timed iterations
(default 10). --sweeps R,V renders sweep index R for reflectivity and V for
velocity instead of the lowest ones. --json emits one machine-readable
summary line instead of the human table. Exits nonzero if the rendered
pixels are not identical across iterations.";

const DEFAULT_ITERS: usize = 10;

/// The three representative viewport shapes rendered per field per
/// iteration: 720p / 1080p / 1440p at a fixed storm-scale zoom.
const BENCH_VIEWPORT_SHAPES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (2560, 1440)];

/// Storm-scale zoom: 0.25 km/px puts a 1080p viewport ~270 km tall,
/// comfortably inside a 460 km Level-II reflectivity range, so every
/// pixel row does real gate sampling work.
const BENCH_KM_PER_PX: f32 = 0.25;

/// Nonzero rotation so the rotation-baked azimuth path is exercised.
/// 20 mrad is a realistic AEQD meridian-convergence angle and a multiple
/// of the app's 5 mrad quantization step.
const BENCH_ROTATION_RAD: f32 = 0.02;

fn bench_viewport_options(width: u32, height: u32) -> ViewportRasterOptions {
    ViewportRasterOptions {
        width,
        height,
        // Radar slightly below center, like a storm-following view; the
        // km/px is isotropic to match the app's AEQD screen frame.
        radar_x_px: width as f32 * 0.5,
        radar_y_px: height as f32 * 0.55,
        km_per_px_x: BENCH_KM_PER_PX,
        km_per_px_y: BENCH_KM_PER_PX,
        rotation_rad: BENCH_ROTATION_RAD,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Run(BenchArgs),
}

#[derive(Debug, PartialEq, Eq)]
struct BenchArgs {
    file: PathBuf,
    iters: usize,
    json: bool,
    /// Explicit (reflectivity, velocity) sweep indices instead of the
    /// lowest sweeps: reproduces a selection made by another build, so the
    /// pixel checksum compares the render pipeline alone.
    sweeps: Option<(usize, usize)>,
}

/// Parse CLI arguments (program name already stripped).
fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut file = None;
    let mut iters = DEFAULT_ITERS;
    let mut json = false;
    let mut sweeps = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => return Ok(Command::Help),
            "--json" => json = true,
            "--iters" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--iters requires a value".to_owned())?;
                iters = parse_iters(value)?;
            }
            _ if arg.starts_with("--iters=") => {
                iters = parse_iters(&arg["--iters=".len()..])?;
            }
            "--sweeps" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--sweeps requires a value".to_owned())?;
                sweeps = Some(parse_sweeps(value)?);
            }
            _ if arg.starts_with("--sweeps=") => {
                sweeps = Some(parse_sweeps(&arg["--sweeps=".len()..])?);
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option {arg}"));
            }
            _ => {
                if file.is_some() {
                    return Err(format!("unexpected extra argument {arg}"));
                }
                file = Some(PathBuf::from(arg));
            }
        }
        index += 1;
    }
    let file = file.ok_or_else(|| "missing <path-to-level2-file> argument".to_owned())?;
    Ok(Command::Run(BenchArgs {
        file,
        iters,
        json,
        sweeps,
    }))
}

/// `R,V`: the reflectivity and velocity sweep indices.
fn parse_sweeps(value: &str) -> Result<(usize, usize), String> {
    value
        .split_once(',')
        .and_then(|(reflectivity, velocity)| {
            Some((
                reflectivity.trim().parse().ok()?,
                velocity.trim().parse().ok()?,
            ))
        })
        .ok_or_else(|| format!("--sweeps expects two sweep indices R,V, got {value:?}"))
}

fn parse_iters(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or_else(|| format!("--iters expects a positive integer, got {value:?}"))
}

struct StageSeries {
    /// Stable machine key used in the JSON output.
    key: &'static str,
    /// Human table label.
    label: &'static str,
    samples_ms: Vec<f64>,
}

impl StageSeries {
    fn new(key: &'static str, label: &'static str, iters: usize) -> Self {
        Self {
            key,
            label,
            samples_ms: Vec::with_capacity(iters),
        }
    }
}

struct BenchReport {
    file: String,
    iters: usize,
    site: String,
    /// Real data time of the decoded volume (RFC 3339; the volume's time
    /// reference, a whole second), never a wall clock: the bench reports
    /// what the file says.
    volume_time: String,
    sweeps: usize,
    reflectivity_sweep: usize,
    reflectivity_elevation_deg: f32,
    velocity_sweep: usize,
    velocity_elevation_deg: f32,
    stages: Vec<StageSeries>,
    /// Per-iteration sum of all stage times.
    totals_ms: Vec<f64>,
    checksum: u64,
    deterministic: bool,
}

struct IterationSample {
    decode_ms: f64,
    reflectivity_ms: f64,
    velocity_ms: f64,
    checksum: u64,
    site: String,
    volume_time: String,
    sweeps: usize,
    reflectivity_sweep: usize,
    reflectivity_elevation_deg: f32,
    velocity_sweep: usize,
    velocity_elevation_deg: f32,
}

fn run_bench(args: &BenchArgs) -> Result<BenchReport, String> {
    // Read the file once; every iteration decodes from these bytes so
    // the disk is out of the measurement.
    let raw = fs::read(&args.file).map_err(|err| format!("read {}: {err}", args.file.display()))?;
    let color_tables = ColorTableSet::default();
    let mut buffers: Vec<Vec<u8>> = BENCH_VIEWPORT_SHAPES
        .iter()
        .map(|&(width, height)| {
            vec![0u8; viewport_rgba_buffer_len(bench_viewport_options(width, height))]
        })
        .collect();

    // Warmup iteration: untimed; its pixel checksum is the reference the
    // timed iterations are compared against.
    let warmup = run_iteration(&raw, args.sweeps, &color_tables, &mut buffers)?;

    let mut stages = vec![
        StageSeries::new("decode", "decode", args.iters),
        StageSeries::new("raster_ref", "reflectivity raster x3", args.iters),
        StageSeries::new("raster_vel", "velocity raster x3 (dealiased)", args.iters),
    ];
    let mut totals_ms = Vec::with_capacity(args.iters);
    let mut deterministic = true;
    for _ in 0..args.iters {
        let sample = run_iteration(&raw, args.sweeps, &color_tables, &mut buffers)?;
        deterministic &= sample.checksum == warmup.checksum;
        stages[0].samples_ms.push(sample.decode_ms);
        stages[1].samples_ms.push(sample.reflectivity_ms);
        stages[2].samples_ms.push(sample.velocity_ms);
        totals_ms.push(sample.decode_ms + sample.reflectivity_ms + sample.velocity_ms);
    }

    Ok(BenchReport {
        file: args.file.display().to_string(),
        iters: args.iters,
        site: warmup.site,
        volume_time: warmup.volume_time,
        sweeps: warmup.sweeps,
        reflectivity_sweep: warmup.reflectivity_sweep,
        reflectivity_elevation_deg: warmup.reflectivity_elevation_deg,
        velocity_sweep: warmup.velocity_sweep,
        velocity_elevation_deg: warmup.velocity_elevation_deg,
        stages,
        totals_ms,
        checksum: warmup.checksum,
        deterministic,
    })
}

fn run_iteration(
    raw: &[u8],
    sweeps: Option<(usize, usize)>,
    color_tables: &ColorTableSet,
    buffers: &mut [Vec<u8>],
) -> Result<IterationSample, String> {
    let started = Instant::now();
    // The app's one shared byte router (local open / URL polling /
    // provider downloads); a Level-II buffer falls through to
    // read_volume_from_bytes, the same entry the archive path uses.
    // No site hint is needed: Archive II embeds the ICAO in the header.
    let volume =
        recast_radar_io::read_supported_volume_bytes(raw).map_err(|err| err.to_string())?;
    let decode_ms = elapsed_ms(started);

    let (reflectivity_sweep, reflectivity) = match sweeps {
        Some((index, _)) => sweep_field(&volume, index, Quantity::Reflectivity)?,
        None => lowest_sweep_with(&volume, Quantity::Reflectivity)
            .ok_or_else(|| "volume has no reflectivity sweep to benchmark".to_owned())?,
    };
    let (velocity_sweep, velocity) = match sweeps {
        Some((_, index)) => sweep_field(&volume, index, Quantity::RadialVelocity)?,
        None => lowest_sweep_with(&volume, Quantity::RadialVelocity)
            .ok_or_else(|| "volume has no velocity sweep to benchmark".to_owned())?,
    };

    let started = Instant::now();
    render_field_viewports(
        &volume,
        reflectivity_sweep,
        &reflectivity,
        false,
        color_tables,
        buffers,
    )?;
    let reflectivity_ms = elapsed_ms(started);
    // Checksums run OUTSIDE the timed stages: they exist to referee
    // byte-identical output across builds, not to be measured.
    let mut checksum = FNV64_OFFSET_BASIS;
    for buffer in buffers.iter() {
        checksum = fnv1a64_words(checksum, buffer);
    }

    let started = Instant::now();
    render_field_viewports(
        &volume,
        velocity_sweep,
        &velocity,
        true,
        color_tables,
        buffers,
    )?;
    let velocity_ms = elapsed_ms(started);
    for buffer in buffers.iter() {
        checksum = fnv1a64_words(checksum, buffer);
    }

    Ok(IterationSample {
        decode_ms,
        reflectivity_ms,
        velocity_ms,
        checksum,
        site: volume.attrs.instrument_name.clone(),
        volume_time: volume.time_reference.to_rfc3339(),
        sweeps: volume.sweeps.len(),
        reflectivity_sweep,
        reflectivity_elevation_deg: volume.sweeps[reflectivity_sweep].fixed_angle_deg,
        velocity_sweep,
        velocity_elevation_deg: volume.sweeps[velocity_sweep].fixed_angle_deg,
    })
}

/// The `quantity` field of `sweep`, when it carries rows.
fn displayable_field(sweep: &Sweep, quantity: Quantity) -> Option<&FieldName> {
    sweep
        .find(quantity)
        .filter(|field| field.nrays as usize > field.absent_rows.len())
        .map(|field| &field.name)
}

/// Sweep `index` and the name of its `quantity` field (`--sweeps`).
fn sweep_field(
    volume: &Volume,
    index: usize,
    quantity: Quantity,
) -> Result<(usize, FieldName), String> {
    let sweep = volume.sweeps.get(index).ok_or_else(|| {
        format!(
            "--sweeps: sweep {index} does not exist ({} sweeps)",
            volume.sweeps.len()
        )
    })?;
    displayable_field(sweep, quantity)
        .map(|name| (index, name.clone()))
        .ok_or_else(|| format!("--sweeps: sweep {index} has no {quantity:?} rows"))
}

/// Lowest sweep carrying rows of a `quantity` field, and that field's name:
/// min by `Sweep::fixed_angle_deg`, then sweep index. The Level II fixed
/// angle is the VCP cut angle (design note `docs/design/fm301-model.md`
/// 5.2), so the cuts of a split cut and the SAILS / MRLE repeats of one
/// angle tie, and the first of them in acquisition order wins. The legacy
/// model used a Level II cut's first radial elevation here, which the
/// BowEcho app still does (`docs/baselines/import-checksums.txt`).
fn lowest_sweep_with(volume: &Volume, quantity: Quantity) -> Option<(usize, FieldName)> {
    volume
        .sweeps
        .iter()
        .enumerate()
        .filter_map(|(index, sweep)| {
            displayable_field(sweep, quantity).map(|name| (index, sweep.fixed_angle_deg, name))
        })
        .min_by(
            |(left_index, left_angle, _), (right_index, right_angle, _)| {
                left_angle
                    .total_cmp(right_angle)
                    .then_with(|| left_index.cmp(right_index))
            },
        )
        .map(|(index, _, name)| (index, name.clone()))
}

/// Mirror the app's render-worker field path: build one
/// `ViewportFieldCache` per field (the dealiased constructor for
/// velocity — DVEL is the app's flagship velocity display), then render
/// it at each bench viewport like a pan/zoom burst reusing the cache.
fn render_field_viewports(
    volume: &Volume,
    sweep_index: usize,
    field: &FieldName,
    dealiased_velocity: bool,
    color_tables: &ColorTableSet,
    buffers: &mut [Vec<u8>],
) -> Result<(), String> {
    let cache = if dealiased_velocity {
        ViewportFieldCache::new_dealiased_velocity_with_color_tables(
            volume,
            sweep_index,
            field,
            color_tables,
        )
    } else {
        ViewportFieldCache::new_with_color_tables_for_family(
            volume,
            sweep_index,
            field,
            color_tables,
            Some(color_family_for_name(field)),
        )
    }
    .map_err(|err| err.to_string())?;
    for (&(width, height), pixels) in BENCH_VIEWPORT_SHAPES.iter().zip(buffers.iter_mut()) {
        cache
            .render_field_rgba_into(volume, bench_viewport_options(width, height), pixels)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

const FNV64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a folded over 8-byte little-endian words (byte-wise on the
/// remainder): identical buffers hash identically, and word folding is
/// ~8x cheaper than byte-wise FNV on the ~27 MB hashed per iteration.
fn fnv1a64_words(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().expect("chunks_exact yields 8-byte chunks"));
        hash ^= word;
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    for &byte in chunks.remainder() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

fn mean_ms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

fn min_ms(samples: &[f64]) -> f64 {
    samples.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max_ms(samples: &[f64]) -> f64 {
    samples.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn render_human(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("file   {}\n", report.file));
    out.push_str(&format!(
        "site   {}  volume {}  {} sweeps\n",
        report.site, report.volume_time, report.sweeps
    ));
    out.push_str(&format!(
        "sweeps reflectivity {} ({:.2} deg)  velocity {} ({:.2} deg)\n",
        report.reflectivity_sweep,
        report.reflectivity_elevation_deg,
        report.velocity_sweep,
        report.velocity_elevation_deg
    ));
    out.push_str(&format!(
        "run    warmup 1 + {} timed iters; viewports {} @ {} km/px, rotation {} mrad\n\n",
        report.iters,
        BENCH_VIEWPORT_SHAPES
            .iter()
            .map(|(width, height)| format!("{width}x{height}"))
            .collect::<Vec<_>>()
            .join(" / "),
        BENCH_KM_PER_PX,
        (BENCH_ROTATION_RAD * 1000.0).round()
    ));
    out.push_str(&format!(
        "{:<32}{:>10}{:>10}{:>10}\n",
        "stage", "mean ms", "min ms", "max ms"
    ));
    for stage in &report.stages {
        out.push_str(&format!(
            "{:<32}{:>10.3}{:>10.3}{:>10.3}\n",
            stage.label,
            mean_ms(&stage.samples_ms),
            min_ms(&stage.samples_ms),
            max_ms(&stage.samples_ms)
        ));
    }
    out.push_str(&format!(
        "{:<32}{:>10.3}{:>10.3}{:>10.3}\n\n",
        "total",
        mean_ms(&report.totals_ms),
        min_ms(&report.totals_ms),
        max_ms(&report.totals_ms)
    ));
    out.push_str(&format!(
        "pixel checksum 0x{:016x}  deterministic across iterations: {}",
        report.checksum,
        if report.deterministic { "yes" } else { "NO" }
    ));
    out
}

fn render_json(report: &BenchReport) -> String {
    let mut out = String::from("{");
    out.push_str(&format!("\"file\":\"{}\",", json_escape(&report.file)));
    out.push_str(&format!("\"site\":\"{}\",", json_escape(&report.site)));
    out.push_str(&format!(
        "\"volume_time\":\"{}\",",
        json_escape(&report.volume_time)
    ));
    out.push_str(&format!("\"sweeps\":{},", report.sweeps));
    out.push_str(&format!(
        "\"reflectivity_sweep\":{},\"velocity_sweep\":{},",
        report.reflectivity_sweep, report.velocity_sweep
    ));
    out.push_str(&format!("\"iters\":{},", report.iters));
    out.push_str(&format!(
        "\"viewports\":[{}],",
        BENCH_VIEWPORT_SHAPES
            .iter()
            .map(|(width, height)| format!("[{width},{height}]"))
            .collect::<Vec<_>>()
            .join(",")
    ));
    out.push_str(&format!("\"km_per_px\":{BENCH_KM_PER_PX},"));
    out.push_str(&format!("\"rotation_rad\":{BENCH_ROTATION_RAD},"));
    out.push_str("\"stages\":{");
    for (index, stage) in report.stages.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "\"{}\":{{\"mean_ms\":{:.3},\"min_ms\":{:.3},\"max_ms\":{:.3}}}",
            stage.key,
            mean_ms(&stage.samples_ms),
            min_ms(&stage.samples_ms),
            max_ms(&stage.samples_ms)
        ));
    }
    out.push_str("},");
    out.push_str(&format!(
        "\"total\":{{\"mean_ms\":{:.3},\"min_ms\":{:.3},\"max_ms\":{:.3}}},",
        mean_ms(&report.totals_ms),
        min_ms(&report.totals_ms),
        max_ms(&report.totals_ms)
    ));
    out.push_str(&format!("\"checksum\":\"0x{:016x}\",", report.checksum));
    out.push_str(&format!("\"deterministic\":{}", report.deterministic));
    out.push('}');
    out
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The dealias eval battery is a separate mode with its own flag set.
    if args.iter().any(|arg| arg == "--dealias") {
        let parsed = match dealias_eval::parse_dealias_args(&args) {
            Ok(parsed) => parsed,
            Err(err) => {
                eprintln!("error: {err}");
                eprintln!("{}", dealias_eval::DEALIAS_USAGE);
                return ExitCode::from(2);
            }
        };
        return match dealias_eval::run_dealias(&parsed) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => {
                eprintln!("error: an engine's output differed between two identical runs");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        };
    }
    let command = match parse_args(&args) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let args = match command {
        Command::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Command::Run(args) => args,
    };
    match run_bench(&args) {
        Ok(report) => {
            if args.json {
                println!("{}", render_json(&report));
            } else {
                println!("{}", render_human(&report));
            }
            if report.deterministic {
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "error: pixel checksum varied across iterations — rendered output is not deterministic"
                );
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parse_args_defaults() {
        let parsed = parse_args(&args(&["KTLX20130520_201643_V06"])).expect("parse");
        assert_eq!(
            parsed,
            Command::Run(BenchArgs {
                file: PathBuf::from("KTLX20130520_201643_V06"),
                iters: DEFAULT_ITERS,
                json: false,
                sweeps: None,
            })
        );
    }

    #[test]
    fn parse_args_iters_and_json() {
        let parsed = parse_args(&args(&["vol.V06", "--iters", "3", "--json"])).expect("parse");
        assert_eq!(
            parsed,
            Command::Run(BenchArgs {
                file: PathBuf::from("vol.V06"),
                iters: 3,
                json: true,
                sweeps: None,
            })
        );
    }

    #[test]
    fn parse_args_iters_equals_form() {
        let parsed = parse_args(&args(&["--iters=7", "vol.V06"])).expect("parse");
        assert_eq!(
            parsed,
            Command::Run(BenchArgs {
                file: PathBuf::from("vol.V06"),
                iters: 7,
                json: false,
                sweeps: None,
            })
        );
    }

    #[test]
    fn parse_args_sweeps() {
        let expected = Command::Run(BenchArgs {
            file: PathBuf::from("vol.V06"),
            iters: 1,
            json: false,
            sweeps: Some((4, 9)),
        });
        for form in [
            &["vol.V06", "--iters", "1", "--sweeps", "4,9"][..],
            &["--sweeps=4, 9", "vol.V06", "--iters=1"][..],
        ] {
            assert_eq!(parse_args(&args(form)).expect("parse"), expected);
        }
        for bad in ["4", "4,", ",9", "a,b", "4,9,1", "-1,2"] {
            assert!(
                parse_args(&args(&["vol.V06", "--sweeps", bad])).is_err(),
                "{bad}"
            );
        }
        assert!(parse_args(&args(&["vol.V06", "--sweeps"])).is_err());
    }

    #[test]
    fn parse_args_rejects_zero_and_garbage_iters() {
        assert!(parse_args(&args(&["vol.V06", "--iters", "0"])).is_err());
        assert!(parse_args(&args(&["vol.V06", "--iters", "ten"])).is_err());
        assert!(parse_args(&args(&["vol.V06", "--iters"])).is_err());
    }

    #[test]
    fn parse_args_rejects_unknown_option_and_extra_positional() {
        assert!(parse_args(&args(&["vol.V06", "--fast"])).is_err());
        assert!(parse_args(&args(&["vol.V06", "other.V06"])).is_err());
    }

    #[test]
    fn parse_args_requires_file() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["--json"])).is_err());
    }

    #[test]
    fn parse_args_help() {
        assert_eq!(
            parse_args(&args(&["--help"])).expect("parse"),
            Command::Help
        );
        assert_eq!(
            parse_args(&args(&["-h", "vol.V06"])).expect("parse"),
            Command::Help
        );
    }

    #[test]
    fn fnv_word_hash_is_stable_and_covers_remainder() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let first = fnv1a64_words(FNV64_OFFSET_BASIS, &bytes);
        let second = fnv1a64_words(FNV64_OFFSET_BASIS, &bytes);
        assert_eq!(first, second);
        // A change in the tail (past the last full 8-byte word) must
        // change the hash — the remainder bytes are folded in too.
        let mut tail_changed = bytes;
        tail_changed[10] = 0xff;
        assert_ne!(first, fnv1a64_words(FNV64_OFFSET_BASIS, &tail_changed));
    }

    #[test]
    fn json_escape_handles_windows_paths_and_quotes() {
        assert_eq!(
            json_escape("C:\\data\\KTLX \"vol\".V06"),
            "C:\\\\data\\\\KTLX \\\"vol\\\".V06"
        );
        assert_eq!(json_escape("a\nb\u{1}"), "a\\nb\\u0001");
    }

    #[test]
    fn json_report_shape() {
        let report = BenchReport {
            file: "C:\\vols\\KTLX.V06".to_owned(),
            iters: 2,
            site: "KTLX".to_owned(),
            volume_time: "2013-05-20T20:16:43+00:00".to_owned(),
            sweeps: 18,
            reflectivity_sweep: 0,
            reflectivity_elevation_deg: 0.48,
            velocity_sweep: 1,
            velocity_elevation_deg: 0.48,
            stages: vec![
                StageSeries {
                    key: "decode",
                    label: "decode",
                    samples_ms: vec![100.0, 110.0],
                },
                StageSeries {
                    key: "raster_ref",
                    label: "reflectivity raster x3",
                    samples_ms: vec![10.0, 12.0],
                },
                StageSeries {
                    key: "raster_vel",
                    label: "velocity raster x3 (dealiased)",
                    samples_ms: vec![20.0, 22.0],
                },
            ],
            totals_ms: vec![130.0, 144.0],
            checksum: 0x0123_4567_89ab_cdef,
            deterministic: true,
        };
        let json = render_json(&report);
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(!json.contains('\n'));
        assert!(json.contains("\"file\":\"C:\\\\vols\\\\KTLX.V06\""));
        assert!(
            json.contains("\"decode\":{\"mean_ms\":105.000,\"min_ms\":100.000,\"max_ms\":110.000}")
        );
        assert!(
            json.contains("\"total\":{\"mean_ms\":137.000,\"min_ms\":130.000,\"max_ms\":144.000}")
        );
        assert!(json.contains("\"checksum\":\"0x0123456789abcdef\""));
        assert!(json.contains("\"deterministic\":true"));
    }

    /// End-to-end stage plumbing against a real archive volume.
    ///
    /// Run with:
    ///   BOWECHO_BENCH_FILE=path/to/KTLX20130520_201643_V06 \
    ///     cargo test -p recast-radar-bench -- --ignored smoke
    #[test]
    #[ignore = "needs BOWECHO_BENCH_FILE pointing at a real Level-II archive volume"]
    fn smoke_bench_runs_one_iteration() {
        let file = std::env::var("BOWECHO_BENCH_FILE")
            .expect("set BOWECHO_BENCH_FILE to a Level-II archive volume path");
        let report = run_bench(&BenchArgs {
            file: PathBuf::from(file),
            iters: 1,
            json: false,
            sweeps: None,
        })
        .expect("bench run");
        assert!(report.deterministic, "warmup vs timed checksum mismatch");
        assert_ne!(report.checksum, FNV64_OFFSET_BASIS, "no pixels hashed");
        assert_eq!(report.stages.len(), 3);
        for stage in &report.stages {
            assert_eq!(stage.samples_ms.len(), 1, "stage {}", stage.key);
            assert!(stage.samples_ms[0] > 0.0, "stage {}", stage.key);
        }
        // Exercise both output renderers on real data.
        assert!(render_human(&report).contains("pixel checksum 0x"));
        assert!(render_json(&report).contains("\"deterministic\":true"));
    }
}
