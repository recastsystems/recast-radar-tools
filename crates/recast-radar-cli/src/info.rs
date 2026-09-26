//! `info`: a one-screen summary of each input file.

use std::io::Write;

use recast_radar_core::model::Volume;
use recast_radar_io::FormatMetadata;
use recast_radar_io_level3::Level3Message;
use serde_json::{Map, Value, json};

use crate::open::{self, Contents, Input, Level3Input, OpenOptions};
use crate::output::{human_bytes, opt_float};
use crate::summary::{max_range_km, nyquist_mps, time_text};
use crate::{CliError, InfoArgs};

pub(crate) fn run(args: &InfoArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let options = OpenOptions::from_args(&args.input, true);
    if args.merge {
        let loaded = open::load_one(&args.files, &options, true, None)?;
        let size = args
            .files
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .sum();
        let input = Input {
            path: format!("{} files merged", args.files.len()).into(),
            size,
            contents: Contents::Volumes(vec![loaded]),
        };
        let report = file_info(&input);
        if args.json {
            serde_json::to_writer_pretty(&mut *out, &report).map_err(std::io::Error::from)?;
            writeln!(out)?;
        } else {
            write_text(&report, out)?;
        }
        return Ok(());
    }
    let mut failures = Vec::new();
    let mut reports = Vec::new();
    for (index, path) in args.files.iter().enumerate() {
        match open::open_path(path, &options) {
            Ok(input) => {
                let report = file_info(&input);
                if args.json {
                    reports.push(report);
                } else {
                    if index > 0 {
                        writeln!(out)?;
                    }
                    write_text(&report, out)?;
                }
            }
            Err(err) if args.files.len() > 1 => {
                eprintln!("recast-radar: {err}");
                if args.json {
                    reports.push(
                        json!({ "path": path.display().to_string(), "error": err.to_string() }),
                    );
                }
                failures.push(err);
            }
            Err(err) => return Err(err),
        }
    }
    if args.json {
        let document = if args.files.len() == 1 {
            reports.pop().unwrap_or(Value::Null)
        } else {
            Value::Array(reports)
        };
        serde_json::to_writer_pretty(&mut *out, &document).map_err(std::io::Error::from)?;
        writeln!(out)?;
    }
    match failures.len() {
        0 => Ok(()),
        count => Err(CliError::Failed(format!(
            "{count} of {} files did not decode",
            args.files.len()
        ))),
    }
}

/// The `info` report of one input, as JSON.
pub(crate) fn file_info(input: &Input) -> Value {
    let mut map = Map::new();
    map.insert("path".to_owned(), json!(input.path.display().to_string()));
    map.insert("size_bytes".to_owned(), json!(input.size));
    map.insert("format".to_owned(), json!(input.format_name()));
    match &input.contents {
        Contents::Volumes(volumes) => {
            let volumes: Vec<Value> = volumes
                .iter()
                .map(|loaded| {
                    volume_info(loaded.label.as_deref(), &loaded.volume, &loaded.metadata)
                })
                .collect();
            map.insert("volumes".to_owned(), Value::Array(volumes));
        }
        Contents::Level3(level3) => {
            map.insert("level3".to_owned(), level3_info(level3));
            let volumes = match &level3.volume {
                Ok(volume) => vec![volume_info(None, volume, &FormatMetadata::None)],
                Err(_) => Vec::new(),
            };
            map.insert("volumes".to_owned(), Value::Array(volumes));
        }
        Contents::Level2Records(summary) => {
            map.insert("records".to_owned(), summary.json());
            map.insert("volumes".to_owned(), Value::Array(Vec::new()));
        }
    }
    Value::Object(map)
}

