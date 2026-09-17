// Render the first reflectivity sweep and the first velocity sweep (dealiased)
// of a Level II file to PNG.
//
// cargo run --release -p recast-radar-tools --features render \
//     --example render_png -- <level2-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::core::{ElevationCut, MomentType, RadarVolume};
use recast_radar_tools::render::{self, RasterOptions};
use recast_radar_tools::{correct, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <level2-file> <out-dir>".into());
    };
    let mut volume = nexrad::decode_volume_from_path(&input)?;
    let options = RasterOptions::default(); // 1024 x 1024

    let index = first_sweep_with(&volume, MomentType::Reflectivity)?;
    let path = out_dir.join("reflectivity.png");
    render::render_moment_png(&volume, index, MomentType::Reflectivity, &path, options)?;
    println!("wrote {}", path.display());

    let index = first_sweep_with(&volume, MomentType::Velocity)?;
    let cut = &mut volume.cuts[index];
    let dealiased = correct::dealias_velocity_grid(cut, &cut.moments[&MomentType::Velocity]);
    cut.moments.insert(MomentType::Velocity, dealiased);
    let path = out_dir.join("velocity.png");
    render::render_moment_png(&volume, index, MomentType::Velocity, &path, options)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn first_sweep_with(volume: &RadarVolume, moment: MomentType) -> Result<usize, String> {
    let has_moment = |cut: &ElevationCut| cut.moments.contains_key(&moment);
    let index = volume.cuts.iter().position(has_moment);
    index.ok_or_else(|| format!("no sweep has {moment}"))
}
