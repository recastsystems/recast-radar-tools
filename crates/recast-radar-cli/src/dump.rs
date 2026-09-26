//! `dump`: every decoded value of a file.
//!
//! The report is one JSON document (`--json`) or the same tree as indented
//! text: the volume, sweep and field metadata of the model, the NEXRAD
//! Level II metadata messages, and for Level III the message header, product
//! description, text header, every symbology and graphic packet (storm
//! positions, forecast tracks, text, symbols) and the tabular pages. Gate
//! data (`--data`) is streamed row by row, so dumping a whole volume never
//! builds it in memory as JSON.
//!
//! The decoder types without serde support are converted from their derived
//! `Debug` text; the mapping is in the `debug_json` module (structs become
//! objects, `Some(v)` becomes `v`, tuple variants become `{"Name": v}`).

use std::io::{self, Write};

use recast_radar_core::fm301::{self, Group, Values, ViewOptions};
use recast_radar_core::model::{Field, Sweep, Volume};
use recast_radar_io::FormatMetadata;
use recast_radar_io_level3::Level3Message;
use recast_radar_io_level3::packets::Packet;
use recast_radar_io_nexrad::NexradVolume;
use serde::Serialize;
use serde::ser::{SerializeSeq, Serializer};
use serde_json::{Map, Value, json};

use crate::debug_json;
use crate::open::{self, Contents, Input, Loaded, OpenOptions};
use crate::summary::{attr_value, field_metadata, sweep_metadata, volume_metadata};
use crate::{CliError, DumpArgs, Fm301Flavor};

pub(crate) fn run(args: &DumpArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let options = OpenOptions::from_args(&args.input, true);
    let input = open::open_path(&args.file, &options)?;
    if args.fm301 {
        return dump_fm301(input, args, out);
    }
    let selection = Selection::new(args);
    let document = Document::new(&input, &selection)?;
    if args.json {
        let mut serializer = serde_json::Serializer::pretty(&mut *out);
        document
            .serialize(&mut serializer)
            .map_err(io::Error::from)?;
        writeln!(out)?;
    } else {
        document.write_text(out)?;
    }
    Ok(())
}

/// The `dump --json` document of a decoded input as a JSON value: every
/// decoded value, with the gate data (and the bins of Level III data
/// packets) when `data` is set and every ray's values when `rays` is set.
pub fn document(input: &Input, data: bool, rays: bool) -> Result<Value, CliError> {
    let selection = Selection {
        sweep: None,
        fields: Vec::new(),
        data,
        rays,
    };
    let document = Document::new(input, &selection)?;
    serde_json::to_value(&document).map_err(|err| CliError::Failed(format!("dump: {err}")))
}

/// Which sweeps and fields to dump, and how much of each.
struct Selection {
    sweep: Option<usize>,
    fields: Vec<String>,
    data: bool,
    rays: bool,
}

impl Selection {
    fn new(args: &DumpArgs) -> Self {
        Self {
            sweep: args.sweep,
            fields: args.field.iter().map(|f| f.to_ascii_uppercase()).collect(),
            data: args.data,
            rays: args.rays,
        }
    }

    fn wants_field(&self, field: &Field) -> bool {
        self.fields.is_empty()
            || self
                .fields
                .iter()
                .any(|name| name.eq_ignore_ascii_case(field.name.as_str()))
    }
}

#[derive(Serialize)]
struct Document<'a> {
    path: String,
    size_bytes: u64,
    format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    level3: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    records: Option<Value>,
    volumes: Vec<VolumeDoc<'a>>,
}

#[derive(Serialize)]
struct VolumeDoc<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    volume: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    format_metadata: Option<Value>,
    sweeps: Vec<SweepDoc<'a>>,
}

#[derive(Serialize)]
struct SweepDoc<'a> {
    #[serde(flatten)]
    meta: Map<String, Value>,
    fields: Vec<FieldDoc<'a>>,
}

#[derive(Serialize)]
struct FieldDoc<'a> {
    #[serde(flatten)]
    meta: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<FieldRows<'a>>,
}

/// A field's physical values, row by row; missing gates are `null`.
struct FieldRows<'a>(&'a Field);

/// One row of a field.
struct FieldRow<'a>(&'a Field, usize);

