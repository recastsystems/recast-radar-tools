//! Chunk timing model checked against real chunk listings.
//!
//! Fixtures are in `tests/fixtures/chunks/`, written by
//! `tools/capture_chunk_listings.py`. There are three consecutive complete
//! volumes from each of eight sites, captured 2026-09-17 00:30-01:31Z from
//! `unidata-nexrad-level2-chunks`:
//! - clear air: KAMA (VCP 35), KDGX (VCP 35 with base tilt and SAILS), KGRB
//!   (VCP 34);
//! - precipitation: KRGX (VCP 12 with base tilt, AVSET), KMAX (VCP 212 with
//!   base tilt and SAILS, full volumes), KEAX (VCP 212 with MESO-SAILS x2,
//!   AVSET), KIWA (VCP 215 with SAILS, AVSET; volume 307 is the TD.1 chunk
//!   capture);
//! - TDWR: TATL (VCP 90, which has no Build 24 definition).
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
//! The defaults in `TimingParameters::WSR88D` were measured on KIWA 307 and
//! 308 only. Checks of the nominal model are therefore out of sample on the
//! other seven sites. Checks of learned statistics always predict a volume
//! that the statistics have not seen. Each tolerance is stated on its test.
//! Measured errors print with
//! `cargo test -p recast-radar-data --test timing -- --nocapture`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use recast_radar_data::RealtimeChunkType;
use recast_radar_data::realtime::timing::{
    ScanCut, ScanPlan, ScanTimingModel, TimingError, TimingParameters, TimingStatistics,
    VolumeObservation, parse_chunk_listing,
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

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chunks")
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
    last_modified: String,
    radials: u16,
    elevation_number: usize,
    first_radial: DateTime<Utc>,
    last_radial: DateTime<Utc>,
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

    fn last_modified(&self, chunk_id: u16) -> TestResult<DateTime<Utc>> {
        self.volume
            .chunk(chunk_id)
            .map(|chunk| chunk.last_modified)
            .ok_or_else(|| fail(format!("{} has no chunk {chunk_id}", self.listing)))
    }

    fn decoded_cuts(&self) -> Vec<DecodedCut> {
        let mut cuts: BTreeMap<usize, DecodedCut> = BTreeMap::new();
        for chunk in &self.chunks {
            let cut = cuts.entry(chunk.elevation_number).or_insert(DecodedCut {
                elevation_number: chunk.elevation_number,
                radials: 0,
                first_radial: chunk.first_radial,
                last_radial: chunk.last_radial,
            });
            cut.radials += u32::from(chunk.radials);
            cut.first_radial = cut.first_radial.min(chunk.first_radial);
            cut.last_radial = cut.last_radial.max(chunk.last_radial);
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
            Ok(ChunkRow {
                chunk_id: field(row, "chunk_id")?.parse()?,
                chunk_type: field(row, "type")?.to_owned(),
                last_modified: field(row, "last_modified")?.to_owned(),
                radials: field(row, "radials")?.parse()?,
                elevation_number: field(row, "elevation_numbers")?.parse()?,
                first_radial: parse_time(field(row, "first_radial_time")?)?,
                last_radial: parse_time(field(row, "last_radial_time")?)?,
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

/// Every manifest volume, grouped by site, in volume order.
fn captures() -> TestResult<Vec<Vec<Capture>>> {
    let dir = fixtures_dir();
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
    }
}

/// `LastModified` minus the model's expectation, per chunk, in seconds.
fn chunk_errors(capture: &Capture, model: &ScanTimingModel) -> Vec<(u16, f64)> {
    capture
        .volume
        .chunks
        .iter()
        .filter_map(|chunk| {
            let modeled = model.chunk_last_modified_seconds(chunk.chunk_id)?;
            Some((
                chunk.chunk_id,
                seconds(chunk.last_modified, capture.volume.volume_time) - modeled,
            ))
        })
        .collect()
}

/// Fixture integrity:
/// - each listing is one complete volume;
/// - listings, decoded chunk tables and the manifest agree;
/// - volumes are consecutive per site;
/// - the key time is the first radial time cut to whole seconds.
#[test]
fn fixtures_are_complete_consecutive_real_volumes() -> TestResult {
    let sites = captures()?;
    check(sites.len() == 8, || {
        format!("{} sites, expected 8", sites.len())
    })?;
    for volumes in &sites {
        check(volumes.len() == 3, || {
            format!("{} volumes at a site", volumes.len())
        })?;
        for pair in volumes.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            check(
                (earlier.volume.volume_id + 1) % 1000 == later.volume.volume_id
                    && later.volume.volume_time > earlier.volume.volume_time,
                || format!("{} -> {} not consecutive", earlier.listing, later.listing),
            )?;
        }
        for capture in volumes {
            let volume = &capture.volume;
            check(volume.is_complete(), || {
                format!("{} incomplete", capture.listing)
            })?;
            check(
                volume.site == capture.site
                    && capture.listed_objects == capture.manifest_chunks
                    && volume.chunks.len() == capture.manifest_chunks
                    && capture.chunks.len() + 1 == volume.chunks.len(),
                || format!("{}: chunk counts disagree", capture.listing),
            )?;
            for (row, chunk) in capture.chunks.iter().zip(volume.chunks.iter().skip(1)) {
                check(
                    row.chunk_id == chunk.chunk_id
                        && parse_time(&row.last_modified)? == chunk.last_modified
                        && row.chunk_type == chunk_type_code(chunk.chunk_type),
                    || {
                        format!(
                            "{} chunk {}: CSV and listing disagree",
                            capture.listing, row.chunk_id
                        )
                    },
                )?;
            }
            let radials: u32 = capture
                .chunks
                .iter()
                .map(|row| u32::from(row.radials))
                .sum();
            check(radials == capture.manifest_radials, || {
                format!("{}: {radials} radials", capture.listing)
            })?;
            let first = capture
                .chunks
                .first()
                .ok_or_else(|| fail(format!("{} has no radial chunks", capture.listing)))?;
            let lead = seconds(first.first_radial, volume.volume_time);
            check((0.0..1.0).contains(&lead), || {
                format!(
                    "{}: first radial {lead} s after the key time",
                    capture.listing
                )
            })?;
        }
    }
    Ok(())
}

/// The committed listing of KIWA volume 307 is the volume whose 70 chunks
/// TD.1 downloaded. Every object size matches the testdata manifest, the
/// sizes sum to the archive twin `l2-kiwa-20260917-003629`, and the
/// Start/End `LastModified` match the TD.1 record.
#[test]
fn kiwa_307_listing_matches_the_td1_chunk_capture() -> TestResult {
    let objects = parse_chunk_listing(&fs::read_to_string(
        fixtures_dir().join("KIWA/307-20260917-003629.xml"),
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

/// Elevation-to-chunk mapping, exact:
/// - With the Message 5 plan, every radial chunk of all 24 volumes maps to
///   the elevation number decoded from its radials.
/// - The End chunk closes the last collected cut.
/// - NEXRAD chunks hold exactly 120 radials (720 or 360 per cut).
/// - TDWR chunks hold 117 to 120, and a cut is at most 3 radials short of 360.
#[test]
fn message5_plans_map_every_radial_chunk_to_its_decoded_elevation() -> TestResult {
    for capture in captures()?.iter().flatten() {
        let plan = capture.plan();
        for row in &capture.chunks {
            let position = plan.chunk_position(row.chunk_id).ok_or_else(|| {
                fail(format!(
                    "{} chunk {} outside the plan",
                    capture.listing, row.chunk_id
                ))
            })?;
            check(position.cut_index + 1 == row.elevation_number, || {
                format!(
                    "{} chunk {}: plan cut {} vs decoded elevation {}",
                    capture.listing,
                    row.chunk_id,
                    position.cut_index + 1,
                    row.elevation_number
                )
            })?;
            let radials_ok = if capture.is_tdwr() {
                (117..=120).contains(&row.radials)
            } else {
                row.radials == 120
            };
            check(radials_ok, || {
                format!(
                    "{} chunk {}: {} radials",
                    capture.listing, row.chunk_id, row.radials
                )
            })?;
        }
        for cut in capture.decoded_cuts() {
            let planned = plan
                .cuts
                .get(cut.elevation_number - 1)
                .map(|planned| u32::from(planned.radials))
                .ok_or_else(|| fail(format!("{}: elevation not in plan", capture.listing)))?;
            let ok = if capture.is_tdwr() {
                cut.radials <= planned && cut.radials + 3 >= planned
            } else {
                cut.radials == planned
            };
            check(ok, || {
                format!("{} {cut:?}: plan {planned} radials", capture.listing)
            })?;
        }
        let end = capture.end_chunk_id()?;
        let position = plan
            .chunk_position(end)
            .ok_or_else(|| fail(format!("{}: End outside the plan", capture.listing)))?;
        check(
            position.closes_cut() && position.cut_index + 1 == capture.elevations_collected,
            || format!("{}: End chunk {end} at {position:?}", capture.listing),
        )?;
        let expected_ids = plan.cut_chunk_ids(position.cut_index);
        check(
            expected_ids.as_ref().map(|ids| *ids.end()) == Some(end),
            || format!("{}: cut_chunk_ids {expected_ids:?}", capture.listing),
        )?;
    }
    Ok(())
}

/// The Build 24 catalog compared with the radars' own Message 5, for every
/// captured VCP 12, 34, 35, 212 and 215 volume:
/// - The catalog rows appear in order in the executed cut sequence. They
///   match on elevation (0.05 deg, the Message 5 angle coding), waveform,
///   SZ-2 phase coding, and radial count.
/// - Azimuth rates match within 0.1%, with two exceptions:
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
///     The catalog keeps the ICD value. The test pins this as the only such
///     case.
/// - Every extra executed cut (SAILS, MESO-SAILS, base tilt) is flagged in
///   Message 5 supplemental data and sits at or below the lowest catalog
///   elevation.
///
/// Where nothing is inserted (KAMA VCP 35, KGRB VCP 34), the catalog plan
/// maps every decoded chunk to its elevation exactly.
#[test]
fn build24_catalog_matches_executed_message5_sequences() -> TestResult {
    let mut compared = 0;
    let mut exact_layouts = 0;
    let mut period_only_matches: Vec<(u16, f32, &str)> = Vec::new();
    for capture in captures()?
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
                })
            })
            .collect();
        let mut next_row = 0;
        let mut inserted = 0;
        for executed in &capture.rows {
            // None: no match. Some(true): matched only through the row's period.
            let matched = definition.rows.get(next_row).and_then(|row| {
                let (code, sz2) = match row.waveform {
                    Waveform::ContiguousSurveillance => ("CS", false),
                    Waveform::Sz2ContiguousSurveillance => ("CS", true),
                    Waveform::ContiguousDopplerWithRangeAmbiguity => ("CD/W", false),
                    Waveform::Sz2ContiguousDoppler => ("CD/W", true),
                    Waveform::Batch => ("B", false),
                    Waveform::ContiguousDopplerWithoutRangeAmbiguity => ("CD/WO", false),
                };
                let shape_ok = (executed.elevation_deg - row.elevation_deg).abs() <= 0.05
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
                        .then_some(false)
                } else if relative(row.azimuth_rate_deg_per_second) <= 0.001 {
                    Some(false)
                } else if relative(360.0 / row.source_period_seconds) <= 0.001 {
                    Some(true)
                } else {
                    None
                }
            });
            if let Some(period_only) = matched {
                if period_only && let Some(row) = definition.rows.get(next_row) {
                    period_only_matches.push((
                        capture.vcp,
                        row.elevation_deg,
                        row.waveform.abbreviation(),
                    ));
                }
                next_row += 1;
                continue;
            }
            let signed = if executed.elevation_deg > 180.0 {
                executed.elevation_deg - 360.0
            } else {
                executed.elevation_deg
            };
            check(
                executed.supplemental != 0 && signed <= lowest + 0.05,
                || {
                    format!(
                        "{}: executed cut {executed:?} matches neither catalog row {next_row} nor an insertion",
                        capture.listing
                    )
                },
            )?;
            inserted += 1;
        }
        check(next_row == definition.rows.len(), || {
            format!(
                "{}: matched {next_row} of {} catalog rows",
                capture.listing,
                definition.rows.len()
            )
        })?;
        check(
            inserted + definition.rows.len() == capture.rows.len(),
            || format!("{}: {inserted} inserted cuts", capture.listing),
        )?;
        if inserted == 0 {
            let plan = ScanPlan::from_build24(definition);
            for row in &capture.chunks {
                check(
                    plan.chunk_position(row.chunk_id)
                        .map(|position| position.cut_index + 1)
                        == Some(row.elevation_number),
                    || {
                        format!(
                            "{} chunk {}: catalog mapping differs",
                            capture.listing, row.chunk_id
                        )
                    },
                )?;
            }
            exact_layouts += 1;
        }
        compared += 1;
    }
    period_only_matches.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    period_only_matches.dedup();
    println!(
        "catalog compared on {compared} volumes, {exact_layouts} with no inserted cuts; rows matched by period only: {period_only_matches:?}"
    );
    check(period_only_matches == [(12, 1.3, "CD/W")], || {
        format!("rows matched only through their period: {period_only_matches:?}")
    })?;
    check(compared == 21 && exact_layouts == 6, || {
        format!("compared {compared}, exact layouts {exact_layouts}")
    })
}

