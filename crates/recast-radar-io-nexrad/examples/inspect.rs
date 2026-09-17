use std::path::PathBuf;

use chrono::{DateTime, Utc};
use recast_radar_core::model::{Sweep, Volume};
use recast_radar_io_nexrad::read_volume_from_path;

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run -p recast-radar-io-nexrad --example inspect -- <level2-file>");
        std::process::exit(2);
    };

    match read_volume_from_path(&path) {
        Ok(volume) => {
            println!("site: {}", volume.attrs.instrument_name);
            println!("time_reference: {}", volume.time_reference);
            if let Some(name) = &volume.scan.name {
                println!("scan: {name}");
            }
            let decode = volume.provenance.decode;
            println!(
                "messages: {} decoded_rays: {} skipped_messages: {}",
                decode.message_count, decode.decoded_ray_count, decode.skipped_message_count
            );
            println!("sweeps: {}", volume.sweeps.len());
            for sweep in &volume.sweeps {
                let start_time = sweep_time(&volume, sweep, f64::min)
                    .map(|time| time.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "--:--:--".to_owned());
                let end_time = sweep_time(&volume, sweep, f64::max)
                    .map(|time| time.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "--:--:--".to_owned());
                let fields = sweep
                    .fields
                    .iter()
                    .map(|field| {
                        let (nrays, ngates) = field.shape();
                        let bytes_per_gate = match field.data.dtype() {
                            "uint8" | "int8" => 1,
                            "uint16" | "int16" => 2,
                            "float32" => 4,
                            _ => 8,
                        };
                        let gate_bytes =
                            nrays.saturating_mul(ngates).saturating_mul(bytes_per_gate);
                        format!(
                            "{}:{nrays}x{ngates} {} {} KiB",
                            field.name,
                            field.data.dtype(),
                            gate_bytes / 1024
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "  sweep #{}: fixed={:.2} deg rays={} range={}x{:.0} m time={start_time}-{end_time} fields=[{}]",
                    sweep.sweep_number,
                    sweep.fixed_angle_deg,
                    sweep.nrays(),
                    sweep.range.ngates(),
                    sweep.range.spacing_m().unwrap_or(0.0),
                    fields
                );
            }
        }
        Err(err) => {
            eprintln!("decode failed: {err}");
            std::process::exit(1);
        }
    }
}

fn sweep_time(volume: &Volume, sweep: &Sweep, pick: fn(f64, f64) -> f64) -> Option<DateTime<Utc>> {
    let time_s = sweep
        .rays
        .time_s
        .iter()
        .copied()
        .filter(|time| time.is_finite())
        .reduce(pick)?;
    volume.instant(time_s)
}
