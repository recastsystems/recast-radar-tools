//! Debug helper: list + download KMKX volumes 16:30-17:10 UTC 2026-06-11.
use std::path::Path;

fn main() {
    let objects = recast_radar_data::recent_level2_objects("KMKX", 1, 400).expect("list");
    let mut picked: Vec<_> = objects
        .into_iter()
        .filter(|o| {
            let name = o.key.rsplit('/').next().unwrap_or("").to_owned();
            name.len() > 19
                && name.contains("20260611_")
                && name[13..19] >= *"163000"
                && name[13..19] <= *"171000"
        })
        .collect();
    picked.sort_by(|a, b| a.key.cmp(&b.key));
    println!("{} volumes", picked.len());
    let dir = Path::new("~/radar-work/data/kmkx-debug");
    for o in picked {
        let key = o.key.clone();
        match recast_radar_data::download_object(recast_radar_data::LEVEL2_ARCHIVE_BUCKET, o, dir) {
            Ok(d) => println!("ok {key} -> {:?}", d.path),
            Err(e) => println!("ERR {key}: {e}"),
        }
    }
}
