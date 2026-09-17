// Render the first reflectivity sweep and the first velocity sweep (dealiased)
// of a Level II file to PNG.
//
// cargo run --release -p recast-radar-tools --features render \
//     --example render_png -- <level2-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::core::{FieldName, Quantity, Volume};
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
