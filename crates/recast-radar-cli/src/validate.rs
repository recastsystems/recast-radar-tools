//! `validate`: decode files and check what they decoded to.
//!
//! A file fails when it does not decode, when the decoded volume breaks a
//! model invariant (`Volume::seal`), when the FM301 view cannot be built, or
//! when a check below finds a structural error. Implausible but decodable
//! content is a warning, which fails only with `--strict`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Utc};
use recast_radar_core::fm301::{self, ViewOptions};
use recast_radar_core::model::{Quantity, RangeCoord, SourceFormat, Sweep, Volume};
use recast_radar_io::FormatMetadata;
use serde_json::{Value, json};

use crate::open::{self, Contents, OpenOptions};
use crate::summary::{field_stats, time_text};
use crate::{CliError, ValidateArgs};

/// Findings for one file.
#[derive(Debug, Default)]
struct Report {
    path: PathBuf,
    format: Option<&'static str>,
    headline: Option<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn status(&self) -> &'static str {
        if !self.errors.is_empty() {
            "FAIL"
        } else if !self.warnings.is_empty() {
            "WARN"
        } else {
            "OK"
        }
    }

    fn json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "status": self.status(),
            "format": self.format,
            "summary": self.headline,
            "errors": self.errors,
            "warnings": self.warnings,
        })
    }
}

pub(crate) fn run(args: &ValidateArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let files = collect_files(&args.paths, args.recursive)?;
    if files.is_empty() {
        return Err(CliError::Usage("no files to validate".to_owned()));
    }
    let options = OpenOptions::from_args(&args.input, true);
    let mut reports = Vec::new();
    for path in files {
        let report = validate_file(&path, &options);
        if !args.json {
            write_report(&report, out)?;
        }
        reports.push(report);
    }
    let failed = reports.iter().filter(|r| !r.errors.is_empty()).count();
    let warned = reports
        .iter()
        .filter(|r| r.errors.is_empty() && !r.warnings.is_empty())
        .count();
    let ok = reports.len() - failed - warned;
    if args.json {
        let document = json!({
            "files": reports.iter().map(Report::json).collect::<Vec<_>>(),
            "ok": ok,
            "warnings": warned,
            "failed": failed,
        });
        serde_json::to_writer_pretty(&mut *out, &document).map_err(std::io::Error::from)?;
        writeln!(out)?;
    } else {
        writeln!(
            out,
            "{} file(s): {ok} ok, {warned} with warnings, {failed} failed",
            reports.len()
        )?;
    }
    if failed > 0 || (args.strict && warned > 0) {
        let what = if failed > 0 {
            format!("{failed} file(s) failed validation")
        } else {
            format!("{warned} file(s) have warnings (--strict)")
        };
        return Err(CliError::Failed(what));
    }
    Ok(())
}

/// Files a directory holds that are not radar data (polling-server
/// listings and configuration, images, documents).
fn skipped_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.')
        || lower == "dir.list"
        || [
            ".cfg", ".txt", ".json", ".md", ".html", ".png", ".pal", ".toml", ".py",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn collect_files(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for path in paths {
        let metadata = fs::metadata(path).map_err(|err| CliError::io(path, err))?;
        if metadata.is_dir() {
            walk(path, recursive, &mut files)?;
        } else {
            files.push(path.clone());
        }
    }
    Ok(files)
}

fn walk(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> Result<(), CliError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|err| CliError::io(dir, err))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for entry in entries {
        let name = entry
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if entry.is_dir() {
            if recursive && !name.starts_with('.') {
                walk(&entry, recursive, files)?;
            }
        } else if !skipped_name(&name) {
            files.push(entry);
        }
    }
    Ok(())
}

