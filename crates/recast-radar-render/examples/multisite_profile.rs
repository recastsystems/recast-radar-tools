#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use recast_radar_core::{Quantity, Volume};
use recast_radar_render::{
    ViewportFieldCache, ViewportRasterOptions, ViewportSampleCache, viewport_rgba_buffer_len,
};

#[path = "legacy_bridge/mod.rs"]
mod legacy_bridge;

const DEFAULT_VIEWPORT_WIDTH: u32 = 1500;
const DEFAULT_VIEWPORT_HEIGHT: u32 = 950;
const DEFAULT_KM_PER_PX: f32 = 0.55;
const DEFAULT_GROUPS: &[usize] = &[1, 5, 10];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = parse_args().map_err(|err| format!("{err}\n\n{}", usage()))?;
    if config.files.is_empty() {
        return Err(usage().into());
    }

    println!(
        "multisite_profile files={} viewport={}x{} km_per_px={:.3} groups={:?}",
        config.files.len(),
        config.viewport.width,
        config.viewport.height,
        config.viewport.km_per_px_x,
        config.groups
    );
    println!(
        "site,file_mb,read_ms,decode_ms,cache_build_ms,render_ms,sweep,rays,cache_mb,rgba_mb,working_set_mb,private_mb"
    );

    let mut loaded = Vec::with_capacity(config.files.len());
    let start_all = Instant::now();
    let mut cumulative = Timings::default();

    for (index, file) in config.files.iter().enumerate() {
        let read_start = Instant::now();
        let raw = std::fs::read(file)?;
        let read = read_start.elapsed();

        let decode_start = Instant::now();
        let volume = legacy_bridge::read_volume_bytes(&raw)?;
        let decode = decode_start.elapsed();

        let (cut, name) = first_sweep_with(&volume, Quantity::Reflectivity)
            .or_else(|| first_sweep_with(&volume, Quantity::RadialVelocity))
            .ok_or_else(|| format!("{} has no DBZH or VRADH sweep", file.display()))?;

        let cache_start = Instant::now();
        let moment_cache = ViewportFieldCache::new(&volume, cut, &name)?;
        let sample_cache = moment_cache.build_sample_cache(&volume, config.viewport)?;
        let cache_build = cache_start.elapsed();

        let mut pixels = vec![0; viewport_rgba_buffer_len(config.viewport)];
        let render_start = Instant::now();
        moment_cache.render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut pixels)?;
        let render = render_start.elapsed();

        let memory = process_memory();
        let file_mb = raw.len() as f64 / 1_048_576.0;
        let cache_mb = sample_cache.storage_bytes() as f64 / 1_048_576.0;
        let rgba_mb = pixels.len() as f64 / 1_048_576.0;
        cumulative.read += read;
        cumulative.decode += decode;
        cumulative.cache_build += cache_build;
        cumulative.render += render;

        println!(
            "{},{:.2},{:.3},{:.3},{:.3},{:.3},{},{},{:.2},{:.2},{},{}",
            volume.attrs.instrument_name,
            file_mb,
            elapsed_ms(read),
            elapsed_ms(decode),
            elapsed_ms(cache_build),
            elapsed_ms(render),
            cut,
            volume.provenance.decode.decoded_ray_count,
            cache_mb,
            rgba_mb,
            fmt_mb(memory.working_set_bytes),
            fmt_mb(memory.private_bytes)
        );

        loaded.push(LoadedSite {
            volume,
            moment_cache,
            sample_cache,
            pixels,
        });

        let count = index + 1;
        if config.groups.contains(&count) {
            let memory = process_memory();
            println!(
                "GROUP,count={},elapsed_ms={:.3},read_ms={:.3},decode_ms={:.3},cache_build_ms={:.3},render_ms={:.3},working_set_mb={},private_mb={}",
                count,
                elapsed_ms(start_all.elapsed()),
                elapsed_ms(cumulative.read),
                elapsed_ms(cumulative.decode),
                elapsed_ms(cumulative.cache_build),
                elapsed_ms(cumulative.render),
                fmt_mb(memory.working_set_bytes),
                fmt_mb(memory.private_bytes)
            );
        }
    }

    let retained_probe = loaded
        .iter()
        .map(|site| {
            site.volume.provenance.decode.decoded_ray_count
                + site.moment_cache.sweep_index()
                + site.sample_cache.sample_count()
                + site.pixels.len()
        })
        .sum::<usize>();
    std::hint::black_box(retained_probe);
    Ok(())
}

