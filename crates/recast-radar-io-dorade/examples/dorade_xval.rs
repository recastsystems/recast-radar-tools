//! Cross-validate a DORADE sweepfile against its GR2 MSG31 (Archive II)
//! twin: same scan exported by the radar in both formats.
//!
//! Usage: cargo run -p recast-radar-io-dorade --example dorade_xval -- <swp.file> <file.msg31>

use recast_radar_core::model::Quantity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(dorade_path), Some(msg31_path)) = (args.next(), args.next()) else {
        eprintln!("usage: dorade_xval <swp.file> <file.msg31>");
        std::process::exit(2);
    };

    let dorade_bytes = std::fs::read(&dorade_path)?;
    let dorade = recast_radar_io_dorade::dorade::read_dorade_sweep_volume(&dorade_bytes)?;
    let msg31 = recast_radar_io_nexrad::read_volume_from_path(msg31_path.as_ref())?;

    println!(
        "DORADE site {} ({:?}, {:?}) t={}",
        dorade.attrs.instrument_name,
        dorade.location.latitude_deg,
        dorade.location.longitude_deg,
        dorade.time_reference
    );
    println!(
        "MSG31  site {} ({:?}, {:?}) t={}",
        msg31.attrs.instrument_name,
        msg31.location.latitude_deg,
        msg31.location.longitude_deg,
        msg31.time_reference
    );

    let dorade_sweep = &dorade.sweeps[0];
    let msg31_sweep = &msg31.sweeps[0];
    println!(
        "DORADE sweep: {} rays, fixed {:.3}; MSG31 sweep: {} rays, fixed {:.3} (of {} sweeps)",
        dorade_sweep.nrays(),
        dorade_sweep.fixed_angle_deg,
        msg31_sweep.nrays(),
        msg31_sweep.fixed_angle_deg,
        msg31.sweeps.len()
    );

    for quantity in [
        Quantity::Reflectivity,
        Quantity::RadialVelocity,
        Quantity::DifferentialReflectivity,
        Quantity::CorrelationCoefficient,
    ] {
        let (Some(left), Some(right)) = (dorade_sweep.find(quantity), msg31_sweep.find(quantity))
        else {
            println!("{quantity:?}: missing on one side");
            continue;
        };
        let (Some((left_first, left_spacing)), Some((right_first, right_spacing))) = (
            left.native_geometry(&dorade_sweep.range),
            right.native_geometry(&msg31_sweep.range),
        ) else {
            println!("{quantity:?}: no gate geometry");
            continue;
        };
        // Align rays by azimuth: find the MSG31 ray closest to each DORADE
        // ray and compare gates over the overlapping range.
        let mut compared = 0usize;
        let mut both_finite = 0usize;
        let mut max_abs_diff = 0.0f32;
        let mut sum_abs_diff = 0.0f64;
        let mut one_sided = 0usize;
        for (dorade_row, target_azimuth) in dorade_sweep.rays.azimuth_deg.iter().enumerate() {
            let Some((msg31_row, _)) = msg31_sweep
                .rays
                .azimuth_deg
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    azimuth_distance(**a, *target_azimuth)
                        .total_cmp(&azimuth_distance(**b, *target_azimuth))
                })
                .filter(|(_, azimuth)| azimuth_distance(**azimuth, *target_azimuth) < 0.25)
            else {
                continue;
            };
            if left.is_absent(dorade_row) || right.is_absent(msg31_row) {
                continue;
            }
            let gates = (left.ngates as usize).min(right.ngates as usize);
            // Compare at matching ranges (gate spacing may differ).
            for gate in 0..gates {
                let range_m = left_first + gate as f64 * left_spacing;
                let right_gate = ((range_m - right_first) / right_spacing).round();
                if right_gate < 0.0 || right_gate as usize >= right.ngates as usize {
                    continue;
                }
                let left_value = left.value(dorade_row, gate);
                let right_value = right.value(msg31_row, right_gate as usize);
                compared += 1;
                match (left_value, right_value) {
                    (Some(a), Some(b)) => {
                        both_finite += 1;
                        let diff = (a - b).abs();
                        max_abs_diff = max_abs_diff.max(diff);
                        sum_abs_diff += f64::from(diff);
                    }
                    (None, None) => {}
                    _ => one_sided += 1,
                }
            }
        }
        println!(
            "{quantity:?} ({} vs {}): {compared} gates compared, {both_finite} both-finite, mean |diff| {:.4}, max |diff| {:.4}, {one_sided} one-sided",
            left.name,
            right.name,
            if both_finite > 0 {
                sum_abs_diff / both_finite as f64
            } else {
                f64::NAN
            },
            max_abs_diff
        );
    }
    Ok(())
}

fn azimuth_distance(a: f32, b: f32) -> f32 {
    let diff = (a - b).rem_euclid(360.0);
    diff.min(360.0 - diff)
}
