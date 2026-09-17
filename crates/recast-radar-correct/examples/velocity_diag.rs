// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

// Quantitative + visual diagnostic for velocity dealias spokes.
//
// For each velocity cut it dealiases with the current production algorithm,
// computes the integer Nyquist "fold" applied to every gate, and measures
// AZIMUTHAL FOLD DISCONTINUITIES -- the signature of radial spokes (a radial
// unfolded to a different integer fold than its azimuthal neighbors). It also
// renders the fold field to a PNG with a diagnostic palette so spokes are
// directly visible as radial streaks.
//
// usage: cargo run --release -p recast-radar-correct --example velocity_diag -- <l2-file> <out-prefix>

use std::f32::consts::PI;
use std::path::PathBuf;

use image::{ImageBuffer, Rgba};
use recast_radar_core::{Field, Quantity, Sweep, Volume};
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

fn row_nyquist(sweep: &Sweep, row: usize) -> Option<f32> {
    sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()?
        .get(row)
        .copied()
        .filter(|value| !value.is_nan())
}

fn median_nyquist(sweep: &Sweep, field: &Field) -> f32 {
    let mut v: Vec<f32> = (0..field.nrays as usize)
        .filter_map(|row| row_nyquist(sweep, row))
        .filter(|x| x.is_finite() && *x > 0.0)
        .collect();
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
}

fn fold_of(observed: f32, dealiased: f32, nyq: f32) -> i32 {
    ((dealiased - observed) / (2.0 * nyq)).round() as i32
}

