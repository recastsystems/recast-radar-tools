// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

// Hunt dealiasing failures: find large clusters where the DEALIASED velocity
// is strongly positive (outbound) and report their raw values — a cluster
// whose dealiased = raw + 2·Nyq with negative surroundings is an over-unfold;
// raw==dealiased positive amid negatives is a missed unfold.
// usage: dealias_blob_probe <l2-file>
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
    let path = std::env::args().nth(1).ok_or("usage: <l2>")?;
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
    let (rows, gates) = dealiased.shape();
    let (first, spacing) = dealiased.native_geometry(&sweep.range).unwrap();
    let spacing = spacing.max(1.0);
    println!("sweep #{index} elev {:.2}", sweep.fixed_angle_deg);

    // Cluster strongly-positive dealiased gates within 80 km.
    let max_gate = (((80_000.0 - first) / spacing) as usize).min(gates);
    let mut flagged = vec![false; rows * gates];
    for row in 0..rows {
        for gate in 0..max_gate {
            if let Some(v) = dealiased.value(row, gate).filter(|v| v.is_finite())
                && v > 10.0
            {
                flagged[row * gates + gate] = true;
            }
        }
    }
    let mut visited = vec![false; rows * gates];
    let mut clusters: Vec<(usize, usize, usize)> = Vec::new(); // (size, peak_cell, cluster_id)
    let mut stack = Vec::new();
    for seed in 0..rows * gates {
        if !flagged[seed] || visited[seed] {
            continue;
        }
        stack.clear();
        stack.push(seed);
        visited[seed] = true;
        let mut size = 0usize;
        let mut peak = f32::NEG_INFINITY;
        let mut peak_cell = seed;
        while let Some(cell) = stack.pop() {
            size += 1;
            let (r, g) = (cell / gates, cell % gates);
            if let Some(v) = dealiased.value(r, g)
                && v > peak
            {
                peak = v;
                peak_cell = cell;
            }
            for (dr, dg) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let rr = ((r as i64 + dr).rem_euclid(rows as i64)) as usize;
                let gg = g as i64 + dg;
                if gg < 0 || gg >= gates as i64 {
                    continue;
                }
                let idx = rr * gates + gg as usize;
                if flagged[idx] && !visited[idx] {
                    visited[idx] = true;
                    stack.push(idx);
                }
            }
        }
        clusters.push((size, peak_cell, clusters.len()));
    }
    clusters.sort_by_key(|cluster| std::cmp::Reverse(cluster.0));
    for (size, peak_cell, _) in clusters.iter().take(5) {
        let (row, gate) = (peak_cell / gates, peak_cell % gates);
        let az = sweep.rays.azimuth_deg.get(row).copied().unwrap_or(0.0);
        let nyq = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|nyquist| nyquist.get(row).copied());
        let range_km = (first + gate as f64 * spacing) / 1000.0;
        let raw = velocity.value(row, gate);
        let dl = dealiased.value(row, gate);
        println!(
            "cluster {size} gates @ az {az:.1} rng {range_km:.1} km: raw {raw:?} -> dealiased {dl:?} (nyq {nyq:?})"
        );
        // neighbors along the radial for context
        for g in gate.saturating_sub(6)..=(gate + 6).min(gates - 1) {
            if g % 2 == 0 {
                let r2 = velocity.value(row, g);
                let d2 = dealiased.value(row, g);
                println!("   g{g}: raw {r2:?} dl {d2:?}");
            }
        }
    }
    Ok(())
}