/// Sweep timing from azimuth rates, nominal parameters, every cut of every
/// volume (360 cuts):
/// - Sweep duration `0.978 * 360 / rate` is within 6% of the decoded rotation
///   time (radial span * N / (N - 1)). Per-site decoded/commanded ratios run
///   from 0.93 (KRGX) to 1.00 (TATL), so a single scale cannot do better
///   than about 5%.
/// - Cut start is within 6% of the elapsed time plus 2 s of the decoded first
///   radial.
#[test]
fn sweep_timing_from_azimuth_rates_matches_decoded_radials() -> TestResult {
    let mut duration_errors = Vec::new();
    let mut worst_start = 0.0f64;
    let mut cuts = 0;
    for capture in captures()?.iter().flatten() {
        let model = capture.model(TimingParameters::WSR88D)?;
        for cut in capture.decoded_cuts() {
            let timing = model
                .cut_timing(cut.elevation_number - 1)
                .ok_or_else(|| fail(format!("{}: no cut timing", capture.listing)))?;
            let radials = f64::from(cut.radials);
            let decoded = seconds(cut.last_radial, cut.first_radial) * radials / (radials - 1.0);
            let relative = timing.sweep_seconds / decoded - 1.0;
            duration_errors.push(relative);
            check(relative.abs() <= 0.06, || {
                format!(
                    "{} elevation {}: modeled sweep {:.2} s, decoded {decoded:.2} s",
                    capture.listing, cut.elevation_number, timing.sweep_seconds
                )
            })?;
            let decoded_start = seconds(cut.first_radial, capture.volume.volume_time);
            let start_error = timing.start_seconds - decoded_start;
            check(start_error.abs() <= 0.06 * decoded_start + 2.0, || {
                format!(
                    "{} elevation {}: modeled start {:.1} s, decoded {decoded_start:.1} s",
                    capture.listing, cut.elevation_number, timing.start_seconds
                )
            })?;
            worst_start = worst_start.max(start_error.abs() - 0.06 * decoded_start);
            cuts += 1;
        }
    }
    println!(
        "sweep duration error over {cuts} cuts: median {:.1}%, max {:.1}%; start error margin used up to {:.1} s beyond 6% of elapsed",
        100.0 * percentile(&abs_values(&duration_errors), 0.5),
        100.0 * percentile(&abs_values(&duration_errors), 1.0),
        worst_start
    );
    check(cuts == 360, || format!("{cuts} cuts"))
}

