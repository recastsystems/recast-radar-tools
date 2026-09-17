// Time dealias_velocity on every velocity sweep of a volume.
// usage: cargo run --release -p recast-radar-correct --example velocity_bench -- <l2-file>

use std::path::PathBuf;
use std::time::Instant;

use recast_radar_core::Quantity;
use recast_radar_correct::dealias_velocity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: velocity_bench <l2-file>")?,
    );
    let volume = recast_radar_io_nexrad::read_volume_from_path(&input)?;

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
