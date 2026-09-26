//! Render the main products of a Level II file through the viewport path, check the images and optionally write them as PNG.

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{ImageBuffer, Rgba};
use recast_radar_core::{FieldName, Quantity, Volume};
use recast_radar_render::{
    ColorTableSet, ViewportFieldCache, ViewportRasterOptions, viewport_rgba_buffer_len,
};

#[path = "support/mod.rs"]
mod support;

const DEFAULT_KM_PER_PX: f32 = 0.16;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = parse_args().map_err(|err| format!("{err}\n\n{}", usage()))?;
    let decode_start = Instant::now();
    let volume = support::read_volume(&config.input)?;
    let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "visual_probe file={} site={} sweeps={} rays={} decode_ms={decode_ms:.3}",
        config.input.display(),
        volume.attrs.instrument_name,
        volume.sweeps.len(),
        volume.provenance.decode.decoded_ray_count
    );

    probe_product(
        &volume,
        Product::Field(Quantity::Reflectivity),
        config.viewport,
        config.out_dir.as_deref(),
        config.strict,
    )?;
    probe_product(
        &volume,
        Product::Field(Quantity::RadialVelocity),
        config.viewport,
        config.out_dir.as_deref(),
        config.strict,
    )?;
    probe_product(
        &volume,
        Product::DealiasedVelocity,
        config.viewport,
        config.out_dir.as_deref(),
        config.strict,
    )?;

    Ok(())
}

