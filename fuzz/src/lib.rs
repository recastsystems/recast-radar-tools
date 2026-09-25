//! Fuzz harness bodies for the recast-radar decoders and the bzip2 encoder.
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

use std::cell::RefCell;

use recast_radar_bzip2::{Decoder, Encoder, Level};
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
    ("bzip2_encode", bzip2_encode),
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
        0 => nexrad::read_volume_from_bytes(data).is_ok(),
        1 => nexrad::read_gzip_volume_from_bytes_with_preview(data, min_radials, |_| {}).is_ok(),
        2 => nexrad::read_volume_from_bytes_with_bzip_preview(data, min_radials, |_| {}).is_ok(),
        _ => {
            let gzip = nexrad::read_gzip_preview_from_bytes(data, min_radials);
            let bzip = nexrad::read_bzip_block_preview_from_bytes(data, min_radials);
            matches!(gzip, Ok(Some(_))) || matches!(bzip, Ok(Some(_)))
        }
    }
}

/// The bzip2 stream decoder (`recast-radar-bzip2`): the input through
/// `decode_stream_into`, then the input paired with its own first half
/// through `decode_two_into`, so the paired path sees every input next to a
/// truncated stream. The output limit keeps memory bounded for any input.
pub fn bzip2(data: &[u8]) -> bool {
    let mut decoder = Decoder::new();
    decoder.set_max_output(BZIP2_MAX_OUTPUT);
    let mut out = Vec::new();
    let single = decoder.decode_stream_into(data, &mut out).is_ok();
    let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
    let (paired, truncated) =
        decoder.decode_two_into(data, &mut out_a, &data[..data.len() / 2], &mut out_b);
    single || paired.is_ok() || truncated.is_ok()
}

thread_local! {
    /// The `bzip2_encode` harness's encoders (one per level) and decoder,
    /// reused across inputs the way a Level II writer reuses them: their
    /// work buffers (about 20 MB and 7 MiB) are allocated once, not per
    /// input, and every input meets buffers that earlier inputs wrote.
    static BZIP2_CODERS: RefCell<(Vec<Encoder>, Decoder)> = RefCell::new((
        (1..=9).filter_map(Level::new).map(Encoder::new).collect(),
        Decoder::new(),
    ));
}

/// The bzip2 encoder (`recast-radar-bzip2`), differentially: the input is
/// compressed at a block size picked from its length (levels 1 to 9, so
/// inputs over 100 kB give multi-block streams at the low levels), then its
/// first quarter is compressed by the same encoder and appended to the same
/// output vector. Each stream must equal the one the reference encoder (the
/// `bzip2` crate: libbz2-rs-sys, a port of libbzip2 1.0.8) writes, except
/// in the `origPtr` field of a block that is an exact repetition of a
/// shorter string (any of its identical rows is valid there), and must
/// decode to what was compressed with `recast-radar-bzip2`'s decoder, and
/// with the reference decoder when it is not the reference's stream. Any
/// difference panics.
///
/// The encoders and the decoder are reused across inputs, so the first
/// stream is written over whatever earlier inputs left in the buffers; the
/// second over what this input left, so a failure that depends on reused
/// state replays from the one input.
pub fn bzip2_encode(data: &[u8]) -> bool {
    let level = 1 + (data.len() % 9) as u32;
    BZIP2_CODERS.with(|coders| {
        let (encoders, decoder) = &mut *coders.borrow_mut();
        let Some(encoder) = encoders.get_mut(level as usize - 1) else {
            return false;
        };
        let mut out = Vec::new();
        encoder.encode_into(data, &mut out);
        check_stream(encoder, decoder, level, data, &out);
        let head = &data[..data.len() / 4];
        let start = out.len();
        encoder.encode_into(head, &mut out);
        check_stream(encoder, decoder, level, head, &out[start..]);
        true
    })
}

/// Checks of one stream the `bzip2_encode` harness wrote for `input` at
/// `level`, with `encoder` as the last call left it; panics on a failure.
fn check_stream(encoder: &Encoder, decoder: &mut Decoder, level: u32, input: &[u8], stream: &[u8]) {
    let mut ours = Vec::new();
    if let Err(e) = decoder.decode_stream_into(stream, &mut ours) {
        panic!("our decoder rejects our stream: {e}");
    }
    assert!(ours == input, "our decoder does not return the input");

    let reference = reference_encode(level, input);
    if stream != reference {
        assert!(
            reference_decode(stream, input.len()) == input,
            "the reference decoder does not return the input"
        );
        assert_eq!(
            stream.len(),
            reference.len(),
            "stream length differs from the reference"
        );
        let fields = encoder.__periodic_orig_ptr_bits();
        for (i, (a, b)) in stream.iter().zip(&reference).enumerate() {
            for bit in 0..8 {
                if (a ^ b) & (0x80 >> bit) != 0 {
                    let at = (i * 8 + bit) as u64;
                    assert!(
                        fields.iter().any(|&f| (f..f + 24).contains(&at)),
                        "bit {at} differs from the reference outside a periodic block's origPtr"
                    );
                }
            }
        }
    }
}

