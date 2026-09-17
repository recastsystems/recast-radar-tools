// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

// Verify azimuthal shear on a real scan: compute LLSD az-shear on the lowest
// velocity tilt and render it (velocity diverging palette) so rotational
// couplets show as green/red dipoles. usage: shear_probe <l2-file> <out.png>

use std::path::PathBuf;

use image::{ImageBuffer, Rgba};
use recast_radar_core::Quantity;
use recast_radar_render::{RasterOptions, render_field_image};

#[path = "legacy_bridge/mod.rs"]
mod legacy_bridge;

fn save_on_black(img: &ImageBuffer<Rgba<u8>, Vec<u8>>, path: &str) {
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(w, h, Rgba([16, 16, 18, 255]));
    for (x, y, px) in img.enumerate_pixels() {
        let a = px.0[3] as u32;
        if a == 0 {
            continue;
        }
        let bg = out.get_pixel(x, y).0;
        let bl = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        out.put_pixel(
            x,
            y,
            Rgba([
                bl(px.0[0], bg[0]),
                bl(px.0[1], bg[1]),
                bl(px.0[2], bg[2]),
                255,
            ]),
        );
    }
    out.save(path).expect("save");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: shear_probe <l2-file> <out.png>")?,
    );
    let out = args
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shear.png".into());
    let mut decoded = legacy_bridge::Decoded::from_path(&input)?;

    let idx = decoded
        .lowest_sweep_with(Quantity::RadialVelocity)
        .ok_or("no velocity")?;
    let mut shear = decoded.azimuthal_shear(idx).ok_or("no velocity")?;
    let (mut n, mut maxabs) = (0u64, 0.0f32);
    let (rows, gates) = shear.field.shape();
    for r in 0..rows {
        for g in 0..gates {
            if let Some(v) = shear.field.value(r, g) {
                n += 1;
                maxabs = maxabs.max(v.abs());
            }
        }
    }
    println!("az-shear sweep #{idx}: n={n} max|shear|={maxabs:.1} x10^-3 s^-1");

    // Render via the velocity diverging palette: the shear field joins the
    // sweep (its gates lie on the sweep's range) classed as a radial
    // velocity, which is the palette the PNG raster picks for it.
    shear.field.quantity = Quantity::RadialVelocity;
    let name = shear.add_to(&mut decoded.volume.sweeps[idx])?;
    let opts = RasterOptions {
        width: 1400,
        height: 1400,
        range_fraction: 60,
    };
    let img = render_field_image(&decoded.volume, idx, &name, opts)?;
    save_on_black(&img, &out);
    println!("wrote {out}");
    Ok(())
}
