# Rendering

The `render` module (feature `render`, crate `recast-radar-render`) draws a
field of a sweep on the CPU, into an RGBA buffer or a PNG file. It reads the
packed values directly through per-code palettes, so it never expands a
field to floats, and it places each field with its own gate geometry.

## A PNG of a sweep

<!-- example: crates/recast-radar-tools/examples/render_png.rs -->
```rust
//! Render the first reflectivity sweep and the first velocity sweep (dealiased)
//! of a Level II file to PNG.
//!
//! cargo run --release -p recast-radar-tools --features render \
//!     --example render_png -- <level2-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::model::{FieldName, Quantity, Volume};
use recast_radar_tools::nexrad;
use recast_radar_tools::render::{self, RasterOptions};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <level2-file> <out-dir>".into());
    };
    let mut volume = nexrad::read_volume_from_path(&input)?;
    let options = RasterOptions::default(); // 1024 x 1024

    let (index, name) = first_sweep_with(&volume, Quantity::Reflectivity)?;
    let path = out_dir.join("reflectivity.png");
    render::render_field_png(&volume, index, &name, &path, options)?;
    println!("wrote {}", path.display());

    let (index, name) = first_sweep_with(&volume, Quantity::RadialVelocity)?;
    // Region-based unfolding; the result joins the sweep as VRADDH.
    let dealiased = render::dealiased_velocity_field(&volume, index, &name)?;
    let sweep = &mut volume.sweeps[index];
    sweep.add_field(dealiased)?;
    sweep.seal()?;
    let path = out_dir.join("velocity.png");
    render::render_field_png(&volume, index, &FieldName::Vraddh, &path, options)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The first sweep with a field of `quantity`, and that field's name.
fn first_sweep_with(volume: &Volume, quantity: Quantity) -> Result<(usize, FieldName), String> {
    volume
        .sweeps
        .iter()
        .enumerate()
        .find_map(|(index, sweep)| {
            sweep
                .find(quantity)
                .map(|field| (index, field.name.clone()))
        })
        .ok_or_else(|| format!("no sweep has a {quantity:?} field"))
}
```

Each image is 1024 by 1024 RGBA with a transparent background
(`RasterOptions::default()`). The radar is at the centre, and the far end of
the sweep's last gate lies `range_fraction` percent (94) of the way from the
centre to the edge. `render_field_image` returns the image instead of
writing it.

Any field of a sweep can be drawn, decoded or derived: add a derived field
to its sweep with `Sweep::add_field` and render it by name, as the
[composite example](processing.md#column-products-and-composites) does with
`CREF`.

## Map views

For a map, draw into a viewport instead of a radar-centred square:
`ViewportRasterOptions` places the radar at a pixel position with a
kilometres-per-pixel scale and the rotation of local north, and
`render_field_viewport_rgba` fills an RGBA buffer. When the same sweep is
drawn again and again (panning, looping), build a `ViewportFieldCache` once
per field, and from it a sample cache for the view
(`build_sample_cache`): later frames then only look colors up. The
storm-relative velocity renderers (`render_storm_relative_velocity_*`)
subtract a storm motion (`StormMotion`) from radial velocity as they draw.

## Color tables

Colors come from a `ColorTableSet`, one `ColorTable` per family
(reflectivity, velocity, spectrum width, differential reflectivity, ...);
`color_family_for_field` picks the family from the field's quantity. The
built-in tables are in `render::color` (`builtin_reflectivity_table`,
`builtin_velocity_table`, ...). `ColorTable::parse_gr_pal` reads a
GRLevelX / GR2Analyst `.pal` palette, and `ColorTableSet::set_family`
installs it. The PNG functions use the default set; the viewport caches take
a set (`ViewportFieldCache::new_with_color_tables`).