/// Compute + print the spoke metric for one velocity cut. Returns the
/// azimuthal fold-discontinuity rate (%) used to pick the worst cut.
fn analyze_cut(volume: &Volume, cut_index: usize) -> f64 {
    let sweep = &volume.sweeps[cut_index];
    let field = sweep.find(Quantity::RadialVelocity).unwrap();
    let nyq_med = median_nyquist(sweep, field);
    let dealiased = dealias_velocity(sweep, field);
    let (rows, gates) = field.shape();

    let fold_at = |row: usize, gate: usize| -> Option<i32> {
        let obs = field.value(row, gate)?;
        let deal = dealiased.value(row, gate)?;
        let nyq = row_nyquist(sweep, row).filter(|v| v.is_finite() && *v > 0.0)?;
        Some(fold_of(obs, deal, nyq))
    };

    // Neighbour-jump counters: a |Δv| > 1.3·Nyquist between physically adjacent
    // gates is a fold boundary (aliasing). We count them in the RAW observed
    // field (= aliasing present) and in the DEALIASED field (= aliasing left
    // unresolved OR newly created spokes). A good dealiaser drives the
    // dealiased count far below the raw count.
    let jump_thresh = 1.3f32;
    let obs_at = |row: usize, gate: usize| field.value(row, gate);
    let deal_at = |row: usize, gate: usize| dealiased.value(row, gate);

    let mut raw_pairs = 0u64;
    let mut raw_jumps = 0u64;
    let mut deal_pairs = 0u64;
    let mut deal_jumps = 0u64;
    let mut folded_gates = 0u64;
    let mut data_gates = 0u64;

    for row in 0..rows {
        let nyq = match row_nyquist(sweep, row).filter(|v| v.is_finite() && *v > 0.0) {
            Some(n) => n,
            None => continue,
        };
        let lim = jump_thresh * nyq;
        for gate in 0..gates {
            if let Some(f) = fold_at(row, gate) {
                data_gates += 1;
                if f != 0 {
                    folded_gates += 1;
                }
            }
            // range neighbour (gate+1) and azimuth neighbour (row+1)
            for (nr, ng) in [(row, gate + 1), (row + 1, gate)] {
                if nr >= rows || ng >= gates {
                    continue;
                }
                if let (Some(a), Some(b)) = (obs_at(row, gate), obs_at(nr, ng)) {
                    raw_pairs += 1;
                    if (a - b).abs() > lim {
                        raw_jumps += 1;
                    }
                }
                if let (Some(a), Some(b)) = (deal_at(row, gate), deal_at(nr, ng)) {
                    deal_pairs += 1;
                    if (a - b).abs() > lim {
                        deal_jumps += 1;
                    }
                }
            }
        }
    }

    let raw_rate = 100.0 * raw_jumps as f64 / raw_pairs.max(1) as f64;
    let deal_rate = 100.0 * deal_jumps as f64 / deal_pairs.max(1) as f64;
    println!(
        "cut#{cut_index:<2} elev={:>4.2} nyq={:.1} | folded={:>4.1}% | RAW fold-boundaries={:.3}% ({}) -> DEALIASED={:.3}% ({}) | {}",
        sweep.fixed_angle_deg,
        nyq_med,
        100.0 * folded_gates as f64 / data_gates.max(1) as f64,
        raw_rate,
        raw_jumps,
        deal_rate,
        deal_jumps,
        if deal_jumps > raw_jumps {
            "WORSE (spokes!)"
        } else if deal_jumps * 2 < raw_jumps {
            "improved"
        } else {
            "~unchanged"
        },
    );
    // Worst = most residual aliasing left in the dealiased field.
    deal_rate
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: velocity_diag <l2-file> <prefix>")?,
    );
    let prefix = args
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "veldiag".into());

    let volume = legacy_bridge::decode_level2(&input)?;

    // analyze EVERY velocity sweep; render the fold image for the worst one.
    let vel_cuts: Vec<usize> = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.find(Quantity::RadialVelocity).is_some())
        .map(|(i, _)| i)
        .collect();
    let vcp = volume
        .scan
        .vcp_pattern
        .map(|pattern| pattern.to_string())
        .unwrap_or_default();
    println!(
        "== {} vcp={vcp} velocity cuts: {:?} ==",
        input.file_name().unwrap().to_string_lossy(),
        vel_cuts
    );

    let mut worst = (vel_cuts[0], -1.0f64);
    for &ci in &vel_cuts {
        let score = analyze_cut(&volume, ci);
        if score > worst.1 {
            worst = (ci, score);
        }
    }
    let cut_index = worst.0;
    println!("-- rendering fold field for worst cut #{cut_index} --");

    let sweep = &volume.sweeps[cut_index];
    let field = sweep.find(Quantity::RadialVelocity).unwrap();
    let nyq_med = median_nyquist(sweep, field);
    let dealiased = dealias_velocity(sweep, field);

    let (rows, gates) = field.shape();

    // ---- spoke metric: azimuthal fold discontinuities ----
    let fold_at = |row: usize, gate: usize| -> Option<i32> {
        let obs = field.value(row, gate)?;
        let deal = dealiased.value(row, gate)?;
        let nyq = row_nyquist(sweep, row).filter(|v| v.is_finite() && *v > 0.0)?;
        Some(fold_of(obs, deal, nyq))
    };

    let _ = nyq_med;

    // ---- visual: fold-field PPI ----
    let size = 1400u32;
    let mut img =
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(size, size, Rgba([255, 255, 255, 255]));
    // azimuth -> row lookup (nearest)
    let mut az_rows: Vec<(f32, usize)> = (0..rows)
        .filter_map(|r| {
            let az = sweep.rays.azimuth_deg.get(r)?.rem_euclid(360.0);
            Some((az, r))
        })
        .collect();
    az_rows.sort_by(|a, b| a.0.total_cmp(&b.0));

    let (first_m, spacing) = field.native_geometry(&sweep.range).unwrap();
    let first_m = first_m as f32;
    let spacing = spacing as f32;
    let max_range = first_m + spacing * gates as f32;
    let view_range = max_range * 0.70;
    let cx = (size as f32 - 1.0) / 2.0;
    let cy = (size as f32 - 1.0) / 2.0;
    let radius_px = cx;

    let nearest_row = |az: f32| -> usize {
        match az_rows.binary_search_by(|probe| probe.0.total_cmp(&az)) {
            Ok(i) => az_rows[i].1,
            Err(i) => {
                let lo = if i == 0 { az_rows.len() - 1 } else { i - 1 };
                let hi = if i >= az_rows.len() { 0 } else { i };
                let dl = (az - az_rows[lo].0)
                    .rem_euclid(360.0)
                    .min((az_rows[lo].0 - az).rem_euclid(360.0));
                let dh = (az - az_rows[hi].0)
                    .rem_euclid(360.0)
                    .min((az_rows[hi].0 - az).rem_euclid(360.0));
                if dl <= dh {
                    az_rows[lo].1
                } else {
                    az_rows[hi].1
                }
            }
        }
    };

    let fold_color = |f: i32| -> [u8; 4] {
        match f {
            0 => [60, 60, 60, 255],
            1 => [255, 170, 170, 255],
            2 => [230, 40, 40, 255],
            f if f >= 3 => [120, 0, 0, 255],
            -1 => [170, 200, 255, 255],
            -2 => [40, 90, 230, 255],
            _ => [0, 0, 110, 255],
        }
    };

    if !az_rows.is_empty() {
        for py in 0..size {
            for px in 0..size {
                let dx = (px as f32 - cx) / radius_px * view_range;
                let dy = (py as f32 - cy) / radius_px * view_range;
                let range = (dx * dx + dy * dy).sqrt();
                if range < first_m || range >= max_range {
                    continue;
                }
                let mut az = dx.atan2(-dy) * 180.0 / PI; // screen y down -> north up
                if az < 0.0 {
                    az += 360.0;
                }
                let row = nearest_row(az);
                let gate = ((range - first_m) / spacing) as usize;
                if gate >= gates {
                    continue;
                }
                if let Some(f) = fold_at(row, gate) {
                    img.put_pixel(px, py, Rgba(fold_color(f)));
                }
            }
        }
    }
    let fold_path = format!("{prefix}_fold.png");
    img.save(&fold_path)?;
    println!("wrote {fold_path}");
    Ok(())
}
