//! Dump a decoded radar volume (any supported local format) as comparable
//! text — the validation harness for real-file golden checks.
//!
//! Mirrors the app's file-open routing (`sniff_local_radar_kind` in app_ui):
//! magic bytes pick the decoder (zip / DORADE / HDF5-ODIM / netCDF3-CfRadial
//! / Archive II), then the volume prints site, per-sweep geometry, and a fixed
//! set of sampled gate values per field. An independent Python reader
//! (h5py / netCDF4) emits the same sample positions so the two outputs can
//! be diffed mechanically — the golden-fixture discipline used for DORADE.
//!
//! Usage: cargo run -p recast-radar-io --example dump_radar -- <file>

use std::path::{Path, PathBuf};

use recast_radar_core::model::{FieldData, Volume};

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run -p recast-radar-io --example dump_radar -- <radar-file>");
        std::process::exit(2);
    };

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("read {}: {err}", path.display());
            std::process::exit(1);
        }
    };

    let (kind, result) = decode_like_the_app(&path, &bytes);
    println!("kind: {kind}");
    let volume = match result {
        Ok(volume) => volume,
        Err(err) => {
            eprintln!("decode failed: {err}");
            std::process::exit(1);
        }
    };
    dump(&volume);
}

/// Magic-byte routing in the same precedence order as
/// `app_ui::sniff_local_radar_kind` (zip > DORADE > HDF5 > CDF > Archive II):
/// zip archives need the path-based batch decoder; everything else goes
/// through the shared `recast_radar_io::read_supported_volume_bytes` router.
fn decode_like_the_app(path: &Path, bytes: &[u8]) -> (&'static str, Result<Volume, String>) {
    let head = &bytes[..bytes.len().min(8)];
    if recast_radar_io_dorade::mobile_archive::looks_like_zip_bytes(head) {
        let result = recast_radar_io::read_mobile_archive_from_path(path)
            .map_err(|err| err.to_string())
            .and_then(|mut volumes| {
                if volumes.is_empty() {
                    Err("archive holds no volumes".to_owned())
                } else {
                    Ok(volumes.remove(0).volume)
                }
            });
        return ("MobileArchive", result);
    }
    let kind = match recast_radar_io::sniff_supported_volume_format(bytes) {
        recast_radar_io::SupportedVolumeFormat::Dorade => "DoradeSweep",
        recast_radar_io::SupportedVolumeFormat::OdimH5 => "OdimH5",
        recast_radar_io::SupportedVolumeFormat::CfRadial => "CfRadial",
        recast_radar_io::SupportedVolumeFormat::CfRadialNetcdf4 => "CfRadialNetcdf4",
        recast_radar_io::SupportedVolumeFormat::CfRadial2 => "CfRadial2",
        recast_radar_io::SupportedVolumeFormat::JmaGrib2Tar => "JmaGrib2Tar",
        recast_radar_io::SupportedVolumeFormat::NexradLevel2 => "NexradLevel2",
        // A format added after this example.
        _ => "Other",
    };
    (
        kind,
        recast_radar_io::read_supported_volume_bytes(bytes).map_err(|err| err.to_string()),
    )
}

fn dump(volume: &Volume) {
    println!(
        "site: id={} name={} lat={} lon={} elev_m={}",
        volume.attrs.instrument_name,
        volume.attrs.site_name.as_deref().unwrap_or("-"),
        fmt_opt(volume.location.latitude_deg),
        fmt_opt(volume.location.longitude_deg),
        fmt_opt(volume.location.altitude_m),
    );
    println!(
        "time: {}",
        volume.time_reference.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!(
        "sweeps={} rays={}",
        volume.sweeps.len(),
        volume.provenance.decode.decoded_ray_count
    );

    for (index, sweep) in volume.sweeps.iter().enumerate() {
        println!(
            "sweep {index} mode={} fixed={:.3} rays={} az0={} el0={} nyq0={} center0={} spacing={} gates={}",
            sweep.sweep_mode.as_str(),
            sweep.fixed_angle_deg,
            sweep.nrays(),
            fmt_opt(sweep.rays.azimuth_deg.first().copied()),
            fmt_opt(sweep.rays.elevation_deg.first().copied()),
            fmt_opt(
                sweep
                    .ray_vars
                    .nyquist_velocity_mps
                    .as_ref()
                    .and_then(|values| values.first().copied())
            ),
            fmt_opt(sweep.range.center_m(0)),
            fmt_opt(sweep.range.spacing_m()),
            sweep.range.ngates(),
        );
        for field in &sweep.fields {
            let (rows, bins) = field.shape();
            let storage = match &field.data {
                FieldData::U8 { .. } => "u8",
                FieldData::U16 { .. } => "u16",
                FieldData::I8 { .. } => "i8",
                FieldData::I16 { .. } => "i16",
                FieldData::I32 { .. } => "i32",
                FieldData::F32 { .. } => "f32",
                FieldData::F64 { .. } => "f64",
            };
            println!(
                "  field {} storage={storage} rows={rows} bins={bins} start={} stride={}",
                field.name.as_str(),
                field.gates.start,
                field.gates.stride
            );
            if rows == 0 || bins == 0 {
                continue;
            }
            for (ray, bin) in sample_positions(rows, bins) {
                println!(
                    "    v[{ray},{bin}]={}",
                    match field.value(ray, bin) {
                        Some(value) => format!("{value:.4}"),
                        None => "None".to_owned(),
                    }
                );
            }
        }
    }
}

/// The fixed sample set shared with the Python reference reader.
fn sample_positions(rows: usize, bins: usize) -> [(usize, usize); 5] {
    [
        (0, 0),
        (0, bins / 2),
        (rows / 4, bins / 3),
        (rows / 2, 10.min(bins - 1)),
        (rows - 1, bins - 1),
    ]
}

fn fmt_opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|v| DisplayF(v).to_string())
        .unwrap_or_else(|| "None".to_owned())
}

/// Format helper: floats to 4 decimals, everything else via Display.
struct DisplayF<T>(T);

impl<T: std::fmt::Display> std::fmt::Display for DisplayF<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self.0.to_string();
        match text.parse::<f64>() {
            Ok(value) => write!(f, "{value:.4}"),
            Err(_) => f.write_str(&text),
        }
    }
}