fn volume_info(label: Option<&str>, volume: &Volume, metadata: &FormatMetadata) -> Value {
    let mut map = Map::new();
    if let Some(label) = label {
        map.insert("label".to_owned(), json!(label));
    }
    map.insert("site".to_owned(), json!(volume.attrs.instrument_name));
    if let Some(name) = &volume.attrs.site_name {
        map.insert("site_name".to_owned(), json!(name));
    }
    map.insert(
        "latitude_deg".to_owned(),
        json!(volume.location.latitude_deg),
    );
    map.insert(
        "longitude_deg".to_owned(),
        json!(volume.location.longitude_deg),
    );
    map.insert("altitude_m".to_owned(), json!(volume.location.altitude_m));
    map.insert(
        "time_reference".to_owned(),
        json!(time_text(&volume.time_reference)),
    );
    if let Some(extent) = volume.ray_time_extent() {
        map.insert(
            "ray_times".to_owned(),
            json!({ "start": time_text(&extent.start), "end": time_text(&extent.end) }),
        );
    }
    let provenance = &volume.provenance;
    map.insert(
        "source_format".to_owned(),
        json!(open::source_format_name(volume)),
    );
    for (key, value) in [
        ("source_version", &provenance.source_version),
        ("conventions", &provenance.source_conventions),
        ("compression", &provenance.compression),
        ("scan_name", &volume.scan.name),
    ] {
        if let Some(value) = value {
            map.insert(key.to_owned(), json!(value));
        }
    }
    if let Some(vcp) = volume.scan.vcp_pattern {
        map.insert("vcp".to_owned(), json!(vcp));
    }
    if volume.attrs.platform_is_mobile {
        map.insert("mobile".to_owned(), json!(true));
    }
    map.insert(
        "platform_type".to_owned(),
        json!(volume.platform_type.as_str()),
    );
    let rays: usize = volume.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    map.insert("sweep_count".to_owned(), json!(volume.sweeps.len()));
    map.insert("ray_count".to_owned(), json!(rays));
    let sweeps: Vec<Value> = volume
        .sweeps
        .iter()
        .enumerate()
        .map(|(index, sweep)| {
            json!({
                "index": index,
                "sweep_number": sweep.sweep_number,
                "mode": sweep.sweep_mode.as_str(),
                "fixed_angle_deg": sweep.fixed_angle_deg,
                "rays": sweep.nrays(),
                "gates": sweep.range.ngates(),
                "first_gate_m": sweep.range.center_m(0),
                "gate_spacing_m": sweep.range.spacing_m(),
                "max_range_km": max_range_km(sweep),
                "nyquist_mps": nyquist_mps(sweep),
                "complete": sweep.complete,
                "fields": sweep.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect();
    map.insert("sweeps".to_owned(), Value::Array(sweeps));
    if let FormatMetadata::Nexrad(nexrad) = metadata {
        let mut level2 = Map::new();
        if let Some(time) = &nexrad.volume_header_time {
            level2.insert("volume_header_time".to_owned(), json!(time_text(time)));
        }
        if let Some(build) = &nexrad.build {
            level2.insert("rda_build".to_owned(), json!(build.to_string()));
        }
        let messages: Vec<&str> = [
            ("2", nexrad.rda_status.is_some()),
            ("3", nexrad.performance.is_some()),
            ("5", nexrad.vcp.is_some()),
            ("13", nexrad.bypass_map.is_some()),
            ("15", nexrad.clutter_filter_map.is_some()),
            ("18", nexrad.adaptation.is_some()),
            ("32", nexrad.prf.is_some()),
        ]
        .into_iter()
        .filter_map(|(code, present)| present.then_some(code))
        .collect();
        level2.insert("metadata_messages".to_owned(), json!(messages));
        if !nexrad.errors.is_empty() {
            level2.insert("metadata_errors".to_owned(), json!(nexrad.errors));
        }
        map.insert("nexrad".to_owned(), Value::Object(level2));
    }
    Value::Object(map)
}

fn level3_info(level3: &Level3Input) -> Value {
    let mut map = Map::new();
    match &level3.message {
        Level3Message::Product(product) => {
            let description = &product.description;
            let info = recast_radar_io_level3::product_info(description.product_code);
            map.insert("message".to_owned(), json!("product"));
            map.insert("product_code".to_owned(), json!(description.product_code));
            if let Some(info) = info {
                map.insert("mnemonic".to_owned(), json!(info.mnemonic));
                map.insert("product_name".to_owned(), json!(info.name));
                map.insert("kind".to_owned(), json!(format!("{:?}", info.kind)));
            }
            if let Some(header) = &product.text_header {
                map.insert("wmo_heading".to_owned(), json!(header.wmo_heading));
                if let Some(awips) = &header.awips_id {
                    map.insert("awips_id".to_owned(), json!(awips));
                }
            }
            map.insert("latitude_deg".to_owned(), json!(description.latitude_deg));
            map.insert("longitude_deg".to_owned(), json!(description.longitude_deg));
            map.insert("height_ft".to_owned(), json!(description.height_ft));
            map.insert("vcp".to_owned(), json!(description.vcp));
            map.insert(
                "volume_scan_time".to_owned(),
                json!(time_text(&description.volume_scan_time)),
            );
            map.insert(
                "generation_time".to_owned(),
                json!(time_text(&description.generation_time)),
            );
            map.insert(
                "elevation_number".to_owned(),
                json!(description.elevation_number),
            );
            if let Some(elevation) = recast_radar_io_level3::volume::elevation_deg(description) {
                map.insert("elevation_deg".to_owned(), json!(elevation));
            }
            map.insert("compressed".to_owned(), json!(description.compressed));
            let packets: usize = product
                .symbology
                .as_ref()
                .map_or(0, |symbology| symbology.layers.iter().map(Vec::len).sum());
            map.insert("symbology_packets".to_owned(), json!(packets));
            if let Some(graphic) = &product.graphic {
                map.insert("graphic_pages".to_owned(), json!(graphic.pages.len()));
            }
            if let Some(tabular) = &product.tabular {
                map.insert("tabular_pages".to_owned(), json!(tabular.pages.len()));
            }
        }
        Level3Message::GeneralStatus(status) => {
            map.insert("message".to_owned(), json!("general-status"));
            if let Some(header) = &status.text_header {
                map.insert("wmo_heading".to_owned(), json!(header.wmo_heading));
            }
        }
        Level3Message::Text(text) => {
            map.insert("message".to_owned(), json!("text"));
            map.insert(
                "wmo_heading".to_owned(),
                json!(text.text_header.wmo_heading),
            );
            if let Some(awips) = &text.text_header.awips_id {
                map.insert("awips_id".to_owned(), json!(awips));
            }
            map.insert("text_bytes".to_owned(), json!(text.text.len()));
        }
        _ => {
            map.insert("message".to_owned(), json!("other"));
        }
    }
    if let Err(reason) = &level3.volume {
        map.insert("no_data_array".to_owned(), json!(reason));
    }
    Value::Object(map)
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "-".to_owned(),
        Some(other) => other.to_string(),
    }
}

fn float(value: Option<&Value>, decimals: usize) -> String {
    opt_float(value.and_then(Value::as_f64), decimals)
}

/// The text form of [`file_info`].
pub(crate) fn write_text(report: &Value, out: &mut dyn Write) -> std::io::Result<()> {
    let size = report["size_bytes"].as_u64().unwrap_or(0);
    writeln!(
        out,
        "{}  {}  {}",
        text(report.get("path")),
        human_bytes(size),
        text(report.get("format"))
    )?;
    if let Some(level3) = report.get("level3") {
        write_level3_text(level3, out)?;
    }
    if let Some(records) = report.get("records") {
        write_records_text(records, out)?;
    }
    let volumes = report["volumes"].as_array().cloned().unwrap_or_default();
    for (index, volume) in volumes.iter().enumerate() {
        if volumes.len() > 1 {
            writeln!(
                out,
                "volume {index}{}",
                volume
                    .get("label")
                    .map(|label| format!(": {}", text(Some(label))))
                    .unwrap_or_default()
            )?;
        }
        write_volume_text(volume, out)?;
    }
    Ok(())
}

fn write_level3_text(level3: &Value, out: &mut dyn Write) -> std::io::Result<()> {
    match level3["message"].as_str() {
        Some("product") => {
            writeln!(
                out,
                "  product   {} {} ({}), code {}",
                text(level3.get("mnemonic")),
                text(level3.get("product_name")),
                text(level3.get("kind")),
                text(level3.get("product_code"))
            )?;
            writeln!(
                out,
                "  header    {} {}",
                text(level3.get("wmo_heading")),
                text(level3.get("awips_id"))
            )?;
            writeln!(
                out,
                "  time      volume scan {}, generated {}",
                text(level3.get("volume_scan_time")),
                text(level3.get("generation_time"))
            )?;
            writeln!(
                out,
                "  radar     {} N {} E {} ft, VCP {}, elevation {} ({} deg)",
                float(level3.get("latitude_deg"), 4),
                float(level3.get("longitude_deg"), 4),
                text(level3.get("height_ft")),
                text(level3.get("vcp")),
                text(level3.get("elevation_number")),
                float(level3.get("elevation_deg"), 2)
            )?;
            let mut blocks = vec![format!(
                "{} symbology packets",
                text(level3.get("symbology_packets"))
            )];
            if let Some(pages) = level3.get("graphic_pages") {
                blocks.push(format!("{pages} graphic pages"));
            }
            if let Some(pages) = level3.get("tabular_pages") {
                blocks.push(format!("{pages} tabular pages"));
            }
            writeln!(out, "  blocks    {}", blocks.join(", "))?;
        }
        Some(kind) => {
            writeln!(
                out,
                "  message   {kind} {}",
                text(level3.get("wmo_heading"))
            )?;
        }
        None => {}
    }
    if let Some(reason) = level3.get("no_data_array") {
        writeln!(out, "  data      none: {}", text(Some(reason)))?;
    }
    Ok(())
}

fn write_records_text(records: &Value, out: &mut dyn Write) -> std::io::Result<()> {
    if let Some(tape) = records.get("tape") {
        writeln!(
            out,
            "  header    {} {} {}",
            text(Some(tape)),
            text(records.get("site")),
            text(records.get("header_time"))
        )?;
    } else {
        writeln!(
            out,
            "  header    none (an intermediate real-time chunk or a bare record)"
        )?;
    }
    let mut counts: Vec<(u8, String)> = records["messages"]
        .as_object()
        .map(|counts| {
            counts
                .iter()
                .filter_map(|(kind, count)| Some((kind.parse::<u8>().ok()?, text(Some(count)))))
                .collect()
        })
        .unwrap_or_default();
    counts.sort();
    let messages: Vec<String> = counts
        .into_iter()
        .map(|(kind, count)| format!("{count} x {}", crate::records::message_name(kind)))
        .collect();
    writeln!(
        out,
        "  records   {} bytes decompressed",
        text(records.get("record_bytes"))
    )?;
    for message in messages {
        writeln!(out, "  message   {message}")?;
    }
    if let Some(cuts) = records["elevations"].as_array() {
        for cut in cuts {
            let moments: Vec<String> = cut["moments"]
                .as_array()
                .map(|names| names.iter().map(|name| text(Some(name))).collect())
                .unwrap_or_default();
            writeln!(
                out,
                "  cut {:>3}   {} deg, {} radials, azimuth {} to {}, {} to {}, status {}, {}",
                text(cut.get("elevation_number")),
                float(cut.get("elevation_deg"), 2),
                text(cut.get("radials")),
                float(cut.get("azimuth_first_deg"), 2),
                float(cut.get("azimuth_last_deg"), 2),
                text(cut.get("first_time")),
                text(cut.get("last_time")),
                text(cut.get("radial_statuses")),
                moments.join(" ")
            )?;
        }
    }
    if let Some(errors) = records["errors"].as_array() {
        for error in errors {
            writeln!(out, "  error     {}", text(Some(error)))?;
        }
    }
    Ok(())
}

fn write_volume_text(volume: &Value, out: &mut dyn Write) -> std::io::Result<()> {
    let mut site = text(volume.get("site"));
    if let Some(name) = volume.get("site_name") {
        site = format!("{site} ({})", text(Some(name)));
    }
    writeln!(
        out,
        "  site      {site}  {} N  {} E  {} m",
        float(volume.get("latitude_deg"), 4),
        float(volume.get("longitude_deg"), 4),
        float(volume.get("altitude_m"), 0)
    )?;
    let mut time = text(volume.get("time_reference"));
    if let Some(rays) = volume.get("ray_times") {
        time = format!(
            "{time}  (rays {} to {})",
            text(rays.get("start")),
            text(rays.get("end"))
        );
    }
    writeln!(out, "  time      {time}")?;
    let mut source = vec![text(volume.get("source_format"))];
    for key in ["source_version", "conventions", "compression"] {
        if let Some(value) = volume.get(key) {
            source.push(text(Some(value)));
        }
    }
    writeln!(out, "  source    {}", source.join(", "))?;
    let mut scan = Vec::new();
    if let Some(vcp) = volume.get("vcp") {
        scan.push(format!("VCP {vcp}"));
    }
    if let Some(name) = volume.get("scan_name") {
        scan.push(text(Some(name)));
    }
    if !scan.is_empty() {
        writeln!(out, "  scan      {}", scan.join(", "))?;
    }
    if let Some(nexrad) = volume.get("nexrad") {
        let messages: Vec<String> = nexrad["metadata_messages"]
            .as_array()
            .map(|codes| codes.iter().map(|code| text(Some(code))).collect())
            .unwrap_or_default();
        writeln!(
            out,
            "  level2    RDA build {}, metadata messages {}{}",
            text(nexrad.get("rda_build")),
            if messages.is_empty() {
                "none".to_owned()
            } else {
                messages.join(" ")
            },
            nexrad
                .get("metadata_errors")
                .and_then(Value::as_array)
                .map(|errors| format!(", {} metadata problems", errors.len()))
                .unwrap_or_default()
        )?;
    }
    writeln!(
        out,
        "  sweeps    {} sweeps, {} rays",
        text(volume.get("sweep_count")),
        text(volume.get("ray_count"))
    )?;
    let sweeps = volume["sweeps"].as_array().cloned().unwrap_or_default();
    if sweeps.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "  {:>5} {:>6}  {:<22} {:>5} {:>5} {:>7} {:>8} {:>7}  fields",
        "sweep", "angle", "mode", "rays", "gates", "gate_m", "range_km", "nyq_m/s"
    )?;
    for sweep in &sweeps {
        let fields: Vec<String> = sweep["fields"]
            .as_array()
            .map(|names| names.iter().map(|name| text(Some(name))).collect())
            .unwrap_or_default();
        writeln!(
            out,
            "  {:>5} {:>6}  {:<22} {:>5} {:>5} {:>7} {:>8} {:>7}  {}{}",
            text(sweep.get("index")),
            float(sweep.get("fixed_angle_deg"), 2),
            text(sweep.get("mode")),
            text(sweep.get("rays")),
            text(sweep.get("gates")),
            float(sweep.get("gate_spacing_m"), 0),
            float(sweep.get("max_range_km"), 1),
            float(sweep.get("nyquist_mps"), 1),
            fields.join(" "),
            if sweep["complete"].as_bool() == Some(false) {
                "  (incomplete)"
            } else {
                ""
            }
        )?;
    }
    Ok(())
}
