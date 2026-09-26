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
    ("level2_writer", level2_writer),
    ("level2_writer_router", level2_writer_router),
    ("io_router", io_router),
    ("odim", odim),
    ("cfradial", cfradial),
    ("dorade", dorade),
    ("dorade_archive", dorade_archive),
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

/// The Level II writer (`recast-radar-io-nexrad::write`): the input decoded
/// as a Level II volume, written, and the written bytes decoded again. Mode
/// (input length mod 4): 0 without the source's metadata, LDM bzip2 records;
/// 1 with its metadata, metadata record and data records' non-radial
/// messages, uncompressed; 2 with them,
/// gzip-wrapped; 3 with them as real-time chunks, concatenated. Modes 2 and
/// 3 also drop gates before the radar and write site KTLX, so Message 1
/// volumes (blank ICAO, Doppler gates from -375 m) are written too.
///
/// A write may be refused (the refusal is typed and no bytes come out). A
/// write that succeeds must decode again with the sweeps and radials the
/// summary reports, the source's rays in their order (only rays without
/// data of a written moment left out, as `WriteSummary::written_rays`
/// lists), every written radial's time and angles, and every written
/// moment's codes, gates and absent rays as the source had them (the writer
/// copies NEXRAD codes; dropped gates are the leading ones the summary
/// reports). Anything else panics.
pub fn level2_writer(data: &[u8]) -> bool {
    use nexrad::write::{self, Compression, SourceMetadata, WriteOptions};

    let Ok(mut source) = nexrad::read_volume_with_metadata(data) else {
        return false;
    };
    let mode = data.len() % 4;
    if mode >= 2 && source.volume.location.latitude_deg.is_none() {
        // Message 1 volumes carry no site position, which the writer
        // refuses to invent: give KTLX's, the site named below.
        let location = &mut source.volume.location;
        location.latitude_deg = Some(35.3331);
        location.longitude_deg = Some(-97.2778);
        location.altitude_m = Some(390.0);
    }
    let record = nexrad::messages::metadata_record(data).ok();
    let messages = write::data_messages(data).unwrap_or_default();
    let with_source = SourceMetadata {
        metadata: Some(&source.metadata),
        metadata_record: record.as_deref(),
        data_messages: &messages,
    };
    let mut options = WriteOptions::default();
    if mode >= 2 {
        options.drop_negative_range_gates = true;
        options.icao = Some("KTLX".to_owned());
    }
    let written = match mode {
        0 => write::write_volume_with_source(&source.volume, SourceMetadata::default(), &options),
        1 => {
            options.compression = Compression::None;
            write::write_volume_with_source(&source.volume, with_source, &options)
        }
        2 => {
            options.gzip = true;
            write::write_volume_with_source(&source.volume, with_source, &options)
        }
        _ => write::realtime::write_realtime_chunks_with_source(
            &source.volume,
            with_source,
            &options,
        )
        .map(|chunked| (chunked.concatenated(), chunked.summary)),
    };
    let Ok((bytes, summary)) = written else {
        return false;
    };
    let again = match nexrad::read_volume_with_metadata(&bytes) {
        Ok(again) => again.volume,
        Err(error) => panic!("the written volume does not decode: {error}"),
    };
    let kept: Vec<usize> = (0..source.volume.sweeps.len())
        .filter(|index| !summary.skipped_sweeps.contains(index))
        .collect();
    assert_eq!(again.sweeps.len(), summary.sweeps, "sweeps");
    assert_eq!(kept.len(), summary.sweeps, "kept sweeps");
    let radials: usize = again.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    assert_eq!(radials, summary.radials, "radials");
    for (out, &index) in kept.iter().enumerate() {
        let (a, b) = (&source.volume.sweeps[index], &again.sweeps[out]);
        let order = written_order(&summary, index, a.nrays());
        // Level II sweeps keep their order; only rays without data go.
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "sweep {index}: order"
        );
        assert_eq!(order.len(), b.nrays(), "sweep {index}: rays");
        for ray in (0..a.nrays()).filter(|ray| order.binary_search(ray).is_err()) {
            assert!(
                summary
                    .moments
                    .iter()
                    .filter(|m| m.sweep == index)
                    .all(|m| a.field(&m.field).is_none_or(|f| f.is_absent(ray))),
                "sweep {index}: ray {ray} left out with data"
            );
        }
        for (radial, &ray) in order.iter().enumerate() {
            // Times since 1970: each volume's reference is its own earliest
            // ray, which moves when a ray or sweep without data is left out.
            assert_eq!(
                epoch_ms(&source.volume, a.rays.time_s[ray]),
                epoch_ms(&again, b.rays.time_s[radial]),
                "sweep {index} radial {radial}: time"
            );
            assert_eq!(
                a.rays.azimuth_deg[ray].to_bits(),
                b.rays.azimuth_deg[radial].to_bits(),
                "sweep {index} radial {radial}: azimuth"
            );
            assert_eq!(
                a.rays.elevation_deg[ray].to_bits(),
                b.rays.elevation_deg[radial].to_bits(),
                "sweep {index} radial {radial}: elevation"
            );
        }
    }
    for report in &summary.moments {
        let Some(out) = kept.iter().position(|&index| index == report.sweep) else {
            panic!("a report for unwritten sweep {}", report.sweep);
        };
        let a_sweep = &source.volume.sweeps[report.sweep];
        let b_sweep = &again.sweeps[out];
        let order = written_order(&summary, report.sweep, a_sweep.nrays());
        let (Some(a), Some(b)) = (a_sweep.field(&report.field), b_sweep.field(&report.field))
        else {
            panic!("sweep {}: {} not written back", report.sweep, report.field);
        };
        let skip = report.dropped_gates;
        assert_eq!(
            absent_in_order(a, &order),
            b.absent_rows,
            "sweep {}: {} absent rows",
            report.sweep,
            report.field
        );
        assert_eq!(
            a.ngates as usize,
            b.ngates as usize + skip,
            "sweep {}: {} gates",
            report.sweep,
            report.field
        );
        for (radial, &ray) in order.iter().enumerate() {
            let (Some(ca), Some(cb)) = (codes(a, ray), codes(b, radial)) else {
                continue;
            };
            assert!(
                ca.get(skip..) == Some(&cb[..]),
                "sweep {}: {} ray {ray} codes differ",
                report.sweep,
                report.field
            );
        }
    }
    true
}

