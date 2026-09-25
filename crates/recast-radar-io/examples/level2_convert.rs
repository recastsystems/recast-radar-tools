//! Convert radar files to a Level II file with the writer's options
//! (`docs/level2/writer.md`).
//!
//! ```text
//! cargo run --release -p recast-radar-io --example level2_convert -- [OPTIONS] OUT SOURCE...
//! ```
//!
//! Every SOURCE is decoded by the format router (a JMA tar with
//! `--jma-station ID` by that station) and the volumes are merged
//! (`merge_volumes`: one scan split across files, such as ODIM
//! per-quantity files or JMA's N5 and N6 tars). Options:
//!
//! - `--quantization precise|compatible|standard`
//! - `--vcp N`, `--icao ID`, `--nyquist M_PER_S`, `--unambiguous-range M`
//! - `--sweeps-starting-at AZ,AZ,...`: keep, in this order, the sweeps whose
//!   first ray is at these azimuths (degrees, to 0.01), for example the
//!   sweeps of one 5-minute JMA cycle, so that one volume holds one scan
//! - `--uncompressed`, `--gzip`
//!
//! The write summary (moments, codings, notes) goes to standard error.

use std::process::ExitCode;

use recast_radar_core::model::{Volume, merge_volumes};
use recast_radar_io_nexrad::write::{
    Compression, Quantization, SourceMetadata, WriteOptions, write_volume_with_source,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("level2_convert: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut options = WriteOptions::default();
    let mut jma_station: Option<String> = None;
    let mut starts: Option<Vec<f32>> = None;
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--quantization" => {
                options.quantization = match value(&arg)?.as_str() {
                    "precise" => Quantization::Precise,
                    "compatible" => Quantization::Compatible,
                    "standard" => Quantization::Standard,
                    other => return Err(format!("unknown policy {other}")),
                }
            }
            "--vcp" => options.vcp = Some(parse(&value(&arg)?)?),
            "--icao" => options.icao = Some(value(&arg)?),
            "--nyquist" => options.nyquist_velocity_mps = Some(parse(&value(&arg)?)?),
            "--unambiguous-range" => options.unambiguous_range_m = Some(parse(&value(&arg)?)?),
            "--jma-station" => jma_station = Some(value(&arg)?),
            "--sweeps-starting-at" => {
                starts = Some(
                    value(&arg)?
                        .split(',')
                        .map(parse)
                        .collect::<Result<_, _>>()?,
                )
            }
            "--uncompressed" => options.compression = Compression::None,
            "--gzip" => options.gzip = true,
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => positional.push(arg),
        }
    }
    let Some((out, sources)) = positional.split_first() else {
        return Err("usage: level2_convert [OPTIONS] OUT SOURCE...".to_owned());
    };
    let mut parts = Vec::new();
    for source in sources {
        let bytes = std::fs::read(source).map_err(|err| format!("{source}: {err}"))?;
        let volume = match &jma_station {
            Some(station) if recast_radar_io_jma::looks_like_jma_tar_bytes(&bytes) => {
                recast_radar_io_jma::read_jma_tar_volumes(&bytes, Some(station))
                    .map_err(|err| format!("{source}: {err}"))?
                    .into_iter()
                    .next()
                    .ok_or(format!("{source}: no station {station}"))?
            }
            _ => recast_radar_io::read_supported_volume_bytes(&bytes)
                .map_err(|err| format!("{source}: {err}"))?,
        };
        parts.push(volume);
    }
    let (mut volume, report) = merge_volumes(parts).map_err(|err| err.to_string())?;
    eprintln!("merge: {report:?}");
    if let Some(starts) = starts {
        volume.sweeps = pick_sweeps(&volume, &starts)?;
    }
    let (bytes, summary) = write_volume_with_source(&volume, SourceMetadata::default(), &options)
        .map_err(|err| err.to_string())?;
    std::fs::write(out, &bytes).map_err(|err| format!("{out}: {err}"))?;
    eprintln!(
        "{out}: {} bytes, site {}, {} cuts, {} radials",
        bytes.len(),
        summary.icao,
        summary.sweeps,
        summary.radials
    );
    for report in &summary.moments {
        eprintln!(
            "  sweep {} {} from {}: {}-bit scale {} offset {}, max error {}",
            report.sweep,
            report.moment,
            report.field,
            report.word_size,
            report.scale,
            report.offset,
            report.max_abs_error
        );
    }
    for skipped in &summary.skipped_fields {
        eprintln!(
            "  sweep {} {} left out: {}",
            skipped.sweep, skipped.field, skipped.reason
        );
    }
    for note in &summary.notes {
        eprintln!("  note: {note}");
    }
    Ok(())
}

fn parse<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("cannot parse {text:?}"))
}

/// The sweeps whose first ray is at each azimuth (to 0.01 degree), in that
/// order; of several, the first that is not already taken.
fn pick_sweeps(
    volume: &Volume,
    starts: &[f32],
) -> Result<Vec<recast_radar_core::model::Sweep>, String> {
    let mut taken = vec![false; volume.sweeps.len()];
    let mut picked = Vec::new();
    for start in starts {
        let found = volume.sweeps.iter().enumerate().find(|(index, sweep)| {
            !taken[*index]
                && sweep
                    .rays
                    .azimuth_deg
                    .first()
                    .is_some_and(|first| (first - start).abs() < 0.005)
        });
        let Some((index, sweep)) = found else {
            return Err(format!("no sweep starts at {start} degrees"));
        };
        taken[index] = true;
        picked.push(sweep.clone());
    }
    Ok(picked)
}