fn probe_product(
    volume: &Volume,
    product: Product,
    viewport: ViewportRasterOptions,
    out_dir: Option<&Path>,
    strict: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((cut, name)) = first_sweep_with(volume, product.base_quantity()) else {
        return Ok(());
    };
    let color_tables = ColorTableSet::default();
    let cache = match product {
        Product::Field(_) => {
            ViewportFieldCache::new_with_color_tables(volume, cut, &name, &color_tables)?
        }
        Product::DealiasedVelocity => ViewportFieldCache::new_dealiased_velocity_with_color_tables(
            volume,
            cut,
            &name,
            &color_tables,
        )?,
    };
    let mut pixels = vec![0; viewport_rgba_buffer_len(viewport)];
    let render_start = Instant::now();
    cache.render_field_rgba_into(volume, viewport, &mut pixels)?;
    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let stats = PixelStats::from_rgba(&pixels);
    let label = product.label();
    println!(
        "visual product={label} sweep={cut} viewport={}x{} render_ms={render_ms:.3} visible={} orange_yellow={} purple_like={} rf_purple_like={}",
        viewport.width,
        viewport.height,
        stats.visible,
        stats.orange_yellow,
        stats.purple_like,
        stats.rf_purple_like
    );

    if strict {
        stats.check(product)?;
    }

    if let Some(out_dir) = out_dir {
        std::fs::create_dir_all(out_dir)?;
        let path = out_dir.join(format!(
            "{}_{}_sweep{cut}.png",
            volume.attrs.instrument_name.to_ascii_lowercase(),
            label.to_ascii_lowercase()
        ));
        save_rgba(&path, viewport.width, viewport.height, pixels)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn save_rgba(
    path: &Path,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixels)
        .expect("RGBA buffer matches viewport dimensions");
    image.save(path)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct PixelStats {
    visible: usize,
    orange_yellow: usize,
    purple_like: usize,
    rf_purple_like: usize,
}

impl PixelStats {
    fn from_rgba(pixels: &[u8]) -> Self {
        let mut stats = Self {
            visible: 0,
            orange_yellow: 0,
            purple_like: 0,
            rf_purple_like: 0,
        };
        for pixel in pixels.chunks_exact(4) {
            let [red, green, blue, alpha] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            if alpha == 0 {
                continue;
            }
            stats.visible += 1;
            if red > 210 && green > 100 && blue < 90 {
                stats.orange_yellow += 1;
            }
            if red > 80 && blue > 115 && green < 110 && blue >= red.saturating_add(10) {
                stats.purple_like += 1;
            }
            if (95..=160).contains(&red)
                && (45..=115).contains(&green)
                && (145..=225).contains(&blue)
            {
                stats.rf_purple_like += 1;
            }
        }
        stats
    }

    fn check(&self, product: Product) -> Result<(), Box<dyn std::error::Error>> {
        if product.is_velocity() && (self.orange_yellow > 0 || self.purple_like > 0) {
            return Err(format!(
                "{} velocity artifact pixels: orange_yellow={} purple_like={}",
                product.label(),
                self.orange_yellow,
                self.purple_like
            )
            .into());
        }
        if matches!(product, Product::Field(Quantity::Reflectivity)) && self.rf_purple_like > 0 {
            return Err(format!(
                "REF has RF-purple-like pixels after RF transparency fix: {}",
                self.rf_purple_like
            )
            .into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum Product {
    Field(Quantity),
    DealiasedVelocity,
}

impl Product {
    fn base_quantity(self) -> Quantity {
        match self {
            Self::Field(quantity) => quantity,
            Self::DealiasedVelocity => Quantity::RadialVelocity,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Field(Quantity::Reflectivity) => "REF",
            Self::Field(Quantity::RadialVelocity) => "VEL",
            Self::Field(_) => "FIELD",
            Self::DealiasedVelocity => "DVEL",
        }
    }

    fn is_velocity(self) -> bool {
        matches!(
            self,
            Self::Field(Quantity::RadialVelocity) | Self::DealiasedVelocity
        )
    }
}

/// The first sweep with a field of `quantity`, and that field's name.
fn first_sweep_with(volume: &Volume, quantity: Quantity) -> Option<(usize, FieldName)> {
    volume.sweeps.iter().enumerate().find_map(|(index, sweep)| {
        sweep
            .find(quantity)
            .map(|field| (index, field.name.clone()))
    })
}

#[derive(Debug)]
struct Config {
    input: PathBuf,
    viewport: ViewportRasterOptions,
    out_dir: Option<PathBuf>,
    strict: bool,
}

fn parse_args() -> Result<Config, String> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mut viewport = parse_viewport("1320x820")?;
    let mut out_dir = None;
    let mut strict = false;
    let mut input = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].to_string_lossy();
        match arg.as_ref() {
            "--viewport" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--viewport needs WIDTHxHEIGHT".to_owned())?
                    .to_string_lossy();
                viewport = parse_viewport(&value)?;
            }
            "--out-dir" => {
                index += 1;
                out_dir = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| "--out-dir needs a directory".to_owned())?,
                ));
            }
            "--strict" => strict = true,
            "--help" | "-h" => return Err(usage()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
            _ => {
                if input.is_some() {
                    return Err(format!("unexpected extra input {}", arg));
                }
                input = Some(PathBuf::from(args.remove(index)));
                continue;
            }
        }
        index += 1;
    }

    Ok(Config {
        input: input.ok_or_else(|| "missing Level II input file".to_owned())?,
        viewport,
        out_dir,
        strict,
    })
}

fn parse_viewport(value: &str) -> Result<ViewportRasterOptions, String> {
    let Some((width, height)) = value.split_once('x') else {
        return Err(format!("invalid viewport {value}; expected WIDTHxHEIGHT"));
    };
    let width = width
        .parse::<u32>()
        .map_err(|_| format!("invalid viewport width in {value}"))?
        .max(1);
    let height = height
        .parse::<u32>()
        .map_err(|_| format!("invalid viewport height in {value}"))?
        .max(1);
    Ok(ViewportRasterOptions {
        width,
        height,
        radar_x_px: width as f32 * 0.5,
        radar_y_px: height as f32 * 0.5,
        km_per_px_x: DEFAULT_KM_PER_PX,
        km_per_px_y: DEFAULT_KM_PER_PX,
        rotation_rad: 0.0,
    })
}

fn usage() -> String {
    "usage: cargo run --release -p recast-radar-render --example visual_probe -- [--strict] [--viewport WIDTHxHEIGHT] [--out-dir DIR] <level2-file>".to_owned()
}