/// The Level II writer on every format the router reads
/// (`recast-radar-io`): the input decoded by `read_supported_volume_bytes`
/// (ODIM_H5, CfRadial, DORADE, JMA, Level II), written as Level II and
/// decoded again, so the quantiser (grid, decimal and covering codings,
/// code tables and computed codes), the gate geometry of foreign ranges
/// (uniform, refined and explicit), the site identifier derivation and the
/// field mapping all see fuzzed volumes. Mode (input length mod 8): the
/// policy is Precise (0 and 3), Compatible (1) or Standard (2) by length
/// mod 4; lengths
/// with `(len / 4) % 2 == 1` also drop gates before the radar, accept any
/// range rounding, supply a Nyquist velocity and unambiguous range where the
/// source has none, and write through the real-time chunker.
///
/// A write may be refused (typed, no bytes). A write that succeeds must
/// decode again with the sweeps and radials the summary reports, each
/// written radial the source ray `WriteSummary::written_rays` names (every
/// ray at most once, a ray left out only when no written moment has data
/// on it) with its time (to the millisecond) and angles (bit for bit), and
/// every written moment's gate count, first gate and spacing (to the
/// metre), absent rays and values: each within the moment's reported
/// `max_abs_error` of the source value, sentinels as sentinels. Anything
/// else panics.
pub fn level2_writer_router(data: &[u8]) -> bool {
    use nexrad::write::{self, Quantization, SourceMetadata, WriteOptions};
    use recast_radar_core::model::{FieldName, Gate};

    let Ok(source) = recast_radar_io::read_supported_volume_bytes(data) else {
        return false;
    };
    let mut options = WriteOptions::default();
    options.quantization = match data.len() % 4 {
        1 => Quantization::Compatible,
        2 => Quantization::Standard,
        _ => Quantization::Precise,
    };
    let loose = (data.len() / 4) % 2 == 1;
    if loose {
        options.drop_negative_range_gates = true;
        options.max_range_error_m = Some(1e9);
        options.nyquist_velocity_mps = Some(26.48);
        options.unambiguous_range_m = Some(150_000.0);
    }
    let written = if loose {
        write::realtime::write_realtime_chunks(&source, &options)
            .map(|chunked| (chunked.concatenated(), chunked.summary))
    } else {
        write::write_volume_with_source(&source, SourceMetadata::default(), &options)
    };
    let Ok((bytes, summary)) = written else {
        return false;
    };
    assert_eq!(summary.bytes, bytes.len(), "summary bytes");
    assert!(
        summary.icao.len() == 4
            && summary
                .icao
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "site {:?}",
        summary.icao
    );
    let again = match nexrad::read_volume_from_bytes(&bytes) {
        Ok(again) => again,
        Err(error) => panic!("the written volume does not decode: {error}"),
    };
    let kept: Vec<usize> = (0..source.sweeps.len())
        .filter(|index| !summary.skipped_sweeps.contains(index))
        .collect();
    assert_eq!(again.sweeps.len(), summary.sweeps, "sweeps");
    assert_eq!(kept.len(), summary.sweeps, "kept sweeps");
    let radials: usize = again.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    assert_eq!(radials, summary.radials, "radials");
    for (out, &index) in kept.iter().enumerate() {
        let (a, b) = (&source.sweeps[index], &again.sweeps[out]);
        let order = written_order(&summary, index, a.nrays());
        assert_eq!(order.len(), b.nrays(), "sweep {index}: rays");
        let mut seen = vec![false; a.nrays()];
        for (radial, &ray) in order.iter().enumerate() {
            assert!(
                !std::mem::replace(&mut seen[ray], true),
                "sweep {index}: ray {ray} twice"
            );
            assert_eq!(
                a.rays.azimuth_deg[ray].to_bits(),
                b.rays.azimuth_deg[radial].to_bits(),
                "sweep {index} radial {radial}: azimuth"
            );
            assert_eq!(
                a.rays.elevation_deg[ray].to_bits(),
                b.rays.elevation_deg[radial].to_bits(),
                "sweep {index} radial {radial}: elevation"
            );
            assert_eq!(
                epoch_ms(&source, a.rays.time_s[ray]),
                epoch_ms(&again, b.rays.time_s[radial]),
                "sweep {index} radial {radial}: time"
            );
        }
        // Rays left out have no data of any written moment.
        for (ray, _) in seen.iter().enumerate().filter(|(_, seen)| !**seen) {
            assert!(
                summary
                    .moments
                    .iter()
                    .filter(|m| m.sweep == index)
                    .all(|m| a.field(&m.field).is_none_or(|f| f.is_absent(ray))),
                "sweep {index}: ray {ray} left out with data"
            );
        }
    }
    for report in &summary.moments {
        let Some(out) = kept.iter().position(|&index| index == report.sweep) else {
            panic!("a report for unwritten sweep {}", report.sweep);
        };
        let (a_sweep, b_sweep) = (&source.sweeps[report.sweep], &again.sweeps[out]);
        let order = written_order(&summary, report.sweep, a_sweep.nrays());
        let at = format!(
            "sweep {} {} <- {}",
            report.sweep, report.moment, report.field
        );
        let Some(a) = a_sweep.field(&report.field) else {
            panic!("{at}: no source field");
        };
        let name = FieldName::from_nexrad_block(report.moment.name().as_bytes());
        let Some(b) = b_sweep.field(&name) else {
            panic!("{at}: not written back");
        };
        let skip = report.dropped_gates;
        assert_eq!(
            a.ngates as usize,
            b.ngates as usize + skip,
            "{at}: gate count"
        );
        assert_eq!(
            absent_in_order(a, &order),
            b.absent_rows,
            "{at}: absent rows"
        );
        let (Some((first, spacing)), Some((first_back, spacing_back))) = (
            a.native_geometry(&a_sweep.range),
            b.native_geometry(&b_sweep.range),
        ) else {
            panic!("{at}: geometry");
        };
        assert_eq!(
            (first + skip as f64 * spacing).round(),
            first_back,
            "{at}: first gate"
        );
        if b.ngates > 1 {
            assert_eq!(spacing.round(), spacing_back, "{at}: gate spacing");
        }
        for (radial, &ray) in order.iter().enumerate() {
            for gate in 0..b.ngates as usize {
                let x = a.gate(ray, gate + skip).unwrap_or(Gate::Missing);
                let y = b.gate(radial, gate).unwrap_or(Gate::Missing);
                match (x, y) {
                    (Gate::Value(x), Gate::Value(y)) if x.is_finite() => {
                        let allowed = report.max_abs_error + 1e-5 * x.abs().max(1.0);
                        assert!(
                            (x - y).abs() <= allowed,
                            "{at}: ray {ray} gate {gate}: {x} came back as {y} \
                             (reported error {})",
                            report.max_abs_error
                        );
                    }
                    (
                        Gate::Value(_) | Gate::Missing | Gate::Undetect,
                        Gate::Missing | Gate::Undetect,
                    ) if !x.value().is_some_and(f32::is_finite) => {}
                    // NEXRAD codes copied as they are, under a scale that
                    // decodes them to non-finite values.
                    (Gate::Value(x), Gate::Value(y)) if !x.is_finite() && !y.is_finite() => {}
                    (Gate::RangeFolded, Gate::RangeFolded) => {}
                    (x, y) => panic!("{at}: ray {ray} gate {gate}: {x:?} came back as {y:?}"),
                }
            }
        }
    }
    true
}

