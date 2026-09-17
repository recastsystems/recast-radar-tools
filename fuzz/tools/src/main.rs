//! `fuzz-tools`: seed corpora and stable replay for the recast-radar fuzz
//! targets.
//!
//! ```text
//! fuzz-tools seeds [OUT_DIR]          write seeds/<target>/ from the testdata manifest
//! fuzz-tools replay <target> <path>.. run inputs (files or directories) through a harness
//! fuzz-tools regressions              replay the testdata entries tagged fuzz-regression
//! ```
//!
//! Seeds are real test files resolved by manifest id through
//! `recast-radar-testdata` (committed fixtures, or sha256-verified downloads
//! cached on first use), copied verbatim or cut down by the derivations in
//! [`Derivation`]. No seed byte is synthesized: derived seeds are record-,
//! block- or chunk-aligned selections and concatenations of real bytes.

use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use recast_radar_fuzz::{Harness, TARGETS, harness};

/// Archive II volume header length.
const L2_VOLUME_HEADER_LEN: usize = 24;
/// Archive II record control word preceding each message header.
const L2_CONTROL_WORD_LEN: usize = 12;
/// Archive II message header length.
const L2_MESSAGE_HEADER_LEN: usize = 16;
/// Fixed Archive II record length (everything except message 31 after the
/// metadata records).
const L2_RECORD_BYTES: usize = 2432;
/// Fixed metadata records at the start of a modern Archive II volume.
const L2_METADATA_RECORDS: usize = 134;

/// How a seed is cut from its real source file(s).
#[derive(Clone, Copy, Debug)]
enum Derivation {
    /// The file, byte for byte.
    Verbatim,
    /// Level II, decompressed to the uncompressed record stream: the volume
    /// header, every metadata record except empty or type-0 records and the
    /// clutter-filter/bypass maps (messages 13 and 15), the first radial
    /// messages (up to 64 KiB) and the last radial messages of the volume (up
    /// to 16 KiB, including the end-of-volume radial). Whole records only.
    L2Sparse,
    /// Level II, decompressed: the volume header, all records before the
    /// first radial message, and the first radial messages (up to 32 KiB) —
    /// the prefix a partial download yields.
    L2Head,
    /// LDM block-bzip2 Level II as published: the volume header and the
    /// leading whole bzip2 blocks, as many as fit in 256 KiB (at least one).
    L2BlockHead,
    /// This file followed by the real-time chunk(s) named here, as a
    /// real-time client assembles a volume.
    ConcatChunks(&'static [&'static str]),
    /// DORADE sweepfile: every descriptor block and the first `n` ray groups
    /// (RYIB/ASIB/RDAT...), cut at a block boundary.
    DoradeHead(usize),
}

impl Derivation {
    fn suffix(self) -> String {
        match self {
            Self::Verbatim => String::new(),
            Self::L2Sparse => ".l2-sparse".to_owned(),
            Self::L2Head => ".l2-head".to_owned(),
            Self::L2BlockHead => ".l2-block-head".to_owned(),
            Self::ConcatChunks(ids) => format!(".plus-{}-chunks", ids.len()),
            Self::DoradeHead(rays) => format!(".head{rays}"),
        }
    }
}

/// One seed: target, testdata manifest id, derivation.
type Seed = (&'static str, &'static str, Derivation);

use Derivation::{ConcatChunks, DoradeHead, L2BlockHead, L2Head, L2Sparse, Verbatim};

const KIWA_CHUNK_S: &str = "l2chunk-kiwa-307-20260917-003629-001-s";
const KIWA_CHUNK_I2: &str = "l2chunk-kiwa-307-20260917-003629-002-i";