impl Serialize for FieldRows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (rows, _) = self.0.shape();
        let mut seq = serializer.serialize_seq(Some(rows))?;
        for row in 0..rows {
            seq.serialize_element(&FieldRow(self.0, row))?;
        }
        seq.end()
    }
}

impl Serialize for FieldRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (_, gates) = self.0.shape();
        let mut seq = serializer.serialize_seq(Some(gates))?;
        for gate in 0..gates {
            seq.serialize_element(&self.0.value(self.1, gate))?;
        }
        seq.end()
    }
}

impl<'a> Document<'a> {
    fn new(input: &'a Input, selection: &Selection) -> Result<Self, CliError> {
        let volumes = input.volumes();
        if let Some(index) = selection.sweep {
            let max = volumes
                .iter()
                .map(|(_, volume, _)| volume.sweeps.len())
                .max()
                .unwrap_or(0);
            if index >= max {
                return Err(CliError::Usage(format!(
                    "--sweep {index}: the file has {max} sweep(s)"
                )));
            }
        }
        let level3 = match &input.contents {
            Contents::Level3(level3) => Some(level3_value(&level3.message, selection.data)),
            _ => None,
        };
        let records = match &input.contents {
            Contents::Level2Records(summary) => Some(summary.json()),
            _ => None,
        };
        Ok(Self {
            path: input.path.display().to_string(),
            size_bytes: input.size,
            format: input.format_name(),
            level3,
            records,
            volumes: volumes
                .into_iter()
                .map(|(label, volume, metadata)| volume_doc(label, volume, metadata, selection))
                .collect(),
        })
    }

    fn write_text(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "path: {}", self.path)?;
        writeln!(out, "size_bytes: {}", self.size_bytes)?;
        writeln!(out, "format: {}", self.format)?;
        if let Some(level3) = &self.level3 {
            writeln!(out, "level3:")?;
            write_value(level3, 1, out)?;
        }
        if let Some(records) = &self.records {
            writeln!(out, "records:")?;
            write_value(records, 1, out)?;
        }
        for (index, volume) in self.volumes.iter().enumerate() {
            writeln!(out, "volume {index}:")?;
            if let Some(label) = volume.label {
                writeln!(out, "  label: {label}")?;
            }
            write_value(&volume.volume, 1, out)?;
            if let Some(metadata) = &volume.format_metadata {
                writeln!(out, "  format_metadata:")?;
                write_value(metadata, 2, out)?;
            }
            for sweep in &volume.sweeps {
                writeln!(
                    out,
                    "  sweep {}:",
                    sweep.meta.get("index").map_or(Value::Null, Clone::clone)
                )?;
                write_value(&Value::Object(sweep.meta.clone()), 2, out)?;
                for field in &sweep.fields {
                    writeln!(
                        out,
                        "    field {}:",
                        field
                            .meta
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                    )?;
                    write_value(&Value::Object(field.meta.clone()), 3, out)?;
                    if let Some(rows) = &field.data {
                        writeln!(out, "      data:")?;
                        write_rows(rows.0, out)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn volume_doc<'a>(
    label: Option<&'a str>,
    volume: &'a Volume,
    metadata: &'a FormatMetadata,
    selection: &Selection,
) -> VolumeDoc<'a> {
    let sweeps = volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(index, _)| selection.sweep.is_none_or(|wanted| wanted == *index))
        .map(|(index, sweep)| sweep_doc(sweep, index, selection))
        .collect();
    VolumeDoc {
        label,
        volume: volume_metadata(volume, selection.rays),
        format_metadata: format_metadata_json(metadata),
        sweeps,
    }
}

fn sweep_doc<'a>(sweep: &'a Sweep, index: usize, selection: &Selection) -> SweepDoc<'a> {
    SweepDoc {
        meta: sweep_metadata(sweep, index, selection.rays),
        fields: sweep
            .fields
            .iter()
            .filter(|field| selection.wants_field(field))
            .map(|field| FieldDoc {
                meta: field_metadata(field, sweep, true, selection.rays),
                data: selection.data.then_some(FieldRows(field)),
            })
            .collect(),
    }
}

