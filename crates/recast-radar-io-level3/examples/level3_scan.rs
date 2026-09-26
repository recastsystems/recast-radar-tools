//! Survey a directory tree of Level III files: decode every file with
//! [`decode_message`] and report, per product code, how many files decoded,
//! the display packet codes and generic component types found (nested SCIT
//! packets and graphic pages included), the files each rare packet appears
//! in, and every decode error.
//!
//! Used to look for real samples of the packets the corpus lacks
//! (`docs/level3/reference.md` section 7) in NCEI archive tarballs.
//!
//! ```text
//! cargo run --release -p recast-radar-io-level3 --example level3_scan -- DIR [DIR...]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use recast_radar_io_level3::packets::generic::GenericComponent;
use recast_radar_io_level3::packets::symbols::SymbolPacket;
use recast_radar_io_level3::{Level3Message, Packet, decode_message};

/// Packet codes reported file by file.
const RARE: &[u16] = &[5, 7, 9, 26, 29, 33, 0xBA0F, 0x3501];

#[derive(Default)]
struct ProductStats {
    files: usize,
    errors: Vec<(PathBuf, String)>,
    packets: BTreeMap<u16, usize>,
    components: BTreeMap<i32, usize>,
    awips: BTreeSet<String>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn visit(packet: &Packet, found: &mut Vec<u16>, components: &mut Vec<i32>) {
    found.push(packet.code());
    match packet {
        Packet::Symbol(SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested)) => {
            for inner in nested {
                visit(inner, found, components);
            }
        }
        Packet::Generic(generic) => {
            for component in &generic.components {
                components.push(match component {
                    GenericComponent::Radial(_) => 1,
                    GenericComponent::Grid(_) => 2,
                    GenericComponent::Area(_) => 3,
                    GenericComponent::Text { .. } => 4,
                    GenericComponent::Table(_) => 5,
                    GenericComponent::Event(_) => 6,
                    GenericComponent::Undecoded { kind, .. } => *kind,
                    _ => -1,
                });
            }
        }
        _ => {}
    }
}

fn main() {
    let mut files = Vec::new();
    for dir in std::env::args().skip(1) {
        walk(Path::new(&dir), &mut files);
    }
    files.sort();
    let mut stats: BTreeMap<i32, ProductStats> = BTreeMap::new();
    let mut rare: BTreeMap<u16, Vec<PathBuf>> = BTreeMap::new();
    let mut unknown: BTreeMap<u16, Vec<PathBuf>> = BTreeMap::new();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let awips = name
            .split('_')
            .nth(2)
            .map(|s| s.get(..3).unwrap_or(s).to_owned());
        match decode_message(&bytes) {
            Ok(Level3Message::Product(product)) => {
                let code = i32::from(product.description.product_code);
                let entry = stats.entry(code).or_default();
                entry.files += 1;
                entry.awips.extend(awips);
                let mut found = Vec::new();
                let mut components = Vec::new();
                for packet in product
                    .symbology
                    .iter()
                    .flat_map(|s| s.layers.iter().flatten())
                {
                    visit(packet, &mut found, &mut components);
                }
                for page in product.graphic.iter().flat_map(|g| g.pages.iter()) {
                    for packet in &page.packets {
                        visit(packet, &mut found, &mut components);
                    }
                }
                for code in &found {
                    *entry.packets.entry(*code).or_default() += 1;
                }
                for kind in &components {
                    *entry.components.entry(*kind).or_default() += 1;
                }
                let unique: BTreeSet<u16> = found.iter().copied().collect();
                for code in unique {
                    if RARE.contains(&code) {
                        rare.entry(code).or_default().push(path.clone());
                    }
                }
                for packet in product
                    .symbology
                    .iter()
                    .flat_map(|s| s.layers.iter().flatten())
                {
                    if let Packet::Unknown { code, .. } = packet {
                        unknown.entry(*code).or_default().push(path.clone());
                    }
                }
            }
            Ok(Level3Message::GeneralStatus(_)) => {
                stats.entry(-2).or_default().files += 1;
            }
            Ok(Level3Message::Text(_)) => {
                stats.entry(-1).or_default().files += 1;
            }
            Ok(_) => {}
            Err(err) => {
                let entry = stats.entry(-999).or_default();
                entry.files += 1;
                entry.awips.extend(awips);
                entry.errors.push((path.clone(), err.to_string()));
            }
        }
    }
    println!("{} files", files.len());
    for (code, s) in &stats {
        let packets: Vec<String> = s
            .packets
            .iter()
            .map(|(c, n)| format!("{c:#x}x{n}"))
            .collect();
        let components: Vec<String> = s
            .components
            .iter()
            .map(|(c, n)| format!("type{c}x{n}"))
            .collect();
        println!(
            "product {code:>4}: {:>5} files  ids {:?}  packets [{}] components [{}]",
            s.files,
            s.awips,
            packets.join(" "),
            components.join(" ")
        );
        for (path, err) in &s.errors {
            println!("    ERROR {}: {err}", path.display());
        }
    }
    for (code, paths) in &rare {
        println!("RARE packet {code:#x} in {} files:", paths.len());
        for path in paths.iter().take(10) {
            println!("    {}", path.display());
        }
    }
    for (code, paths) in &unknown {
        println!("UNKNOWN packet {code:#x} in {} files:", paths.len());
        for path in paths.iter().take(5) {
            println!("    {}", path.display());
        }
    }
}