const SEEDS: &[Seed] = &[
    // level2_volume: small real files as published (LDM bzip2 status-only
    // stub, truncated gzip message 1 volume, whole gzip AR2V0001 volume,
    // real-time start chunk, a headerless intermediate chunk, and the two
    // chunks assembled), plus record- and block-aligned cuts of larger
    // volumes. The L2Sparse set covers every archive header generation
    // (ARCHIVE2 1991/1999/2003, AR2V0001 with message 1 and the mislabelled
    // message 31, AR2V0004/6/8), message 1 vs 31, VOL block 44/52, RAD block
    // 20/28, 8- and 16-bit ZDR, CFP, SAILS/MESO-SAILS, TDWR, message 32, and
    // RDA builds 10 through 24.
    ("level2_volume", "l2-tbwi-20230601-175101-stub", Verbatim),
    ("level2_volume", "l2-ktlx-19990503-230052", Verbatim),
    ("level2_volume", "l2-kvwx-20080415-235337", Verbatim),
    ("level2_volume", KIWA_CHUNK_S, Verbatim),
    ("level2_volume", KIWA_CHUNK_I2, Verbatim),
    (
        "level2_volume",
        KIWA_CHUNK_S,
        ConcatChunks(&[KIWA_CHUNK_I2]),
    ),
    ("level2_volume", "l2-kbox-20220129-150537", L2BlockHead),
    ("level2_volume", "l2-klix-20050829-130035", L2Head),
    ("level2_volume", "l2-kpah-20080415-235014", L2Head),
    ("level2_volume", "l2-kiwa-20260917-003629", L2Head),
    ("level2_volume", "l2-ktlx-19910605-162126", L2Sparse),
    ("level2_volume", "l2-ktlx-19990503-230052", L2Sparse),
    ("level2_volume", "l2-ktlx-20030508-221041", L2Sparse),
    ("level2_volume", "l2-klix-20050829-130035", L2Sparse),
    ("level2_volume", "l2-kvwx-20080415-235337", L2Sparse),
    ("level2_volume", "l2-kpah-20080415-235014", L2Sparse),
    ("level2_volume", "l2-kvnx-20110315-000203", L2Sparse),
    ("level2_volume", "l2-koax-20140616-205305", L2Sparse),
    ("level2_volume", "l2-klix-20210829-180425", L2Sparse),
    ("level2_volume", "l2-kbox-20220129-150537", L2Sparse),
    ("level2_volume", "l2-tstl-20230331-230314", L2Sparse),
    ("level2_volume", "l2-pahg-20250909-212549", L2Sparse),
    ("level2_volume", "l2-kiwa-20260917-003629", L2Sparse),
    // io_router: one small real file per routed format, plus gzip and
    // block-bzip Level II.
    ("io_router", "l2-tbwi-20230601-175101-stub", Verbatim),
    ("io_router", "l2-ktlx-19990503-230052", Verbatim),
    ("io_router", KIWA_CHUNK_S, Verbatim),
    ("io_router", "l2-kvnx-20110315-000203", L2Sparse),
    ("io_router", "odim-imgw-ram-20260711-0015-kdp-max", Verbatim),
    (
        "io_router",
        "odim-espdg-20260707-1927-pvol-dbzh-vradh",
        Verbatim,
    ),
    (
        "io_router",
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        Verbatim,
    ),
    (
        "io_router",
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
        Verbatim,
    ),
    (
        "io_router",
        "dorade-cow2-20260521-225514-sur-head24",
        Verbatim,
    ),
    ("io_router", "jma-n6-20191012-090000-rs47773", Verbatim),
    // odim: every committed ODIM_H5 file (superblock v0/v1, v2 object
    // headers, vlen strings, float64, Cartesian composites) plus the
    // netCDF-4 (HDF5) CfRadial file.
    ("odim", "odim-bejab-20190606-0000-pvol", Verbatim),
    ("odim", "odim-bewid-20130429-0430-pvol-dbzh-scan1", Verbatim),
    ("odim", "odim-norst-20170421-0908-pvol", Verbatim),
    ("odim", "odim-espdg-20260707-1927-pvol-dbzh-vradh", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-kdp-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-phidp-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-rhohv-max", Verbatim),
    ("odim", "odim-imgw-ram-20260711-0015-zdr-max", Verbatim),
    ("odim", "odim-iesha-20260305-0115-pvol", Verbatim),
    ("odim", "odim-dkrom-20260820-1130-pvol", Verbatim),
    ("odim", "cfrad1-xsapr-sgp-20110520-ppi-netcdf4", Verbatim),
    // cfradial: every committed CfRadial 1 file.
    (
        "cfradial",
        "cfrad1-xsapr-sgp-20110520-ppi-classic",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        Verbatim,
    ),
    (
        "cfradial",
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        Verbatim,
    ),
    // dorade: the committed sweepfiles, head-trimmed to a few rays
    // (little/big endian, uncompressed/HRD RLE, CSFD/CELV, PPI/RHI/SUR),
    // plus one complete sweepfile for the end-of-sweep path.
    ("dorade", "dorade-cow2-20260521-225514-sur-head24", Verbatim),
    ("dorade", "dorade-noxp-20090501-190244-ppi", DoradeHead(8)),
    (
        "dorade",
        "dorade-noxp-20090525-203211-sector",
        DoradeHead(8),
    ),
    (
        "dorade",
        "dorade-dow6-20211230-222139-rhi-head41",
        DoradeHead(4),
    ),
    ("dorade", "dorade-noxp-20090501-190324-ppi", Verbatim),
    // jma: both committed single-station tars.
    ("jma", "jma-n6-20191012-090000-rs47773", Verbatim),
    ("jma", "jma-n5-20191012-090000-rs47773", Verbatim),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("seeds") => write_seeds(
            args.get(1)
                .map_or_else(|| fuzz_dir().join("seeds"), PathBuf::from),
        ),
        Some("replay") if args.len() >= 3 => replay(&args[1], args[2..].iter().map(PathBuf::from)),
        Some("regressions") => replay_regressions(),
        _ => {
            eprintln!(
                "usage: fuzz-tools seeds [OUT_DIR]\n       fuzz-tools replay <target> <file-or-dir>...\n       fuzz-tools regressions\ntargets: {}",
                TARGETS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from(".."), Path::to_path_buf)
}

fn other_error(message: String) -> io::Error {
    io::Error::other(message)
}

fn testdata_bytes(id: &str) -> io::Result<Vec<u8>> {
    recast_radar_testdata::bytes(id).map_err(|err| other_error(format!("testdata `{id}`: {err}")))
}

fn write_seeds(out: PathBuf) -> io::Result<bool> {
    let mut all_written = true;
    for &(target, id, derivation) in SEEDS {
        let name = format!("{id}{}", derivation.suffix());
        let bytes = match derive(id, derivation) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("SKIP {target:<14} {name}: {err}");
                all_written = false;
                continue;
            }
        };
        let dir = out.join(target);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(&name), &bytes)?;
        println!("{target:<14} {:>9}  {name}", bytes.len());
    }
    Ok(all_written)
}

