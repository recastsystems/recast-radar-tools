//! Cross-check the pipelined block-bzip decode against the independent
//! normalize-then-parse path on real Level II files.
//!
//! Usage: cargo run --release -p recast-radar-io-nexrad --example verify_pipeline -- <file>...

// Developer tool, not library code: a panic on bad input or I/O is its error report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_io_nexrad::{
    normalize_archive_bytes, read_volume_from_bytes, read_volume_from_bytes_with_bzip_preview,
};

fn main() {
    let mut all_ok = true;
    for path in std::env::args().skip(1) {
        let raw = std::fs::read(&path).unwrap();
        let pipelined = match read_volume_from_bytes(&raw) {
            Ok(volume) => volume,
            Err(err) => {
                println!("{path}: decode error: {err}");
                continue;
            }
        };
        let (normalized, compression) = normalize_archive_bytes(&raw).unwrap();
        let reference = read_volume_from_bytes(&normalized).unwrap();

        let cuts_match = pipelined.sweeps == reference.sweeps;
        let site_match =
            pipelined.location == reference.location && pipelined.attrs == reference.attrs;
        let vcp_match = pipelined.scan == reference.scan;
        let radials_match = pipelined.provenance.decode.decoded_ray_count
            == reference.provenance.decode.decoded_ray_count;

        let mut preview_radials = None;
        let with_preview = read_volume_from_bytes_with_bzip_preview(&raw, 360, |preview| {
            preview_radials = Some(preview.provenance.decode.decoded_ray_count);
        })
        .unwrap();
        let preview_full_match = with_preview.sweeps == pipelined.sweeps;

        let ok = cuts_match && site_match && vcp_match && radials_match && preview_full_match;
        all_ok &= ok;
        println!(
            "{path}: compression={compression:?} cuts={} radials={} preview_radials={preview_radials:?} \
             cuts_match={cuts_match} site_match={site_match} vcp_match={vcp_match} \
             radials_match={radials_match} preview_full_match={preview_full_match} => {}",
            pipelined.sweeps.len(),
            pipelined.provenance.decode.decoded_ray_count,
            if ok { "OK" } else { "MISMATCH" }
        );
    }
    if !all_ok {
        std::process::exit(1);
    }
}