fn validate_file(path: &Path, options: &OpenOptions) -> Report {
    let mut report = Report {
        path: path.to_path_buf(),
        ..Report::default()
    };
    let input = match open::open_path(path, options) {
        Ok(input) => input,
        Err(err) => {
            report.errors.push(match err {
                CliError::Decode { message, .. } => format!("does not decode: {message}"),
                other => other.to_string(),
            });
            return report;
        }
    };
    report.format = Some(input.format_name());
    if let Contents::Level3(level3) = &input.contents
        && let Err(reason) = &level3.volume
    {
        report.headline = Some(format!("no data array: {reason}"));
        return report;
    }
    if let Contents::Level2Records(summary) = &input.contents {
        report.headline = Some(format!(
            "{} message(s), {} radial(s) in {} cut(s)",
            summary.messages.values().sum::<usize>(),
            summary.radials(),
            summary.elevations.len()
        ));
        report.warnings.extend(summary.errors.iter().cloned());
        if summary.more_errors > 0 {
            report
                .warnings
                .push(format!("{} more message errors", summary.more_errors));
        }
        return report;
    }
    let volumes = input.volumes();
    if volumes.is_empty() {
        report.errors.push("holds no radar volume".to_owned());
        return report;
    }
    let mut headlines = Vec::new();
    for (index, (label, volume, metadata)) in volumes.iter().enumerate() {
        let prefix = if volumes.len() > 1 {
            format!(
                "volume {index}{}: ",
                label.map(|l| format!(" ({l})")).unwrap_or_default()
            )
        } else {
            String::new()
        };
        let mut findings = Findings::default();
        check_volume(volume, metadata, &mut findings);
        report
            .errors
            .extend(findings.errors.into_iter().map(|e| format!("{prefix}{e}")));
        report.warnings.extend(
            findings
                .warnings
                .into_iter()
                .map(|w| format!("{prefix}{w}")),
        );
        headlines.push(format!(
            "{} {} {} sweep(s)",
            volume.attrs.instrument_name,
            time_text(&volume.time_reference),
            volume.sweeps.len()
        ));
    }
    report.headline = Some(headlines.join("; "));
    report
}

#[derive(Debug, Default)]
struct Findings {
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// Plausible physical range of a quantity, in its usual units.
fn plausible_range(quantity: Quantity) -> Option<(f32, f32)> {
    Some(match quantity {
        Quantity::Reflectivity | Quantity::TotalPower => (-100.0, 100.0),
        Quantity::RadialVelocity | Quantity::DealiasedRadialVelocity => (-150.0, 150.0),
        Quantity::SpectrumWidth => (0.0, 100.0),
        Quantity::DifferentialReflectivity => (-20.0, 20.0),
        Quantity::CorrelationCoefficient => (0.0, 2.0),
        Quantity::DifferentialPhase => (-400.0, 800.0),
        Quantity::SpecificDifferentialPhase => (-100.0, 100.0),
        _ => return None,
    })
}

fn check_volume(volume: &Volume, metadata: &FormatMetadata, findings: &mut Findings) {
    let level3 = volume.provenance.source_format == SourceFormat::NexradLevel3;
    if volume.sweeps.is_empty() {
        findings.errors.push("has no sweeps".to_owned());
        return;
    }
    if let Err(err) = volume.clone().seal() {
        findings
            .errors
            .push(format!("breaks a model invariant: {err}"));
    }
    if let Err(err) = fm301::volume_view(volume, ViewOptions::WMO, None) {
        findings
            .errors
            .push(format!("FM301 view cannot be built: {err}"));
    }
    if volume.attrs.instrument_name.trim().is_empty() {
        findings.warnings.push("has no radar name".to_owned());
    }
    match (volume.location.latitude_deg, volume.location.longitude_deg) {
        (Some(lat), Some(lon)) => {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=360.0).contains(&lon) {
                findings
                    .errors
                    .push(format!("location {lat}, {lon} is not on Earth"));
            } else if lat == 0.0 && lon == 0.0 {
                findings
                    .warnings
                    .push("location is 0 N 0 E (probably unset)".to_owned());
            }
        }
        _ => findings.warnings.push("has no location".to_owned()),
    }
    let year = volume.time_reference.year();
    if year < 1985 || volume.time_reference > Utc::now() + chrono::Duration::days(1) {
        findings.warnings.push(format!(
            "time {} is implausible",
            time_text(&volume.time_reference)
        ));
    }
    if let FormatMetadata::Nexrad(nexrad) = metadata {
        for error in &nexrad.errors {
            findings
                .warnings
                .push(format!("Level II metadata: {error}"));
        }
    }

    let mut values_anywhere = false;
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        values_anywhere |= check_sweep(index, sweep, level3, findings);
    }
    if !values_anywhere {
        findings
            .warnings
            .push("no field of any sweep has a valid gate".to_owned());
    }
}

