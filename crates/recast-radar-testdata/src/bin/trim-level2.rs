//! Trim a real NEXRAD Level II archive volume into a small committed fixture.
//!
//! ```text
//! trim-level2 (INPUT | --id ID) OUTPUT [--sweeps N] [--max-radials N | --max-bytes N]
//! ```
//!
//! `--id` reads the verified file for a manifest id (downloading it into the
//! cache if needed). Without `--sweeps` the first split-cut pair is kept (or
//! the first sweep when there is none). `--max-bytes` picks the largest
//! record-aligned radial limit whose output fits. The printed `derivation`
//! line is the command to record in the manifest; it reproduces the output
//! byte for byte. See `recast_radar_testdata::trim` for the rules.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use recast_radar_testdata::sha256_hex;
use recast_radar_testdata::trim::{Framing, TOOL_NAME, TrimOptions, TrimReport, trim_level2};

const USAGE: &str =
    "usage: trim-level2 (INPUT | --id ID) OUTPUT [--sweeps N] [--max-radials N | --max-bytes N]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("trim-level2: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut id = None;
    let mut positional = Vec::new();
    let mut option_args = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--id" => id = Some(iter.next().ok_or("`--id` needs a value")?),
            "--sweeps" | "--max-radials" | "--max-bytes" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("`{arg}` needs a value"))?;
                option_args.push(arg);
                option_args.push(value);
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown argument `{flag}`\n{USAGE}"));
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let options = TrimOptions::from_args(&option_args).map_err(|e| e.to_string())?;
    let (input, output) = match (id, positional.as_slice()) {
        (Some(id), [output]) => {
            let path = recast_radar_testdata::path(&id).map_err(|e| e.to_string())?;
            (path, output.clone())
        }
        (None, [input, output]) => (input.clone(), output.clone()),
        _ => return Err(USAGE.to_owned()),
    };

    let source = fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let trimmed =
        trim_level2(&source, &options).map_err(|e| format!("{}: {e}", input.display()))?;
    fs::write(&output, &trimmed.bytes).map_err(|e| format!("{}: {e}", output.display()))?;

    println!(
        "source      {} ({} bytes, sha256 {})",
        input.display(),
        source.len(),
        sha256_hex(&source)
    );
    println!(
        "output      {} ({} bytes, sha256 {})",
        output.display(),
        trimmed.bytes.len(),
        sha256_hex(&trimmed.bytes)
    );
    print_report(&trimmed.report);
    Ok(())
}

fn print_report(report: &TrimReport) {
    println!("derivation  {TOOL_NAME} {}", report.options.to_args());
    let container = report.source_container.description();
    let framing = match report.source_framing {
        Framing::LdmRecords => format!(
            "{} ({} read)",
            Framing::LdmRecords.description(),
            report.source_records_read
        ),
        other => other.description().to_owned(),
    };
    println!("source fmt  {container}, {framing}");
    println!(
        "split cut   {}",
        if report.split_cut { "yes" } else { "no" }
    );
    println!("metadata    {} messages", report.metadata_messages);
    for (n, sweep) in report.sweeps.iter().enumerate() {
        println!(
            "sweep {}     elevation #{} ({:.2} deg, Message {}): {}/{} radials, azimuth {:.1} to {:.1} deg, moments {}",
            n + 1,
            sweep.elevation_number,
            sweep.mean_elevation_deg,
            sweep.message_type,
            sweep.radials_kept,
            sweep.radials_total,
            sweep.first_azimuth_deg,
            sweep.last_azimuth_deg,
            sweep.moments.join(" ")
        );
    }
    let indices: Vec<String> = report
        .source_record_indices
        .iter()
        .map(ToString::to_string)
        .collect();
    println!(
        "records     {} (source records {})",
        report.records,
        indices.join(" ")
    );
    let types: Vec<String> = report
        .message_types
        .iter()
        .map(|(msg_type, count)| format!("{msg_type}:{count}"))
        .collect();
    println!("messages    {}", types.join(" "));
}
