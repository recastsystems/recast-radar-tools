//! Decode Meteo-France radar files and print each sweep (developer probe).
//!
//! cargo run --release -p recast-radar-io-bufr --example mf_probe -- FILE...

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                println!("{path}: {err}");
                continue;
            }
        };
        let start = std::time::Instant::now();
        let volume = recast_radar_io_bufr::read_meteofrance_volume(&bytes);
        let elapsed = start.elapsed();
        match volume {
            Ok(volume) => {
                println!(
                    "{path}: {:?} site {} ({:?}, {:?}, {:?} m) at {} in {:.1} ms",
                    volume.provenance.source_format,
                    volume.attrs.instrument_name,
                    volume.location.latitude_deg,
                    volume.location.longitude_deg,
                    volume.location.altitude_m,
                    volume.time_reference,
                    elapsed.as_secs_f64() * 1000.0
                );
                for sweep in &volume.sweeps {
                    let fields: Vec<String> = sweep
                        .fields
                        .iter()
                        .map(|field| {
                            let mut lo = f32::INFINITY;
                            let mut hi = f32::NEG_INFINITY;
                            let mut n = 0;
                            for ray in 0..field.nrays as usize {
                                for gate in 0..field.ngates as usize {
                                    if let Some(v) = field.value(ray, gate) {
                                        lo = lo.min(v);
                                        hi = hi.max(v);
                                        n += 1;
                                    }
                                }
                            }
                            format!("{} [{lo}, {hi}] n={n}", field.name)
                        })
                        .collect();
                    println!(
                        "  sweep {} {:.2} deg {:?} rays {} range {:?}: {}",
                        sweep.sweep_number,
                        sweep.fixed_angle_deg,
                        sweep.sweep_mode,
                        sweep.nrays(),
                        sweep.range,
                        fields.join("; ")
                    );
                }
            }
            Err(err) => println!("{path}: ERROR {err}"),
        }
    }
}
