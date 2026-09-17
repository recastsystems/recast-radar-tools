//! Zip-archive ingest for mobile research radar data (DOW/COW/RaXPol).
//!
//! Field deployments are distributed as `.zip` files holding DORADE
//! sweepfiles (`swp.*`) and/or GR2-style `.msg31` Level II twins, often for
//! several radars in one archive (e.g. a Goodland deployment zip carries
//! `DORADE/DOW7/...` next to `DORADE/COW2/...`). This module discovers radar
//! members, groups DORADE sweeps into volume scans, and decodes everything
//! into FM301 [`recast_radar_core::model::Volume`]s.
//!
//! Lift-and-improve of `gurt-rs/src/archive.rs`. Divergences:
//! - **Volume grouping**: the reference treated every archive member as its
//!   own single-sweep "volume". Here DORADE sweeps are grouped per
//!   instrument into ascending fixed-angle runs ordered by sweep start
//!   time, the way a VCP executes: a 2009 Goshen DOW7 volume spread across
//!   `Tilt 0.5/ ... Tilt 4.0/` member directories reassembles into one
//!   five-cut volume scan, while a COW2 single-tilt 12-second surveillance
//!   sequence becomes one frame per sweep instead of a 24-cut blob. VOLD
//!   volume numbers are deliberately NOT the key: the corpus shows they are
//!   writer-dependent (Goshen DOW7 increments per sweep, COW2 per volume
//!   scan), so elevation-run segmentation is the only convention that holds
//!   across radars. A new run also starts after a 15-minute gap
//!   (deployment pause).
//! - **Member classification by content**: members are sniffed (DORADE
//!   descriptor magic, `AR2V` volume header, gzip/bzip2 wrappers) rather
//!   than trusted by extension alone; naming is only a pre-filter.
//! - **Parallel decode**: members decode on the rayon pool.
//! - **Level II members**: this crate does not decode Archive II itself;
//!   callers pass the Level II decoder (normally
//!   `recast_radar_io_nexrad::read_volume_from_bytes`), and
//!   `recast_radar_io` provides wrappers that do so.
//!
//! Sibling-directory grouping for loose (non-zip) sweepfiles lives here too:
//! opening one `swp.*` file pulls in the rest of its ascending run from the
//! same directory.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use recast_radar_core::bounded_read::{
    MAX_DECODED_BATCH_BYTES, read_to_end_limited, volume_field_capacity_bytes,
};
use recast_radar_core::model::Volume;
use zip::ZipArchive;

use crate::dorade::{
    DoradeVolumeBuilder, looks_like_dorade_bytes, looks_like_dorade_name, peek_dorade_sweep,
};
use crate::{DoradeError, Result};

const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";
/// Empty-archive variant of the zip magic ("PK\x05\x06") is not radar data.
const VOLUME_HEADER_MAGIC: &[u8; 4] = b"AR2V";
/// Mobile deployments can contain hundreds of real sweeps, but retaining an
/// unbounded archive before parallel decode lets a zip bomb or accidental
/// multi-day tree exhaust memory. These ceilings are deliberately much
/// larger than one operational volume while keeping peak input retention
/// finite and diagnosable.
const MAX_MOBILE_MEMBER_BYTES: usize = 256 * 1024 * 1024;
const MAX_MOBILE_ARCHIVE_BYTES: usize = 1024 * 1024 * 1024;
const MAX_MOBILE_MEMBERS: usize = 4096;

/// `true` when the buffer starts with a local-file zip signature.
pub fn looks_like_zip_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == ZIP_MAGIC
}

/// `true` when the path claims to be a zip archive.
pub fn looks_like_zip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// One decoded volume scan plus where it came from inside the archive.
#[derive(Clone, Debug)]
pub struct MobileVolume {
    pub volume: Volume,
    /// Display label: first member name of the group (`swp....` or `*.msg31`).
    pub member_label: String,
    /// Number of archive members merged into this volume.
    pub member_count: usize,
}

/// Decode one DORADE volume run from its sweepfile bytes, in order.
fn decode_dorade_run(sweeps: &[&[u8]]) -> Result<Volume> {
    let mut builder = DoradeVolumeBuilder::new();
    for sweep in sweeps {
        builder.append(sweep)?;
    }
    builder.finish()
}