/// Format metadata the model has no slot for, as `dump` gives it under
/// `format_metadata`: the NEXRAD Level II metadata messages (2, 3, 5, 13, 15,
/// 18, 32 and the per-sweep constant blocks) as `{"nexrad": {...}}`, built
/// from their `Debug` text. `None` for formats without such metadata.
pub fn format_metadata_json(metadata: &FormatMetadata) -> Option<Value> {
    match metadata {
        FormatMetadata::Nexrad(nexrad) => Some(json!({ "nexrad": debug_json::to_json(nexrad) })),
        _ => None,
    }
}

/// Longest list of numbers a Level III data packet shows without `--data`.
const ELIDED_ARRAY_LEN: usize = 64;

/// Every decoded value of a Level III message. The bins of data packets
/// (radial, raster, digital precipitation and generic radial data) are left
/// out without `--data`, as the gate data of the volume is.
fn level3_value(message: &Level3Message, data: bool) -> Value {
    match message {
        Level3Message::Product(product) => {
            let mut map = Map::new();
            map.insert("message".to_owned(), json!("product"));
            if let Some(header) = &product.text_header {
                map.insert("text_header".to_owned(), debug_json::to_json(header));
            }
            map.insert(
                "message_header".to_owned(),
                debug_json::to_json(&product.message_header),
            );
            map.insert(
                "description".to_owned(),
                debug_json::to_json(&product.description),
            );
            if let Some(symbology) = &product.symbology {
                let layers: Vec<Value> = symbology
                    .layers
                    .iter()
                    .map(|layer| {
                        Value::Array(
                            layer
                                .iter()
                                .map(|packet| packet_value(packet, data))
                                .collect(),
                        )
                    })
                    .collect();
                map.insert("symbology".to_owned(), json!({ "layers": layers }));
            }
            if let Some(graphic) = &product.graphic {
                let pages: Vec<Value> = graphic
                    .pages
                    .iter()
                    .map(|page| {
                        json!({
                            "number": page.number,
                            "packets": page
                                .packets
                                .iter()
                                .map(|packet| packet_value(packet, data))
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                map.insert(
                    "graphic".to_owned(),
                    json!({ "layout": debug_json::to_json(&graphic.layout), "pages": pages }),
                );
            }
            if let Some(tabular) = &product.tabular {
                let mut table = Map::new();
                table.insert("layout".to_owned(), debug_json::to_json(&tabular.layout));
                if let Some(header) = &tabular.message_header {
                    table.insert("message_header".to_owned(), debug_json::to_json(header));
                }
                if let Some(description) = &tabular.description {
                    table.insert("description".to_owned(), debug_json::to_json(description));
                }
                table.insert("data_bytes".to_owned(), json!(tabular.data.len()));
                let pages: Vec<Value> =
                    tabular.pages.iter().map(|page| json!(page.lines)).collect();
                table.insert("pages".to_owned(), Value::Array(pages));
                map.insert("tabular".to_owned(), Value::Object(table));
            }
            Value::Object(map)
        }
        Level3Message::GeneralStatus(status) => json!({
            "message": "general-status",
            "general_status": debug_json::to_json(status),
        }),
        Level3Message::Text(text) => json!({
            "message": "text",
            "text_header": debug_json::to_json(&text.text_header),
            "text": text.text,
        }),
        other => json!({ "message": "other", "value": debug_json::to_json(other) }),
    }
}

/// One display packet: its code, kind and decoded values.
fn packet_value(packet: &Packet, data: bool) -> Value {
    let (kind, value, bins) = match packet {
        Packet::Radial(p) => ("radial", debug_json::to_json(p), true),
        Packet::Raster(p) => ("raster", debug_json::to_json(p), true),
        Packet::DigitalPrecip(p) => ("digital_precip", debug_json::to_json(p), true),
        Packet::Generic(p) => ("generic", debug_json::to_json(p), true),
        Packet::Text(p) => ("text", debug_json::to_json(p), false),
        Packet::Symbol(p) => ("symbol", debug_json::to_json(p), false),
        Packet::Vectors(p) => ("vectors", debug_json::to_json(p), false),
        Packet::Contour(p) => ("contour", debug_json::to_json(p), false),
        Packet::Unknown { bytes, .. } => ("unknown", json!({ "bytes": bytes }), false),
        other => ("other", debug_json::to_json(other), false),
    };
    let value = if bins && !data {
        elide_long_arrays(value)
    } else {
        value
    };
    json!({
        "code": format!("0x{:04X}", packet.code()),
        "kind": kind,
        "packet": value,
    })
}

/// `value` with every list of more than [`ELIDED_ARRAY_LEN`] numbers
/// replaced by `{"elided_values": N}`.
fn elide_long_arrays(value: Value) -> Value {
    match value {
        Value::Array(items)
            if items.len() > ELIDED_ARRAY_LEN && items.iter().all(Value::is_number) =>
        {
            json!({ "elided_values": items.len() })
        }
        Value::Array(items) => Value::Array(items.into_iter().map(elide_long_arrays).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, elide_long_arrays(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Print a JSON tree as indented `key: value` lines.
fn write_value(value: &Value, depth: usize, out: &mut dyn Write) -> io::Result<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                write_entry(key, value, depth, out)?;
            }
        }
        Value::Array(items) if is_block(items) => {
            for (index, item) in items.iter().enumerate() {
                write_entry(&format!("[{index}]"), item, depth, out)?;
            }
        }
        other => writeln!(out, "{}{}", "  ".repeat(depth), inline(other))?,
    }
    Ok(())
}

/// One `key: value` line, or `key:` and the value indented below it.
fn write_entry(key: &str, value: &Value, depth: usize, out: &mut dyn Write) -> io::Result<()> {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(inner) if !inner.is_empty() => {
            writeln!(out, "{indent}{key}:")?;
            write_value(value, depth + 1, out)
        }
        Value::Array(items) if is_block(items) => {
            writeln!(out, "{indent}{key}:")?;
            write_value(value, depth + 1, out)
        }
        Value::String(text) if text.contains('\n') => {
            writeln!(out, "{indent}{key}:")?;
            for line in text.lines() {
                writeln!(out, "{indent}  {line}")?;
            }
            Ok(())
        }
        other => writeln!(out, "{indent}{key}: {}", inline(other)),
    }
}

/// Lists printed one item per line: those holding objects or lists (packets,
/// pages, clutter map radials), and lists of long text lines.
fn is_block(items: &[Value]) -> bool {
    items.iter().any(|item| {
        item.is_object() || item.is_array() || item.as_str().is_some_and(|text| text.len() > 40)
    })
}

fn inline(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(inline).collect::<Vec<_>>().join(", ")
        ),
        other => other.to_string(),
    }
}

fn write_rows(field: &Field, out: &mut dyn Write) -> io::Result<()> {
    let (rows, gates) = field.shape();
    for row in 0..rows {
        write!(out, "        {row}:")?;
        for gate in 0..gates {
            match field.value(row, gate) {
                Some(value) => write!(out, " {value}")?,
                None => write!(out, " -")?,
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

fn dump_fm301(input: Input, args: &DumpArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let path = input.path.clone();
    let options = match args.flavor {
        Fm301Flavor::Xradar => ViewOptions::XRADAR,
        Fm301Flavor::Wmo => ViewOptions::WMO,
    };
    let mut groups = Vec::new();
    for loaded in input.into_volumes() {
        let Loaded {
            volume, metadata, ..
        } = loaded;
        let group = match metadata {
            FormatMetadata::Nexrad(metadata) => {
                let nexrad = NexradVolume {
                    volume,
                    metadata: *metadata,
                };
                let view = fm301::volume_view(&nexrad.volume, options, Some(&nexrad))
                    .map_err(|err| CliError::Failed(format!("{}: {err}", path.display())))?;
                group_value(&view.root)
            }
            _ => {
                let view = fm301::volume_view(&volume, options, None)
                    .map_err(|err| CliError::Failed(format!("{}: {err}", path.display())))?;
                group_value(&view.root)
            }
        };
        groups.push(group);
    }
    if groups.is_empty() {
        return Err(CliError::Failed(format!(
            "{}: holds no radar volume",
            path.display()
        )));
    }
    if args.json {
        let document = if groups.len() == 1 {
            groups.swap_remove(0)
        } else {
            Value::Array(groups)
        };
        serde_json::to_writer_pretty(&mut *out, &document).map_err(io::Error::from)?;
        writeln!(out)?;
    } else {
        for group in &groups {
            write_group_text(group, 0, out)?;
        }
    }
    Ok(())
}

fn values_dtype(values: &Values<'_>) -> &'static str {
    match values {
        Values::Borrowed(array) => array.dtype(),
        Values::Mapped { native, .. } => native.dtype(),
        Values::Owned(array) => array.dtype(),
        Values::Scalar(scalar) => scalar.dtype(),
        Values::Text(_) => "string",
    }
}

fn group_value(group: &Group<'_>) -> Value {
    let mut map = Map::new();
    map.insert(
        "name".to_owned(),
        json!(if group.name.is_empty() {
            "/"
        } else {
            &group.name
        }),
    );
    let dims: Map<String, Value> = group
        .dims
        .iter()
        .map(|(name, len)| (name.to_string(), json!(len)))
        .collect();
    map.insert("dimensions".to_owned(), Value::Object(dims));
    let variables: Vec<Value> = group
        .variables
        .iter()
        .map(|variable| {
            let mut var = Map::new();
            var.insert("name".to_owned(), json!(variable.name));
            var.insert("dims".to_owned(), json!(variable.dims));
            var.insert("dtype".to_owned(), json!(values_dtype(&variable.values)));
            match &variable.values {
                Values::Scalar(scalar) => {
                    var.insert("value".to_owned(), crate::summary::scalar_value(*scalar));
                }
                Values::Text(text) => {
                    var.insert("value".to_owned(), json!(text));
                }
                _ => {}
            }
            let attrs: Map<String, Value> = variable
                .attrs
                .iter()
                .map(|(name, value)| (name.to_string(), attr_value(value)))
                .collect();
            var.insert("attrs".to_owned(), Value::Object(attrs));
            Value::Object(var)
        })
        .collect();
    map.insert("variables".to_owned(), Value::Array(variables));
    let attrs: Map<String, Value> = group
        .attrs
        .iter()
        .map(|(name, value)| (name.to_string(), attr_value(value)))
        .collect();
    map.insert("attrs".to_owned(), Value::Object(attrs));
    map.insert(
        "groups".to_owned(),
        Value::Array(group.children.iter().map(group_value).collect()),
    );
    Value::Object(map)
}

/// `ncdump -h`-style text of a group tree.
fn write_group_text(group: &Value, depth: usize, out: &mut dyn Write) -> io::Result<()> {
    let indent = "  ".repeat(depth);
    let name = group["name"].as_str().unwrap_or("?");
    writeln!(out, "{indent}group: {name} {{")?;
    if let Some(dims) = group["dimensions"].as_object().filter(|d| !d.is_empty()) {
        writeln!(out, "{indent}  dimensions:")?;
        for (dim, len) in dims {
            writeln!(out, "{indent}    {dim} = {len} ;")?;
        }
    }
    if let Some(variables) = group["variables"].as_array().filter(|v| !v.is_empty()) {
        writeln!(out, "{indent}  variables:")?;
        for variable in variables {
            let dims: Vec<&str> = variable["dims"]
                .as_array()
                .map(|dims| dims.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let value = variable
                .get("value")
                .map(|value| format!(" = {}", inline(value)))
                .unwrap_or_default();
            let dims = if dims.is_empty() {
                String::new()
            } else {
                format!("({})", dims.join(", "))
            };
            writeln!(
                out,
                "{indent}    {} {}{dims}{value} ;",
                variable["dtype"].as_str().unwrap_or("?"),
                variable["name"].as_str().unwrap_or("?"),
            )?;
            if let Some(attrs) = variable["attrs"].as_object() {
                for (attr, value) in attrs {
                    writeln!(
                        out,
                        "{indent}      {}:{attr} = {} ;",
                        variable["name"].as_str().unwrap_or("?"),
                        inline(value)
                    )?;
                }
            }
        }
    }
    if let Some(attrs) = group["attrs"].as_object().filter(|a| !a.is_empty()) {
        writeln!(out, "{indent}  // group attributes:")?;
        for (attr, value) in attrs {
            writeln!(out, "{indent}    :{attr} = {} ;", inline(value))?;
        }
    }
    if let Some(children) = group["groups"].as_array() {
        for child in children {
            write_group_text(child, depth + 1, out)?;
        }
    }
    writeln!(out, "{indent}}}")
}
