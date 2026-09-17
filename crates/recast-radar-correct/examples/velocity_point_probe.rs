// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

// Probe raw vs dealiased velocity at given az/range points on the lowest
// velocity tilt — for debugging fold failures reported in the field.
// usage: velocity_point_probe <l2-file> <az_deg> <range_km> [<az> <range> ...]
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
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: <l2> <az> <rng_km> ...")?;
    let points: Vec<f64> = args.filter_map(|a| a.parse().ok()).collect();
    let volume = legacy_bridge::decode_level2(path.as_ref() as &std::path::Path)?;
    let (index, sweep) = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.find(Quantity::RadialVelocity).is_some())
        .min_by(|a, b| a.1.fixed_angle_deg.total_cmp(&b.1.fixed_angle_deg))
        .ok_or("no velocity")?;
    let velocity = sweep.find(Quantity::RadialVelocity).unwrap();
    let dealiased = dealias_velocity(sweep, velocity);
    let (first_m, spacing_m) = velocity.native_geometry(&sweep.range).unwrap();
    println!(
        "sweep #{index} elev {:.2} gates {} spacing {} m first {} m",
        sweep.fixed_angle_deg, velocity.ngates, spacing_m, first_m
    );
    for pair in points.chunks(2) {
        let [az, range_km] = pair else { continue };
        // nearest ray by azimuth
        let mut best = (usize::MAX, f32::INFINITY);
        for (row, azimuth) in sweep.rays.azimuth_deg.iter().enumerate() {
            let diff = (azimuth - *az as f32).rem_euclid(360.0);
            let diff = diff.min(360.0 - diff);
            if diff < best.1 {
                best = (row, diff);
            }
        }
        let row = best.0;
        let nyquist = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|nyquist| nyquist.get(row).copied());
        let gate = ((range_km * 1000.0 - first_m) / spacing_m.max(1.0)).round() as usize;
        // sample a 5-gate window around the point
        println!("az {az:.1} rng {range_km:.1} km (row {row}, gate {gate}, nyq {nyquist:?}):");
        for g in gate.saturating_sub(2)..=(gate + 2).min(velocity.ngates as usize - 1) {
            let raw = velocity.value(row, g);
            let dl = dealiased.value(row, g);
            println!("  gate {g}: raw {raw:?} -> dealiased {dl:?}");
        }
    }
    Ok(())
}
