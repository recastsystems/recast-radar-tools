// Render one field of one sweep of a Level II file to a PNG.
//
// usage: cargo run -p recast-radar-render --example render_reflectivity_png -- <level2-file> <out.png> [sweep-index] [field]
//
// `field` is an FM301 name (DBZH, VRADH, ZDR, ...) or a NEXRAD block name
// (REF, VEL, SW, ...); the default is DBZH.

use std::path::{Path, PathBuf};

use recast_radar_core::FieldName;
use recast_radar_render::{RasterOptions, render_field_png};

#[path = "support/mod.rs"]
mod support;

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let Some(input) = args.next() else {
        eprintln!(
            "usage: cargo run -p recast-radar-render --example render_reflectivity_png -- <level2-file> <out.png> [sweep-index] [field]"
        );
        std::process::exit(2);
    };
    let Some(output) = args.next() else {
        eprintln!(
            "usage: cargo run -p recast-radar-render --example render_reflectivity_png -- <level2-file> <out.png> [sweep-index] [field]"
        );
        std::process::exit(2);
    };
    let sweep_index = std::env::args()
        .nth(3)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let field = std::env::args()
        .nth(4)
        .as_deref()
        .map(parse_field)
        .unwrap_or(FieldName::Dbzh);

    match run(&input, &output, sweep_index, &field) {
        Ok(()) => println!("wrote {}", output.display()),
        Err(err) => {
            eprintln!("render failed: {err}");
            std::process::exit(1);
        }
    }
}

fn run(
    input: &Path,
    output: &Path,
    sweep_index: usize,
    field: &FieldName,
) -> Result<(), Box<dyn std::error::Error>> {
    let volume = support::read_volume(input)?;
    render_field_png(
        &volume,
        sweep_index,
        field,
        output,
        RasterOptions::default(),
    )?;
    Ok(())
}

fn parse_field(value: &str) -> FieldName {
    FieldName::from_nexrad_block(value.to_ascii_uppercase().as_bytes())
}