/// Bytes a decoded volume retains (fields and ray tables).
fn retained_bytes(volume: &Volume) -> usize {
    let rays: usize = volume.sweeps.iter().map(|sweep| sweep.nrays()).sum();
    volume_field_capacity_bytes(volume).saturating_add(rays.saturating_mul(3 * size_of::<f64>()))
}

/// Read one sweepfile of a volume run (bounded by the member size limit).
pub(crate) fn read_member_file(path: &Path) -> Result<Vec<u8>> {
    read_file_limited(path, MAX_MOBILE_MEMBER_BYTES)
}

/// Decode every radar volume in a zip archive, sorted by scan time.
///
/// DORADE members group per instrument into ascending fixed-angle runs (see
/// module docs); `.msg31`/`AR2V` members decode one volume each through
/// `decode_level2`. Non-radar members are ignored; corrupt members fail the
/// whole load with a descriptive error (a deployment archive with
/// undecodable scans should be visible, not silently thinner).
pub fn read_mobile_archive_from_path<F, E>(
    path: &Path,
    decode_level2: F,
) -> Result<Vec<MobileVolume>>
where
    F: Fn(&[u8]) -> std::result::Result<Volume, E> + Sync,
    E: Display,
{
    let members = read_radar_members(path)?;
    if members.is_empty() {
        return Err(DoradeError::InvalidMessage {
            offset: 0,
            reason: format!(
                "zip archive {} contains no radar members (swp.* or .msg31/AR2V)",
                path.display()
            ),
        });
    }
    decode_members(path, members, &decode_level2, MAX_DECODED_BATCH_BYTES)
}

/// Decode every radar volume under a deployment FOLDER (recursive, a few
/// levels). Research data ships as directories of per-sweep DORADE files
/// — one file per tilt — so the folder, not the file, is the natural
/// open unit (field report). Same sniffing and volume grouping as zips;
/// Level II members decode through `decode_level2`.
pub fn read_mobile_dir_from_path<F, E>(dir: &Path, decode_level2: F) -> Result<Vec<MobileVolume>>
where
    F: Fn(&[u8]) -> std::result::Result<Volume, E> + Sync,
    E: Display,
{
    let mut members = Vec::new();
    let mut budget = MemberBudget::default();
    collect_dir_members(dir, dir, &mut members, &mut budget, 0)?;
    if members.is_empty() {
        return Err(DoradeError::InvalidMessage {
            offset: 0,
            reason: format!(
                "folder {} contains no radar files (swp.* sweepfiles or .msg31/AR2V)",
                dir.display()
            ),
        });
    }
    members.sort_by(|left, right| left.name.cmp(&right.name));
    decode_members(dir, members, &decode_level2, MAX_DECODED_BATCH_BYTES)
}

/// Deployment trees are shallow (day/instrument levels); the cap only
/// guards against scanning an accidentally-chosen huge root.
const MAX_DIR_DEPTH: usize = 4;

