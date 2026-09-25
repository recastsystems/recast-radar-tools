//! Record and message layout of Level II files, and the writer's output for
//! the same volumes compared with it (`docs/level2/writer.md`).
//!
//! ```text
//! cargo run --release -p recast-radar-io-nexrad --example level2_layout -- [--rewrite] PATH...
//! ```
//!
//! A PATH that is a directory is searched recursively for Level II files
//! (`*.ar2v`, `*.ar2v.gz`, `*.msg31.gz`, `*_V06`, `*_V06.gz`, `*_V07`). For
//! each file it prints the layout: the header's tape name, extension and
//! site, the wrapper and record layout (LDM bzip2 records and their radial
//! counts, or uncompressed frames), the metadata messages before the first
//! radial, the Message 5 pattern and cut count, and per elevation cut the
//! radial count, azimuth resolution, radial statuses, VCP number, constant
//! block sizes and moments (word size, scale, offset, gates, first gate,
//! spacing).
//!
//! With `--rewrite`, each file is also decoded, written again by the Level
//! II writer (default options, the file's own record compression, no
//! source metadata: the writer builds every message as for a foreign
//! volume), and the two layouts are compared item by item. The last line
//! counts the files whose items all agree, and per item the files where it
//! differs.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use recast_radar_io_nexrad::messages::vcp::VolumeCoveragePattern;
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use recast_radar_io_nexrad::parse_message_header;
use recast_radar_io_nexrad::write::{
    Compression, SourceMetadata, WriteOptions, write_volume_with_source,
};

/// Fixed frame of metadata messages.
const FRAME: usize = 2432;

/// The layout items of one file, each as comparable text.
#[derive(Default)]
struct Layout {
    items: BTreeMap<&'static str, String>,
    cuts: Vec<Cut>,
}

/// One elevation cut: the radials and moments of its first radial, its
/// constant block sizes, and its radial statuses (runs collapsed).
#[derive(PartialEq)]
struct Cut {
    moments: String,
    blocks: String,
    statuses: String,
}

/// One comparable part of a [`Cut`].
type CutPart = fn(&Cut) -> &str;

/// An elevation cut while its radials are read.
struct CutScan {
    number: u8,
    radials: usize,
    resolution: u8,
    statuses: Vec<u8>,
    vcp: u16,
    blocks: String,
    moments: String,
}

fn be_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// Run-length text of a sequence: `18x4 5`.
fn runs<T: PartialEq + std::fmt::Display>(values: &[T]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while let Some(value) = values.get(index) {
        let count = values[index..].iter().take_while(|v| *v == value).count();
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = if count > 1 {
            write!(out, "{value}x{count}")
        } else {
            write!(out, "{value}")
        };
        index += count;
    }
    out
}

/// LDM records after the volume header: the bzip2 streams' decompressed
/// sizes are not needed, only how many records there are and whether the
/// last one is marked.
fn ldm_record_count(file: &[u8]) -> Option<(usize, bool)> {
    let mut cursor = 24;
    let mut count = 0;
    while cursor < file.len() {
        let control = be_i32(file, cursor)?;
        if file.get(cursor + 4..cursor + 7) != Some(b"BZh") {
            return None;
        }
        count += 1;
        cursor += 4 + control.unsigned_abs() as usize;
        if control < 0 {
            return Some((count, cursor == file.len()));
        }
    }
    Some((count, false))
}

