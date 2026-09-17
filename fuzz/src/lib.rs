//! Fuzz harness bodies for the recast-radar decoders.
//!
//! Each `fuzz_targets/<name>.rs` binary is a one-line libFuzzer wrapper around
//! the function of the same name here, so the stable `fuzz-tools replay`
//! runner (see `tools/`) executes exactly the code the fuzzer ran. A harness
//! must never panic, abort, hang, or exhaust memory on any input; decode
//! errors are the expected outcome for most inputs. Each harness returns
//! whether any entry point it called decoded successfully, which the replay
//! runner reports to confirm that seeds get past the parsers' error paths
//! (the fuzz targets ignore it).
//!
//! Harnesses that cover several entry points pick one from the input length
//! (never from the bytes), so every mode sees unmodified real-file prefixes
//! and a crash input replays deterministically.

use recast_radar_io_cfradial as cfradial_io;
use recast_radar_io_dorade as dorade_io;
use recast_radar_io_jma as jma_io;
use recast_radar_io_nexrad as nexrad;
use recast_radar_io_odim as odim_io;

/// Output cap for the `bzip2` harness: above the 16 MiB per-record limit the
/// Level II decoder uses, so a legitimate record always decodes fully, and
/// far below what a hostile stream of run-only blocks (about 46 MB per block)
/// could claim.
const BZIP2_MAX_OUTPUT: usize = 64 << 20;

/// A fuzz harness: decode `data`; `true` when any entry point returned `Ok`.
pub type Harness = fn(&[u8]) -> bool;

/// Every harness by target name, in `fuzz_targets/` order.
pub const TARGETS: &[(&str, Harness)] = &[
    ("level2_volume", level2_volume),
    ("io_router", io_router),
    ("odim", odim),
    ("cfradial", cfradial),
    ("dorade", dorade),
    ("jma", jma),
    ("bzip2", bzip2),
];

/// Look up a harness by target name.
pub fn harness(name: &str) -> Option<Harness> {
    TARGETS
        .iter()
        .find(|(target, _)| *target == name)
        .map(|(_, harness)| *harness)
}

/// NEXRAD Archive II / Level II volume decoding (`recast-radar-io-nexrad`):
/// gzip, whole-file bzip2, LDM block-bzip2 and uncompressed record streams,
/// through the whole-buffer, streaming-gzip and first-cut preview entry
/// points (mode = input length mod 4).
pub fn level2_volume(data: &[u8]) -> bool {
    // Preview threshold from the last byte: 0..=255 radials.
    let min_radials = usize::from(data.last().copied().unwrap_or(0));
    match data.len() % 4 {
        0 => nexrad::decode_volume_from_bytes(data).is_ok(),
        1 => nexrad::decode_gzip_volume_from_bytes_with_preview(data, min_radials, |_| {}).is_ok(),
        2 => nexrad::decode_volume_from_bytes_with_bzip_preview(data, min_radials, |_| {}).is_ok(),
        _ => {
            let gzip = nexrad::decode_gzip_preview_from_bytes(data, min_radials);
            let bzip = nexrad::decode_bzip_block_preview_from_bytes(data, min_radials);
            matches!(gzip, Ok(Some(_))) || matches!(bzip, Ok(Some(_)))
        }
    }
}

/// The bzip2 stream decoder (`recast-radar-bzip2`): the input through
/// `decode_stream_into`, then the input paired with its own first half
/// through `decode_two_into`, so the paired path sees every input next to a
/// truncated stream. The output limit keeps memory bounded for any input.
pub fn bzip2(data: &[u8]) -> bool {
    let mut decoder = recast_radar_bzip2::Decoder::new();
    decoder.set_max_output(BZIP2_MAX_OUTPUT);
    let mut out = Vec::new();
    let single = decoder.decode_stream_into(data, &mut out).is_ok();
    let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
    let (paired, truncated) =
        decoder.decode_two_into(data, &mut out_a, &data[..data.len() / 2], &mut out_b);
    single || paired.is_ok() || truncated.is_ok()
}

/// The format router (`recast-radar-io`): zip/gzip unwrapping, magic-byte
/// sniffing, and dispatch to every volume decoder.
pub fn io_router(data: &[u8]) -> bool {
    let _ = recast_radar_io::sniff_supported_volume_format(data);
    recast_radar_io::decode_supported_volume_bytes(data).is_ok()
}

/// ODIM_H5 polar volumes and Cartesian composites over the pure-Rust HDF5
/// reader (`recast-radar-io-odim`).
pub fn odim(data: &[u8]) -> bool {
    let _ = odim_io::looks_like_hdf5_bytes(data);
    let polar = odim_io::decode_odim_h5_volume(data).is_ok();
    let cartesian = odim_io::decode_odim_h5_cartesian_max(data).is_ok();
    polar || cartesian
}

/// CfRadial 1.x over the classic netCDF reader (`recast-radar-io-cfradial`).
pub fn cfradial(data: &[u8]) -> bool {
    let _ = cfradial_io::looks_like_netcdf3_bytes(data);
    cfradial_io::decode_cfradial1_volume(data).is_ok()
}

/// DORADE sweepfiles (`recast-radar-io-dorade`): header peek, then a
/// single-sweep decode (even lengths) or a two-sweep volume built from the
/// same bytes (odd lengths).
pub fn dorade(data: &[u8]) -> bool {
    let _ = dorade_io::looks_like_dorade_bytes(data);
    let peek = dorade_io::peek_dorade_sweep(data).is_ok();
    let decoded = if data.len().is_multiple_of(2) {
        dorade_io::decode_dorade_sweep_volume(data).is_ok()
    } else {
        dorade_io::decode_dorade_volume_from_slices(&[data, data]).is_ok()
    };
    peek || decoded
}

/// JMA GRIB2 radar tars (`recast-radar-io-jma`): all stations, first
/// station, or the station catalog plus a site-filtered decode (mode =
/// input length mod 3).
pub fn jma(data: &[u8]) -> bool {
    let _ = jma_io::looks_like_jma_tar_bytes(data);
    match data.len() % 3 {
        0 => jma_io::decode_jma_tar_volumes(data, None).is_ok(),
        1 => jma_io::decode_jma_tar_first_station(data).is_ok(),
        _ => match jma_io::jma_tar_station_headers(data) {
            Ok(stations) => stations.last().is_some_and(|station| {
                let filter = format!("RS{}", station.number);
                jma_io::decode_jma_tar_volumes(data, Some(&filter)).is_ok()
            }),
            Err(_) => false,
        },
    }
}