fn collect_dir_members(
    root: &Path,
    dir: &Path,
    members: &mut Vec<RadarMember>,
    budget: &mut MemberBudget,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DIR_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|source| DoradeError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_members(root, &path, members, budget, depth + 1)?;
            continue;
        }
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if !plausible_radar_member_name(&name) {
            continue;
        }
        let declared = file_len_usize(&path)?;
        budget.reserve(&name, declared)?;
        let bytes = read_file_limited(&path, MAX_MOBILE_MEMBER_BYTES)?;
        if looks_like_dorade_bytes(&bytes)
            || bytes.starts_with(VOLUME_HEADER_MAGIC)
            || bytes.starts_with(&[0x1f, 0x8b])
            || bytes.starts_with(b"BZh")
        {
            members.push(RadarMember { name, bytes });
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RadarMember {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct MemberBudget {
    candidates: usize,
    expanded_bytes: usize,
}

impl MemberBudget {
    fn reserve(&mut self, name: &str, bytes: usize) -> Result<()> {
        if bytes > MAX_MOBILE_MEMBER_BYTES {
            return Err(invalid_archive(format!(
                "archive member {name} declares {bytes} bytes (per-member limit {MAX_MOBILE_MEMBER_BYTES})"
            )));
        }
        let candidates = self
            .candidates
            .checked_add(1)
            .ok_or_else(|| invalid_archive("mobile archive member count overflow".to_owned()))?;
        if candidates > MAX_MOBILE_MEMBERS {
            return Err(invalid_archive(format!(
                "mobile archive contains more than {MAX_MOBILE_MEMBERS} candidate radar members"
            )));
        }
        let expanded_bytes = self
            .expanded_bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid_archive("mobile archive expanded-size overflow".to_owned()))?;
        if expanded_bytes > MAX_MOBILE_ARCHIVE_BYTES {
            return Err(invalid_archive(format!(
                "mobile archive candidate members exceed the {MAX_MOBILE_ARCHIVE_BYTES}-byte aggregate limit"
            )));
        }
        self.candidates = candidates;
        self.expanded_bytes = expanded_bytes;
        Ok(())
    }
}

fn read_radar_members(path: &Path) -> Result<Vec<RadarMember>> {
    let file = File::open(path).map_err(|source| DoradeError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|err| DoradeError::InvalidMessage {
        offset: 0,
        reason: format!("not a readable zip archive: {err}"),
    })?;

    let mut members = Vec::new();
    let mut budget = MemberBudget::default();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| DoradeError::InvalidMessage {
                offset: 0,
                reason: format!("zip entry {index}: {err}"),
            })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        if !plausible_radar_member_name(&name) {
            continue;
        }
        let declared = usize::try_from(entry.size()).map_err(|_| {
            invalid_archive(format!("zip entry {name} size overflows this platform"))
        })?;
        budget.reserve(&name, declared)?;
        let bytes =
            read_to_end_limited(&mut entry, MAX_MOBILE_MEMBER_BYTES, "mobile archive member")
                .map_err(|err| with_member(&name, DoradeError::Compression(err)))?;
        if bytes.len() != declared {
            return Err(invalid_archive(format!(
                "zip entry {name} decoded to {} bytes, expected {declared}",
                bytes.len()
            )));
        }
        // Content sniff: extension pre-filter only narrows the candidates.
        if looks_like_dorade_bytes(&bytes)
            || bytes.starts_with(VOLUME_HEADER_MAGIC)
            || bytes.starts_with(&[0x1f, 0x8b])
            || bytes.starts_with(b"BZh")
        {
            members.push(RadarMember { name, bytes });
        }
    }
    members.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(members)
}

/// Names worth opening: `swp.*` sweepfiles and Level II-style members.
fn plausible_radar_member_name(name: &str) -> bool {
    let file_name = name.rsplit('/').next().unwrap_or("");
    if file_name.is_empty() || file_name.starts_with('.') {
        return false;
    }
    if looks_like_dorade_name(file_name) {
        return true;
    }
    Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "msg31" | "ar2v" | "raw" | "gz" | "bz2" | "v06" | "v08"
            )
        })
}

/// Maximum start-time gap between consecutive sweeps of one volume scan.
const MAX_INTRA_VOLUME_GAP_MINUTES: i64 = 15;
/// Fixed angles within this tolerance count as "not ascending".
const FIXED_ANGLE_EPSILON_DEG: f32 = 0.05;

/// A sweep waiting to be grouped: start time + fixed angle drive the
/// ascending-run segmentation, `label` breaks time ties deterministically.
struct GroupableSweep<T> {
    start_time: Option<DateTime<Utc>>,
    fixed_angle_deg: f32,
    label: String,
    payload: T,
}

/// Group one instrument's sweeps (already time-sorted) into volume scans:
/// a scan continues while the fixed angle strictly ascends and sweeps stay
/// within [`MAX_INTRA_VOLUME_GAP_MINUTES`] of each other.
fn segment_volume_runs<T>(mut sweeps: Vec<GroupableSweep<T>>) -> Vec<Vec<GroupableSweep<T>>> {
    sweeps.sort_by(|left, right| {
        left.start_time
            .cmp(&right.start_time)
            .then_with(|| left.label.cmp(&right.label))
    });
    let mut runs: Vec<Vec<GroupableSweep<T>>> = Vec::new();
    for sweep in sweeps {
        let continues_run = runs.last().and_then(|run| run.last()).is_some_and(|last| {
            let ascending = sweep.fixed_angle_deg > last.fixed_angle_deg + FIXED_ANGLE_EPSILON_DEG;
            let close_in_time = match (last.start_time, sweep.start_time) {
                (Some(previous), Some(current)) => {
                    (current - previous).num_minutes() <= MAX_INTRA_VOLUME_GAP_MINUTES
                }
                _ => true,
            };
            ascending && close_in_time
        });
        match runs.last_mut() {
            Some(run) if continues_run => run.push(sweep),
            _ => runs.push(vec![sweep]),
        }
    }
    runs
}

