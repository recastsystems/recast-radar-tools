//! Print what the `nexrad` crate reads from each Level II file given on the
//! command line, one JSON object per line: per sweep the elevation number,
//! the radial count and, per moment, the count and sum of the gate values,
//! the below-threshold and range-folded gate counts.

use nexrad::model::data::{MomentData, MomentValue, Radial};

fn moment_json(
    radials: &[Radial],
    name: &str,
    get: fn(&Radial) -> Option<&MomentData>,
) -> Option<String> {
    let (mut values, mut sum, mut below, mut folded, mut present) = (0u64, 0f64, 0u64, 0u64, 0u64);
    for radial in radials {
        let Some(moment) = get(radial) else { continue };
        present += 1;
        for value in moment.iter() {
            match value {
                MomentValue::Value(v) => {
                    values += 1;
                    sum += f64::from(v);
                }
                MomentValue::BelowThreshold => below += 1,
                MomentValue::RangeFolded => folded += 1,
            }
        }
    }
    (present > 0).then(|| {
        format!(r#""{name}":{{"radials":{present},"values":{values},"sum":{sum},"below":{below},"folded":{folded}}}"#)
    })
}

fn main() {
    for path in std::env::args().skip(1) {
        let escaped = path.replace(std::path::MAIN_SEPARATOR, "/");
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                println!(r#"{{"path":"{escaped}","error":"{error}"}}"#);
                continue;
            }
        };
        match nexrad::load(&data) {
            Ok(scan) => {
                let sweeps: Vec<String> = scan
                    .sweeps()
                    .iter()
                    .map(|sweep| {
                        let radials = sweep.radials();
                        let moments: Vec<String> = [
                            moment_json(radials, "REF", Radial::reflectivity),
                            moment_json(radials, "VEL", Radial::velocity),
                            moment_json(radials, "SW", Radial::spectrum_width),
                            moment_json(radials, "ZDR", Radial::differential_reflectivity),
                            moment_json(radials, "PHI", Radial::differential_phase),
                            moment_json(radials, "RHO", Radial::correlation_coefficient),
                        ]
                        .into_iter()
                        .flatten()
                        .collect();
                        format!(
                            r#"{{"elevation_number":{},"radials":{},"moments":{{{}}}}}"#,
                            sweep.elevation_number(),
                            radials.len(),
                            moments.join(",")
                        )
                    })
                    .collect();
                println!(r#"{{"path":"{escaped}","sweeps":[{}]}}"#, sweeps.join(","));
            }
            Err(error) => {
                let message = error.to_string().replace('"', "'");
                println!(r#"{{"path":"{escaped}","error":"{message}"}}"#);
            }
        }
    }
}
