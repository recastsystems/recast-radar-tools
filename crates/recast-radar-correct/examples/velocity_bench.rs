// Time dealias_velocity on every velocity sweep of a volume.
// usage: cargo run --release -p recast-radar-correct --example velocity_bench -- <l2-file>
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

use std::path::PathBuf;
use std::time::Instant;

use recast_radar_core::Quantity;
use recast_radar_correct::dealias_velocity;

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
    let input = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: velocity_bench <l2-file>")?,
    );
    let volume = legacy_bridge::decode_level2(&input)?;

    let mut total = std::time::Duration::ZERO;
    let mut total_gates = 0usize;
    for (idx, sweep) in volume.sweeps.iter().enumerate() {
        let Some(field) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        let (rows, ngates) = field.shape();
        let gates = rows * ngates;
        // warm + best-of-5 to get a stable per-sweep number
        let mut best = std::time::Duration::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let out = dealias_velocity(sweep, field);
            let dt = t.elapsed();
            std::hint::black_box(&out);
            best = best.min(dt);
        }
        total += best;
        total_gates += gates;
        println!(
            "sweep#{idx:<2} elev={:>4.2} {}x{} = {gates:>7} gates  ->  {:>6.2} ms  ({:.1} Mgate/s)",
            sweep.fixed_angle_deg,
            rows,
            ngates,
            best.as_secs_f64() * 1e3,
            gates as f64 / best.as_secs_f64() / 1e6,
        );
    }
    println!(
        "\nTOTAL volume dealias (best-of-5 per sweep): {:.2} ms over {} gates ({:.1} Mgate/s)",
        total.as_secs_f64() * 1e3,
        total_gates,
        total_gates as f64 / total.as_secs_f64() / 1e6,
    );
    Ok(())
}