/// Decode grouped members in parallel. The retained size of every decoded
/// volume counts against `batch_limit` (normally [`MAX_DECODED_BATCH_BYTES`]);
/// in-flight decodes on other threads may briefly hold more.
fn decode_members<F, E>(
    archive_path: &Path,
    members: Vec<RadarMember>,
    decode_level2: &F,
    batch_limit: usize,
) -> Result<Vec<MobileVolume>>
where
    F: Fn(&[u8]) -> std::result::Result<Volume, E> + Sync,
    E: Display,
{
    // Split DORADE sweeps from Level II members, peeking DORADE headers for
    // the grouping metadata.
    let mut per_instrument: BTreeMap<String, Vec<GroupableSweep<RadarMember>>> = BTreeMap::new();
    let mut level2_members: Vec<RadarMember> = Vec::new();
    for member in members {
        if looks_like_dorade_bytes(&member.bytes) {
            let header =
                peek_dorade_sweep(&member.bytes).map_err(|err| with_member(&member.name, err))?;
            per_instrument
                .entry(header.instrument)
                .or_default()
                .push(GroupableSweep {
                    start_time: header.start_time,
                    fixed_angle_deg: header.fixed_angle_deg,
                    label: member.name.clone(),
                    payload: member,
                });
        } else {
            level2_members.push(member);
        }
    }

    let archive_label = archive_path.display().to_string();
    let mut volumes: Vec<MobileVolume> = Vec::new();
    let decoded_bytes = AtomicUsize::new(0);

    let runs: Vec<Vec<GroupableSweep<RadarMember>>> = per_instrument
        .into_values()
        .flat_map(segment_volume_runs)
        .collect();
    let dorade_volumes: Vec<MobileVolume> = runs
        .into_par_iter()
        .map(|run| {
            let sweeps: Vec<&[u8]> = run
                .iter()
                .map(|sweep| sweep.payload.bytes.as_slice())
                .collect();
            let mut volume = decode_dorade_run(&sweeps).map_err(|err| {
                // Name the run's first member; a bad sweep names itself in
                // the error text.
                with_member(&run[0].payload.name, err)
            })?;
            charge_batch(&decoded_bytes, &volume, batch_limit)?;
            let member_label = run[0].payload.name.clone();
            volume.provenance.source_path = Some(format!("{archive_label}::{member_label}"));
            Ok(MobileVolume {
                volume,
                member_label,
                member_count: run.len(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    volumes.extend(dorade_volumes);

    let level2_volumes: Vec<MobileVolume> = level2_members
        .into_par_iter()
        .map(|member| {
            let mut volume =
                decode_level2(&member.bytes).map_err(|err| with_member(&member.name, err))?;
            charge_batch(&decoded_bytes, &volume, batch_limit)?;
            volume.provenance.source_path = Some(format!("{archive_label}::{}", member.name));
            Ok(MobileVolume {
                volume,
                member_label: member.name,
                member_count: 1,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    volumes.extend(level2_volumes);

    // By scan time (the volume's time reference), then member label.
    volumes.sort_by(|left, right| {
        left.volume
            .time_reference
            .cmp(&right.volume.time_reference)
            .then_with(|| left.member_label.cmp(&right.member_label))
    });
    Ok(volumes)
}

/// Add a decoded volume's retained size (fields and ray tables) to the
/// archive-wide total, failing once it would pass `limit`.
fn charge_batch(total: &AtomicUsize, volume: &Volume, limit: usize) -> Result<()> {
    let bytes = retained_bytes(volume);
    total
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(bytes).filter(|next| *next <= limit)
        })
        .map(|_| ())
        .map_err(|used| {
            DoradeError::LimitExceeded(format!(
                "mobile archive decodes to more than {limit} bytes (limit); {used} bytes \
                 were already decoded before a {bytes}-byte volume"
            ))
        })
}

fn with_member(name: &str, err: impl Display) -> DoradeError {
    DoradeError::InvalidMessage {
        offset: 0,
        reason: format!("archive member {name}: {err}"),
    }
}

fn invalid_archive(reason: String) -> DoradeError {
    DoradeError::InvalidMessage { offset: 0, reason }
}

fn file_len_usize(path: &Path) -> Result<usize> {
    let metadata = std::fs::metadata(path).map_err(|source| DoradeError::Io {
        path: path.display().to_string(),
        source,
    })?;
    usize::try_from(metadata.len()).map_err(|_| {
        invalid_archive(format!(
            "file {} size overflows this platform",
            path.display()
        ))
    })
}

fn read_file_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let declared = file_len_usize(path)?;
    if declared > limit {
        return Err(invalid_archive(format!(
            "file {} is {declared} bytes (limit {limit})",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(|source| DoradeError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let bytes = read_to_end_limited(&mut file, limit, "mobile radar file")
        .map_err(DoradeError::Compression)?;
    if bytes.len() != declared {
        return Err(invalid_archive(format!(
            "file {} changed while reading (expected {declared} bytes, read {})",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Descriptor blocks live at the head of a sweepfile; this is enough bytes
/// to peek COMM/SSWB/VOLD/RADD/PARM*/CELV/CSFD/SWIB without reading rays.
const PEEK_HEAD_BYTES: usize = 64 * 1024;

/// Decode the full volume scan a loose sweepfile belongs to.
///
/// Scans the file's directory for sibling `swp.*` files from the same
/// instrument, segments them into ascending fixed-angle runs (see module
/// docs), and decodes the run containing `path` as one volume. Sibling
/// headers are peeked from the first `PEEK_HEAD_BYTES` only, so opening a
/// file in a large deployment directory stays cheap.
pub fn read_dorade_volume_for_path(path: &Path) -> Result<Volume> {
    let bytes = read_file_limited(path, MAX_MOBILE_MEMBER_BYTES)?;
    let header = peek_dorade_sweep(&bytes)?;

    let mut sweeps: Vec<GroupableSweep<PathBuf>> = vec![GroupableSweep {
        start_time: header.start_time,
        fixed_angle_deg: header.fixed_angle_deg,
        label: path.display().to_string(),
        payload: path.to_path_buf(),
    }];
    if let Some(directory) = path.parent()
        && let Ok(entries) = std::fs::read_dir(directory)
    {
        for entry in entries.flatten() {
            let sibling = entry.path();
            if sibling == *path || !sibling.is_file() {
                continue;
            }
            if !crate::dorade::looks_like_dorade_path(&sibling) {
                continue;
            }
            let Some(head) = read_file_head(&sibling, PEEK_HEAD_BYTES) else {
                continue;
            };
            let Ok(sibling_header) = peek_dorade_sweep(&head) else {
                continue;
            };
            if sibling_header.instrument == header.instrument {
                sweeps.push(GroupableSweep {
                    start_time: sibling_header.start_time,
                    fixed_angle_deg: sibling_header.fixed_angle_deg,
                    label: sibling.display().to_string(),
                    payload: sibling,
                });
            }
        }
    }

    let runs = segment_volume_runs(sweeps);
    let run = runs
        .into_iter()
        .find(|run| run.iter().any(|sweep| sweep.payload == *path))
        .ok_or_else(|| {
            invalid_archive(format!(
                "sweep {} is missing from its own volume run",
                path.display()
            ))
        })?;

    // The opened file's bytes are already in hand; the run's other sweeps
    // are read here, in scan order.
    let mut all: Vec<Vec<u8>> = Vec::with_capacity(run.len());
    let mut bytes = Some(bytes);
    for sweep in &run {
        all.push(if sweep.payload == *path {
            bytes.take().unwrap_or_default()
        } else {
            read_member_file(&sweep.payload)?
        });
    }
    let sweeps: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
    let mut volume = decode_dorade_run(&sweeps).map_err(|err| with_member(&run[0].label, err))?;
    volume.provenance.source_path = Some(path.display().to_string());
    Ok(volume)
}

fn read_file_head(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut head = vec![0u8; limit];
    let mut filled = 0usize;
    loop {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(count) => {
                filled += count;
                if filled == head.len() {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    head.truncate(filled);
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // Real inputs:
    // - `dorade-noxp-20090610-003210-heads-zip`: one directory of the VORTEX-2
    //   NOXP archive 2009.NOX.sweep.0609.tar.gz as a zip, members in tar order
    //   under their tar paths: corrections, NOX090610003210.RAWAL8D.log, the
    //   1.0, 0.5 and 2.0 deg sweeps (6-ray head trims), sigmet_dorade.out.
    // - The same three sweeps as loose corpus files, the consecutive NOXP
    //   single-tilt 0.5 deg sweeps of 2009-05-01 19:02:44Z and 19:03:24Z, and
    //   the COW2 surveillance sweep of 2026-05-21.
    // Expected values: tools/golden_io_formats.py, section `dorade` (SSWB
    // start times, SWIB fixed angles, RADD instrument names).

    const NOXP_ZIP: &str = "dorade-noxp-20090610-003210-heads-zip";
    const NOXP_0610_05: &str = "dorade-noxp-20090610-003210-ppi-head6";
    const NOXP_0610_10: &str = "dorade-noxp-20090610-003222-ppi-head6";
    const NOXP_0610_20: &str = "dorade-noxp-20090610-003226-ppi-head6";
    const NOXP_0501_A: &str = "dorade-noxp-20090501-190244-ppi";
    const NOXP_0501_B: &str = "dorade-noxp-20090501-190324-ppi";
    const COW2: &str = "dorade-cow2-20260521-225514-sur-head24";

    /// These archives hold DORADE sweeps only; a Level II member would be a
    /// test bug.
    fn no_level2_members(_bytes: &[u8]) -> std::result::Result<Volume, String> {
        Err("unexpected Level II member".to_owned())
    }

    fn corpus_path(id: &str) -> PathBuf {
        recast_radar_testdata::path(id).unwrap_or_else(|err| panic!("{err}"))
    }

    /// A fresh scratch directory per test (and per process, so concurrent
    /// worktrees do not collide).
    fn scratch_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "recast_radar_mobile_archive_{test}_{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Copy corpus files into `dir` under their committed file names
    /// (`swp.<time>.<instrument>...`).
    fn copy_into(dir: &Path, ids: &[&str]) -> Vec<PathBuf> {
        ids.iter()
            .map(|id| {
                let source = corpus_path(id);
                let target = dir.join(source.file_name().unwrap());
                std::fs::copy(&source, &target).unwrap();
                target
            })
            .collect()
    }

    const NOXP_DIR: &str = "2009/NOX/sweep/0609/NOX090610003210.RAWAL8D/";

    #[test]
    fn groups_zip_members_into_ascending_elevation_runs_per_instrument() {
        // Zip: three sweeps of one volume (SSWB 00:32:10 / 00:32:22 /
        // 00:32:26Z; fixed 0.5 / 1.0 / 2.0 deg) stored 1.0, 0.5, 2.0, among
        // three text members that must be ignored.
        let volumes =
            read_mobile_archive_from_path(&corpus_path(NOXP_ZIP), no_level2_members).unwrap();
        assert_eq!(volumes.len(), 1);
        let volume = &volumes[0];
        assert_eq!(volume.volume.attrs.instrument_name, "NOXPRVP");
        assert_eq!(volume.member_count, 3);
        assert_eq!(
            volume.member_label,
            format!("{NOXP_DIR}swp.1090610003210.NOXPRVP.0.0.5_PPI_v1")
        );
        let angles: Vec<f32> = volume
            .volume
            .sweeps
            .iter()
            .map(|sweep| sweep.fixed_angle_deg)
            .collect();
        assert_eq!(angles, [0.499_877_93, 0.999_755_86, 1.999_511_7]);
        assert_eq!(volume.volume.provenance.decode.decoded_ray_count, 18);
        assert!(
            volume
                .volume
                .provenance
                .source_path
                .as_deref()
                .is_some_and(|source| source.ends_with("::2009/NOX/sweep/0609/NOX090610003210.RAWAL8D/swp.1090610003210.NOXPRVP.0.0.5_PPI_v1"))
        );

        // Per instrument: the same sweeps next to the COW2 sweep in a
        // deployment folder give one volume per radar, in scan-time order.
        let dir = scratch_dir("per_instrument");
        copy_into(&dir, &[NOXP_0610_20, COW2, NOXP_0610_05, NOXP_0610_10]);
        let volumes = read_mobile_dir_from_path(&dir, no_level2_members).unwrap();
        let summary: Vec<(&str, usize, usize)> = volumes
            .iter()
            .map(|v| {
                (
                    v.volume.attrs.instrument_name.as_str(),
                    v.member_count,
                    v.volume.sweeps.len(),
                )
            })
            .collect();
        assert_eq!(summary, [("NOXPRVP", 3, 3), ("COW2", 1, 1)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_elevation_sequences_become_one_volume_per_sweep() {
        // Consecutive NOXP single-tilt sweeps: both 0.49987793 deg, SSWB
        // 19:02:44Z and 19:03:24Z. A repeated angle starts a new volume. Both
        // sweeps' rays run backwards past the SSWB start (to 19:02:42Z and
        // 19:03:22Z), and the earliest ray is the time reference.
        let dir = scratch_dir("same_elevation");
        let paths = copy_into(&dir, &[NOXP_0501_A, NOXP_0501_B]);
        let volumes = read_mobile_dir_from_path(&dir, no_level2_members).unwrap();
        assert_eq!(volumes.len(), 2);
        assert!(
            volumes
                .iter()
                .all(|v| v.member_count == 1 && v.volume.sweeps.len() == 1)
        );
        assert_eq!(
            volumes[0].volume.time_reference,
            Utc.with_ymd_and_hms(2009, 5, 1, 19, 2, 42).unwrap()
        );
        assert_eq!(
            volumes[1].volume.time_reference,
            Utc.with_ymd_and_hms(2009, 5, 1, 19, 3, 22).unwrap()
        );
        // Opening one loose sweep does not pull in its same-angle sibling.
        let volume = read_dorade_volume_for_path(&paths[0]).unwrap();
        assert_eq!(volume.sweeps.len(), 1);
        assert_eq!(volume.sweeps[0].nrays(), 51);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn long_time_gap_splits_an_ascending_run() {
        // 0.5 deg at 2009-05-01 19:02:44Z, then 1.0 deg at 2009-06-10
        // 00:32:22Z: ascending, but 39 days apart.
        let dir = scratch_dir("time_gap");
        copy_into(&dir, &[NOXP_0501_A, NOXP_0610_10]);
        let volumes = read_mobile_dir_from_path(&dir, no_level2_members).unwrap();
        let runs: Vec<(f32, usize)> = volumes
            .iter()
            .map(|v| (v.volume.sweeps[0].fixed_angle_deg, v.volume.sweeps.len()))
            .collect();
        assert_eq!(runs, [(0.499_877_93, 1), (0.999_755_86, 1)]);
        std::fs::remove_dir_all(&dir).ok();

        // The same 1.0 deg sweep after its own volume's 0.5 deg sweep (12 s
        // earlier) continues the run.
        let dir = scratch_dir("no_time_gap");
        copy_into(&dir, &[NOXP_0610_05, NOXP_0610_10]);
        let volumes = read_mobile_dir_from_path(&dir, no_level2_members).unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].volume.sweeps.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_archive_without_radar_members() {
        // The real zip with every sweep member renamed from "swp." to "swp_"
        // (local headers and central directory; data and CRCs untouched)
        // leaves only non-radar members.
        let mut bytes = std::fs::read(corpus_path(NOXP_ZIP)).unwrap();
        let mut renamed = 0;
        let needle = b"RAWAL8D/swp.";
        let mut index = 0;
        while let Some(found) = bytes[index..]
            .windows(needle.len())
            .position(|window| window == needle)
        {
            let at = index + found + needle.len() - 1;
            bytes[at] = b'_';
            renamed += 1;
            index = at;
        }
        // 3 sweeps x (local header + central directory entry).
        assert_eq!(renamed, 6);
        let dir = scratch_dir("no_radar_members");
        let zip_path = dir.join("NOX090610003210.RAWAL8D.renamed.zip");
        std::fs::write(&zip_path, &bytes).unwrap();

        let err = read_mobile_archive_from_path(&zip_path, no_level2_members).unwrap_err();
        assert!(err.to_string().contains("no radar members"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loose_sweepfile_groups_directory_siblings_from_same_run() {
        // One volume's three sweeps plus an older single-tilt NOXP sweep in the
        // same folder: opening the 1.0 deg sweep pulls in its run only.
        let dir = scratch_dir("loose_siblings");
        let paths = copy_into(
            &dir,
            &[NOXP_0610_05, NOXP_0610_10, NOXP_0610_20, NOXP_0501_A],
        );

        let volume = read_dorade_volume_for_path(&paths[1]).unwrap();
        assert_eq!(volume.attrs.instrument_name, "NOXPRVP");
        let angles: Vec<f32> = volume
            .sweeps
            .iter()
            .map(|sweep| sweep.fixed_angle_deg)
            .collect();
        assert_eq!(angles, [0.499_877_93, 0.999_755_86, 1.999_511_7]);
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2009, 6, 10, 0, 32, 10).unwrap()
        );
        assert!(close_to(volume.location.latitude_deg.unwrap(), 37.597_79));

        // SSWB 19:02:44Z; the earliest ray is at 19:02:42Z.
        let older = read_dorade_volume_for_path(&paths[3]).unwrap();
        assert_eq!(older.sweeps.len(), 1);
        assert_eq!(
            older.time_reference,
            Utc.with_ymd_and_hms(2009, 5, 1, 19, 2, 42).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// DORADE stores the site latitude as f32; the model widens it to f64.
    fn close_to(actual: f64, expected: f32) -> bool {
        (actual - f64::from(expected)).abs() < 1e-5
    }

    #[test]
    fn archive_decoding_beyond_the_batch_limit_is_rejected() {
        // The committed real DORADE sweepfiles (`swp.*`, eight after the
        // io-formats corpus additions), as a deployment folder.
        let dir = recast_radar_testdata::testdata_dir().join("files/other/dorade");
        let sweepfiles = std::fs::read_dir(&dir)
            .expect("committed dorade directory")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("swp."))
            })
            .count();
        assert!(sweepfiles >= 5, "{sweepfiles} sweepfiles");
        let mut members = Vec::new();
        let mut budget = MemberBudget::default();
        collect_dir_members(&dir, &dir, &mut members, &mut budget, 0)
            .expect("read committed sweepfiles");
        assert_eq!(members.len(), sweepfiles);
        members.sort_by(|left, right| left.name.cmp(&right.name));
        let volumes = decode_members(
            &dir,
            members.iter().map(clone_member).collect(),
            &no_level2_members,
            MAX_DECODED_BATCH_BYTES,
        )
        .expect("real sweepfiles fit the default batch limit");
        let needed: usize = volumes
            .iter()
            .map(|mobile| retained_bytes(&mobile.volume))
            .sum();

        let error = decode_members(&dir, members, &no_level2_members, needed - 1)
            .expect_err("one byte short of the decoded size must fail");
        assert!(
            matches!(&error, DoradeError::LimitExceeded(reason) if reason.contains("limit")),
            "unexpected error: {error}"
        );
    }

    fn clone_member(member: &RadarMember) -> RadarMember {
        RadarMember {
            name: member.name.clone(),
            bytes: member.bytes.clone(),
        }
    }

    #[test]
    fn zip_sniffers_match_magic_and_extension() {
        // PKWARE APPNOTE: a local file header starts "PK\x03\x04"; the
        // end-of-central-directory record "PK\x05\x06" is the last 22 bytes
        // of a zip without a comment.
        let path = corpus_path(NOXP_ZIP);
        let bytes = std::fs::read(&path).unwrap();
        assert!(looks_like_zip_bytes(&bytes));
        let eocd = &bytes[bytes.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert!(!looks_like_zip_bytes(eocd));
        assert!(!looks_like_zip_bytes(&bytes[..3]));
        // A real DORADE sweep and a real tar are not zips.
        assert!(!looks_like_zip_bytes(
            &std::fs::read(corpus_path(COW2)).unwrap()
        ));

        assert!(looks_like_zip_path(&path));
        assert!(looks_like_zip_path(&path.with_extension("ZIP")));
        assert!(!looks_like_zip_path(&corpus_path(
            "jma-n5-20191012-090000-rs47773"
        )));
        assert!(!looks_like_zip_path(&corpus_path(COW2)));
    }

    #[test]
    fn member_name_prefilter_accepts_observed_layouts() {
        assert!(plausible_radar_member_name(
            "DORADE/COW2/swp.1260516225229.COW2.515.1.0_SUR_v237"
        ));
        assert!(plausible_radar_member_name(
            "GR2 MSG31/COW2/nexrad.20260516_225229_COW2_v237_SUR.msg31"
        ));
        assert!(!plausible_radar_member_name("GR2 - README.txt"));
        assert!(!plausible_radar_member_name("DORADE/COW2/"));
    }
}
