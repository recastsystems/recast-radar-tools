//! Write Level II files from real corpus volumes for the independent-reader
//! check (`tools/level2_writer_check.py`, `docs/level2/writer.md`).
//!
//! ```text
//! cargo run --release -p recast-radar-io --example level2_writer_check -- [--quantization precise|compatible|standard] <output-dir>
//! ```
//!
//! Writes one Level II file per source and variant into the directory, and
//! `manifest.json` describing each: the source (manifest id, local path,
//! format), the variant, the quantisation policy (the writer's default
//! unless `--quantization` names another), and for every written sweep the
//! moments with their source field, coding and error as the writer reported
//! them. Sources that are neither committed nor cached are skipped.

use std::path::{Path, PathBuf};

use recast_radar_core::model::{FieldName, Volume, merge_volumes};
use recast_radar_io_nexrad::write::{
    Compression, Moment, Quantization, SourceMetadata, WriteOptions, WriteSummary, data_messages,
    write_volume_with_source,
};
use serde_json::{Value, json};

/// (manifest id, source format label, variants)
const SOURCES: &[(&str, &str, &[&str])] = &[
    ("l2-ktlx-20240315-000217", "nexrad-level2", &["bzip2"]),
    ("l2-kilx-20260418-013553", "nexrad-level2", &["bzip2"]),
    (
        "l2-ktlx-20240315-000217-trim",
        "nexrad-level2",
        &["bzip2", "none", "gzip", "bzip2-gzip"],
    ),
    ("l2-kdvn-20200810-180401-trim", "nexrad-level2", &["bzip2"]),
    ("l2-tstl-20230331-230314-trim", "nexrad-level2", &["bzip2"]),
    (
        "l2-klix-20050829-130035-trim",
        "nexrad-level2",
        &["bzip2", "none"],
    ),
    ("l2-kiwa-20260917-003629", "nexrad-level2", &["bzip2"]),
    (
        "l2-kvwx-20080415-235337",
        "nexrad-level2",
        &["bzip2", "none"],
    ),
    (
        "odim-bejab-20190606-0000-pvol",
        "odim-h5",
        &["bzip2", "none", "gzip", "bzip2-gzip", "no-sweep-1-fields"],
    ),
    ("odim-dkrom-20260820-1130-pvol", "odim-h5", &["bzip2"]),
    ("odim-iesha-20260305-0115-pvol", "odim-h5", &["bzip2"]),
    ("odim-norst-20170421-0908-pvol", "odim-h5", &["bzip2"]),
    (
        "odim-espdg-20260707-1927-pvol-dbzh-vradh",
        "odim-h5",
        &["bzip2"],
    ),
    (
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        "cfradial1",
        &["bzip2"],
    ),
    (
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        "cfradial1",
        &["bzip2"],
    ),
    (
        "dorade-cow2-20260521-225514-sur-head24",
        "dorade",
        &["bzip2"],
    ),
    ("dorade-noxp-20090525-203211-sector", "dorade", &["bzip2"]),
    (
        "jma-n5-20191012-090000-rs47773",
        "jma-grib2-tar",
        &["bzip2", "none"],
    ),
    (
        "jma-n6-20191012-090000-rs47773",
        "jma-grib2-tar",
        &["bzip2", "none"],
    ),
];

/// The volume a variant writes: the source, or for `no-sweep-1-fields` the
/// source with sweep 1's fields removed (a sweep with nothing to write,
/// which the writer leaves out; Py-ART and xradar fail on a cut whose
/// radials carry no moment).
fn variant_volume(variant: &str, volume: &Volume) -> Volume {
    let mut volume = volume.clone();
    if variant == "no-sweep-1-fields"
        && let Some(sweep) = volume.sweeps.get_mut(1)
    {
        sweep.fields.clear();
    }
    volume
}

fn options(variant: &str, id: &str, quantization: Quantization) -> WriteOptions {
    let mut options = WriteOptions::default();
    options.quantization = quantization;
    // "gzip" is the layout of NOAA's 1991 to 2015 archive files (gzip over
    // uncompressed records); "bzip2-gzip" wraps LDM records in gzip.
    match variant {
        "none" => options.compression = Compression::None,
        "gzip" => {
            options.compression = Compression::None;
            options.gzip = true;
        }
        "bzip2-gzip" => options.gzip = true,
        _ => {}
    }
    if id.starts_with("l2-klix-2005") {
        options.drop_negative_range_gates = true;
    }
    if id.starts_with("dorade-noxp") {
        options.field_map = vec![
            (FieldName::parse("DB_ZDR"), Moment::Zdr),
            (FieldName::parse("DB_PHIDP"), Moment::Phi),
            (FieldName::parse("DB_RHOHV"), Moment::Rho),
        ];
    }
    options
}

fn summary_json(summary: &WriteSummary) -> Value {
    let moments: Vec<Value> = summary
        .moments
        .iter()
        .map(|m| {
            json!({
                "sweep": m.sweep,
                "moment": m.moment.name(),
                "field": m.field.as_str(),
                "word_size": m.word_size,
                "scale": m.scale,
                "offset": m.offset,
                "exact": m.exact,
                "max_abs_error": m.max_abs_error,
                "clamped_gates": m.clamped_gates,
                "dropped_gates": m.dropped_gates,
                "absent_rays": m.absent_rays,
            })
        })
        .collect();
    let written_rays: Vec<Value> = summary
        .written_rays
        .iter()
        .map(|rays| json!({ "sweep": rays.sweep, "rays": rays.rays }))
        .collect();
    json!({
        "icao": summary.icao,
        "sweeps": summary.sweeps,
        "radials": summary.radials,
        "skipped_sweeps": summary.skipped_sweeps,
        "written_rays": written_rays,
        "notes": summary.notes,
        "moments": moments,
    })
}