fn layout(raw: &[u8]) -> Result<Layout, String> {
    let mut layout = Layout::default();
    let gzip = raw.starts_with(&[0x1f, 0x8b]);
    let unwrapped = if gzip {
        recast_radar_io_nexrad::gzip::inflate_gzip_members_limited(raw, 1 << 30, "gzip")
            .map_err(|e| format!("gzip: {e}"))?
    } else {
        raw.to_vec()
    };
    let header = unwrapped.get(..24).ok_or("shorter than a volume header")?;
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace('\0', "\\0");
    layout.items.insert("tape", text(&header[..8]));
    layout.items.insert("extension", text(&header[8..12]));
    layout.items.insert("site", text(&header[20..24]));
    let records = messages::record_bytes(&unwrapped).map_err(|e| e.to_string())?;
    let storage = match ldm_record_count(&unwrapped) {
        Some((count, closed)) => format!(
            "{}LDM bzip2 records ({count}{})",
            if gzip { "gzip of " } else { "" },
            if closed { "" } else { ", last one unmarked" }
        ),
        None => format!("{}uncompressed", if gzip { "gzip of " } else { "" }),
    };
    layout.items.insert("storage", storage);

    // Frames before the first radial, then the radials.
    let mut metadata = Vec::new();
    let mut vcp = "none".to_owned();
    let mut vcp_size = "-";
    let mut cursor = 0;
    while cursor + 28 <= records.len() {
        let message = parse_message_header(&records, cursor + 12).map_err(|e| e.to_string())?;
        if matches!(message.message_type, 1 | 31) {
            break;
        }
        if message.size_halfwords != 0 {
            metadata.push(message.message_type);
        }
        if message.message_type == 5 && vcp == "none" {
            // Decoded from the frame, whatever size the header gives (some
            // converted files leave the 8-halfword header out of it).
            let body = records.get(cursor + 28..(cursor + FRAME).min(records.len()));
            (vcp, vcp_size) = match body.map(VolumeCoveragePattern::decode) {
                Some(Ok(pattern)) => (
                    format!(
                        "VCP {}, {} cuts",
                        pattern.pattern_number,
                        pattern.cuts.len()
                    ),
                    if usize::from(message.size_halfwords) == 8 + 11 + 23 * pattern.cuts.len() {
                        "with its header (ICD)"
                    } else {
                        "without its header"
                    },
                ),
                _ => ("undecodable".to_owned(), "-"),
            };
        }
        cursor += FRAME;
    }
    layout.items.insert("metadata", runs(&metadata));
    layout.items.insert("message 5", vcp);
    layout.items.insert("message 5 size", vcp_size.to_owned());

    // Radials: per elevation number, in order of appearance.
    let mut frames = Vec::new();
    let mut per_record = Vec::new();
    let mut cuts: Vec<CutScan> = Vec::new();
    for item in MessageWalker::new(&records[cursor..]) {
        let Ok((message, body)) = item else { continue };
        let MessageBody::DigitalRadarDataGeneric(radial) = body else {
            continue;
        };
        frames.push(12 + message.message_len());
        let header = &radial.header;
        let status = header.radial_status_code;
        match cuts.last_mut() {
            Some(cut) if cut.number == header.elevation_number => {
                cut.radials += 1;
                if cut.statuses.last() != Some(&status) {
                    cut.statuses.push(status);
                }
            }
            _ => {
                let blocks = format!(
                    "VOL {} ELV {} RAD {}",
                    radial.volume.map_or(0, |b| b.block_size),
                    radial.elevation.map_or(0, |b| b.block_size),
                    radial.radial.map_or(0, |b| b.block_size)
                );
                let moments: Vec<String> = radial
                    .moments
                    .iter()
                    .map(|m| {
                        format!(
                            "{}:{}bit/{}/{} {}x{}m@{}m",
                            m.name.to_string().trim(),
                            m.data_word_size,
                            m.scale,
                            m.offset,
                            m.gate_count,
                            m.gate_spacing_m,
                            m.first_gate_range_m
                        )
                    })
                    .collect();
                cuts.push(CutScan {
                    number: header.elevation_number,
                    radials: 1,
                    resolution: header.azimuth_resolution.code(),
                    statuses: vec![status],
                    vcp: radial.volume.map_or(0, |vol| vol.vcp_number),
                    blocks,
                    moments: moments.join(" "),
                });
            }
        }
    }
    // Radials per LDM record, from each record decompressed alone.
    if let Ok(blocks) = ldm_records(&unwrapped) {
        for block in &blocks {
            let count = MessageWalker::new(block)
                .filter(|item| matches!(item, Ok((_, MessageBody::DigitalRadarDataGeneric(_)))))
                .count();
            per_record.push(count);
        }
    }
    let fixed = !frames.is_empty() && frames.iter().all(|len| *len == FRAME);
    layout.items.insert(
        "radial frames",
        if fixed {
            "each radial in its own 2432-byte frame".to_owned()
        } else {
            "variable-length radials".to_owned()
        },
    );
    if !per_record.is_empty() {
        let text = if per_record.iter().sum::<usize>() == frames.len() {
            runs(&per_record)
        } else {
            // Frames run across record boundaries: the records are pieces
            // of one stream, not whole messages.
            "radials run across records".to_owned()
        };
        layout.items.insert("radials per record", text);
    }
    layout.items.insert("cuts", cuts.len().to_string());
    for cut in cuts {
        layout.cuts.push(Cut {
            moments: format!(
                "elevation {}: {} radials, resolution {}, VCP {}, {}",
                cut.number, cut.radials, cut.resolution, cut.vcp, cut.moments
            ),
            blocks: cut.blocks,
            statuses: runs(&cut.statuses),
        });
    }
    Ok(layout)
}

