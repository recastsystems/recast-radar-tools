// Dealias (unfold) the Doppler velocity of every sweep in a Level II file.
//
// cargo run --release -p recast-radar-tools --example dealias_velocity -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::core::MomentType;
use recast_radar_tools::{correct, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);
    let mut volume = nexrad::decode_volume_from_path(&path)?;

    for cut in &mut volume.cuts {
        let Some(raw) = cut.moments.get(&MomentType::Velocity) else {
            continue;
        };
        // Region-based unfolding. The result has the same rows and gates.
        let dealiased = correct::dealias_velocity_grid(cut, raw);

        let mut unfolded = 0;
        for row in 0..raw.radial_count() {
            for gate in 0..raw.gate_range.gate_count {
                let before = raw.scaled_value(row, gate).unwrap_or(f32::NAN);
                let after = dealiased.scaled_value(row, gate).unwrap_or(f32::NAN);
                if (after - before).abs() > 1.0 {
                    unfolded += 1;
                }
            }
        }
        let nyquist = cut.radials.first().and_then(|r| r.nyquist_velocity_mps);
        println!(
            "{:>5.2} deg: Nyquist {:.1} m/s, {unfolded} gates unfolded",
            cut.elevation_deg,
            nyquist.unwrap_or(f32::NAN)
        );

        // Keep the dealiased copy in place of the raw velocity.
        cut.moments.insert(MomentType::Velocity, dealiased);
    }
    Ok(())
}