fn write_one(
    out: &Path,
    name: &str,
    volume: &Volume,
    source: SourceMetadata<'_>,
    options: &WriteOptions,
) -> Option<(PathBuf, WriteSummary)> {
    match write_volume_with_source(volume, source, options) {
        Ok((bytes, summary)) => {
            let path = out.join(name);
            if let Err(err) = std::fs::write(&path, &bytes) {
                eprintln!("{}: {err}", path.display());
                std::process::exit(1);
            }
            Some((path, summary))
        }
        Err(err) => {
            eprintln!("{name}: write refused: {err}");
            None
        }
    }
}

/// The policy named on the command line.
fn parse_quantization(name: &str) -> Option<Quantization> {
    match name {
        "precise" => Some(Quantization::Precise),
        "compatible" => Some(Quantization::Compatible),
        "standard" => Some(Quantization::Standard),
        _ => None,
    }
}

fn main() {
    let usage = || -> ! {
        eprintln!(
            "usage: level2_writer_check [--quantization precise|compatible|standard] <output-dir>"
        );
        std::process::exit(2);
    };
    let mut quantization = Quantization::default();
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--quantization" {
            quantization = args
                .next()
                .as_deref()
                .and_then(parse_quantization)
                .unwrap_or_else(|| usage());
        } else if out.is_none() {
            out = Some(PathBuf::from(arg));
        } else {
            usage();
        }
    }
    let Some(out) = out else { usage() };
    let policy = format!("{quantization:?}").to_ascii_lowercase();
    if let Err(err) = std::fs::create_dir_all(&out) {
        eprintln!("{}: {err}", out.display());
        std::process::exit(1);
    }
    let mut entries = Vec::new();
    let mut jma_parts = Vec::new();
    for (id, format, variants) in SOURCES {
        let path = match recast_radar_testdata::local_path(id) {
            Ok(path) => path,
            Err(err) => {
                eprintln!("skipping {id}: {err}");
                continue;
            }
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("{}: {err}", path.display());
                std::process::exit(1);
            }
        };
        let (volume, metadata, record, messages) = if *format == "nexrad-level2" {
            let decoded = match recast_radar_io_nexrad::read_volume_with_metadata(&bytes) {
                Ok(decoded) => decoded,
                Err(err) => {
                    eprintln!("{id}: {err}");
                    std::process::exit(1);
                }
            };
            let record = recast_radar_io_nexrad::messages::metadata_record(&bytes)
                .map(|record| record.into_owned())
                .ok();
            let messages = data_messages(&bytes).unwrap_or_default();
            (decoded.volume, Some(decoded.metadata), record, messages)
        } else {
            match recast_radar_io::read_supported_volume_bytes(&bytes) {
                Ok(volume) => (volume, None, None, Vec::new()),
                Err(err) => {
                    eprintln!("{id}: {err}");
                    std::process::exit(1);
                }
            }
        };
        if format.starts_with("jma") {
            jma_parts.push(volume.clone());
        }
        for variant in *variants {
            let source = SourceMetadata {
                metadata: metadata.as_ref(),
                metadata_record: record.as_deref(),
                data_messages: &messages,
            };
            // MetPy recognises gzip by the `.gz` suffix of a path.
            let suffix = if variant.ends_with("gzip") { ".gz" } else { "" };
            let name = format!("{id}.{variant}.ar2v{suffix}");
            let Some((written, summary)) = write_one(
                &out,
                &name,
                &variant_volume(variant, &volume),
                source,
                &options(variant, id, quantization),
            ) else {
                continue;
            };
            entries.push(json!({
                "output": written.display().to_string(),
                "source_id": id,
                "source_path": path.display().to_string(),
                "source_format": format,
                "variant": variant,
                "quantization": policy,
                "summary": summary_json(&summary),
            }));
        }
    }
    if jma_parts.len() == 2
        && let Ok((merged, _)) = merge_volumes(jma_parts)
    {
        for variant in ["bzip2", "none"] {
            let Some((written, summary)) = write_one(
                &out,
                &format!("jma-rs47773-merged.{variant}.ar2v"),
                &merged,
                SourceMetadata::default(),
                &options(variant, "jma-merged", quantization),
            ) else {
                continue;
            };
            entries.push(json!({
                "output": written.display().to_string(),
                "source_id": "jma-n5+n6-20191012-090000-rs47773",
                "source_path": "",
                "source_format": "jma-grib2-tar",
                "variant": variant,
                "quantization": policy,
                "summary": summary_json(&summary),
            }));
        }
    }
    let manifest = json!({ "outputs": entries });
    let path = out.join("manifest.json");
    let text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    if let Err(err) = std::fs::write(&path, text) {
        eprintln!("{}: {err}", path.display());
        std::process::exit(1);
    }
    println!(
        "{} files and {}",
        manifest["outputs"].as_array().map_or(0, Vec::len),
        path.display()
    );
}
