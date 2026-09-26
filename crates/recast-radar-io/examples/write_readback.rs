//! Write radar files with every volume writer and read each output back,
//! without keeping the outputs: a survey tool for many real files (one per
//! site of a polling mirror, say).
//!
//! ```text
//! cargo run --release -p recast-radar-io --example write_readback -- <file or directory>...
//! ```
//!
//! A directory contributes its files and the first radar file of each
//! subdirectory (one per site of a polling directory). For each file
//! and writer (CfRadial 1 with the default layout, CfRadial 2 / FM301,
//! ODIM_H5) the output is read back through the format router and compared
//! with the source gate by gate, as the writer tests do
//! (`tests/common/compare.rs`): sweeps, rays (angles to 1e-4 degrees, times
//! to a microsecond), gate geometry, and every gate's value (float32
//! tolerance), missing, undetect and range-folded state. One line per file
//! and writer with the gates compared, or the first difference; a summary at
//! the end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use recast_radar_core::model::Volume;
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_io_cfradial::{
    Cfradial1Options, Cfradial2Options, RangeLayout, write_cfradial1, write_cfradial2,
};
use recast_radar_io_odim::{OdimWriteOptions, write_odim_h5_volume};

#[path = "../tests/common/compare.rs"]
mod compare;

use compare::{Expect, cfradial1_expect, cfradial2_expect, compare_volumes, odim_expect};

fn radar_files(arg: &Path) -> Vec<PathBuf> {
    if arg.is_file() {
        return vec![arg.to_path_buf()];
    }
    let Ok(entries) = std::fs::read_dir(arg) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let is_radar = |path: &Path| {
        path.is_file()
            && path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                !name.starts_with("dir.list") && !name.ends_with(".json") && !name.ends_with(".txt")
            })
    };
    let mut files: Vec<PathBuf> = paths
        .iter()
        .filter(|path| is_radar(path))
        .cloned()
        .collect();
    files.extend(paths.iter().filter(|path| path.is_dir()).filter_map(|dir| {
        let mut inner: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_radar(path))
            .collect();
        inner.sort();
        inner.into_iter().next()
    }));
    files
}

fn main() {
    type Write = fn(&Volume) -> Result<Vec<u8>, String>;
    type Relation = fn(&Volume) -> Expect;
    let writers: [(&str, Write, Relation); 3] = [
        (
            "cfradial1",
            |v| write_cfradial1(v, &Cfradial1Options::default()).map_err(|e| e.to_string()),
            |_| cfradial1_expect("CfRadial 1".into(), RangeLayout::Auto),
        ),
        (
            "fm301",
            |v| write_cfradial2(v, &Cfradial2Options::default()).map_err(|e| e.to_string()),
            |_| cfradial2_expect("FM301".into()),
        ),
        (
            "odim",
            |v| write_odim_h5_volume(v, &OdimWriteOptions::default()).map_err(|e| e.to_string()),
            |v| odim_expect("ODIM_H5".into(), v),
        ),
    ];
    let files: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .flat_map(|arg| radar_files(Path::new(&arg)))
        .collect();
    let mut summary: BTreeMap<&str, [usize; 4]> = BTreeMap::new();
    let mut undecodable = 0;
    for path in &files {
        let name = path.display();
        let Ok(bytes) = std::fs::read(path) else {
            println!("{name}: unreadable");
            continue;
        };
        let volume = match read_supported_volume_bytes(&bytes) {
            Ok(volume) => volume,
            Err(err) => {
                undecodable += 1;
                println!("{name}: not decoded: {err}");
                continue;
            }
        };
        for (writer, write, relation) in writers {
            let counts = summary.entry(writer).or_default();
            let line = match write(&volume) {
                Err(err) => {
                    counts[1] += 1;
                    format!("REFUSED {err}")
                }
                Ok(output) => match read_supported_volume_bytes(&output) {
                    Err(err) => {
                        counts[2] += 1;
                        format!("UNREADABLE ({} bytes): {err}", output.len())
                    }
                    Ok(read) => match compare_volumes(&volume, &read, &relation(&volume)) {
                        Ok(tally) => {
                            counts[0] += 1;
                            format!(
                                "ok ({} bytes; gates: {} values, {} missing, {} undetect)",
                                output.len(),
                                tally.values,
                                tally.missing,
                                tally.undetect
                            )
                        }
                        Err(difference) => {
                            counts[3] += 1;
                            format!("DIFFERS: {difference}")
                        }
                    },
                },
            };
            println!("{name} {writer}: {line}");
        }
    }
    println!(
        "{} files, {undecodable} not decoded; per writer ok/refused/unreadable/differs:",
        files.len()
    );
    for (writer, [ok, refused, unreadable, differs]) in summary {
        println!("  {writer}: {ok}/{refused}/{unreadable}/{differs}");
    }
}
