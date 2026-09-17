// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// Render native vs smoothed reflectivity PNGs for visual comparison.
// usage: smooth_probe <l2-file> <out-dir>
use image::{ImageBuffer, Rgba};
use recast_radar_core::{FieldName, Quantity, Volume};
use recast_radar_render::{
    ColorTableFamily, ColorTableSet, ViewportFieldCache, ViewportRasterOptions,
    viewport_rgba_buffer_len,
};
use std::path::PathBuf;
use std::time::Instant;

#[path = "support/mod.rs"]
mod support;

fn save(volume: &Volume, cache: &ViewportFieldCache, options: ViewportRasterOptions, path: &str) {
    let mut px = vec![0u8; viewport_rgba_buffer_len(options)];
    let (w, h) = cache
        .render_field_rgba_into(volume, options, &mut px)
        .expect("render");
    let mut img = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(w, h, Rgba([15, 17, 20, 255]));
    for (i, p) in px.chunks_exact(4).enumerate() {
        let a = p[3] as u32;
        if a == 0 {
            continue;
        }
        let (x, y) = (i as u32 % w, i as u32 / w);
        let bg = img.get_pixel(x, y).0;
        let bl = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        img.put_pixel(
            x,
            y,
            Rgba([bl(p[0], bg[0]), bl(p[1], bg[1]), bl(p[2], bg[2]), 255]),
        );
    }
    img.save(path).expect("save");
    println!("wrote {path}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(args.next().ok_or("usage: smooth_probe <l2> <dir>")?);
    let dir = args
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());
    let decoded = support::Decoded::from_path(&input)?;
    let volume = &decoded.volume;
    let tables = ColorTableSet::default();
    // Zoomed view (~±60 km) where smoothing is most visible.
    let options = ViewportRasterOptions {
        width: 900,
        height: 900,
        radar_x_px: 450.0,
        radar_y_px: 450.0,
        km_per_px_x: 0.135,
        km_per_px_y: 0.135,
        rotation_rad: 0.0,
    };
    let sweep = decoded
        .lowest_sweep_with(Quantity::Reflectivity)
        .ok_or("no DBZH")?;

    let native =
        ViewportFieldCache::new_with_color_tables(volume, sweep, &FieldName::Dbzh, &tables)?;
    save(
        volume,
        &native,
        options,
        &format!("{dir}/smooth_native.png"),
    );

    let start = Instant::now();
    let smoothed = decoded.smoothed(sweep, &FieldName::Dbzh).ok_or("no DBZH")?;
    println!(
        "smoothing pass: {:.1} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let smoothed = ViewportFieldCache::new_derived(
        volume,
        sweep,
        smoothed.field,
        &smoothed.range,
        ColorTableFamily::Reflectivity,
        &tables,
    )?;
    save(volume, &smoothed, options, &format!("{dir}/smooth_on.png"));
    Ok(())
}