/// Chunk publication from the volume key time alone, nominal parameters,
/// NEXRAD volumes (TDWR publishes about 25 s after collection; see the
/// learned test). With `D` the modeled volume duration and `t` a chunk's
/// modeled elapsed time:
/// - the End chunk is within `0.06 * D + 3 s`;
/// - at least 90% of chunks are within `0.05 * t + 3 s`.
///
/// Build 24 catalog plans (KAMA VCP 35, KGRB VCP 34) use default-PRF Doppler
/// rates, so their End chunk tolerance is `0.10 * D`.
#[test]
fn nominal_model_predicts_chunk_publication() -> TestResult {
    for capture in captures()?
        .iter()
        .flatten()
        .filter(|capture| !capture.is_tdwr())
    {
        let model = capture.model(TimingParameters::WSR88D)?;
        let end = capture.end_chunk_id()?;
        let duration = model
            .chunk_last_modified_seconds(end)
            .ok_or_else(|| fail("no End model"))?;
        let errors = chunk_errors(capture, &model);
        let end_error = errors.last().map_or(f64::NAN, |(_, error)| *error);
        check(end_error.abs() <= 0.06 * duration + 3.0, || {
            format!(
                "{}: End chunk off by {end_error:.1} s over {duration:.0} s",
                capture.listing
            )
        })?;
        let within = errors
            .iter()
            .filter(|(chunk_id, error)| {
                let elapsed = model.chunk_last_modified_seconds(*chunk_id).unwrap_or(0.0);
                error.abs() <= 0.05 * elapsed + 3.0
            })
            .count();
        check(within * 10 >= errors.len() * 9, || {
            format!(
                "{}: {within} of {} chunks within tolerance",
                capture.listing,
                errors.len()
            )
        })?;
        let mut line = format!(
            "nominal {} vol {}: End {end_error:+.1} s of {duration:.0} s, |err| p50 {:.1} p90 {:.1}",
            capture.site,
            capture.volume.volume_id,
            percentile(
                &abs_values(&errors.iter().map(|(_, e)| *e).collect::<Vec<_>>()),
                0.5
            ),
            percentile(
                &abs_values(&errors.iter().map(|(_, e)| *e).collect::<Vec<_>>()),
                0.9
            ),
        );
        if let Some(definition) = build_24_definition(capture.vcp)
            && definition.rows.len() == capture.rows.len()
        {
            let catalog =
                ScanTimingModel::new(ScanPlan::from_build24(definition), TimingParameters::WSR88D)?;
            let catalog_end = seconds(capture.last_modified(end)?, capture.volume.volume_time)
                - catalog
                    .chunk_last_modified_seconds(end)
                    .ok_or_else(|| fail("catalog plan lacks the End chunk"))?;
            check(catalog_end.abs() <= 0.10 * duration, || {
                format!(
                    "{}: catalog plan End off by {catalog_end:.1} s",
                    capture.listing
                )
            })?;
            line.push_str(&format!("; catalog plan End {catalog_end:+.1} s"));
        }
        println!("{line}");
    }
    Ok(())
}