/// Each LDM record's decompressed bytes (the metadata record first).
fn ldm_records(file: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    let mut cursor = 24;
    while cursor < file.len() {
        let control = be_i32(file, cursor).ok_or("truncated control word")?;
        let end = cursor + 4 + control.unsigned_abs() as usize;
        let record = file.get(cursor..end).ok_or("truncated record")?;
        out.push(
            messages::record_bytes(record)
                .map_err(|e| e.to_string())?
                .into_owned(),
        );
        cursor = end;
        if control < 0 {
            break;
        }
    }
    Ok(out)
}

fn print_layout(layout: &Layout) {
    for (item, value) in &layout.items {
        println!("  {item}: {value}");
    }
    for cut in &layout.cuts {
        println!("  {}, {}, status {}", cut.moments, cut.blocks, cut.statuses);
    }
}

/// The writer's output for the volume of `raw`, with its record compression.
fn rewrite(raw: &[u8], theirs: &Layout) -> Result<Vec<u8>, String> {
    let decoded = recast_radar_io_nexrad::read_volume_with_metadata(raw)
        .map_err(|e| format!("decode: {e}"))?;
    let mut options = WriteOptions::default();
    if theirs
        .items
        .get("storage")
        .is_some_and(|storage| storage.ends_with("uncompressed"))
    {
        options.compression = Compression::None;
    }
    let (bytes, summary) =
        write_volume_with_source(&decoded.volume, SourceMetadata::default(), &options)
            .map_err(|e| format!("write: {e}"))?;
    for skipped in &summary.skipped_fields {
        println!(
            "  writer skipped sweep {} field {}: {}",
            skipped.sweep, skipped.field, skipped.reason
        );
    }
    for note in &summary.notes {
        println!("  writer note: {note}");
    }
    Ok(bytes)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for child in children {
            collect(&child, out);
        }
        return;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let level2 = [".ar2v", ".ar2v.gz", ".msg31.gz", "_V06", "_V06.gz", "_V07"]
        .iter()
        .any(|suffix| name.ends_with(suffix));
    if level2 {
        out.push(path.to_path_buf());
    }
}

fn main() -> ExitCode {
    let mut rewrite_too = false;
    let mut paths = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == "--rewrite" {
            rewrite_too = true;
        } else {
            collect(Path::new(&arg), &mut paths);
        }
    }
    if paths.is_empty() {
        eprintln!("usage: level2_layout [--rewrite] PATH...");
        return ExitCode::from(2);
    }
    let mut same = 0;
    let mut failed = 0;
    let mut differences: BTreeMap<String, usize> = BTreeMap::new();
    for path in &paths {
        println!("{}", path.display());
        let raw = match std::fs::read(path) {
            Ok(raw) => raw,
            Err(error) => {
                println!("  read: {error}");
                failed += 1;
                continue;
            }
        };
        let theirs = match layout(&raw) {
            Ok(layout) => layout,
            Err(error) => {
                println!("  layout: {error}");
                failed += 1;
                continue;
            }
        };
        print_layout(&theirs);
        if !rewrite_too {
            continue;
        }
        let ours = match rewrite(&raw, &theirs).and_then(|bytes| layout(&bytes)) {
            Ok(layout) => layout,
            Err(error) => {
                println!("  REWRITE FAILED: {error}");
                failed += 1;
                continue;
            }
        };
        let mut differ = Vec::new();
        for (item, value) in &theirs.items {
            let written = ours.items.get(item).map_or("(none)", String::as_str);
            if written != value {
                differ.push((item.to_string(), format!("{value} -> {written}")));
            }
        }
        for index in 0..theirs.cuts.len().max(ours.cuts.len()) {
            let (a, b) = (theirs.cuts.get(index), ours.cuts.get(index));
            let parts: [(&str, CutPart); 3] = [
                ("cut moments", |cut| &cut.moments),
                ("cut block sizes", |cut| &cut.blocks),
                ("cut statuses", |cut| &cut.statuses),
            ];
            for (item, part) in parts {
                let (a, b) = (a.map_or("(none)", part), b.map_or("(none)", part));
                if a != b {
                    differ.push((format!("{item} {}", index + 1), format!("{a} -> {b}")));
                }
            }
        }
        if differ.is_empty() {
            same += 1;
            println!("  rewritten: same layout");
        }
        let mut kinds = std::collections::BTreeSet::new();
        for (item, change) in &differ {
            println!("  rewritten {item}: {change}");
            kinds.insert(if item.starts_with("cut ") {
                item.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ')
                    .to_owned()
            } else {
                item.clone()
            });
        }
        for kind in kinds {
            *differences.entry(kind).or_default() += 1;
        }
    }
    println!(
        "{} files, {failed} failed{}",
        paths.len(),
        if rewrite_too {
            format!(
                ", {same} rewritten with the same layout; files differing, by item: {differences:?}"
            )
        } else {
            String::new()
        }
    );
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