/// One bzip2 stream decoded by the reference (libbz2-rs-sys); panics on any
/// error.
fn reference_decode(stream: &[u8], size: usize) -> Vec<u8> {
    let mut d = bzip2::Decompress::new(false);
    let mut out = Vec::with_capacity(size + 1);
    loop {
        let before = (d.total_in(), d.total_out());
        let input = &stream[d.total_in() as usize..];
        match d.decompress_vec(input, &mut out) {
            Ok(bzip2::Status::StreamEnd) => return out,
            Ok(_) => {}
            Err(e) => panic!("the reference decoder rejects our stream: {e}"),
        }
        if (d.total_in(), d.total_out()) == before {
            out.reserve(out.capacity().max(1 << 16));
        }
    }
}

/// The reference encoder (libbz2-rs-sys) in one `BZ_FINISH` call, as
/// `BZ2_bzBuffToBuffCompress` does.
fn reference_encode(level: u32, data: &[u8]) -> Vec<u8> {
    let mut c = bzip2::Compress::new(bzip2::Compression::new(level), 30);
    let mut out = Vec::with_capacity(data.len() + data.len() / 50 + 1024);
    match c.compress_vec(data, &mut out, bzip2::Action::Finish) {
        Ok(bzip2::Status::StreamEnd) => out,
        other => panic!("reference encoder: {other:?}"),
    }
}

/// The format router (`recast-radar-io`): zip/gzip unwrapping, magic-byte
/// sniffing, and dispatch to every volume decoder.
pub fn io_router(data: &[u8]) -> bool {
    let _ = recast_radar_io::sniff_supported_volume_format(data);
    recast_radar_io::read_supported_volume_bytes(data).is_ok()
}

/// ODIM_H5 polar volumes and Cartesian composites over the pure-Rust HDF5
/// reader (`recast-radar-io-odim`).
pub fn odim(data: &[u8]) -> bool {
    let _ = odim_io::looks_like_hdf5_bytes(data);
    let polar = odim_io::read_odim_h5_volume(data).is_ok();
    let cartesian = odim_io::decode_odim_h5_cartesian_max(data).is_ok();
    polar || cartesian
}

/// CfRadial 1.x over the classic netCDF reader (`recast-radar-io-cfradial`).
pub fn cfradial(data: &[u8]) -> bool {
    let _ = cfradial_io::looks_like_netcdf3_bytes(data);
    cfradial_io::read_cfradial1_volume(data).is_ok()
}

/// DORADE sweepfiles (`recast-radar-io-dorade`): header peek, then a
/// single-sweep decode (even lengths) or a two-sweep volume built from the
/// same bytes (odd lengths).
pub fn dorade(data: &[u8]) -> bool {
    let _ = dorade_io::looks_like_dorade_bytes(data);
    let peek = dorade_io::peek_dorade_sweep(data).is_ok();
    let decoded = if data.len().is_multiple_of(2) {
        dorade_io::read_dorade_sweep_volume(data).is_ok()
    } else {
        dorade_io::read_dorade_volume_from_slices(&[data, data]).is_ok()
    };
    peek || decoded
}

/// JMA GRIB2 radar tars (`recast-radar-io-jma`): all stations, first
/// station, or the station catalog plus a site-filtered decode (mode =
/// input length mod 3).
pub fn jma(data: &[u8]) -> bool {
    let _ = jma_io::looks_like_jma_tar_bytes(data);
    match data.len() % 3 {
        0 => jma_io::read_jma_tar_volumes(data, None).is_ok(),
        1 => jma_io::read_jma_tar_first_station(data).is_ok(),
        _ => match jma_io::jma_tar_station_headers(data) {
            Ok(stations) => stations.last().is_some_and(|station| {
                let filter = format!("RS{}", station.number);
                jma_io::read_jma_tar_volumes(data, Some(&filter)).is_ok()
            }),
            Err(_) => false,
        },
    }
}
