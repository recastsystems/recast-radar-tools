// Print each sweep's elevation, Nyquist, and near-Nyquist fraction (aliasing
// pressure) — for understanding cascade-dealias behavior on real volumes.
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use recast_radar_core::Quantity;

/// Level II decoding through the un-migrated `recast-radar-io-nexrad`,
/// bridged to the FM301 model (design note 13.3) until `fm301-io` lands.
#[allow(deprecated)]
mod legacy_bridge {
    use recast_radar_core::Volume;
    use std::path::Path;

    pub fn decode_level2(path: &Path) -> Result<Volume, Box<dyn std::error::Error>> {
        let legacy = recast_radar_io_nexrad::decode_volume_from_path(path)?;
        Ok(recast_radar_core::legacy::volume_from_legacy(legacy)?.0)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("usage: cut_probe <l2>")?;
    let volume = legacy_bridge::decode_level2(path.as_ref())?;
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        let Some(field) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        let nyq = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|nyquist| nyquist.first().copied())
            .unwrap_or(f32::NAN);
        let mut near = 0u32;
        let mut total = 0u32;
        let (rows, gates) = field.shape();
        for row in (0..rows).step_by(4) {
            for gate in (0..gates).step_by(4) {
                if let Some(v) = field.value(row, gate).filter(|v| v.is_finite()) {
                    total += 1;
                    if v.abs() > 0.85 * nyq {
                        near += 1;
                    }
                }
            }
        }
        let pct = if total > 0 {
            100.0 * near as f32 / total as f32
        } else {
            0.0
        };
        println!(
            "#{i:02} {:5.2} deg  nyq {nyq:5.1} m/s  near-Nyquist {pct:4.1}%  ({total} samples)",
            sweep.fixed_angle_deg
        );
    }
    Ok(())
}
