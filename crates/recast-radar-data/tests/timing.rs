//! Chunk timing model checked against real chunk listings.
//!
//! Two fixture sets, both written by `tools/capture_chunk_listings.py` from
//! `unidata-nexrad-level2-chunks` (three consecutive complete volumes per
//! site):
//!
//! - `tests/fixtures/chunks/` (the *fitted* set): eight sites, volumes from
//!   2026-09-17 00:30-01:31Z, captured while the model was built. The
//!   defaults in `TimingParameters::WSR88D` were measured on two of its
//!   volumes (KIWA 307 and 308) and the tolerances below were chosen with
//!   these captures in view.
//!   - clear air: KAMA (VCP 35), KDGX (VCP 35 with base tilt and SAILS), KGRB
//!     (VCP 34);
//!   - precipitation: KRGX (VCP 12 with base tilt, AVSET), KMAX (VCP 212
//!     with base tilt and SAILS, full volumes), KEAX (VCP 212 with MESO-SAILS
//!     x2, AVSET), KIWA (VCP 215 with SAILS, AVSET; volume 307 is the TD.1
//!     chunk capture);
//!   - TDWR: TATL (VCP 90, which has no Build 24 definition).
//! - `tests/fixtures/chunks-holdout/` (the *held-out* set): ten other sites,
//!   captured at 02:46Z and 03:09Z after the model, its defaults and the
//!   tolerances were fixed, and never used to change them. Volumes from
//!   02:30-03:04Z (TLAS 01:28-01:40Z, across the 999 -> 1 id wrap): KTLX (VCP
//!   35), KMUX (VCP 35 with base tilt and SAILS; status-only chunks), KHNX
//!   (VCP 34), KMSX (VCP 34 with base tilt), KDMX (VCP 212, AVSET), KGJX (VCP
//!   212 with MESO-SAILS x2), KBUF (VCP 215 with MESO-SAILS and staggered-PRT
//!   batch cuts, AVSET changing between volumes), PAHG (VCP 215, AVSET), TDEN
//!   (TDWR VCP 80) and TLAS (TDWR VCP 90).
//!
//! For each volume the fixtures hold:
//! - the raw `ListObjectsV2` XML (keys, sizes, `LastModified`);
//! - `*.vcp.csv`: the Message 5 cut table from the Start chunk;
//! - `*.chunks.csv`: per-chunk Message 31 contents (radial count, elevation
//!   number, first and last radial times), decoded from the downloaded
//!   chunks.
//!
//! MetPy 1.7.1 `Level2File` read each whole volume and agreed on sweeps,
//! radial counts and times, and the VCP table.
//!
//! Every accuracy claim runs on both sets with the same tolerance. Where the
//! held-out set breaks a claim, its test names the volumes or cuts that break
//! it, pins the measured deviation and states the cause; nothing was relaxed
//! for it. Checks of learned statistics always predict a volume that the
//! statistics have not seen. Measured errors print with
//! `cargo test -p recast-radar-data --test timing -- --nocapture`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use recast_radar_data::RealtimeChunkType;
use recast_radar_data::realtime::timing::{
    MIN_RADIAL_CHUNK_BYTES, ScanCut, ScanPlan, ScanTimingModel, TimingError, TimingParameters,
    TimingStatistics, VolumeObservation, parse_chunk_listing,
};
use recast_radar_data::realtime::vcp_catalog::{DopplerPrfValue, Waveform, build_24_definition};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn fail(message: impl Into<String>) -> Box<dyn Error> {
    message.into().into()
}

fn check(condition: bool, message: impl FnOnce() -> String) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(fail(message()))
    }
}

/// Which fixture set a check runs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Set {
    Fitted,
    Holdout,
}

impl Set {
    fn dir(self) -> PathBuf {
        let name = match self {
            Set::Fitted => "chunks",
            Set::Holdout => "chunks-holdout",
        };
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
}

/// Every failed check of one test, reported together.
#[derive(Debug, Default)]
struct Failures(Vec<String>);

impl Failures {
    fn check(&mut self, condition: bool, message: impl FnOnce() -> String) {
        if !condition {
            self.0.push(message());
        }
    }

    fn finish(self) -> TestResult {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(fail(self.0.join("\n")))
        }
    }
}

#[derive(Clone, Debug)]
struct VcpRow {
    elevation_deg: f32,
    waveform: String,
    /// Message 5 channel configuration; 2 is SZ-2 phase coding.
    channel: u8,
    super_res: u8,
    azimuth_rate: f32,
    supplemental: u16,
}

#[derive(Clone, Debug)]
struct ChunkRow {
    chunk_id: u16,
    chunk_type: String,
    size: u64,
    last_modified: String,
    radials: u16,
    /// `None` for a chunk without radials.
    elevation_number: Option<usize>,
    first_radial: Option<DateTime<Utc>>,
    last_radial: Option<DateTime<Utc>>,
    other_messages: String,
}

#[derive(Clone, Debug)]
struct Capture {
    site: String,
    listing: String,
    vcp: u16,
    manifest_chunks: usize,
    manifest_radials: u32,
    elevations_collected: usize,
    volume: VolumeObservation,
    listed_objects: usize,
    rows: Vec<VcpRow>,
    chunks: Vec<ChunkRow>,
}

#[derive(Clone, Copy, Debug)]
struct DecodedCut {
    elevation_number: usize,
    radials: u32,
    first_radial: DateTime<Utc>,
    last_radial: DateTime<Utc>,
}

impl Capture {
    fn is_tdwr(&self) -> bool {
        self.site.starts_with('T')
    }

    /// The Message 5 plan: 720 radials when the super-resolution bit is set.
    fn plan(&self) -> ScanPlan {
        ScanPlan::new(
            Some(self.vcp),
            self.rows
                .iter()
                .map(|row| {
                    ScanCut::new(
                        row.elevation_deg,
                        row.azimuth_rate,
                        ScanCut::radials_for_azimuth_spacing(row.super_res & 1 == 1),
                    )
                })
                .collect(),
        )
    }

    fn model(&self, parameters: TimingParameters) -> TestResult<ScanTimingModel> {
        Ok(ScanTimingModel::new(self.plan(), parameters)?)
    }

    fn end_chunk_id(&self) -> TestResult<u16> {
        self.volume
            .end_chunk_id()
            .ok_or_else(|| fail(format!("{} has no End chunk", self.listing)))
    }

    /// Plan chunk number of the chunk closing the last collected cut.
    fn end_plan_number(&self) -> TestResult<u16> {
        self.volume
            .closing_plan_chunk_number(self.end_chunk_id()?)
            .ok_or_else(|| fail(format!("{} has no radial chunks", self.listing)))
    }

    fn last_modified(&self, chunk_id: u16) -> TestResult<DateTime<Utc>> {
        self.volume
            .chunk(chunk_id)
            .map(|chunk| chunk.last_modified)
            .ok_or_else(|| fail(format!("{} has no chunk {chunk_id}", self.listing)))
    }

    fn last_modified_by_plan_number(&self, number: u16) -> TestResult<DateTime<Utc>> {
        self.volume
            .chunk_by_plan_number(number)
            .map(|chunk| chunk.last_modified)
            .ok_or_else(|| fail(format!("{} has no plan chunk {number}", self.listing)))
    }

    fn decoded_cuts(&self) -> Vec<DecodedCut> {
        let mut cuts: BTreeMap<usize, DecodedCut> = BTreeMap::new();
        for chunk in &self.chunks {
            let (Some(elevation_number), Some(first), Some(last)) = (
                chunk.elevation_number,
                chunk.first_radial,
                chunk.last_radial,
            ) else {
                continue;
            };
            let cut = cuts.entry(elevation_number).or_insert(DecodedCut {
                elevation_number,
                radials: 0,
                first_radial: first,
                last_radial: last,
            });
            cut.radials += u32::from(chunk.radials);
            cut.first_radial = cut.first_radial.min(first);
            cut.last_radial = cut.last_radial.max(last);
        }
        cuts.into_values().collect()
    }
}

fn seconds(later: DateTime<Utc>, earlier: DateTime<Utc>) -> f64 {
    (later - earlier).num_milliseconds() as f64 / 1000.0
}

fn parse_time(value: &str) -> TestResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn optional_time(value: &str) -> TestResult<Option<DateTime<Utc>>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_time(value).map(Some)
    }
}

fn csv_rows(path: &Path) -> TestResult<Vec<BTreeMap<String, String>>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or_else(|| fail(format!("{} is empty", path.display())))?
        .split(',')
        .collect();
    lines
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() != header.len() {
                return Err(fail(format!("{}: bad row {line}", path.display())));
            }
            Ok(header
                .iter()
                .zip(fields)
                .map(|(key, value)| ((*key).to_owned(), value.to_owned()))
                .collect())
        })
        .collect()
}

fn field<'a>(row: &'a BTreeMap<String, String>, key: &str) -> TestResult<&'a str> {
    row.get(key)
        .map(String::as_str)
        .ok_or_else(|| fail(format!("missing field {key}")))
}

