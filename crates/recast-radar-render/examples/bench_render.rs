// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use recast_radar_core::FieldName;
use recast_radar_render::{
    RasterOptions, StormMotion, ViewportFieldCache, ViewportRasterOptions, render_field_image,
    render_storm_relative_velocity_image, viewport_rgba_buffer_len,
};

#[path = "support/mod.rs"]
mod support;
use support::stages;

const DECODE_RUNS: usize = 5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(input) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!(
            "usage: cargo run -p recast-radar-render --example bench_render -- <level2-file>"
        );
        std::process::exit(2);
    };

    let mut decode_timings = Vec::new();
    let mut volume = None;
    for _ in 0..DECODE_RUNS {
        let decode_start = Instant::now();
        let decoded = support::read_volume(&input)?;
        decode_timings.push(decode_start.elapsed());
        volume = Some(decoded);
    }
    decode_timings.sort();
    let volume = volume.expect("decode runs produced a volume");
    println!(
        "decode_ms={:.3} decode_best_ms={:.3} decode_runs={} site={} sweeps={} rays={}",
        elapsed_ms(decode_timings[decode_timings.len() / 2]),
        elapsed_ms(decode_timings[0]),
        decode_timings.len(),
        volume.attrs.instrument_name,
        volume.sweeps.len(),
        volume.provenance.decode.decoded_ray_count
    );

    let mut read_timings = Vec::new();
    let mut normalize_timings = Vec::new();
    let mut parse_timings = Vec::new();
    let mut raw_len = 0;
    let mut normalized_len = 0;
    let mut compression = None;
    let mut parsed = None;
    for _ in 0..DECODE_RUNS {
        let raw_start = Instant::now();
        let raw = std::fs::read(&input)?;
        read_timings.push(raw_start.elapsed());
        raw_len = raw.len();

        let normalize_start = Instant::now();
        let (normalized, archive_compression) = stages::normalize(&raw)?;
        normalize_timings.push(normalize_start.elapsed());
        normalized_len = normalized.len();
        compression = Some(archive_compression);

        let parse_start = Instant::now();
        let decoded = stages::parse_normalized(&normalized, archive_compression)?;
        parse_timings.push(parse_start.elapsed());
        parsed = Some(decoded);
    }
    read_timings.sort();
    normalize_timings.sort();
    parse_timings.sort();
    let compression = compression.expect("breakdown runs produced compression");
    let parsed = parsed.expect("breakdown runs produced a parsed volume");
    println!(
        "decode_breakdown read_ms={:.3} read_best_ms={:.3} normalize_ms={:.3} normalize_best_ms={:.3} parse_ms={:.3} parse_best_ms={:.3} raw_bytes={} normalized_bytes={} compression={:?} site={} sweeps={} rays={}",
        elapsed_ms(read_timings[read_timings.len() / 2]),
        elapsed_ms(read_timings[0]),
        elapsed_ms(normalize_timings[normalize_timings.len() / 2]),
        elapsed_ms(normalize_timings[0]),
        elapsed_ms(parse_timings[parse_timings.len() / 2]),
        elapsed_ms(parse_timings[0]),
        raw_len,
        normalized_len,
        compression,
        parsed.attrs.instrument_name,
        parsed.sweeps.len(),
        parsed.provenance.decode.decoded_ray_count
    );

    if compression == stages::ArchiveCompression::Gzip {
        let mut stream_timings = Vec::new();
        let mut streamed = None;
        for _ in 0..DECODE_RUNS {
            let file = std::fs::File::open(&input)?;
            let stream_start = Instant::now();
            let decoded = stages::read_gzip_stream(file)?;
            stream_timings.push(stream_start.elapsed());
            streamed = Some(decoded);
        }
        stream_timings.sort();
        let streamed = streamed.expect("streaming gzip decode produced a volume");
        println!(
            "decode_gzip_stream_ms={:.3} decode_gzip_stream_best_ms={:.3} site={} sweeps={} rays={}",
            elapsed_ms(stream_timings[stream_timings.len() / 2]),
            elapsed_ms(stream_timings[0]),
            streamed.attrs.instrument_name,
            streamed.sweeps.len(),
            streamed.provenance.decode.decoded_ray_count
        );
    }

    for (cut, field) in [
        (0, FieldName::Dbzh),
        (1, FieldName::Vradh),
        (1, FieldName::Wradh),
        (6, FieldName::Dbzh),
        (6, FieldName::Vradh),
    ] {
        let mut timings = Vec::new();
        for _ in 0..8 {
            let start = Instant::now();
            let image = render_field_image(
                &volume,
                cut,
                &field,
                RasterOptions {
                    width: 1024,
                    height: 1024,
                    range_fraction: 94,
                },
            )?;
            std::hint::black_box(image.as_raw());
            timings.push(start.elapsed());
        }
        timings.sort();
        println!(
            "render cut={cut} field={} median_ms={:.3} best_ms={:.3}",
            field.as_str(),
            elapsed_ms(timings[timings.len() / 2]),
            elapsed_ms(timings[0])
        );
    }

    let storm_motion = StormMotion {
        direction_deg: 45.0,
        speed_mps: 35.0 * 0.514_444,
    };
    for cut in [1, 6] {
        let mut timings = Vec::new();
        for _ in 0..8 {
            let start = Instant::now();
            let image = render_storm_relative_velocity_image(
                &volume,
                cut,
                &FieldName::Vradh,
                storm_motion,
                RasterOptions {
                    width: 1024,
                    height: 1024,
                    range_fraction: 94,
                },
            )?;
            std::hint::black_box(image.as_raw());
            timings.push(start.elapsed());
        }
        timings.sort();
        println!(
            "render cut={cut} field=SRV median_ms={:.3} best_ms={:.3}",
            elapsed_ms(timings[timings.len() / 2]),
            elapsed_ms(timings[0])
        );
    }

    let viewport = ViewportRasterOptions {
        width: 1320,
        height: 820,
        radar_x_px: 660.0,
        radar_y_px: 410.0,
        km_per_px_x: 0.16,
        km_per_px_y: 0.16,
        rotation_rad: 0.0,
    };
    for (cut, field) in [(1, FieldName::Vradh), (6, FieldName::Dbzh)] {
        let mut timings = Vec::new();
        let mut pixels = vec![0; viewport_rgba_buffer_len(viewport)];
        let cache = ViewportFieldCache::new(&volume, cut, &field)?;
        for _ in 0..8 {
            let start = Instant::now();
            cache.render_field_rgba_into(&volume, viewport, &mut pixels)?;
            std::hint::black_box(&pixels);
            timings.push(start.elapsed());
        }
        timings.sort();
        println!(
            "viewport cached cut={cut} field={} size={}x{} median_ms={:.3} best_ms={:.3}",
            field.as_str(),
            viewport.width,
            viewport.height,
            elapsed_ms(timings[timings.len() / 2]),
            elapsed_ms(timings[0])
        );

        let mut build_timings = Vec::new();
        let mut sample_cache = None;
        for _ in 0..8 {
            let start = Instant::now();
            let built_cache = cache.build_sample_cache(&volume, viewport)?;
            std::hint::black_box(built_cache.sample_count());
            build_timings.push(start.elapsed());
            sample_cache = Some(built_cache);
        }
        build_timings.sort();
        let sample_cache = sample_cache.expect("sample cache was built");
        println!(
            "viewport sample_cache_build cut={cut} field={} size={}x{} samples={} storage_bytes={} median_ms={:.3} best_ms={:.3}",
            field.as_str(),
            sample_cache.width(),
            sample_cache.height(),
            sample_cache.sample_count(),
            sample_cache.storage_bytes(),
            elapsed_ms(build_timings[build_timings.len() / 2]),
            elapsed_ms(build_timings[0])
        );

        let mut sample_timings = Vec::new();
        for _ in 0..8 {
            let start = Instant::now();
            cache.render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut pixels)?;
            std::hint::black_box(&pixels);
            sample_timings.push(start.elapsed());
        }
        sample_timings.sort();
        println!(
            "viewport sample_cache cut={cut} field={} size={}x{} median_ms={:.3} best_ms={:.3}",
            field.as_str(),
            viewport.width,
            viewport.height,
            elapsed_ms(sample_timings[sample_timings.len() / 2]),
            elapsed_ms(sample_timings[0])
        );

        cache.render_field_rgba_with_sample_cache(&volume, &sample_cache, &mut pixels)?;
        let mut reuse_timings = Vec::new();
        for _ in 0..8 {
            let start = Instant::now();
            cache.render_field_rgba_with_sample_cache_reusing_transparency(
                &volume,
                &sample_cache,
                &mut pixels,
            )?;
            std::hint::black_box(&pixels);
            reuse_timings.push(start.elapsed());
        }
        reuse_timings.sort();
        println!(
            "viewport sample_cache_reuse cut={cut} field={} size={}x{} median_ms={:.3} best_ms={:.3}",
            field.as_str(),
            viewport.width,
            viewport.height,
            elapsed_ms(reuse_timings[reuse_timings.len() / 2]),
            elapsed_ms(reuse_timings[0])
        );
    }

    let mut timings = Vec::new();
    let mut pixels = vec![0; viewport_rgba_buffer_len(viewport)];
    let velocity_cache = ViewportFieldCache::new(&volume, 1, &FieldName::Vradh)?;
    let velocity_sample_cache = velocity_cache.build_sample_cache(&volume, viewport)?;
    let velocity_palette_cache = velocity_cache
        .build_storm_relative_velocity_palette_cache(&volume, storm_motion)?
        .expect("benchmark velocity field uses u8 palette cache");
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache.render_storm_relative_velocity_rgba_into(
            &volume,
            storm_motion,
            viewport,
            &mut pixels,
        )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport cached cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    let mut timings = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache.render_storm_relative_velocity_rgba_into_with_palette_cache(
            &volume,
            storm_motion,
            &velocity_palette_cache,
            viewport,
            &mut pixels,
        )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport cached_palette cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    let mut timings = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache.render_storm_relative_velocity_rgba_with_sample_cache(
            &volume,
            storm_motion,
            &velocity_sample_cache,
            &mut pixels,
        )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport sample_cache cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    let mut timings = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache.render_storm_relative_velocity_rgba_with_sample_cache_and_palette_cache(
            &volume,
            storm_motion,
            &velocity_palette_cache,
            &velocity_sample_cache,
            &mut pixels,
        )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport sample_cache_palette cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    velocity_cache.render_storm_relative_velocity_rgba_with_sample_cache(
        &volume,
        storm_motion,
        &velocity_sample_cache,
        &mut pixels,
    )?;
    let mut timings = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache.render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency(
            &volume,
            storm_motion,
            &velocity_sample_cache,
            &mut pixels,
        )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport sample_cache_reuse cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    velocity_cache.render_storm_relative_velocity_rgba_with_sample_cache_and_palette_cache(
        &volume,
        storm_motion,
        &velocity_palette_cache,
        &velocity_sample_cache,
        &mut pixels,
    )?;
    let mut timings = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        velocity_cache
            .render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency_and_palette_cache(
                &volume,
                storm_motion,
                &velocity_palette_cache,
                &velocity_sample_cache,
                &mut pixels,
            )?;
        std::hint::black_box(&pixels);
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "viewport sample_cache_reuse_palette cut=1 field=SRV size={}x{} median_ms={:.3} best_ms={:.3}",
        viewport.width,
        viewport.height,
        elapsed_ms(timings[timings.len() / 2]),
        elapsed_ms(timings[0])
    );

    Ok(())
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