/// A ray time (`time_s` after the volume's reference) in milliseconds since
/// 1970, as Level II stores it.
fn epoch_ms(volume: &recast_radar_core::model::Volume, time_s: f64) -> i64 {
    volume.time_reference.timestamp_millis() + (time_s * 1000.0).round() as i64
}

/// The source ray of each written radial of source sweep `sweep`
/// (`WriteSummary::written_rays`, else the rays in storage order).
fn written_order(summary: &nexrad::write::WriteSummary, sweep: usize, nrays: usize) -> Vec<usize> {
    summary
        .written_rays
        .iter()
        .find(|rays| rays.sweep == sweep)
        .map_or_else(|| (0..nrays).collect(), |rays| rays.rays.clone())
}

/// The written radials whose source ray `field` does not provide.
fn absent_in_order(field: &recast_radar_core::model::Field, order: &[usize]) -> Vec<u32> {
    order
        .iter()
        .enumerate()
        .filter(|(_, ray)| field.is_absent(**ray))
        .map(|(radial, _)| radial as u32)
        .collect()
}

/// One row's codes as `u16`, for 8- and 16-bit fields (Level II moments).
fn codes(field: &recast_radar_core::model::Field, ray: usize) -> Option<Vec<u16>> {
    use recast_radar_core::model::RowRef;
    match field.row(ray)? {
        RowRef::U8(values) => Some(values.iter().map(|v| u16::from(*v)).collect()),
        RowRef::U16(values) => Some(values.to_vec()),
        _ => None,
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

/// Mobile-radar zip archives (`recast-radar-io-dorade`):
/// `read_mobile_archive_from_bytes`, which inflates the radar members,
/// groups DORADE sweepfiles into volume runs and decodes them, and hands
/// Level II members to `recast-radar-io-nexrad`.
pub fn dorade_archive(data: &[u8]) -> bool {
    let _ = dorade_io::looks_like_zip_bytes(data);
    dorade_io::read_mobile_archive_from_bytes(data, "fuzz input", nexrad::read_volume_from_bytes)
        .is_ok()
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
