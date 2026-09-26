// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Render the full product set for a scan to PNGs through the SAME
//! ViewportFieldCache path the GUI uses — visual proof every product + palette
//! works end to end (base moments, dual-pol, dealiased velocity, the derived
//! volumetric/shear products). usage: product_gallery <l2-file> <out-dir>

use std::path::PathBuf;

use image::{ImageBuffer, Rgba};
use recast_radar_core::{Quantity, Volume};
use recast_radar_render::{
    ColorTableFamily, ColorTableSet, ViewportFieldCache, ViewportRasterOptions,
    viewport_rgba_buffer_len,
};

#[path = "support/mod.rs"]
mod support;
use support::{Decoded, Derived, ECHO_TOP_THRESHOLD_DBZ};

fn save_cache(
    volume: &Volume,
    cache: &ViewportFieldCache,
    opts: ViewportRasterOptions,
    path: &str,
) {
    let mut px = vec![0u8; viewport_rgba_buffer_len(opts)];
    let Ok((w, h)) = cache.render_field_rgba_into(volume, opts, &mut px) else {
        eprintln!("render failed: {path}");
        return;
    };
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
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: product_gallery <l2-file> <out-dir>")?,
    );
    let dir = args
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());
    let decoded = Decoded::from_path(&input)?;
    let volume = &decoded.volume;
    let tables = ColorTableSet::default();

    // Full-disk viewport centred on the radar (~±250 km).
    let opts = ViewportRasterOptions {
        width: 900,
        height: 900,
        radar_x_px: 450.0,
        radar_y_px: 450.0,
        km_per_px_x: 0.56,
        km_per_px_y: 0.56,
        rotation_rad: 0.0,
    };

    let ref_sweep = decoded.lowest_sweep_with(Quantity::Reflectivity);
    let vel_sweep = decoded.lowest_sweep_with(Quantity::RadialVelocity);

    // Base + dual-pol moments via their normal caches.
    let base = [
        ("REF", Quantity::Reflectivity),
        ("CC", Quantity::CorrelationCoefficient),
        ("ZDR", Quantity::DifferentialReflectivity),
        ("SW", Quantity::SpectrumWidth),
    ];
    for (label, quantity) in base {
        if let Some(sweep) = decoded.lowest_sweep_with(quantity)
            && let Some(field) = volume.sweeps[sweep].find(quantity)
            && let Ok(c) =
                ViewportFieldCache::new_with_color_tables(volume, sweep, &field.name, &tables)
        {
            save_cache(volume, &c, opts, &format!("{dir}/gallery_{label}.png"));
        }
    }

    // Dealiased velocity.
    if let Some(sweep) = vel_sweep
        && let Some(velocity) = volume.sweeps[sweep].find(Quantity::RadialVelocity)
        && let Ok(c) = ViewportFieldCache::new_dealiased_velocity_with_color_tables(
            volume,
            sweep,
            &velocity.name,
            &tables,
        )
    {
        save_cache(
            volume,
            &c,
            opts,
            &format!("{dir}/gallery_VEL_dealiased.png"),
        );
    }

    // Derived volume products on the base reflectivity tilt.
    if let Some(base_idx) = ref_sweep {
        let derived: Vec<(&str, Option<Derived>, ColorTableFamily)> = vec![
            (
                "CREF",
                decoded.composite_reflectivity(base_idx),
                ColorTableFamily::Reflectivity,
            ),
            (
                "EchoTops",
                decoded.echo_tops(base_idx, ECHO_TOP_THRESHOLD_DBZ),
                ColorTableFamily::EchoTops,
            ),
            ("VIL", decoded.vil(base_idx), ColorTableFamily::Vil),
            (
                "VILDensity",
                decoded.vil_density(base_idx),
                ColorTableFamily::VilDensity,
            ),
            (
                "MEHS",
                decoded.mehs(base_idx, 3200.0, 6400.0),
                ColorTableFamily::HailSize,
            ),
        ];
        for (label, derived, family) in derived {
            if let Some(derived) = derived
                && let Ok(c) = ViewportFieldCache::new_derived(
                    volume,
                    base_idx,
                    derived.field,
                    &derived.range,
                    family,
                    &tables,
                )
            {
                save_cache(volume, &c, opts, &format!("{dir}/gallery_{label}.png"));
            }
        }
    }

    // Per-sweep velocity derivatives.
    if let Some(sweep) = vel_sweep {
        for (label, derived) in [
            ("AzShear", decoded.azimuthal_shear(sweep)),
            ("Divergence", decoded.radial_divergence(sweep)),
        ] {
            if let Some(derived) = derived
                && let Ok(c) = ViewportFieldCache::new_derived(
                    volume,
                    sweep,
                    derived.field,
                    &derived.range,
                    ColorTableFamily::AzimuthalShear,
                    &tables,
                )
            {
                save_cache(volume, &c, opts, &format!("{dir}/gallery_{label}.png"));
            }
        }
    }

    // A cross-section through the strongest composite cell, colorized REF.
    if let Some(base_idx) = ref_sweep
        && let Some(comp) = decoded.composite_reflectivity(base_idx)
    {
        let base_sweep = &volume.sweeps[base_idx];
        let (rows, gates) = comp.field.shape();
        let (mut best, mut rg) = (f32::NEG_INFINITY, (0usize, 0usize));
        for r in 0..rows {
            for g in 0..gates {
                if let Some(v) = comp.field.value(r, g).filter(|v| *v > best) {
                    best = v;
                    rg = (r, g);
                }
            }
        }
        let az = base_sweep.rays.azimuth_deg[rg.0].to_radians();
        let (first_center_m, spacing_m) = comp.field.native_geometry(&comp.range).unwrap();
        let rkm = ((first_center_m + rg.1 as f64 * spacing_m) / 1000.0) as f32;
        let (e, n) = (rkm * az.sin(), rkm * az.cos());
        if let Some(xs) =
            decoded.reflectivity_cross_section((e - 30.0, n), (e + 30.0, n), 700, 320, 18_000.0)
        {
            let table = tables.for_family(ColorTableFamily::Reflectivity);
            let mut img =
                ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(700, 320, Rgba([15, 17, 20, 255]));
            for y in 0..320 {
                for x in 0..700 {
                    let v = xs.values[y * 700 + x];
                    if v.is_finite() {
                        let c = table.color_for_value(v);
                        if c[3] > 0 {
                            img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], 255]));
                        }
                    }
                }
            }
            let path = format!("{dir}/gallery_CrossSection.png");
            img.save(&path)?;
            println!("wrote {path}");
        }
    }
    Ok(())
}