fn derive(id: &str, derivation: Derivation) -> io::Result<Vec<u8>> {
    let source = testdata_bytes(id)?;
    match derivation {
        Derivation::Verbatim => Ok(source),
        Derivation::L2Sparse => {
            let normalized = normalize_l2(&source)?;
            l2_sparse(&normalized)
        }
        Derivation::L2Head => {
            let normalized = normalize_l2(&source)?;
            l2_head(&normalized)
        }
        Derivation::L2BlockHead => l2_block_head(&source),
        Derivation::ConcatChunks(more) => {
            let mut bytes = source;
            for id in more {
                bytes.extend_from_slice(&testdata_bytes(id)?);
            }
            Ok(bytes)
        }
        Derivation::DoradeHead(rays) => dorade_head(&source, rays),
    }
}

fn normalize_l2(source: &[u8]) -> io::Result<Vec<u8>> {
    recast_radar_io_nexrad::normalize_archive_bytes(source)
        .map(|(bytes, _)| bytes)
        .map_err(|err| other_error(format!("cannot decompress Level II: {err}")))
}

/// One Archive II record in an uncompressed record stream.
#[derive(Clone, Copy, Debug)]
struct L2Record {
    start: usize,
    end: usize,
    message_type: u8,
    size_halfwords: u16,
}

impl L2Record {
    fn is_radial(&self) -> bool {
        matches!(self.message_type, 1 | 31)
    }