#[derive(Clone, Debug)]
struct Config {
    files: Vec<PathBuf>,
    groups: Vec<usize>,
    viewport: ViewportRasterOptions,
}

struct LoadedSite {
    volume: Volume,
    moment_cache: ViewportFieldCache,
    sample_cache: ViewportSampleCache,
    pixels: Vec<u8>,
}

#[derive(Default)]
struct Timings {
    read: Duration,
    decode: Duration,
    cache_build: Duration,
    render: Duration,
}

#[derive(Default)]
struct ProcessMemory {
    working_set_bytes: Option<u64>,
    private_bytes: Option<u64>,
}

/// The first sweep with a field of `quantity`, and that field's name.
fn first_sweep_with(
    volume: &Volume,
    quantity: Quantity,
) -> Option<(usize, recast_radar_core::FieldName)> {
    volume.sweeps.iter().enumerate().find_map(|(index, sweep)| {
        sweep
            .find(quantity)
            .map(|field| (index, field.name.clone()))
    })
}

fn parse_args() -> Result<Config, String> {
    let mut args = std::env::args().skip(1);
    let mut groups = DEFAULT_GROUPS.to_vec();
    let mut viewport_width = DEFAULT_VIEWPORT_WIDTH;
    let mut viewport_height = DEFAULT_VIEWPORT_HEIGHT;
    let mut km_per_px = DEFAULT_KM_PER_PX;
    let mut files = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--groups" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--groups requires a comma-separated value".to_owned())?;
                groups = value
                    .split(',')
                    .map(|value| {
                        value
                            .parse::<usize>()
                            .map_err(|_| format!("invalid group size {value}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                groups.sort_unstable();
                groups.dedup();
            }
            "--viewport" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--viewport requires WIDTHxHEIGHT".to_owned())?;
                let (width, height) = value
                    .split_once('x')
                    .ok_or_else(|| format!("invalid viewport {value}"))?;
                viewport_width = width
                    .parse::<u32>()
                    .map_err(|_| format!("invalid viewport width {width}"))?;
                viewport_height = height
                    .parse::<u32>()
                    .map_err(|_| format!("invalid viewport height {height}"))?;
            }
            "--km-per-px" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--km-per-px requires a numeric value".to_owned())?;
                km_per_px = value
                    .parse::<f32>()
                    .map_err(|_| format!("invalid km-per-px {value}"))?;
            }
            "--help" | "-h" => return Err(usage()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }

    Ok(Config {
        files,
        groups,
        viewport: ViewportRasterOptions {
            width: viewport_width,
            height: viewport_height,
            radar_x_px: viewport_width as f32 * 0.5,
            radar_y_px: viewport_height as f32 * 0.5,
            km_per_px_x: km_per_px,
            km_per_px_y: km_per_px,
            rotation_rad: 0.0,
        },
    })
}

fn usage() -> String {
    "usage: cargo run --release -p recast-radar-render --example multisite_profile -- [--groups 1,5,10] [--viewport WIDTHxHEIGHT] [--km-per-px N] <level2-file>...".to_owned()
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn fmt_mb(bytes: Option<u64>) -> String {
    bytes
        .map(|bytes| format!("{:.1}", bytes as f64 / 1_048_576.0))
        .unwrap_or_else(|| "n/a".to_owned())
}

/// Resident and virtual size from `/proc/self/status`. Platforms without
/// procfs (Windows, macOS) report `n/a`: the example stays free of FFI.
fn process_memory() -> ProcessMemory {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return ProcessMemory::default();
    };
    let kib_field = |name: &str| {
        status.lines().find_map(|line| {
            let value = line.strip_prefix(name)?.trim();
            let kb = value.split_whitespace().next()?.parse::<u64>().ok()?;
            Some(kb * 1024)
        })
    };
    ProcessMemory {
        working_set_bytes: kib_field("VmRSS:"),
        private_bytes: kib_field("VmSize:"),
    }
}