/// Returns whether any field of the sweep has a valid gate.
fn check_sweep(index: usize, sweep: &Sweep, level3: bool, findings: &mut Findings) -> bool {
    let raster = sweep.sweep_mode.as_str() == "raster";
    let nrays = sweep.nrays();
    if nrays == 0 {
        findings.errors.push(format!("sweep {index} has no rays"));
        return false;
    }
    if !sweep.complete {
        findings
            .warnings
            .push(format!("sweep {index} is incomplete"));
    }
    if !raster {
        let bad_azimuth = sweep
            .rays
            .azimuth_deg
            .iter()
            .filter(|az| !az.is_finite())
            .count();
        if bad_azimuth > 0 {
            findings.errors.push(format!(
                "sweep {index}: {bad_azimuth} of {nrays} rays have no azimuth"
            ));
        }
        let outside = sweep
            .rays
            .azimuth_deg
            .iter()
            .filter(|az| az.is_finite() && !(-0.01..=360.01).contains(*az))
            .count();
        if outside > 0 {
            findings.warnings.push(format!(
                "sweep {index}: {outside} azimuths outside 0 to 360 deg"
            ));
        }
        let elevations = &sweep.rays.elevation_deg;
        let missing = elevations.iter().filter(|el| !el.is_finite()).count();
        if missing > 0 && !level3 {
            findings.warnings.push(format!(
                "sweep {index}: {missing} of {nrays} rays have no elevation"
            ));
        }
        let steep = elevations
            .iter()
            .filter(|el| el.is_finite() && !(-10.0..=180.0).contains(*el))
            .count();
        if steep > 0 {
            findings.warnings.push(format!(
                "sweep {index}: {steep} elevations outside -10 to 180 deg"
            ));
        }
    }
    let bad_times = sweep.rays.time_s.iter().filter(|t| !t.is_finite()).count();
    if bad_times > 0 {
        findings.warnings.push(format!(
            "sweep {index}: {bad_times} of {nrays} rays have no time"
        ));
    }
    match &sweep.range {
        RangeCoord::Uniform {
            spacing_m, ngates, ..
        } => {
            if *ngates > 0 && !(spacing_m.is_finite() && *spacing_m > 0.0) && !raster {
                findings.errors.push(format!(
                    "sweep {index}: gate spacing {spacing_m} m is not positive"
                ));
            }
        }
        RangeCoord::Explicit { centers_m } => {
            if centers_m.windows(2).any(|pair| pair[1] <= pair[0]) && !raster {
                findings.warnings.push(format!(
                    "sweep {index}: explicit gate ranges do not increase"
                ));
            }
        }
    }
    if sweep.fields.is_empty() {
        findings
            .warnings
            .push(format!("sweep {index} has no fields"));
        return false;
    }
    let mut any_values = false;
    for field in &sweep.fields {
        let name = field.name.as_str();
        if field.nrays as usize != nrays {
            findings.errors.push(format!(
                "sweep {index} field {name}: {} rows for {nrays} rays",
                field.nrays
            ));
        }
        if let Some(end) = field.gates.end(field.ngates)
            && end > sweep.range.ngates() as u64
        {
            findings.errors.push(format!(
                "sweep {index} field {name}: gates reach range gate {end}, past the sweep's {}",
                sweep.range.ngates()
            ));
        }
        let stats = field_stats(field);
        any_values |= stats.values > 0;
        if stats.non_finite > 0 {
            findings.warnings.push(format!(
                "sweep {index} field {name}: {} gates decode to non-finite values",
                stats.non_finite
            ));
        }
        if let (Some((lo, hi)), Some(min), Some(max)) =
            (plausible_range(field.quantity), stats.min, stats.max)
            && (min < lo || max > hi)
        {
            findings.warnings.push(format!(
                "sweep {index} field {name}: values {min} to {max} outside the plausible {lo} to {hi}"
            ));
        }
    }
    any_values
}

fn write_report(report: &Report, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "{:<4}  {}{}",
        report.status(),
        report.path.display(),
        match (&report.format, &report.headline) {
            (Some(format), Some(headline)) => format!("  ({format}: {headline})"),
            (Some(format), None) => format!("  ({format})"),
            _ => String::new(),
        }
    )?;
    for error in &report.errors {
        writeln!(out, "      error: {error}")?;
    }
    for warning in &report.warnings {
        writeln!(out, "      warning: {warning}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_directory_listings_are_not_radar_files() {
        for name in [
            "dir.list",
            "config.cfg",
            "grlevel2.cfg",
            ".hidden",
            "notes.txt",
        ] {
            assert!(skipped_name(name), "{name}");
        }
        for name in [
            "KXWA20260924_214316_V06.ar2v",
            "KBPP_20260924_2145.gz",
            "KTLX20240315_000217_V06",
            "bejab.pvol.hdf",
        ] {
            assert!(!skipped_name(name), "{name}");
        }
    }

    #[test]
    fn committed_fixtures_of_every_format_validate() {
        for id in [
            "l2-ktlx-20240315-000217-trim",
            "l2-ktlx-19910605-162126-trim",
            "odim-bejab-20190606-0000-pvol",
            "cfrad1-xsapr-sgp-20110520-ppi-classic",
            "dorade-noxp-20090501-190244-ppi",
            "jma-n5-20191012-090000-rs47773",
            "l3-byx-n0q-20150124-2106",
        ] {
            let path = recast_radar_testdata::path(id).unwrap_or_else(|err| panic!("{err}"));
            let report = validate_file(&path, &OpenOptions::default());
            assert!(report.errors.is_empty(), "{id}: {:?}", report.errors);
        }
    }
}