    fn len(&self) -> usize {
        self.end - self.start
    }
}

/// Split an uncompressed Archive II stream into records with the decoder's
/// framing: fixed 2432-byte records, except message 31 after the metadata
/// records (or packed back to back from the start), which is
/// control word + message length.
fn l2_records(bytes: &[u8]) -> Vec<L2Record> {
    let mut records = Vec::new();
    let mut cursor = L2_VOLUME_HEADER_LEN;
    let mut variable_msg31 = false;
    while let Some(header) = bytes
        .get(cursor + L2_CONTROL_WORD_LEN..cursor + L2_CONTROL_WORD_LEN + L2_MESSAGE_HEADER_LEN)
    {
        let size_halfwords = u16::from_be_bytes([header[0], header[1]]);
        let message_type = header[3];
        if size_halfwords == 0 && records.len() >= L2_METADATA_RECORDS {
            break;
        }
        let variable_len = L2_CONTROL_WORD_LEN + usize::from(size_halfwords) * 2;
        let len = if message_type == 31
            && (variable_msg31 || records.len() >= L2_METADATA_RECORDS || {
                let next = cursor + variable_len;
                next == bytes.len()
                    || bytes.get(next + L2_CONTROL_WORD_LEN + 3).copied() == Some(31)
            }) {
            variable_msg31 = true;
            variable_len
        } else {
            L2_RECORD_BYTES
        };
        let end = (cursor + len).min(bytes.len());
        records.push(L2Record {
            start: cursor,
            end,
            message_type,
            size_halfwords,
        });
        cursor = end;
    }
    records
}

fn take_until(records: &[L2Record], budget: usize) -> usize {
    let mut used = 0;
    let mut count = 0;
    for record in records {
        if count > 0 && used + record.len() > budget {
            break;
        }
        used += record.len();
        count += 1;
    }
    count
}

fn l2_sparse(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let records = l2_records(bytes);
    let radials: Vec<L2Record> = records
        .iter()
        .copied()
        .filter(L2Record::is_radial)
        .collect();
    if radials.is_empty() {
        return Err(other_error("no radial messages".to_owned()));
    }
    let first = take_until(&radials, 64 * 1024);
    let reversed: Vec<L2Record> = radials[first..].iter().rev().copied().collect();
    let last = take_until(&reversed, 16 * 1024);

    let mut out = bytes[..L2_VOLUME_HEADER_LEN].to_vec();
    let metadata = records.iter().filter(|record| {
        !record.is_radial()
            && record.size_halfwords != 0
            && !matches!(record.message_type, 0 | 13 | 15)
    });
    let tail = radials.len() - last..radials.len();
    for record in metadata.chain(&radials[..first]).chain(&radials[tail]) {
        out.extend_from_slice(&bytes[record.start..record.end]);
    }
    Ok(out)
}

fn l2_head(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let records = l2_records(bytes);
    let Some(first_radial) = records.iter().position(L2Record::is_radial) else {
        return Err(other_error("no radial messages".to_owned()));
    };
    let radials = take_until(&records[first_radial..], 32 * 1024);
    let end = records[first_radial + radials - 1].end;
    Ok(bytes[..end].to_vec())
}

fn l2_block_head(bytes: &[u8]) -> io::Result<Vec<u8>> {
    const BUDGET: usize = 256 * 1024;
    let mut cursor = L2_VOLUME_HEADER_LEN;
    let mut end = None;
    while let Some(word) = bytes.get(cursor..cursor + 4) {
        let size = i32::from_be_bytes([word[0], word[1], word[2], word[3]]).unsigned_abs() as usize;
        let block_end = cursor + 4 + size;
        if size == 0 || block_end > bytes.len() || (end.is_some() && block_end > BUDGET) {
            break;
        }
        end = Some(block_end);
        cursor = block_end;
    }
    end.map(|end| bytes[..end].to_vec())
        .ok_or_else(|| other_error("no whole bzip2 block after the volume header".to_owned()))
}