fn toml_blocks<'a>(text: &'a str, header: &str) -> Vec<BTreeMap<&'a str, &'a str>> {
    text.split(header)
        .skip(1)
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.split_once(" = "))
                .map(|(key, value)| (key.trim(), value.trim().trim_matches('"')))
                .collect()
        })
        .collect()
}

fn load_capture(dir: &Path, entry: &BTreeMap<&str, &str>) -> TestResult<Capture> {
    let get = |key: &str| {
        entry
            .get(key)
            .copied()
            .ok_or_else(|| fail(format!("manifest entry lacks {key}")))
    };
    let listing = get("listing")?.to_owned();
    let objects = parse_chunk_listing(&fs::read_to_string(dir.join(&listing))?)?;
    let listed_objects = objects.len();
    let mut volumes = VolumeObservation::from_chunks(objects);
    if volumes.len() != 1 {
        return Err(fail(format!("{listing}: {} volumes", volumes.len())));
    }
    let volume = volumes.remove(0);

    let rows = csv_rows(&dir.join(get("vcp_csv")?))?
        .iter()
        .map(|row| {
            Ok(VcpRow {
                elevation_deg: field(row, "elevation_deg")?.parse()?,
                waveform: field(row, "waveform")?.to_owned(),
                channel: field(row, "channel")?.parse()?,
                super_res: field(row, "super_res")?.parse()?,
                azimuth_rate: field(row, "azimuth_rate_deg_s")?.parse()?,
                supplemental: field(row, "supplemental")?.parse()?,
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    let chunks = csv_rows(&dir.join(get("chunks_csv")?))?
        .iter()
        .map(|row| {
            let elevations = field(row, "elevation_numbers")?;
            Ok(ChunkRow {
                chunk_id: field(row, "chunk_id")?.parse()?,
                chunk_type: field(row, "type")?.to_owned(),
                size: field(row, "size")?.parse()?,
                last_modified: field(row, "last_modified")?.to_owned(),
                radials: field(row, "radials")?.parse()?,
                elevation_number: if elevations.is_empty() {
                    None
                } else {
                    Some(elevations.parse()?)
                },
                first_radial: optional_time(field(row, "first_radial_time")?)?,
                last_radial: optional_time(field(row, "last_radial_time")?)?,
                other_messages: field(row, "other_messages")?.to_owned(),
            })
        })
        .collect::<TestResult<Vec<_>>>()?;

    Ok(Capture {
        site: get("site")?.to_owned(),
        vcp: get("vcp")?.parse()?,
        manifest_chunks: get("chunks")?.parse()?,
        manifest_radials: get("radials")?.parse()?,
        elevations_collected: get("elevations_collected")?.parse()?,
        listing,
        volume,
        listed_objects,
        rows,
        chunks,
    })
}

/// Every manifest volume of a set, grouped by site, in volume order.
fn captures(set: Set) -> TestResult<Vec<Vec<Capture>>> {
    let dir = set.dir();
    let manifest = fs::read_to_string(dir.join("manifest.toml"))?;
    let mut sites: BTreeMap<String, Vec<Capture>> = BTreeMap::new();
    for entry in toml_blocks(&manifest, "[[volume]]") {
        let capture = load_capture(&dir, &entry)?;
        sites.entry(capture.site.clone()).or_default().push(capture);
    }
    for volumes in sites.values_mut() {
        volumes.sort_by_key(|capture| capture.volume.volume_time);
    }
    Ok(sites.into_values().collect())
}

fn find_site<'a>(sites: &'a [Vec<Capture>], site: &str) -> TestResult<&'a [Capture]> {
    sites
        .iter()
        .find(|volumes| volumes.first().is_some_and(|capture| capture.site == site))
        .map(Vec::as_slice)
        .ok_or_else(|| fail(format!("no {site} captures")))
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted.get(index).copied().unwrap_or(f64::NAN)
}

fn abs_values(values: &[f64]) -> Vec<f64> {
    values.iter().map(|value| value.abs()).collect()
}

fn chunk_type_code(chunk_type: RealtimeChunkType) -> &'static str {
    match chunk_type {
        RealtimeChunkType::Start => "S",
        RealtimeChunkType::Intermediate => "I",
        RealtimeChunkType::End => "E",
        other => panic!("chunk type {other:?} has no code"),
    }
}

/// The volume id after `id` on the real-time ring (999 -> 1), written out
/// here rather than taken from the crate.
fn following_volume_id(id: u16) -> u16 {
    if id == 999 { 1 } else { id + 1 }
}

/// `LastModified` minus the model's expectation, per chunk that holds
/// radials (and the Start chunk), keyed by plan chunk number, in seconds.
fn chunk_errors(capture: &Capture, model: &ScanTimingModel) -> Vec<(u16, f64)> {
    capture
        .volume
        .chunks
        .iter()
        .filter_map(|chunk| {
            let number = capture.volume.plan_chunk_number(chunk.chunk_id)?;
            let modeled = model.chunk_last_modified_seconds(number)?;
            Some((
                number,
                seconds(chunk.last_modified, capture.volume.volume_time) - modeled,
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Fixture integrity
// ---------------------------------------------------------------------------

/// Fixture integrity:
/// - each listing is one complete volume, three consecutive volumes per site
///   (the id after 999 is 1);
/// - listings, decoded chunk tables and the manifest agree on ids, types,
///   sizes, `LastModified` and radial counts;
/// - a chunk holds no radials exactly when its listed size is below
///   `MIN_RADIAL_CHUNK_BYTES`, and such a chunk holds one Message 2 only;
/// - the key time is the first radial time cut to whole seconds.
fn fixtures_are_complete_consecutive_real_volumes(set: Set, expected_sites: usize) -> TestResult {
    let sites = captures(set)?;
    let mut failures = Failures::default();
    failures.check(sites.len() == expected_sites, || {
        format!("{} sites, expected {expected_sites}", sites.len())
    });
    for volumes in &sites {
        failures.check(volumes.len() == 3, || {
            format!("{} volumes at a site", volumes.len())
        });
        for pair in volumes.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            failures.check(
                following_volume_id(earlier.volume.volume_id) == later.volume.volume_id
                    && later.volume.volume_time > earlier.volume.volume_time,
                || format!("{} -> {} not consecutive", earlier.listing, later.listing),
            );
        }
        for capture in volumes {
            let volume = &capture.volume;
            failures.check(volume.is_complete(), || {
                format!("{} incomplete", capture.listing)
            });
            failures.check(
                volume.site == capture.site
                    && capture.listed_objects == capture.manifest_chunks
                    && volume.chunks.len() == capture.manifest_chunks
                    && capture.chunks.len() + 1 == volume.chunks.len(),
                || format!("{}: chunk counts disagree", capture.listing),
            );
            for (row, chunk) in capture.chunks.iter().zip(volume.chunks.iter().skip(1)) {
                failures.check(
                    row.chunk_id == chunk.chunk_id
                        && parse_time(&row.last_modified).ok() == Some(chunk.last_modified)
                        && row.chunk_type == chunk_type_code(chunk.chunk_type)
                        && row.size == chunk.size,
                    || {
                        format!(
                            "{} chunk {}: CSV and listing disagree",
                            capture.listing, row.chunk_id
                        )
                    },
                );
                failures.check(
                    (row.radials == 0) == chunk.is_status_only()
                        && (row.radials > 0 || row.other_messages == "2x1"),
                    || {
                        format!(
                            "{} chunk {}: {} radials, {} bytes (limit {MIN_RADIAL_CHUNK_BYTES}), messages {:?}",
                            capture.listing, row.chunk_id, row.radials, row.size, row.other_messages
                        )
                    },
                );
            }
            let radials: u32 = capture
                .chunks
                .iter()
                .map(|row| u32::from(row.radials))
                .sum();
            failures.check(radials == capture.manifest_radials, || {
                format!("{}: {radials} radials", capture.listing)
            });
            match capture.chunks.iter().find_map(|row| row.first_radial) {
                Some(first) => {
                    let lead = seconds(first, volume.volume_time);
                    failures.check((0.0..1.0).contains(&lead), || {
                        format!(
                            "{}: first radial {lead} s after the key time",
                            capture.listing
                        )
                    });
                }
                None => failures.check(false, || {
                    format!("{} has no radial chunks", capture.listing)
                }),
            }
        }
    }
    failures.finish()
}

#[test]
fn fixtures_are_complete_consecutive_real_volumes_fitted() -> TestResult {
    fixtures_are_complete_consecutive_real_volumes(Set::Fitted, 8)
}

#[test]
fn fixtures_are_complete_consecutive_real_volumes_holdout() -> TestResult {
    fixtures_are_complete_consecutive_real_volumes(Set::Holdout, 10)
}

// ---------------------------------------------------------------------------
// Elevation-to-chunk mapping
// ---------------------------------------------------------------------------

/// Elevation-to-chunk mapping, exact:
/// - With the Message 5 plan, every radial chunk maps (by plan chunk number)
///   to the elevation number decoded from its radials, and every chunk
///   without radials has no plan chunk number.
/// - The End chunk closes the last collected cut.
/// - NEXRAD chunks hold exactly 120 radials (720 or 360 per cut).
/// - TDWR chunks hold 117 to 120, and a cut is at most 3 radials short of 360.
fn message5_plans_map_every_radial_chunk_to_its_decoded_elevation(set: Set) -> TestResult {
    let mut failures = Failures::default();
    for capture in captures(set)?.iter().flatten() {
        let plan = capture.plan();
        for row in &capture.chunks {
            let Some(elevation_number) = row.elevation_number else {
                failures.check(
                    capture.volume.plan_chunk_number(row.chunk_id).is_none(),
                    || {
                        format!(
                            "{} chunk {}: no radials but a plan number",
                            capture.listing, row.chunk_id
                        )
                    },
                );
                continue;
            };
            let position = capture
                .volume
                .plan_chunk_number(row.chunk_id)
                .and_then(|number| plan.chunk_position(number));
            failures.check(
                position.is_some_and(|position| position.cut_index + 1 == elevation_number),
                || {
                    format!(
                        "{} chunk {}: plan position {position:?} vs decoded elevation {elevation_number}",
                        capture.listing, row.chunk_id
                    )
                },
            );
            let radials_ok = if capture.is_tdwr() {
                (117..=120).contains(&row.radials)
            } else {
                row.radials == 120
            };
            failures.check(radials_ok, || {
                format!(
                    "{} chunk {}: {} radials",
                    capture.listing, row.chunk_id, row.radials
                )
            });
        }
        for cut in capture.decoded_cuts() {
            let planned = plan
                .cuts
                .get(cut.elevation_number - 1)
                .map(|planned| u32::from(planned.radials));
            let ok = planned.is_some_and(|planned| {
                if capture.is_tdwr() {
                    cut.radials <= planned && cut.radials + 3 >= planned
                } else {
                    cut.radials == planned
                }
            });
            failures.check(ok, || {
                format!("{} {cut:?}: plan {planned:?} radials", capture.listing)
            });
        }
        let end = capture.end_plan_number()?;
        let position = plan.chunk_position(end);
        failures.check(
            position.is_some_and(|position| {
                position.closes_cut() && position.cut_index + 1 == capture.elevations_collected
            }),
            || format!("{}: End plan chunk {end} at {position:?}", capture.listing),
        );
        let expected_ids = position.and_then(|position| plan.cut_chunk_ids(position.cut_index));
        failures.check(
            expected_ids.as_ref().map(|ids| *ids.end()) == Some(end),
            || format!("{}: cut_chunk_ids {expected_ids:?}", capture.listing),
        );
    }
    failures.finish()
}

#[test]
fn message5_plans_map_every_radial_chunk_to_its_decoded_elevation_fitted() -> TestResult {
    message5_plans_map_every_radial_chunk_to_its_decoded_elevation(Set::Fitted)
}

#[test]
fn message5_plans_map_every_radial_chunk_to_its_decoded_elevation_holdout() -> TestResult {
    message5_plans_map_every_radial_chunk_to_its_decoded_elevation(Set::Holdout)
}

// ---------------------------------------------------------------------------
// Build 24 catalog
// ---------------------------------------------------------------------------

/// What comparing one capture's Message 5 with the Build 24 catalog found.
#[derive(Debug, Default)]
struct CatalogComparison {
    compared: usize,
    exact_layouts: usize,
    /// (VCP, catalog elevation, waveform) matched only through the period.
    period_only: Vec<(u16, f32, &'static str)>,
    /// (VCP, catalog elevation, executed azimuth rate, catalog rate) of batch
    /// rows the radar ran as staggered pulse pair (Message 5 waveform 5).
    staggered_batch: Vec<(u16, f32, f32, f32)>,
}

/// The Build 24 catalog compared with the radars' own Message 5, for every
/// NEXRAD volume:
/// - The catalog rows appear in order in the executed cut sequence. They
///   match on elevation (0.05 deg, the Message 5 angle coding), waveform,
///   SZ-2 phase coding, and radial count.
/// - Azimuth rates match within 0.1%, with three exceptions:
///   - SZCD rows. Their rate follows the PRF selected on site (Appendix C:
///     "the RPG adjusts the Azimuth Rate accordingly", rate = 1 / (64 * PRT)),
///     so they match within 2% of one of the SZCD rates the VCP's table
///     lists. The union over the VCP's SZCD rows is used. VCP 35's 1.3 deg
///     SZCD row lists only the default cell (transcribed as FIXED), yet both
///     VCP 35 sites ran it at the rate of their other split-cut Doppler rows:
///     20.028 deg/s at KAMA 480/481, 14.458 at KAMA 482 and KDGX.
///   - A row may match the rate implied by its own period (360 / period)
///     instead. Only VCP 12's 1.3 deg CD/W row needs this: ICD 2620002AA
///     lists 25.994 deg/s with period 14.40 s (the 0.5/0.9 deg rows list
///     24.994 deg/s and 14.40 s), and KRGX reports 24.994 deg/s in Message 5.
///     The catalog keeps the ICD value.
///   - Batch rows run as staggered pulse pair (SPP, Message 5 waveform 5).
///     The Build 24 table has no SPP rows; the RPG substitutes SPP for batch
///     cuts when staggered PRT is enabled on site, with its own azimuth rate.
///     Such rows match on elevation only, and their rates are reported.
/// - Every extra executed cut (SAILS, MESO-SAILS, base tilt) is flagged in
///   Message 5 supplemental data and sits at or below the lowest catalog
///   elevation.
///
/// Where nothing is inserted, the catalog plan maps every decoded chunk to
/// its elevation exactly.
fn compare_with_build24_catalog(set: Set) -> TestResult<CatalogComparison> {
    let mut result = CatalogComparison::default();
    let mut failures = Failures::default();
    for capture in captures(set)?
        .iter()
        .flatten()
        .filter(|capture| !capture.is_tdwr())
    {
        let definition = build_24_definition(capture.vcp).ok_or_else(|| {
            fail(format!(
                "{}: VCP {} not in the catalog",
                capture.listing, capture.vcp
            ))
        })?;
        let lowest = definition
            .rows
            .iter()
            .map(|row| row.elevation_deg)
            .fold(f32::INFINITY, f32::min);
        let sz2_doppler_rates: Vec<f32> = definition
            .rows
            .iter()
            .filter(|row| row.waveform == Waveform::Sz2ContiguousDoppler)
            .flat_map(|row| {
                row.doppler_prfs.iter().map(move |cell| match cell.value {
                    DopplerPrfValue::AzimuthRateDegPerSecond(rate) => rate,
                    DopplerPrfValue::PulseCount(_) => row.azimuth_rate_deg_per_second,
                    other => panic!("Doppler PRF value {other:?} is not handled here"),
                })
            })
            .collect();
        let mut next_row = 0;
        let mut inserted = 0;
        let mut staggered = 0;
        for executed in &capture.rows {
            #[derive(PartialEq)]
            enum Match {
                Rate,
                Period,
                Staggered,
            }
            let matched = definition.rows.get(next_row).and_then(|row| {
                let (code, sz2) = match row.waveform {
                    Waveform::ContiguousSurveillance => ("CS", false),
                    Waveform::Sz2ContiguousSurveillance => ("CS", true),
                    Waveform::ContiguousDopplerWithRangeAmbiguity => ("CD/W", false),
                    Waveform::Sz2ContiguousDoppler => ("CD/W", true),
                    Waveform::Batch => ("B", false),
                    Waveform::ContiguousDopplerWithoutRangeAmbiguity => ("CD/WO", false),
                    other => panic!("waveform {other:?} has no code"),
                };
                let elevation_ok = (executed.elevation_deg - row.elevation_deg).abs() <= 0.05;
                if elevation_ok
                    && row.waveform == Waveform::Batch
                    && executed.waveform == "SPP"
                    && executed.supplemental == 0
                {
                    return Some(Match::Staggered);
                }
                let shape_ok = elevation_ok
                    && executed.waveform == code
                    && (executed.channel == 2) == sz2
                    && ScanPlan::build24_radials(row.waveform)
                        == ScanCut::radials_for_azimuth_spacing(executed.super_res & 1 == 1);
                if !shape_ok {
                    return None;
                }
                let relative = |rate: f32| (executed.azimuth_rate / rate - 1.0).abs();
                if row.waveform == Waveform::Sz2ContiguousDoppler {
                    sz2_doppler_rates
                        .iter()
                        .any(|rate| relative(*rate) <= 0.02)
                        .then_some(Match::Rate)
                } else if relative(row.azimuth_rate_deg_per_second) <= 0.001 {
                    Some(Match::Rate)
                } else if relative(360.0 / row.source_period_seconds) <= 0.001 {
                    Some(Match::Period)
                } else {
                    None
                }
            });
            if let Some(kind) = matched {
                if let Some(row) = definition.rows.get(next_row) {
                    match kind {
                        Match::Period => result.period_only.push((
                            capture.vcp,
                            row.elevation_deg,
                            row.waveform.abbreviation(),
                        )),
                        Match::Staggered => {
                            staggered += 1;
                            result.staggered_batch.push((
                                capture.vcp,
                                row.elevation_deg,
                                executed.azimuth_rate,
                                row.azimuth_rate_deg_per_second,
                            ));
                        }
                        Match::Rate => {}
                    }
                }
                next_row += 1;
                continue;
            }
            let signed = if executed.elevation_deg > 180.0 {
                executed.elevation_deg - 360.0
            } else {
                executed.elevation_deg
            };
            failures.check(
                executed.supplemental != 0 && signed <= lowest + 0.05,
                || {
                    format!(
                        "{}: executed cut {executed:?} matches neither catalog row {next_row} nor an insertion",
                        capture.listing
                    )
                },
            );
            inserted += 1;
        }
        failures.check(next_row == definition.rows.len(), || {
            format!(
                "{}: matched {next_row} of {} catalog rows",
                capture.listing,
                definition.rows.len()
            )
        });
        failures.check(
            inserted + definition.rows.len() == capture.rows.len(),
            || format!("{}: {inserted} inserted cuts", capture.listing),
        );
        if inserted == 0 && staggered == 0 {
            let plan = ScanPlan::from_build24(definition);
            for row in &capture.chunks {
                let Some(elevation_number) = row.elevation_number else {
                    continue;
                };
                failures.check(
                    capture
                        .volume
                        .plan_chunk_number(row.chunk_id)
                        .and_then(|number| plan.chunk_position(number))
                        .map(|position| position.cut_index + 1)
                        == Some(elevation_number),
                    || {
                        format!(
                            "{} chunk {}: catalog mapping differs",
                            capture.listing, row.chunk_id
                        )
                    },
                );
            }
            result.exact_layouts += 1;
        }
        result.compared += 1;
    }
    result
        .period_only
        .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    result.period_only.dedup();
    failures.finish()?;
    Ok(result)
}

#[test]
fn build24_catalog_matches_executed_message5_sequences_fitted() -> TestResult {
    let result = compare_with_build24_catalog(Set::Fitted)?;
    println!("fitted catalog comparison: {result:?}");
    check(result.period_only == [(12, 1.3, "CD/W")], || {
        format!(
            "rows matched only through their period: {:?}",
            result.period_only
        )
    })?;
    check(
        result.compared == 21 && result.exact_layouts == 6 && result.staggered_batch.is_empty(),
        || format!("{result:?}"),
    )
}

/// Held out: no VCP 12 volume, so no period-only match. KBUF (VCP 215) ran
/// its six batch rows (1.8 to 6.4 deg) as staggered pulse pair in all three
/// volumes; the rates it reported are pinned.
#[test]
fn build24_catalog_matches_executed_message5_sequences_holdout() -> TestResult {
    let result = compare_with_build24_catalog(Set::Holdout)?;
    println!("held-out catalog comparison: {result:?}");
    check(result.period_only.is_empty(), || {
        format!(
            "rows matched only through their period: {:?}",
            result.period_only
        )
    })?;
    let mut staggered: Vec<(u16, f32, f32)> = result
        .staggered_batch
        .iter()
        .map(|(vcp, elevation, executed, _)| (*vcp, *elevation, *executed))
        .collect();
    staggered.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    staggered.dedup();
    check(
        staggered
            == [
                (215, 1.8, 15.8313),
                (215, 2.4, 19.7974),
                (215, 3.1, 18.7866),
                (215, 4.0, 20.083),
                (215, 5.1, 19.1711),
                (215, 6.4, 20.1819),
            ],
        || format!("staggered batch rows: {staggered:?}"),
    )?;
    check(
        result.compared == 24 && result.exact_layouts == 12 && result.staggered_batch.len() == 18,
        || format!("{result:?}"),
    )
}

// ---------------------------------------------------------------------------
// Sweep and chunk timing
// ---------------------------------------------------------------------------

/// One broken claim: the volume, what was claimed, and the measured value.
#[derive(Clone, Debug, PartialEq)]
struct Violation {
    listing: String,
    claim: &'static str,
    value: f64,
}

/// Asserts that `violations` are exactly `known` (listing, claim, measured
/// value within 0.05).
fn expect_violations(violations: &[Violation], known: &[(&str, &str, f64)]) -> TestResult {
    let matches = violations.len() == known.len()
        && violations
            .iter()
            .zip(known)
            .all(|(found, (listing, claim, value))| {
                found.listing == *listing
                    && found.claim == *claim
                    && (found.value - value).abs() <= 0.05
            });
    check(matches, || {
        format!("violations differ from the known list:\nfound {violations:#?}\nknown {known:#?}")
    })
}

/// One cut: (capture, elevation number, modeled sweep seconds, decoded sweep
/// seconds, start error beyond its tolerance).
type CutSweep = (Capture, usize, f64, f64, f64);

/// Per cut of every volume, the decoded rotation time and the one modeled
/// with nominal parameters.
fn sweep_timings(set: Set) -> TestResult<Vec<CutSweep>> {
    let mut cuts = Vec::new();
    for capture in captures(set)?.into_iter().flatten() {
        let model = capture.model(TimingParameters::WSR88D)?;
        for cut in capture.decoded_cuts() {
            let timing = model
                .cut_timing(cut.elevation_number - 1)
                .ok_or_else(|| fail(format!("{}: no cut timing", capture.listing)))?;
            let radials = f64::from(cut.radials);
            let decoded = seconds(cut.last_radial, cut.first_radial) * radials / (radials - 1.0);
            let decoded_start = seconds(cut.first_radial, capture.volume.volume_time);
            let start_excess =
                (timing.start_seconds - decoded_start).abs() - (0.06 * decoded_start + 2.0);
            cuts.push((
                capture.clone(),
                cut.elevation_number,
                timing.sweep_seconds,
                decoded,
                start_excess,
            ));
        }
    }
    let errors: Vec<f64> = cuts
        .iter()
        .map(|(_, _, modeled, decoded, _)| modeled / decoded - 1.0)
        .collect();
    println!(
        "{set:?} sweep duration error over {} cuts: median {:.1}%, max {:.1}%",
        cuts.len(),
        100.0 * percentile(&abs_values(&errors), 0.5),
        100.0 * percentile(&abs_values(&errors), 1.0),
    );
    Ok(cuts)
}

/// Sweep timing from azimuth rates, nominal (WSR-88D) parameters, every cut:
/// - Sweep duration `0.978 * 360 / rate` is within 6% of the decoded rotation
///   time (radial span * N / (N - 1)). Per-site decoded/commanded ratios run
///   from 0.93 (KRGX) to 1.00 (TATL), so a single scale cannot do better
///   than about 5%.
/// - Cut start is within 6% of the elapsed time plus 2 s of the decoded first
///   radial.
#[test]
fn sweep_timing_from_azimuth_rates_matches_decoded_radials_fitted() -> TestResult {
    let cuts = sweep_timings(Set::Fitted)?;
    let outside: Vec<(String, usize)> = cuts
        .iter()
        .filter(|(_, _, modeled, decoded, start_excess)| {
            (modeled / decoded - 1.0).abs() > 0.06 || *start_excess > 0.0
        })
        .map(|(capture, elevation, ..)| (capture.listing.clone(), *elevation))
        .collect();
    check(cuts.len() == 360 && outside.is_empty(), || {
        format!("{} cuts; outside tolerance: {outside:?}", cuts.len())
    })
}

/// Held out, 455 cuts. Every WSR-88D cut (8 sites, 338 cuts) meets both
/// tolerances. The TDWR cuts do not all, for two reasons:
/// - TLAS rotates 1.00 to 1.06 times its commanded time (decoded 19.05 or
///   20.06 s per cut; TDWR radial times are whole seconds), so the WSR-88D
///   scale 0.978 models its cuts 2.7% or 7.6% short, and the 20.06 s cuts
///   break the 6% tolerance. Learned statistics absorb this (see the learned
///   tests, which TLAS passes).
/// - TDEN (VCP 80) reports, for cuts 4, 13 and 23, azimuth rates that differ
///   from the rotation it executes: cut 13 (1.0 deg) is listed at 30 deg/s
///   but rotates at the 21.6 deg/s of every other cut at or below 1.0 deg,
///   cut 23 (24 deg) the other way round, and cut 4 (2.5 deg) is listed at
///   26 deg/s but rotates at 30 deg/s like cut 15 at the same elevation.
///   Those cuts are 11-45% off in all three volumes. The remaining TDEN cuts
///   are within 6.5% (1 s radial time resolution over 11-17 s rotations).
#[test]
fn sweep_timing_from_azimuth_rates_matches_decoded_radials_holdout() -> TestResult {
    let cuts = sweep_timings(Set::Holdout)?;
    let mut failures = Failures::default();
    failures.check(cuts.len() == 455, || format!("{} cuts", cuts.len()));
    let mut wsr88d = 0;
    for (capture, elevation, modeled, decoded, start_excess) in &cuts {
        let relative = modeled / decoded - 1.0;
        let within = relative.abs() <= 0.06 && *start_excess <= 0.0;
        match capture.site.as_str() {
            "TLAS" => {
                let commanded_ratio = decoded / (modeled / TimingParameters::WSR88D.rotation_scale);
                failures.check((1.0..=1.06).contains(&commanded_ratio), || {
                    format!(
                        "{} cut {elevation}: decoded/commanded {commanded_ratio:.3}",
                        capture.listing
                    )
                });
                failures.check((-0.08..=-0.026).contains(&relative), || {
                    format!(
                        "{} cut {elevation}: {:.1}%",
                        capture.listing,
                        100.0 * relative
                    )
                });
            }
            "TDEN" if [4, 13, 23].contains(elevation) => {
                failures.check((0.11..=0.45).contains(&relative.abs()), || {
                    format!(
                        "{} cut {elevation}: {:.1}%",
                        capture.listing,
                        100.0 * relative
                    )
                });
            }
            "TDEN" => failures.check(relative.abs() <= 0.065 && *start_excess <= 0.0, || {
                format!(
                    "{} cut {elevation}: {:.1}%",
                    capture.listing,
                    100.0 * relative
                )
            }),
            _ => {
                wsr88d += 1;
                failures.check(within, || {
                    format!(
                        "{} cut {elevation}: sweep {:.1}%, start excess {start_excess:.1} s",
                        capture.listing,
                        100.0 * relative
                    )
                });
            }
        }
    }
    failures.check(wsr88d == 338, || format!("{wsr88d} WSR-88D cuts"));
    failures.finish()
}

/// Chunk publication from the volume key time alone, nominal parameters,
/// NEXRAD volumes (TDWR publishes about 25 s after collection; see the
/// learned test). With `D` the modeled volume duration and `t` a chunk's
/// modeled elapsed time:
/// - the End chunk is within `0.06 * D + 3 s`;
/// - at least 90% of chunks are within `0.05 * t + 3 s`.
///
/// Build 24 catalog plans of volumes without inserted cuts use default-PRF
/// Doppler rates, so their End chunk tolerance is `0.10 * D`.
///
/// The held-out set meets every tolerance (24 NEXRAD volumes, 12 of them
/// also through the catalog plan).
fn nominal_model_predicts_chunk_publication(set: Set) -> TestResult<usize> {
    let mut failures = Failures::default();
    let mut catalog_checked = 0;
    for capture in captures(set)?
        .iter()
        .flatten()
        .filter(|capture| !capture.is_tdwr())
    {
        let model = capture.model(TimingParameters::WSR88D)?;
        let end = capture.end_plan_number()?;
        let duration = model
            .chunk_last_modified_seconds(end)
            .ok_or_else(|| fail("no End model"))?;
        let errors = chunk_errors(capture, &model);
        let end_error = errors.last().map_or(f64::NAN, |(_, error)| *error);
        failures.check(end_error.abs() <= 0.06 * duration + 3.0, || {
            format!(
                "{}: End chunk off by {end_error:.1} s over {duration:.0} s",
                capture.listing
            )
        });
        let within = errors
            .iter()
            .filter(|(number, error)| {
                let elapsed = model.chunk_last_modified_seconds(*number).unwrap_or(0.0);
                error.abs() <= 0.05 * elapsed + 3.0
            })
            .count();
        failures.check(within * 10 >= errors.len() * 9, || {
            format!(
                "{}: {within} of {} chunks within tolerance",
                capture.listing,
                errors.len()
            )
        });
        let absolute = abs_values(&errors.iter().map(|(_, e)| *e).collect::<Vec<_>>());
        let mut line = format!(
            "nominal {} vol {}: End {end_error:+.1} s of {duration:.0} s, |err| p50 {:.1} p90 {:.1}",
            capture.site,
            capture.volume.volume_id,
            percentile(&absolute, 0.5),
            percentile(&absolute, 0.9),
        );
        if let Some(definition) = build_24_definition(capture.vcp)
            && definition.rows.len() == capture.rows.len()
            && capture.rows.iter().all(|row| row.waveform != "SPP")
        {
            let catalog =
                ScanTimingModel::new(ScanPlan::from_build24(definition), TimingParameters::WSR88D)?;
            let catalog_end = seconds(
                capture.last_modified_by_plan_number(end)?,
                capture.volume.volume_time,
            ) - catalog
                .chunk_last_modified_seconds(end)
                .ok_or_else(|| fail("catalog plan lacks the End chunk"))?;
            failures.check(catalog_end.abs() <= 0.10 * duration, || {
                format!(
                    "{}: catalog plan End off by {catalog_end:.1} s",
                    capture.listing
                )
            });
            catalog_checked += 1;
            line.push_str(&format!("; catalog plan End {catalog_end:+.1} s"));
        }
        println!("{line}");
    }
    failures.finish()?;
    Ok(catalog_checked)
}

#[test]
fn nominal_model_predicts_chunk_publication_fitted() -> TestResult {
    nominal_model_predicts_chunk_publication(Set::Fitted).map(|_| ())
}

#[test]
fn nominal_model_predicts_chunk_publication_holdout() -> TestResult {
    let catalog_checked = nominal_model_predicts_chunk_publication(Set::Holdout)?;
    check(catalog_checked == 12, || {
        format!("{catalog_checked} volumes checked through the catalog plan")
    })
}

/// Learned statistics, out of sample. For each site, volume n is predicted
/// from its key time with statistics learned from volumes before n (n = 1,
/// 2; TDWR included):
/// - the End chunk (AVSET cutoff, as a plan chunk number) is exact;
/// - the End chunk's `LastModified` is within 6 s;
/// - the median chunk error is within 2 s;
/// - the 90th percentile is within 7 s (publication backlogs reached 15 s).
///
/// Summed over all predictions, the learned median error is below the
/// nominal one.
fn learned_statistics_predict_the_next_volume(set: Set) -> TestResult<Vec<Violation>> {
    let mut violations = Vec::new();
    let mut learned_medians = 0.0;
    let mut nominal_medians = 0.0;
    for volumes in &captures(set)? {
        let mut statistics = TimingStatistics::default();
        for (index, capture) in volumes.iter().enumerate() {
            if index > 0 {
                let model = statistics.model(capture.plan())?;
                let end = capture.end_plan_number()?;
                let expected_end = statistics.expected_end_chunk_id(&capture.plan());
                if expected_end != Some(end) {
                    violations.push(Violation {
                        listing: capture.listing.clone(),
                        claim: "learned End chunk",
                        value: expected_end.map_or(f64::NAN, f64::from),
                    });
                }
                let errors: Vec<f64> = chunk_errors(capture, &model)
                    .into_iter()
                    .map(|(_, e)| e)
                    .collect();
                let end_error = errors.last().copied().unwrap_or(f64::NAN);
                let median = percentile(&abs_values(&errors), 0.5);
                let p90 = percentile(&abs_values(&errors), 0.9);
                for (claim, value, limit) in [
                    ("End chunk error", end_error.abs(), 6.0),
                    ("median chunk error", median, 2.0),
                    ("p90 chunk error", p90, 7.0),
                ] {
                    if value.is_nan() || value > limit {
                        violations.push(Violation {
                            listing: capture.listing.clone(),
                            claim,
                            value,
                        });
                    }
                }
                let nominal: Vec<f64> =
                    chunk_errors(capture, &capture.model(TimingParameters::WSR88D)?)
                        .into_iter()
                        .map(|(_, e)| e)
                        .collect();
                learned_medians += median;
                nominal_medians += percentile(&abs_values(&nominal), 0.5);
                println!(
                    "learned {} vol {} from {} volume(s): End {end_error:+.1} s, |err| p50 {median:.1} p90 {p90:.1}; parameters {:?}",
                    capture.site,
                    capture.volume.volume_id,
                    statistics.volumes_observed(),
                    statistics.parameters()
                );
            }
            statistics.observe_volume(&capture.plan(), &capture.volume)?;
        }
    }
    println!(
        "{set:?} sum of median |err|: learned {learned_medians:.1} s, nominal {nominal_medians:.1} s"
    );
    if learned_medians >= nominal_medians {
        violations.push(Violation {
            listing: String::new(),
            claim: "learned below nominal",
            value: learned_medians - nominal_medians,
        });
    }
    Ok(violations)
}

#[test]
fn learned_statistics_predict_the_next_volume_fitted() -> TestResult {
    expect_violations(
        &learned_statistics_predict_the_next_volume(Set::Fitted)?,
        &[],
    )
}

/// Held out, 20 predictions. Three break a claim:
/// - KBUF 55: AVSET raised the top elevation between volumes (volume 54
///   collected 16 elevations, 55 collected 17), so the End chunk learned from
///   54 (plan chunk 79) misses the actual 82. Its timing claims hold.
/// - TDEN 644 and 645 (VCP 80): median chunk error 2.8 s and 2.6 s. The
///   three cuts whose Message 5 azimuth rates differ from the executed
///   rotation (see the sweep test) displace the chunks after them.
#[test]
fn learned_statistics_predict_the_next_volume_holdout() -> TestResult {
    expect_violations(
        &learned_statistics_predict_the_next_volume(Set::Holdout)?,
        &[
            ("KBUF/055-20260917-025235.xml", "learned End chunk", 79.0),
            ("TDEN/644-20260917-025705.xml", "median chunk error", 2.8),
            ("TDEN/645-20260917-030305.xml", "median chunk error", 2.6),
        ],
    )
}

/// Volume rollover. Projecting a completed volume gives the next volume's key
/// time:
/// - learned statistics that include at least one earlier rollover (volume 2
///   predicted from volume 1): within 3 s;
/// - nominal parameters for NEXRAD (volumes 1 and 2 predicted from 0 and 1):
///   within 6 s.
///
/// The next Start chunk follows at the learned Start offset: within 3 s of
/// its actual `LastModified`, whose volume also appears in the fixtures.
fn projections_predict_the_next_volume(set: Set) -> TestResult<Vec<Violation>> {
    let mut violations = Vec::new();
    for volumes in &captures(set)? {
        let mut statistics = TimingStatistics::default();
        for (index, capture) in volumes.iter().enumerate() {
            statistics.observe_volume(&capture.plan(), &capture.volume)?;
            let Some(next) = volumes.get(index + 1) else {
                continue;
            };
            if !capture.is_tdwr() {
                let nominal = capture
                    .model(TimingParameters::WSR88D)?
                    .project(&capture.volume, None);
                let error = seconds(next.volume.volume_time, nominal.expected_next_volume_time);
                if error.abs() > 6.0 {
                    violations.push(Violation {
                        listing: capture.listing.clone(),
                        claim: "nominal next volume",
                        value: error,
                    });
                }
                println!(
                    "nominal rollover {} {} -> {}: {error:+.1} s",
                    capture.site, capture.volume.volume_id, next.volume.volume_id
                );
            }
            if index >= 1 {
                let projection = statistics.project(capture.plan(), &capture.volume)?;
                if !(projection.complete && projection.next_chunk().is_none()) {
                    violations.push(Violation {
                        listing: capture.listing.clone(),
                        claim: "complete",
                        value: 0.0,
                    });
                }
                let error = seconds(
                    next.volume.volume_time,
                    projection.expected_next_volume_time,
                );
                let start_error =
                    seconds(next.last_modified(1)?, projection.expected_next_start_chunk);
                for (claim, value) in [
                    ("learned next volume", error),
                    ("learned next Start chunk", start_error),
                ] {
                    if value.abs() > 3.0 {
                        violations.push(Violation {
                            listing: capture.listing.clone(),
                            claim,
                            value,
                        });
                    }
                }
                println!(
                    "learned rollover {} {} -> {}: key {error:+.1} s, Start chunk {start_error:+.1} s",
                    capture.site, capture.volume.volume_id, next.volume.volume_id
                );
            }
        }
    }
    Ok(violations)
}

#[test]
fn projections_predict_the_next_volume_fitted() -> TestResult {
    expect_violations(&projections_predict_the_next_volume(Set::Fitted)?, &[])
}

/// Held out, 10 learned and 16 nominal rollovers. Two learned rollovers break
/// the 3 s claim; every nominal one holds:
/// - KMUX 480 -> 481: the key time came 3.4 s later than projected, and the
///   Start chunk with it.
/// - TLAS 999 -> 1: the key time is within 3 s (-2.3 s), but TDWR Start
///   chunks trail the key time by a varying amount (28, 34 and 27 s for
///   volumes 998, 999 and 1), so the Start chunk came 6.3 s before the
///   learned median offset.
#[test]
fn projections_predict_the_next_volume_holdout() -> TestResult {
    expect_violations(
        &projections_predict_the_next_volume(Set::Holdout)?,
        &[
            ("KMUX/480-20260917-024709.xml", "learned next volume", 3.4),
            (
                "KMUX/480-20260917-024709.xml",
                "learned next Start chunk",
                3.4,
            ),
            (
                "TLAS/999-20260917-013443.xml",
                "learned next Start chunk",
                -6.3,
            ),
        ],
    )
}

/// Remaining-volume projection replayed on real listings. At every distinct
/// `LastModified` before the End chunk, the volume is cut to what a listing
/// would have shown and projected.
///
/// With statistics learned from the previous volumes (volumes 1 and 2):
/// - the projected End chunk (plan chunk number) is exact at every instant;
/// - the next chunk is within 4 s at 90% or more of instants;
/// - the End chunk is within 6 s at 90% or more of instants and within 10 s
///   at all of them.
///
/// With nominal parameters on the first volume (NEXRAD volumes that collect
/// the whole plan), the End chunk is within `0.03 * D + 3 s` at 90% or more
/// of instants and within `0.05 * D + 3 s` at all.
fn projections_replay_real_listings(set: Set) -> TestResult<Vec<Violation>> {
    let mut violations = Vec::new();
    for volumes in &captures(set)? {
        let mut statistics = TimingStatistics::default();
        for (index, capture) in volumes.iter().enumerate() {
            let end_id = capture.end_chunk_id()?;
            let end = capture.end_plan_number()?;
            let actual_end = capture.last_modified(end_id)?;
            let duration = seconds(actual_end, capture.volume.volume_time);
            let mut instants: Vec<DateTime<Utc>> = capture
                .volume
                .chunks
                .iter()
                .map(|chunk| chunk.last_modified)
                .filter(|at| *at < actual_end)
                .collect();
            instants.sort();
            instants.dedup();
            let nominal_eligible = index == 0
                && !capture.is_tdwr()
                && capture.plan().full_end_chunk_id() == u32::from(end);
            let mut next_errors = Vec::new();
            let mut end_errors = Vec::new();
            let mut nominal_end_errors = Vec::new();
            let mut wrong_end = 0;
            for at in instants {
                let observed = capture.volume.observed_by(at);
                if index > 0 {
                    let projection = statistics.project(capture.plan(), &observed)?;
                    if projection.expected_end_plan_chunk_number != end {
                        wrong_end += 1;
                    }
                    if let Some(next) = projection.next_chunk()
                        && let Some(number) = next.plan_chunk_number
                    {
                        next_errors.push(seconds(
                            capture.last_modified_by_plan_number(number)?,
                            next.expected_last_modified,
                        ));
                    }
                    end_errors.push(seconds(actual_end, projection.expected_volume_end));
                }
                if nominal_eligible {
                    let projection = capture
                        .model(TimingParameters::WSR88D)?
                        .project(&observed, None);
                    nominal_end_errors.push(seconds(actual_end, projection.expected_volume_end));
                }
            }
            let mut push = |claim, value: f64, limit: f64| {
                if value.is_nan() || value > limit {
                    violations.push(Violation {
                        listing: capture.listing.clone(),
                        claim,
                        value,
                    });
                }
            };
            if index > 0 {
                let next_p90 = percentile(&abs_values(&next_errors), 0.9);
                let end_p90 = percentile(&abs_values(&end_errors), 0.9);
                let end_max = percentile(&abs_values(&end_errors), 1.0);
                push("instants with a wrong End chunk", f64::from(wrong_end), 0.0);
                push("next chunk p90", next_p90, 4.0);
                push("End p90", end_p90, 6.0);
                push("End max", end_max, 10.0);
                println!(
                    "learned replay {} vol {} ({} instants): next chunk |err| p50 {:.1} p90 {next_p90:.1} max {:.1}; End |err| p50 {:.1} p90 {end_p90:.1} max {end_max:.1}; wrong End chunk at {wrong_end}",
                    capture.site,
                    capture.volume.volume_id,
                    end_errors.len(),
                    percentile(&abs_values(&next_errors), 0.5),
                    percentile(&abs_values(&next_errors), 1.0),
                    percentile(&abs_values(&end_errors), 0.5),
                );
            }
            if nominal_eligible {
                let p90 = percentile(&abs_values(&nominal_end_errors), 0.9);
                let max = percentile(&abs_values(&nominal_end_errors), 1.0);
                push("nominal End p90", p90, 0.03 * duration + 3.0);
                push("nominal End max", max, 0.05 * duration + 3.0);
                println!(
                    "nominal replay {} vol {}: End |err| p90 {p90:.1} max {max:.1} over {duration:.0} s",
                    capture.site, capture.volume.volume_id
                );
            }
            statistics.observe_volume(&capture.plan(), &capture.volume)?;
        }
    }
    Ok(violations)
}

#[test]
fn projections_replay_real_listings_fitted() -> TestResult {
    expect_violations(&projections_replay_real_listings(Set::Fitted)?, &[])
}

/// Held out, 20 learned and 5 nominal replays. The nominal replays hold; four
/// learned replays break a claim:
/// - KBUF 55 (AVSET change, see the learned test): the End chunk stays at the
///   learned plan chunk 79 until chunks past it appear, at 79 of 81
///   instants, and the End error is 14.7 s at p90 (14.8 s at worst).
/// - PAHG 764: next chunk 4.1 s at p90.
/// - TDEN 644 and 645 (VCP 80 rate mismatches): next chunk 5.6 s and 6.0 s
///   at p90; End 6.6 s and 8.2 s at p90, and 10.1 s at worst in 645.
#[test]
fn projections_replay_real_listings_holdout() -> TestResult {
    expect_violations(
        &projections_replay_real_listings(Set::Holdout)?,
        &[
            (
                "KBUF/055-20260917-025235.xml",
                "instants with a wrong End chunk",
                79.0,
            ),
            ("KBUF/055-20260917-025235.xml", "End p90", 14.7),
            ("KBUF/055-20260917-025235.xml", "End max", 14.8),
            ("PAHG/764-20260917-025916.xml", "next chunk p90", 4.1),
            ("TDEN/644-20260917-025705.xml", "next chunk p90", 5.6),
            ("TDEN/644-20260917-025705.xml", "End p90", 6.6),
            ("TDEN/645-20260917-030305.xml", "next chunk p90", 6.0),
            ("TDEN/645-20260917-030305.xml", "End p90", 8.2),
            ("TDEN/645-20260917-030305.xml", "End max", 10.1),
        ],
    )
}

// ---------------------------------------------------------------------------
// Single-volume checks
// ---------------------------------------------------------------------------

/// The committed listing of KIWA volume 307 is the volume whose 70 chunks
/// TD.1 downloaded. Every object size matches the testdata manifest, the
/// sizes sum to the archive twin `l2-kiwa-20260917-003629`, and the
/// Start/End `LastModified` match the TD.1 record.
#[test]
fn kiwa_307_listing_matches_the_td1_chunk_capture() -> TestResult {
    let objects = parse_chunk_listing(&fs::read_to_string(
        Set::Fitted.dir().join("KIWA/307-20260917-003629.xml"),
    )?)?;
    let manifest = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/level2/manifest.toml"),
    )?;
    let mut manifest_sizes: BTreeMap<String, u64> = BTreeMap::new();
    for entry in toml_blocks(&manifest, "[[file]]") {
        if let (Some(id), Some(size)) = (entry.get("id"), entry.get("size")) {
            manifest_sizes.insert((*id).to_owned(), size.parse()?);
        }
    }
    check(objects.len() == 70, || format!("{} objects", objects.len()))?;
    for object in &objects {
        let id = format!(
            "l2chunk-kiwa-307-20260917-003629-{:03}-{}",
            object.chunk_id,
            chunk_type_code(object.chunk_type).to_lowercase()
        );
        let expected = manifest_sizes
            .get(&id)
            .ok_or_else(|| fail(format!("testdata manifest has no {id}")))?;
        check(object.object.size == *expected, || {
            format!(
                "{id}: listing {} bytes, manifest {expected}",
                object.object.size
            )
        })?;
    }
    let total: u64 = objects.iter().map(|object| object.object.size).sum();
    check(
        manifest_sizes.get("l2-kiwa-20260917-003629") == Some(&total),
        || format!("chunk sizes sum to {total}"),
    )?;
    let volume = VolumeObservation::from_chunks(objects)
        .into_iter()
        .next()
        .ok_or_else(|| fail("no volume"))?;
    check(
        volume.chunk(1).map(|chunk| chunk.last_modified)
            == Some(parse_time("2026-09-17T00:36:31Z")?)
            && volume.chunk(70).map(|chunk| chunk.last_modified)
                == Some(parse_time("2026-09-17T00:41:50Z")?),
        || "Start/End LastModified differ from the TD.1 record".to_owned(),
    )
}

/// KIWA 307 at 00:38:00Z, 91 s into the volume. A listing then shows chunks
/// 1-22, and chunk 23 is next, in cut 4 (0.88 deg SZ-2 Doppler). The
/// projection uses statistics learned from volume 306 and must:
/// - name as the current cut the one whose radials the next chunk holds;
/// - put that cut's completion within 6 s of its last chunk's real
///   `LastModified`;
/// - list cuts in plan order, with chunk ranges that tile chunks 2-70.
///
/// Accuracy across every instant, including publication backlogs, is
/// checked by `projections_replay_real_listings`.
#[test]
fn kiwa_307_mid_volume_projection_names_the_current_cut() -> TestResult {
    let sites = captures(Set::Fitted)?;
    let (previous, capture) = match find_site(&sites, "KIWA")? {
        [previous, capture, ..] => (previous, capture),
        _ => return Err(fail("KIWA needs two volumes")),
    };
    let mut statistics = TimingStatistics::default();
    statistics.observe_volume(&previous.plan(), &previous.volume)?;

    let at = parse_time("2026-09-17T00:38:00Z")?;
    let observed = capture.volume.observed_by(at);
    let projection = statistics.project(capture.plan(), &observed)?;
    let next = projection
        .next_chunk()
        .ok_or_else(|| fail("volume already complete"))?;
    let current = projection
        .current_cut()
        .ok_or_else(|| fail("no current cut"))?;
    let decoded = capture
        .chunks
        .iter()
        .find(|row| row.chunk_id == next.chunk_id)
        .ok_or_else(|| fail("next chunk not in the CSV"))?;
    check(
        current.cut_index + 1 == decoded.elevation_number.unwrap_or(0),
        || {
            format!(
                "current cut {} vs decoded elevation {:?}",
                current.cut_index + 1,
                decoded.elevation_number
            )
        },
    )?;
    let actual_complete = capture.last_modified(current.last_chunk_id)?;
    let error = seconds(actual_complete, current.expected_complete);
    check(error.abs() <= 6.0, || {
        format!("current cut completion off by {error:.1} s")
    })?;

    let mut expected_first = 2;
    for (index, cut) in projection.cuts.iter().enumerate() {
        check(
            cut.cut_index == index && cut.first_chunk_id == expected_first,
            || format!("cut {index} starts at chunk {}", cut.first_chunk_id),
        )?;
        expected_first = cut.last_chunk_id + 1;
    }
    check(
        expected_first == projection.expected_end_chunk_id + 1
            && projection.expected_end_chunk_id == 70,
        || format!("cuts end at {}", expected_first - 1),
    )?;
    println!(
        "KIWA 307 at {at}: next chunk {} expected {}, current cut {} ({:.2} deg) completes {} (actual {actual_complete}), End expected {} (actual {})",
        next.chunk_id,
        next.expected_last_modified,
        current.cut_index + 1,
        current.elevation_deg,
        current.expected_complete,
        projection.expected_volume_end,
        capture.last_modified(70)?,
    );
    Ok(())
}

/// Learning rejects volumes the plan does not describe and leaves the
/// statistics untouched:
/// - a volume still in progress (KIWA 306 cut at 00:33:10Z);
/// - a KAMA VCP 35 volume paired with the KMAX VCP 212 plan, whose End chunk
///   falls inside a cut;
/// - a KGRB VCP 34 volume paired with the KRGX VCP 12 plan (49 chunks close
///   a cut, but the fit is implausible).
#[test]
fn learning_rejects_volumes_the_plan_does_not_describe() -> TestResult {
    let sites = captures(Set::Fitted)?;
    let first = |site: &str| -> TestResult<&Capture> {
        find_site(&sites, site)?
            .first()
            .ok_or_else(|| fail(format!("no {site} capture")))
    };
    let (kiwa, kama, kmax, kgrb, krgx) = (
        first("KIWA")?,
        first("KAMA")?,
        first("KMAX")?,
        first("KGRB")?,
        first("KRGX")?,
    );
    let mut statistics = TimingStatistics::default();
    let before = statistics.clone();

    let partial = kiwa.volume.observed_by(parse_time("2026-09-17T00:33:10Z")?);
    check(
        matches!(
            statistics.observe_volume(&kiwa.plan(), &partial),
            Err(TimingError::IncompleteVolume { .. })
        ),
        || "partial volume accepted".to_owned(),
    )?;
    let wrong_plan = statistics.observe_volume(&kmax.plan(), &kama.volume);
    check(
        matches!(
            wrong_plan,
            Err(TimingError::PlanMismatch { .. } | TimingError::ImplausibleFit { .. })
        ),
        || format!("KAMA volume with KMAX plan: {wrong_plan:?}"),
    )?;
    let wrong_plan = statistics.observe_volume(&krgx.plan(), &kgrb.volume);
    check(
        matches!(
            wrong_plan,
            Err(TimingError::PlanMismatch { .. } | TimingError::ImplausibleFit { .. })
        ),
        || format!("KGRB volume with KRGX plan: {wrong_plan:?}"),
    )?;
    check(statistics == before, || {
        "rejected volumes changed the statistics".to_owned()
    })
}

/// TLAS 998, 999 and 1 (held out): the gap between volumes is learned across
/// the 999 -> 1 id wrap as it is between 998 and 999. For each rollover the
/// fit reports the next key time minus the modeled end of the previous
/// volume's last cut, recomputed here from that volume's fit and its
/// Message 5 plan.
#[test]
fn inter_volume_gap_is_learned_across_the_999_to_1_wrap() -> TestResult {
    let sites = captures(Set::Holdout)?;
    let tlas = find_site(&sites, "TLAS")?;
    let ids: Vec<u16> = tlas
        .iter()
        .map(|capture| capture.volume.volume_id)
        .collect();
    check(ids == [998, 999, 1], || format!("TLAS volumes {ids:?}"))?;
    let mut statistics = TimingStatistics::default();
    let mut fits = Vec::new();
    for capture in tlas {
        fits.push(statistics.observe_volume(&capture.plan(), &capture.volume)?);
    }
    check(fits[0].inter_volume_gap_seconds.is_none(), || {
        format!("gap before any rollover: {:?}", fits[0])
    })?;
    let mut gaps = Vec::new();
    for pair in 0..2 {
        let (previous, next) = (&tlas[pair], &tlas[pair + 1]);
        let fit = &fits[pair];
        let plan = previous.plan();
        let end_cut = plan
            .chunk_position(fit.end_plan_chunk_number)
            .ok_or_else(|| fail("End outside the plan"))?
            .cut_index;
        let commanded: f64 = plan
            .cuts
            .iter()
            .take(end_cut + 1)
            .map(|cut| 360.0 / f64::from(cut.azimuth_rate_deg_per_second))
            .sum();
        let radials_end = fit.rotation_scale * commanded
            + TimingParameters::WSR88D.inter_cut_gap_seconds * end_cut as f64;
        let expected = seconds(next.volume.volume_time, previous.volume.volume_time) - radials_end;
        let learned = fits[pair + 1].inter_volume_gap_seconds;
        check(
            learned.is_some_and(|gap| (gap - expected).abs() < 1e-6 && (0.0..60.0).contains(&gap)),
            || {
                format!(
                    "{} -> {}: learned gap {learned:?}, expected {expected:.3} s",
                    previous.volume.volume_id, next.volume.volume_id
                )
            },
        )?;
        gaps.push(expected);
    }
    let median = (gaps[0] + gaps[1]) / 2.0;
    println!(
        "TLAS inter-volume gaps: 998 -> 999 {:.2} s, 999 -> 1 {:.2} s",
        gaps[0], gaps[1]
    );
    check(
        (statistics.parameters().inter_volume_gap_seconds - median).abs() < 1e-6,
        || format!("learned {:?}", statistics.parameters()),
    )
}

/// KMUX 480 (held out) published three status-only chunks (ids 20, 33 and
/// 46: 127-130 bytes, one Message 2 each, no radials) between cuts. Plan
/// chunk numbers skip them:
/// - chunk 21 is plan chunk 20, the first chunk of cut 4 (decoded elevation
///   4), and the End chunk 70 is plan chunk 67, the plan's last;
/// - statistics learned from volume 479 (no status-only chunks) accept the
///   volume and predict End plan chunk 67;
/// - projected right after chunk 21 was listed, the End chunk is expected at
///   id 68 (plan 67 plus the one status-only chunk seen so far) and the next
///   chunk, id 22, is plan chunk 21.
#[test]
fn status_only_chunks_do_not_shift_plan_chunk_numbers() -> TestResult {
    let sites = captures(Set::Holdout)?;
    let kmux = find_site(&sites, "KMUX")?;
    let (previous, capture) = match kmux {
        [previous, capture, ..] => (previous, capture),
        _ => return Err(fail("KMUX needs two volumes")),
    };
    check(capture.volume.volume_id == 480, || {
        format!("volume {}", capture.volume.volume_id)
    })?;
    let status_rows: Vec<u16> = capture
        .chunks
        .iter()
        .filter(|row| row.radials == 0 && row.other_messages == "2x1")
        .map(|row| row.chunk_id)
        .collect();
    check(status_rows == [20, 33, 46], || format!("{status_rows:?}"))?;
    check(capture.volume.status_only_chunks() == 3, || {
        format!("{} status-only chunks", capture.volume.status_only_chunks())
    })?;
    let volume = &capture.volume;
    let plan = capture.plan();
    check(
        volume.plan_chunk_number(20).is_none()
            && volume.plan_chunk_number(21) == Some(20)
            && volume.plan_chunk_number(70) == Some(67)
            && plan.full_end_chunk_id() == 67,
        || "plan chunk numbers".to_owned(),
    )?;
    let row_21 = capture
        .chunks
        .iter()
        .find(|row| row.chunk_id == 21)
        .ok_or_else(|| fail("chunk 21"))?;
    check(
        plan.chunk_position(20)
            .map(|position| (position.cut_index + 1, position.part))
            == row_21.elevation_number.map(|elevation| (elevation, 0)),
        || format!("chunk 21 decoded elevation {:?}", row_21.elevation_number),
    )?;

    let mut statistics = TimingStatistics::default();
    let previous_fit = statistics.observe_volume(&previous.plan(), &previous.volume)?;
    check(previous_fit.status_only_chunks == 0, || {
        format!("{previous_fit:?}")
    })?;
    check(statistics.expected_end_chunk_id(&plan) == Some(67), || {
        format!("{:?}", statistics.expected_end_chunk_id(&plan))
    })?;

    let at = capture.last_modified(21)?;
    let observed = volume.observed_by(at);
    let projection = statistics.project(plan.clone(), &observed)?;
    let next = projection
        .next_chunk()
        .ok_or_else(|| fail("no next chunk"))?;
    check(
        projection.status_only_chunks == 1
            && projection.expected_end_plan_chunk_number == 67
            && projection.expected_end_chunk_id == 68
            && next.chunk_id == 22
            && next.plan_chunk_number == Some(21),
        || format!("{projection:#?}"),
    )?;

    let fit = statistics.observe_volume(&plan, volume)?;
    check(
        fit.end_chunk_id == 70 && fit.end_plan_chunk_number == 67 && fit.status_only_chunks == 3,
        || format!("{fit:?}"),
    )
}

/// A [`ScanPlan`] from the Message 5 that `recast-radar-io-nexrad` decodes
/// out of a real-time Start chunk: elevation, azimuth rate and the
/// half-degree azimuth bit of each cut. This crate does not depend on the
/// decoder, so callers write this mapping themselves.
fn plan_from_start_chunk(bytes: &[u8]) -> TestResult<ScanPlan> {
    let metadata = recast_radar_io_nexrad::NexradMetadata::from_metadata_record(bytes);
    let vcp = metadata
        .vcp
        .ok_or_else(|| fail(format!("no Message 5: {:?}", metadata.errors)))?;
    Ok(ScanPlan::new(
        Some(vcp.pattern_number),
        vcp.cuts
            .iter()
            .map(|cut| {
                ScanCut::new(
                    cut.elevation_angle_deg,
                    cut.azimuth_rate_deg_per_s,
                    ScanCut::radials_for_azimuth_spacing(
                        cut.super_resolution.half_degree_azimuth(),
                    ),
                )
            })
            .collect(),
    ))
}

/// The Rust Message 5 decoder (`recast_radar_io_nexrad::NexradMetadata`) on
/// real Start chunks gives the plans the tests take from the Python capture
/// tables: KIWA 307 (WSR-88D VCP 215 with SAILS; testdata
/// `l2chunk-kiwa-307-20260917-003629-001-s`) and TLAS 998 and 999 (TDWR VCP
/// 90; downloaded by the `tlas-chunk-too-large` cassette). Same VCP, cut
/// count and radial counts; elevations and azimuth rates within 0.001 (the
/// tables print four decimals). With the decoded plan, every KIWA 307 radial
/// chunk maps to its decoded elevation.
#[test]
fn rust_message5_decoder_gives_the_captured_plans() -> TestResult {
    let kiwa_start = match recast_radar_testdata::path("l2chunk-kiwa-307-20260917-003629-001-s") {
        Ok(path) => path,
        Err(err) if err.is_offline() => {
            eprintln!("skipping: {err}");
            return Ok(());
        }
        Err(err) => return Err(fail(err.to_string())),
    };
    let fitted = captures(Set::Fitted)?;
    let holdout = captures(Set::Holdout)?;
    let kiwa_307 = find_site(&fitted, "KIWA")?
        .iter()
        .find(|capture| capture.volume.volume_id == 307)
        .ok_or_else(|| fail("KIWA 307"))?;
    let tlas = find_site(&holdout, "TLAS")?;
    let cases = [
        (fs::read(&kiwa_start)?, kiwa_307),
        (
            recast_radar_testdata::bytes("l2chunk-tlas-998-20260917-012843-001-s")?,
            &tlas[0],
        ),
        (
            recast_radar_testdata::bytes("l2chunk-tlas-999-20260917-013443-001-s")?,
            &tlas[1],
        ),
    ];
    for (bytes, capture) in &cases {
        let decoded = plan_from_start_chunk(bytes)?;
        let captured = capture.plan();
        check(
            decoded.vcp == captured.vcp && decoded.cuts.len() == captured.cuts.len(),
            || {
                format!(
                    "{}: VCP {:?} with {} cuts, captured {:?} with {}",
                    capture.listing,
                    decoded.vcp,
                    decoded.cuts.len(),
                    captured.vcp,
                    captured.cuts.len()
                )
            },
        )?;
        for (index, (rust, python)) in decoded.cuts.iter().zip(&captured.cuts).enumerate() {
            check(
                (rust.signed_elevation_deg() - python.signed_elevation_deg()).abs() <= 1e-3
                    && (rust.azimuth_rate_deg_per_second - python.azimuth_rate_deg_per_second)
                        .abs()
                        <= 1e-3
                    && rust.radials == python.radials,
                || {
                    format!(
                        "{} cut {}: {rust:?} vs {python:?}",
                        capture.listing,
                        index + 1
                    )
                },
            )?;
        }
    }
    let decoded = plan_from_start_chunk(&cases[0].0)?;
    for row in &kiwa_307.chunks {
        let position = kiwa_307
            .volume
            .plan_chunk_number(row.chunk_id)
            .and_then(|number| decoded.chunk_position(number));
        check(
            position.map(|position| position.cut_index + 1) == row.elevation_number,
            || {
                format!(
                    "KIWA 307 chunk {}: {position:?} vs {:?}",
                    row.chunk_id, row.elevation_number
                )
            },
        )?;
    }
    Ok(())
}
