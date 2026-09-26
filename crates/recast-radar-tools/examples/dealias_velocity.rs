//! Dealias (unfold) the Doppler velocity of every sweep in a Level II file.
//!
//! cargo run --release -p recast-radar-tools --example dealias_velocity -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::model::Quantity;
use recast_radar_tools::{correct, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);
    let mut volume = nexrad::read_volume_from_path(&path)?;

    for sweep in &mut volume.sweeps {
        let Some(raw) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        // Region-based unfolding. The result (VRADDH) has the same rays and
        // gates as the source field.
        let dealiased = correct::dealias_velocity(sweep, raw);

        let mut unfolded = 0;
        let (rows, gates) = raw.shape();
        for row in 0..rows {
            for gate in 0..gates {
                let before = raw.value(row, gate).unwrap_or(f32::NAN);
                let after = dealiased.value(row, gate).unwrap_or(f32::NAN);
                if (after - before).abs() > 1.0 {
                    unfolded += 1;
                }
            }
        }
        let nyquist = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|values| values.first().copied());
        println!(
            "{:>5.2} deg: Nyquist {:.1} m/s, {unfolded} gates unfolded",
            sweep.fixed_angle_deg,
            nyquist.unwrap_or(f32::NAN)
        );

        // Keep the dealiased field beside the raw velocity.
        sweep.add_field(dealiased)?;
    }
    Ok(())
}