fn dorade_head(bytes: &[u8], rays: usize) -> io::Result<Vec<u8>> {
    let length_at = |offset: usize, big_endian: bool| -> Option<usize> {
        let word: [u8; 4] = bytes.get(offset + 4..offset + 8)?.try_into().ok()?;
        let len = if big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        };
        usize::try_from(len).ok()
    };
    let big_endian = match (length_at(0, false), length_at(0, true)) {
        (Some(le), _) if (8..=bytes.len()).contains(&le) => false,
        (_, Some(be)) if (8..=bytes.len()).contains(&be) => true,
        _ => return Err(other_error("not a DORADE descriptor block".to_owned())),
    };
    let mut cursor = 0;
    let mut ray_groups = 0;
    while let Some(len) = length_at(cursor, big_endian) {
        if &bytes[cursor..cursor + 4] == b"RYIB" {
            if ray_groups == rays {
                return Ok(bytes[..cursor].to_vec());
            }
            ray_groups += 1;
        }
        if len < 8 || cursor + len > bytes.len() {
            break;
        }
        cursor += len;
    }
    Err(other_error(format!(
        "sweepfile has {ray_groups} ray groups before its end, wanted more than {rays}"
    )))
}

fn replay(target: &str, paths: impl Iterator<Item = PathBuf>) -> io::Result<bool> {
    let harness =
        harness(target).ok_or_else(|| other_error(format!("unknown target `{target}`")))?;
    let mut files = Vec::new();
    for path in paths {
        collect_files(&path, &mut files)?;
    }
    replay_files(target, harness, &files)
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        entries.sort();
        for entry in entries {
            collect_files(&entry, files)?;
        }
    } else {
        files.push(path.to_path_buf());
    }
    Ok(())
}

fn replay_files(target: &str, harness: Harness, files: &[PathBuf]) -> io::Result<bool> {
    let mut clean = true;
    for file in files {
        let data = fs::read(file)?;
        let started = Instant::now();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| harness(&data)));
        let millis = started.elapsed().as_secs_f64() * 1e3;
        let status = match outcome {
            Ok(true) => "decoded",
            Ok(false) => "rejected",
            Err(_) => "PANIC",
        };
        clean &= outcome.is_ok();
        println!(
            "{target:<14} {status:<8} {millis:>9.1} ms {:>9} B  {}",
            data.len(),
            file.display()
        );
    }
    Ok(clean)
}

/// Tag on every fuzz regression input in the testdata manifest
/// (`testdata/fuzz/manifest.toml`).
const REGRESSION_TAG: &str = "fuzz-regression";
/// Tag prefix naming the harness that found a regression input.
const TARGET_TAG_PREFIX: &str = "fuzz-target:";

/// Replay every testdata entry tagged `fuzz-regression` through the harness
/// named by its `fuzz-target:` tag, then through `io_router`, which routes
/// every format.
fn replay_regressions() -> io::Result<bool> {
    let ids = recast_radar_testdata::ids_with_tag(REGRESSION_TAG);
    if ids.is_empty() {
        return Err(other_error(format!(
            "no testdata entries tagged `{REGRESSION_TAG}`"
        )));
    }
    let mut clean = true;
    for id in ids {
        let entry = recast_radar_testdata::entry(id)
            .ok_or_else(|| other_error(format!("testdata `{id}`: no manifest entry")))?;
        let mut targets: Vec<&str> = entry
            .tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(TARGET_TAG_PREFIX))
            .collect();
        if targets.is_empty() {
            return Err(other_error(format!(
                "testdata `{id}`: no `{TARGET_TAG_PREFIX}<target>` tag"
            )));
        }
        if !targets.contains(&"io_router") {
            targets.push("io_router");
        }
        let path = recast_radar_testdata::path(id)
            .map_err(|err| other_error(format!("testdata `{id}`: {err}")))?;
        for target in targets {
            let harness = harness(target).ok_or_else(|| {
                other_error(format!("testdata `{id}`: unknown target `{target}`"))
            })?;
            clean &= replay_files(target, harness, std::slice::from_ref(&path))?;
        }
    }
    Ok(clean)
}
