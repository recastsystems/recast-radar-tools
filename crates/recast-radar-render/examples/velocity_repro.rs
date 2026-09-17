// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// Reproduction harness for velocity dealias spokes + color-table edge cases.
// Renders raw velocity and dealiased velocity (current algorithm) to PNGs so
// the radial spoke artifacts are directly visible.
//
// usage: cargo run --release -p recast-radar-render --example velocity_repro -- <level2-file> <out-prefix>

use std::path::PathBuf;

use image::{ImageBuffer, Rgba};
use recast_radar_core::{FieldName, Quantity};
use recast_radar_render::{RasterOptions, dealiased_velocity_field, render_field_image};

#[path = "support/mod.rs"]
mod support;

/// Composite an RGBA image over a dark background (radar displays are black)
/// and save, so near-white strong velocities are visible.
fn save_on_black(
    img: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(w, h, Rgba([16, 16, 18, 255]));
    for (x, y, px) in img.enumerate_pixels() {
        let a = px.0[3] as u32;
        if a == 0 {
            continue;
        }
        let bg = out.get_pixel(x, y).0;
        let blend = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        out.put_pixel(
            x,
            y,
            Rgba([
                blend(px.0[0], bg[0]),
                blend(px.0[1], bg[1]),
                blend(px.0[2], bg[2]),
                255,
            ]),
        );
    }
    out.save(path)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: velocity_repro <level2-file> <prefix>")?,
    );
    let prefix = args
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "velrepro".to_string());

    let mut volume = support::read_volume(&input)?;

    // Lowest-elevation sweep that actually carries velocity.
    let sweep_index = support::lowest_sweep_with(&volume, Quantity::RadialVelocity)
        .ok_or("no velocity field in volume")?;
    let sweep = &volume.sweeps[sweep_index];
    let elev = sweep.fixed_angle_deg;
    let field = sweep.find(Quantity::RadialVelocity).unwrap();
    let name = field.name.clone();
    let mut nyqs: Vec<f32> = sweep
        .ray_vars
        .nyquist_velocity_mps
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    nyqs.sort_by(f32::total_cmp);
    let nyq = nyqs.get(nyqs.len() / 2).copied().unwrap_or(f32::NAN);

    println!(
        "site={} time={} sweep=#{sweep_index} elev={elev:.2} rows={} gates={} nyquist={nyq:.2} m/s",
        volume.attrs.instrument_name, volume.time_reference, field.nrays, field.ngates,
    );

    let range_fraction = std::env::args()
        .nth(3)
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(70);
    let opts = RasterOptions {
        width: 1600,
        height: 1600,
        range_fraction,
    };

    // 1) Raw (aliased) velocity.
    let raw_path = format!("{prefix}_raw.png");
    let raw_img = render_field_image(&volume, sweep_index, &name, opts)?;
    save_on_black(&raw_img, &raw_path)?;
    println!("wrote {raw_path}");

    // 2) Dealiased velocity via the current production algorithm, added to
    //    the sweep as VRADDH.
    let dealiased = dealiased_velocity_field(&volume, sweep_index, &name)?;
    let sweep = &mut volume.sweeps[sweep_index];
    sweep.add_field(dealiased)?;
    sweep.seal()?;
    let deal_path = format!("{prefix}_dealiased.png");
    let deal_img = render_field_image(&volume, sweep_index, &FieldName::Vraddh, opts)?;
    save_on_black(&deal_img, &deal_path)?;
    println!("wrote {deal_path}");

    Ok(())
}
