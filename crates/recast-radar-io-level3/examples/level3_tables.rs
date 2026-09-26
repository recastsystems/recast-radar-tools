//! Print the typed storm attribute tables of Level III files: one line per
//! table with its row count, then any table line that did not parse as a row.
//!
//! ```text
//! cargo run -p recast-radar-io-level3 --example level3_tables -- FILE...
//! ```

use recast_radar_io_level3::decode_product;

fn main() {
    let (mut tables, mut rows, mut unparsed) = (0usize, 0usize, 0usize);
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else {
            println!("{path}: unreadable");
            continue;
        };
        let product = match decode_product(&bytes) {
            Ok(product) => product,
            Err(err) => {
                println!("{path}: {err}");
                continue;
            }
        };
        let found: Vec<(&str, usize, Vec<String>)> = [
            product
                .storm_tracking()
                .map(|t| ("storm_tracking", t.cells.len(), t.unparsed)),
            product
                .legacy_storm_tracking()
                .map(|t| ("legacy_storm_tracking", t.cells.len(), t.unparsed)),
            product
                .hail_index()
                .map(|t| ("hail_index", t.cells.len(), t.unparsed)),
            product
                .legacy_hail_index()
                .map(|t| ("legacy_hail_index", t.cells.len(), t.unparsed)),
            product
                .mesocyclone_table()
                .map(|t| ("mesocyclone_table", t.features.len(), t.unparsed)),
            product
                .tvs_table()
                .map(|t| ("tvs_table", t.features.len(), t.unparsed)),
            product
                .legacy_tvs_table()
                .map(|t| ("legacy_tvs_table", t.features.len(), t.unparsed)),
            product
                .mesocyclone_detections()
                .map(|t| ("mesocyclone_detections", t.circulations.len(), t.unparsed)),
            product
                .cell_attributes()
                .map(|t| ("cell_attributes", t.cells.len(), t.unparsed)),
            product
                .legacy_cell_attributes()
                .map(|t| ("legacy_cell_attributes", t.cells.len(), t.unparsed)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if found.is_empty() {
            println!(
                "{path}: product {} has no table",
                product.description.product_code
            );
        }
        for (name, count, lines) in found {
            println!(
                "{path}: product {} {name} {count} rows, {} unparsed",
                product.description.product_code,
                lines.len()
            );
            for line in &lines {
                println!("    unparsed: {line:?}");
            }
            tables += 1;
            rows += count;
            unparsed += lines.len();
        }
    }
    println!("{tables} tables, {rows} rows, {unparsed} unparsed lines");
}
