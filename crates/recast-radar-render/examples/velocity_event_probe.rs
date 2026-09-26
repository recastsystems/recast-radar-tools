//! Print the strongest inbound and outbound velocity gates inside an event's
//! bounding box for every low sweep of a Level II file, with the colors the
//! velocity table gives them raw and dealiased.
//!
//! usage: cargo run --release -p recast-radar-render --example velocity_event_probe -- <level2-file>

use std::cmp::Ordering;
use std::path::PathBuf;

use recast_radar_core::{Field, Quantity, Sweep, Volume};
use recast_radar_render::color::{ColorTable, builtin_velocity_table};
use recast_radar_render::dealiased_velocity_field;

#[path = "support/mod.rs"]
mod support;

const EARTH_KM_PER_DEG: f32 = 111.32;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: velocity_event_probe <level2-file>")?,
    );
    let volume = support::read_volume(&input)?;
    let table = builtin_velocity_table();
    let flipped = table.mirrored_values(format!("{} flipped", table.name()));

    println!(
        "file={} site={} volume={} sweeps={} rays={}",
        input.display(),
        volume.attrs.instrument_name,
        volume.time_reference,
        volume.sweeps.len(),
        volume.provenance.decode.decoded_ray_count
    );

    let (site_lat, site_lon) = site_location(&volume).ok_or("missing site location")?;
    let bbox = event_bbox(&volume.attrs.instrument_name).unwrap_or((
        site_lat - 0.5,
        site_lat + 0.5,
        site_lon - 0.6,
        site_lon + 0.6,
    ));
    println!(
        "site_lat={site_lat:.4} site_lon={site_lon:.4} bbox_lat={:.3}..{:.3} bbox_lon={:.3}..{:.3} table={}",
        bbox.0,
        bbox.1,
        bbox.2,
        bbox.3,
        table.name()
    );

    for (sweep_index, sweep) in volume.sweeps.iter().enumerate() {
        if sweep.fixed_angle_deg > 1.4 {
            continue;
        }
        let Some(field) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        let nyquist = median_nyquist(sweep);
        let dealiased = dealiased_velocity_field(&volume, sweep_index, &field.name)?;
        let samples = collect_samples(sweep, field, Some(&dealiased), site_lat, site_lon, bbox);
        if samples.is_empty() {
            println!(
                "sweep=#{sweep_index:02} elev={:.2} rows={} gates={} nyq={:?} no samples in bbox",
                sweep.fixed_angle_deg, field.nrays, field.ngates, nyquist
            );
            continue;
        }
        let positive = samples
            .iter()
            .filter(|sample| sample.raw_value > 0.0)
            .max_by(|left, right| abs_cmp(left.raw_value, right.raw_value));
        let negative = samples
            .iter()
            .filter(|sample| sample.raw_value < 0.0)
            .max_by(|left, right| abs_cmp(left.raw_value, right.raw_value));
        println!(
            "sweep=#{sweep_index:02} elev={:.2} rows={} gates={} nyq={:.2?} samples={} pos={} neg={}",
            sweep.fixed_angle_deg,
            field.nrays,
            field.ngates,
            nyquist,
            samples.len(),
            samples
                .iter()
                .filter(|sample| sample.raw_value > 0.0)
                .count(),
            samples
                .iter()
                .filter(|sample| sample.raw_value < 0.0)
                .count()
        );
        if let Some(sample) = negative {
            print_sample("strong_neg", sample, &table, &flipped);
        }
        if let Some(sample) = positive {
            print_sample("strong_pos", sample, &table, &flipped);
        }
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct GateSample {
    row: usize,
    gate: usize,
    azimuth: f32,
    range_km: f32,
    lat: f32,
    lon: f32,
    raw_value: f32,
    dealiased_value: Option<f32>,
}

fn collect_samples(
    sweep: &Sweep,
    field: &Field,
    dealiased: Option<&Field>,
    site_lat: f32,
    site_lon: f32,
    bbox: (f32, f32, f32, f32),
) -> Vec<GateSample> {
    let mut samples = Vec::new();
    let Some((first_center_m, spacing_m)) = field.native_geometry(&sweep.range) else {
        return samples;
    };
    let (rows, gates) = field.shape();
    for row in 0..rows {
        let Some(azimuth_deg) = sweep.rays.azimuth_deg.get(row).copied() else {
            continue;
        };
        let azimuth = azimuth_deg.to_radians();
        let sin_az = azimuth.sin();
        let cos_az = azimuth.cos();
        for gate in 0..gates {
            let Some(raw_value) = field.value(row, gate) else {
                continue;
            };
            let range_km = ((first_center_m + spacing_m * gate as f64) / 1000.0) as f32;
            let dy = cos_az * range_km;
            let dx = sin_az * range_km;
            let lat = site_lat + dy / EARTH_KM_PER_DEG;
            let lon = site_lon + dx / (EARTH_KM_PER_DEG * site_lat.to_radians().cos().max(0.01));
            if lat < bbox.0 || lat > bbox.1 || lon < bbox.2 || lon > bbox.3 {
                continue;
            }
            samples.push(GateSample {
                row,
                gate,
                azimuth: azimuth_deg,
                range_km,
                lat,
                lon,
                raw_value,
                dealiased_value: dealiased.and_then(|field| field.value(row, gate)),
            });
        }
    }
    samples
}

fn print_sample(label: &str, sample: &GateSample, table: &ColorTable, flipped: &ColorTable) {
    println!(
        "  {label}: raw={:+.1} dvel={} row={} gate={} az={:.1} range={:.1} lat={:.4} lon={:.4} color={} flipped={} dvel_color={}",
        sample.raw_value,
        sample
            .dealiased_value
            .map(|value| format!("{value:+.1}"))
            .unwrap_or_else(|| "n/a".to_owned()),
        sample.row,
        sample.gate,
        sample.azimuth,
        sample.range_km,
        sample.lat,
        sample.lon,
        color_string(table.color_for_value(sample.raw_value)),
        color_string(flipped.color_for_value(sample.raw_value)),
        sample
            .dealiased_value
            .map(|value| color_string(table.color_for_value(value)))
            .unwrap_or_else(|| "n/a".to_owned())
    );
}

fn color_string(color: [u8; 4]) -> String {
    format!("rgba({},{},{},{})", color[0], color[1], color[2], color[3])
}

fn site_location(volume: &Volume) -> Option<(f32, f32)> {
    match volume.attrs.instrument_name.as_str() {
        "KTWX" => Some((38.9969, -96.2326)),
        "KICT" => Some((37.6546, -97.4431)),
        _ => volume
            .location
            .latitude_deg
            .zip(volume.location.longitude_deg)
            .map(|(lat, lon)| (lat as f32, lon as f32)),
    }
}

fn event_bbox(site: &str) -> Option<(f32, f32, f32, f32)> {
    match site {
        "KTWX" => Some((38.95, 39.35, -96.85, -96.10)),
        "KICT" => Some((37.40, 38.70, -98.40, -96.70)),
        _ => None,
    }
}

fn median_nyquist(sweep: &Sweep) -> Option<f32> {
    let mut values = sweep
        .ray_vars
        .nyquist_velocity_mps
        .iter()
        .flatten()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.total_cmp(right));
    values.get(values.len() / 2).copied()
}

fn abs_cmp(left: f32, right: f32) -> Ordering {
    left.abs().total_cmp(&right.abs())
}