/// Learned statistics, out of sample. For each site, volume n is predicted
/// from its key time with statistics learned from volumes before n (n = 1,
/// 2; all 8 sites, TDWR included):
/// - the End chunk id (AVSET cutoff) is exact;
/// - the End chunk's `LastModified` is within 6 s;
/// - the median chunk error is within 2 s;
/// - the 90th percentile is within 7 s (publication backlogs reached 15 s).
///
/// Summed over all 16 predictions, the learned median error is below the
/// nominal one.
#[test]
fn learned_statistics_predict_the_next_volume() -> TestResult {
    let mut learned_medians = 0.0;
    let mut nominal_medians = 0.0;
    for volumes in &captures()? {
        let mut statistics = TimingStatistics::default();
        for (index, capture) in volumes.iter().enumerate() {
            if index > 0 {
                let model = statistics.model(capture.plan())?;
                let end = capture.end_chunk_id()?;
                check(
                    statistics.expected_end_chunk_id(&capture.plan()) == Some(end),
                    || format!("{}: learned End chunk differs from {end}", capture.listing),
                )?;
                let errors: Vec<f64> = chunk_errors(capture, &model)
                    .into_iter()
                    .map(|(_, e)| e)
                    .collect();
                let end_error = errors.last().copied().unwrap_or(f64::NAN);
                let median = percentile(&abs_values(&errors), 0.5);
                let p90 = percentile(&abs_values(&errors), 0.9);
                check(
                    end_error.abs() <= 6.0 && median <= 2.0 && p90 <= 7.0,
                    || {
                        format!(
                            "{}: End {end_error:.1} s, p50 {median:.1} s, p90 {p90:.1} s",
                            capture.listing
                        )
                    },
                )?;
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
    println!("sum of median |err|: learned {learned_medians:.1} s, nominal {nominal_medians:.1} s");
    check(learned_medians < nominal_medians, || {
        format!("learned {learned_medians:.1} s is not below nominal {nominal_medians:.1} s")
    })
}

/// Volume rollover. Projecting a completed volume gives the next volume's key
/// time:
/// - learned statistics that include at least one earlier rollover (volume 2
///   predicted from volume 1, 8 sites): within 3 s;
/// - nominal parameters for NEXRAD (volumes 1 and 2 predicted from 0 and 1,
///   7 sites): within 6 s.
///
/// The next Start chunk follows at the learned Start offset: within 3 s of
/// its actual `LastModified`, whose volume also appears in the fixtures.
#[test]
fn projections_predict_the_next_volume() -> TestResult {
    for volumes in &captures()? {
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
                check(error.abs() <= 6.0, || {
                    format!(
                        "{}: nominal next volume off by {error:.1} s",
                        capture.listing
                    )
                })?;
                println!(
                    "nominal rollover {} -> {}: {error:+.1} s",
                    capture.volume.volume_id, next.volume.volume_id
                );
            }
            if index >= 1 {
                let projection = statistics.project(capture.plan(), &capture.volume)?;
                check(
                    projection.complete && projection.next_chunk().is_none(),
                    || format!("{}: completed volume not complete", capture.listing),
                )?;
                let error = seconds(
                    next.volume.volume_time,
                    projection.expected_next_volume_time,
                );
                let start_error =
                    seconds(next.last_modified(1)?, projection.expected_next_start_chunk);
                check(error.abs() <= 3.0 && start_error.abs() <= 3.0, || {
                    format!(
                        "{}: next volume off by {error:.1} s, Start chunk {start_error:.1} s",
                        capture.listing
                    )
                })?;
                println!(
                    "learned rollover {} {} -> {}: key {error:+.1} s, Start chunk {start_error:+.1} s",
                    capture.site, capture.volume.volume_id, next.volume.volume_id
                );
            }
        }
    }
    Ok(())
}

/// Remaining-volume projection replayed on real listings. At every distinct
/// `LastModified` before the End chunk, the volume is cut to what a listing
/// would have shown and projected.
///
/// With statistics learned from the previous volumes (volumes 1 and 2, all 8
/// sites):
/// - the projected End chunk id is exact at every instant;
/// - the next chunk is within 4 s at 90% or more of instants;
/// - the End chunk is within 6 s at 90% or more of instants and within 10 s
///   at all of them.
///
/// With nominal parameters on the first volume (NEXRAD sites that collect the
/// whole plan: KAMA, KDGX, KGRB, KMAX), the End chunk is within
/// `0.03 * D + 3 s` at 90% or more of instants and within `0.05 * D + 3 s` at
/// all.
#[test]
fn projections_replay_real_listings() -> TestResult {
    for volumes in &captures()? {
        let mut statistics = TimingStatistics::default();
        for (index, capture) in volumes.iter().enumerate() {
            let end = capture.end_chunk_id()?;
            let actual_end = capture.last_modified(end)?;
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
            for at in instants {
                let observed = capture.volume.observed_by(at);
                if index > 0 {
                    let projection = statistics.project(capture.plan(), &observed)?;
                    check(projection.expected_end_chunk_id == end, || {
                        format!(
                            "{} at {at}: End chunk {}",
                            capture.listing, projection.expected_end_chunk_id
                        )
                    })?;
                    if let Some(next) = projection.next_chunk() {
                        next_errors.push(seconds(
                            capture.last_modified(next.chunk_id)?,
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
            if index > 0 {
                let next_p90 = percentile(&abs_values(&next_errors), 0.9);
                let end_p90 = percentile(&abs_values(&end_errors), 0.9);
                let end_max = percentile(&abs_values(&end_errors), 1.0);
                check(next_p90 <= 4.0 && end_p90 <= 6.0 && end_max <= 10.0, || {
                    format!(
                        "{}: next chunk p90 {next_p90:.1} s, End p90 {end_p90:.1} s, End max {end_max:.1} s",
                        capture.listing
                    )
                })?;
                println!(
                    "learned replay {} vol {} ({} instants): next chunk |err| p50 {:.1} p90 {next_p90:.1} max {:.1}; End |err| p50 {:.1} p90 {end_p90:.1} max {end_max:.1}",
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
                check(
                    p90 <= 0.03 * duration + 3.0 && max <= 0.05 * duration + 3.0,
                    || {
                        format!(
                            "{}: nominal End p90 {p90:.1} s, max {max:.1} s",
                            capture.listing
                        )
                    },
                )?;
                println!(
                    "nominal replay {} vol {}: End |err| p90 {p90:.1} max {max:.1} over {duration:.0} s",
                    capture.site, capture.volume.volume_id
                );
            }
            statistics.observe_volume(&capture.plan(), &capture.volume)?;
        }
    }
    Ok(())
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
    let sites = captures()?;
    let kiwa = sites
        .iter()
        .find(|volumes| {
            volumes
                .first()
                .is_some_and(|capture| capture.site == "KIWA")
        })
        .ok_or_else(|| fail("no KIWA captures"))?;
    let (previous, capture) = match kiwa.as_slice() {
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
    check(current.cut_index + 1 == decoded.elevation_number, || {
        format!(
            "current cut {} vs decoded elevation {}",
            current.cut_index + 1,
            decoded.elevation_number
        )
    })?;
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
    let sites = captures()?;
    let find = |site: &str| {
        sites
            .iter()
            .find(|volumes| volumes.first().is_some_and(|capture| capture.site == site))
            .and_then(|volumes| volumes.first())
            .ok_or_else(|| fail(format!("no {site} capture")))
    };
    let (kiwa, kama, kmax, kgrb, krgx) = (
        find("KIWA")?,
        find("KAMA")?,
        find("KMAX")?,
        find("KGRB")?,
        find("KRGX")?,
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
