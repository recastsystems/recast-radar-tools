//! Print the graphic alphanumeric pages (text packets in reading order) and
//! tabular pages of Level III files.
//!
//! ```text
//! cargo run -p recast-radar-io-level3 --example level3_pages -- FILE...
//! ```

use recast_radar_io_level3::{Packet, decode_product};

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let product = match decode_product(&bytes) {
            Ok(product) => product,
            Err(err) => {
                println!("{path}: {err}");
                continue;
            }
        };
        println!("==== {path} product {}", product.description.product_code);
        for page in product.graphic.iter().flat_map(|g| g.pages.iter()) {
            println!("-- graphic page {}", page.number);
            for packet in &page.packets {
                if let Packet::Text(text) = packet {
                    println!("  [{:>4},{:>4}] {:?}", text.i, text.j, text.text);
                }
            }
        }
        for (n, page) in product
            .tabular
            .iter()
            .flat_map(|t| t.pages.iter())
            .enumerate()
        {
            println!("-- tabular page {n}");
            for line in &page.lines {
                println!("  |{line}|");
            }
        }
    }
}
