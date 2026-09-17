//! Live download-and-decode check for NEXRAD Level II sites.
//!
//! For each site given on the command line, in parallel:
//!
//! 1. Archive: find the site's newest volume in the `unidata-nexrad-level2`
//!    bucket (today or yesterday, UTC), download it and decode it with
//!    `recast_radar_io::decode_supported_volume_bytes`.
//! 2. Real time: join the site's newest volume in the
//!    `unidata-nexrad-level2-chunks` bucket with the blocking
//!    [`ChunkIterator`] (from its Start chunk). When the iterator first
//!    catches up with a volume still being collected, decode the chunks
//!    taken so far; then keep polling until the volume's End chunk and decode
//!    the complete volume.
//!
//! Every decoded volume must name the requested site, carry a VCP, have a
//! volume time within [`MAX_KEY_TIME_DIFFERENCE_SECS`] of the time in its
//! object key, and have at least one cut, each with radials and moments.
//!
//! Network failures are retried rather than failing the site at once: the
//! archive listing and download under [`ARCHIVE_RETRY`], and the real-time
//! iterator by its own design (after a request error it pauses and carries
//! on, relisting or abandoning a volume whose chunk cannot be downloaded).
//! Iterator errors are listed in the report. A site fails on a decode error,
//! a failed check, archive requests that still fail after the retries, or a
//! real-time volume that does not end within the timeout. The process exits
//! with status 1 when any site failed.
//!
//! Usage:
//!   cargo run --release -p recast-radar-data --example live_decode -- KTLX KMKX KHDC PAHG TJUA
//!   cargo run --release -p recast-radar-data --example live_decode -- --realtime-timeout-secs 1500 KTLX
//!
//! `.github/workflows/live-decode.yml` runs it weekly. When
//! `GITHUB_STEP_SUMMARY` is set, a Markdown table of the results is appended
//! to that file.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use recast_radar_data::realtime::iterator::{ChunkEvent, ChunkIterator, ChunkIteratorConfig};
use recast_radar_data::realtime::retry::{Jitter, RetryPolicy, random_seed};
use recast_radar_data::{
    LEVEL2_ARCHIVE_BUCKET, RealtimeChunkType, VOLUME_FETCH_RETRY, fetch_volume_bytes_with_retry,
    latest_level2_object, level2_object_time_utc,
};

/// Largest accepted difference between a decoded volume time and the time
/// in the object key (archive file name or real-time chunk key).
const MAX_KEY_TIME_DIFFERENCE_SECS: i64 = 60;

/// Retries of the archive listing and download: five attempts over about a
/// minute (delay ceilings 4, 8, 16 and 32 s), enough to ride out a short S3
/// or network outage.
const ARCHIVE_RETRY: RetryPolicy = RetryPolicy {
    max_attempts: 5,
    initial_delay: Duration::from_secs(4),
    max_delay: Duration::from_secs(32),
    multiplier: 2.0,
    jitter: Jitter::Equal,
};

/// Default wait for the joined real-time volume's End chunk. The longest
/// WSR-88D volumes (VCP 31/32) take about 10 minutes; a volume abandoned
/// mid-way restarts the wait for the next one within this budget.
const DEFAULT_REALTIME_TIMEOUT_SECS: u64 = 1200;

fn main() {
    let mut sites = Vec::new();
    let mut realtime_timeout = Duration::from_secs(DEFAULT_REALTIME_TIMEOUT_SECS);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--realtime-timeout-secs" {
            match args.next().and_then(|value| value.parse::<u64>().ok()) {
                Some(secs) => realtime_timeout = Duration::from_secs(secs),
                None => usage_error("--realtime-timeout-secs needs a whole number of seconds"),
            }
        } else if arg.starts_with('-') {
            usage_error(&format!("unknown option {arg}"));
        } else {
            sites.push(arg.trim().to_ascii_uppercase());
        }
    }
    if sites.is_empty() {
        usage_error("no sites given");
    }

    let started = Instant::now();
    let reports: Vec<SiteReport> = std::thread::scope(|scope| {
        let handles: Vec<_> = sites
            .iter()
            .map(|site| {
                let site = site.clone();
                scope.spawn(move || check_site(&site, realtime_timeout))
            })
            .collect();
        handles
            .into_iter()
            .zip(&sites)
            .map(|(handle, site)| {
                handle.join().unwrap_or_else(|_| SiteReport {
                    site: site.clone(),
                    archive: Err("worker thread panicked".to_owned()),
                    realtime: Err("worker thread panicked".to_owned()),
                })
            })
            .collect()
    });

    println!();
    println!(
        "== live decode results ({} sites, {:.0} s) ==",
        reports.len(),
        started.elapsed().as_secs_f64()
    );
    let mut failures = 0usize;
    for report in &reports {
        let ok = report.archive.is_ok() && report.realtime.is_ok();
        if !ok {
            failures += 1;
        }
        println!("{} {}", report.site, if ok { "OK" } else { "FAILED" });
        match &report.archive {
            Ok(archive) => println!("  archive   {}", archive.describe()),
            Err(message) => println!("  archive   FAILED: {message}"),
        }
        match &report.realtime {
            Ok(realtime) => {
                for line in realtime.describe() {
                    println!("  realtime  {line}");
                }
            }
            Err(message) => println!("  realtime  FAILED: {message}"),
        }
    }
    write_step_summary(&reports);

    if failures > 0 {
        eprintln!("{failures} of {} site(s) failed", reports.len());
        std::process::exit(1);
    }
}

fn usage_error(message: &str) -> ! {
    eprintln!("live_decode: {message}");
    eprintln!("usage: live_decode [--realtime-timeout-secs N] SITE [SITE ...]");
    std::process::exit(2);
}

struct SiteReport {
    site: String,
    archive: Result<ArchiveReport, String>,
    realtime: Result<RealtimeReport, String>,
}

fn check_site(site: &str, realtime_timeout: Duration) -> SiteReport {
    let archive = check_archive(site);
    if let Err(message) = &archive {
        progress(site, &format!("archive FAILED: {message}"));
    }
    let realtime = check_realtime(site, realtime_timeout);
    if let Err(message) = &realtime {
        progress(site, &format!("realtime FAILED: {message}"));
    }
    SiteReport {
        site: site.to_owned(),
        archive,
        realtime,
    }
}

fn progress(site: &str, message: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "[{site}] {message}");
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// Archive bucket

struct ArchiveReport {
    key: String,
    bytes: usize,
    age: chrono::Duration,
    download: Duration,
    summary: VolumeSummary,
}

impl ArchiveReport {
    fn describe(&self) -> String {
        format!(
            "{}  {}  age {}  download {:.1} s  {}",
            self.key,
            megabytes(self.bytes),
            minutes(self.age),
            self.download.as_secs_f64(),
            self.summary.describe()
        )
    }
}

/// Run `attempt` under [`ARCHIVE_RETRY`], logging each failure.
fn with_archive_retries<T>(
    site: &str,
    what: &str,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let mut backoff = ARCHIVE_RETRY.backoff(random_seed());
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(message) => match backoff.next_delay() {
                Some(delay) => {
                    progress(
                        site,
                        &format!(
                            "archive: {what} failed ({message}); retrying in {:.1} s",
                            delay.as_secs_f64()
                        ),
                    );
                    std::thread::sleep(delay);
                }
                None => {
                    return Err(format!(
                        "{what}: {message} (after {} attempts)",
                        backoff.failures()
                    ));
                }
            },
        }
    }
}

fn check_archive(site: &str) -> Result<ArchiveReport, String> {
    let object = with_archive_retries(site, "listing the newest archive volume", || {
        latest_level2_object(site, 1).map_err(|err| err.to_string())
    })?;
    let key_time = level2_object_time_utc(&object)
        .ok_or_else(|| format!("no volume time in archive key {}", object.key))?;
    let url = format!(
        "https://{LEVEL2_ARCHIVE_BUCKET}.s3.amazonaws.com/{}",
        object.key
    );
    progress(
        site,
        &format!(
            "archive: downloading {} ({})",
            object.key,
            megabytes(object.size as usize)
        ),
    );
    let download_started = Instant::now();
    let bytes = with_archive_retries(site, &format!("downloading {url}"), || {
        let bytes = fetch_volume_bytes_with_retry(&url, &VOLUME_FETCH_RETRY, std::thread::sleep)
            .map_err(|err| err.to_string())?;
        if bytes.len() as u64 == object.size {
            Ok(bytes)
        } else {
            Err(format!(
                "downloaded {} bytes, listing says {}",
                bytes.len(),
                object.size
            ))
        }
    })?;
    let download = download_started.elapsed();
    let summary = decode_and_check(&bytes, site, key_time)
        .map_err(|message| format!("{}: {message}", object.key))?;
    progress(site, &format!("archive: {}", summary.describe()));
    Ok(ArchiveReport {
        key: object
            .key
            .rsplit('/')
            .next()
            .unwrap_or(&object.key)
            .to_owned(),
        bytes: bytes.len(),
        age: Utc::now().signed_duration_since(key_time),
        download,
        summary,
    })
}

// ---------------------------------------------------------------------------
// Real-time chunk bucket

struct RealtimeReport {
    volume_id: u16,
    volume_time: DateTime<Utc>,
    first_caught_up_at: Option<usize>,
    /// Volume id, chunk count and summary of the live-edge decode.
    partial: Option<(u16, usize, VolumeSummary)>,
    complete_chunks: usize,
    complete_bytes: usize,
    complete: VolumeSummary,
    waited: Duration,
    abandoned: Vec<String>,
    retries: Vec<String>,
    /// Errors the iterator reported and carried on after.
    errors: Vec<String>,
    requests: u64,
    listing_bytes: u64,
    chunk_bytes: u64,
}

impl RealtimeReport {
    fn describe(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "volume {} {}  waited {:.0} s  {} requests ({} listing + {} chunk bytes)",
            self.volume_id,
            self.volume_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            self.waited.as_secs_f64(),
            self.requests,
            self.listing_bytes,
            self.chunk_bytes
        )];
        let caught_up = self.first_caught_up_at.map_or_else(
            || "never caught up (the volume was already complete)".to_owned(),
            |chunks| format!("first caught up at {chunks} chunk(s)"),
        );
        match &self.partial {
            Some((volume_id, chunks, summary)) => lines.push(format!(
                "partial   {caught_up}; decoded volume {volume_id} at {chunks} chunks: {}",
                summary.describe()
            )),
            None => lines.push(format!("partial   none; {caught_up}")),
        }
        lines.push(format!(
            "complete  {} chunks, {}: {}",
            self.complete_chunks,
            megabytes(self.complete_bytes),
            self.complete.describe()
        ));
        for note in &self.abandoned {
            lines.push(format!("abandoned {note}"));
        }
        for note in &self.retries {
            lines.push(format!("retry     {note}"));
        }
        for note in &self.errors {
            lines.push(format!("error     {note}"));
        }
        lines
    }
}

fn check_realtime(site: &str, timeout: Duration) -> Result<RealtimeReport, String> {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut iter = ChunkIterator::live(site, ChunkIteratorConfig::default())
        .map_err(|err| format!("building the HTTPS client: {err}"))?;

    // The volume being assembled: id, start time, chunk count, bytes.
    let mut current: Option<(u16, DateTime<Utc>)> = None;
    let mut chunk_count = 0usize;
    let mut bytes: Vec<u8> = Vec::new();
    let mut first_caught_up_at: Option<usize> = None;
    let mut partial: Option<(u16, usize, VolumeSummary)> = None;
    let mut abandoned = Vec::new();
    let mut retries = Vec::new();
    let mut errors = Vec::new();

    loop {
        let event = match iter
            .next()
            .ok_or_else(|| "chunk iterator ended".to_owned())?
        {
            Ok(event) => event,
            Err(err) => {
                // The iterator pauses (an Idle event follows) and carries
                // on; the deadline bounds how long errors can go on.
                progress(site, &format!("realtime: error, continuing: {err}"));
                errors.push(err.to_string());
                continue;
            }
        };
        match event {
            ChunkEvent::Chunk(chunk) => {
                let info = chunk.info;
                if current != Some((info.volume_id, info.volume_time)) {
                    if info.chunk_type != RealtimeChunkType::Start {
                        return Err(format!(
                            "volume {} started with chunk {} ({}), not a Start chunk",
                            info.volume_id,
                            info.chunk_id,
                            info.chunk_type.label()
                        ));
                    }
                    progress(
                        site,
                        &format!(
                            "realtime: following volume {} ({})",
                            info.volume_id,
                            info.volume_time.to_rfc3339_opts(SecondsFormat::Secs, true)
                        ),
                    );
                    current = Some((info.volume_id, info.volume_time));
                    chunk_count = 0;
                    bytes.clear();
                }
                let data = chunk
                    .data
                    .ok_or_else(|| format!("chunk {} came without bytes", info.object.key))?;
                bytes.extend_from_slice(&data);
                chunk_count += 1;
                if info.chunk_type == RealtimeChunkType::End {
                    let complete =
                        decode_and_check(&bytes, site, info.volume_time).map_err(|message| {
                            format!(
                                "complete volume {} ({chunk_count} chunks): {message}",
                                info.volume_id
                            )
                        })?;
                    progress(
                        site,
                        &format!(
                            "realtime: volume {} complete, {chunk_count} chunks: {}",
                            info.volume_id,
                            complete.describe()
                        ),
                    );
                    let stats = iter.stats();
                    return Ok(RealtimeReport {
                        volume_id: info.volume_id,
                        volume_time: info.volume_time,
                        first_caught_up_at,
                        partial,
                        complete_chunks: chunk_count,
                        complete_bytes: bytes.len(),
                        complete,
                        waited: started.elapsed(),
                        abandoned,
                        retries,
                        errors,
                        requests: stats.requests,
                        listing_bytes: stats.listing_bytes,
                        chunk_bytes: stats.chunk_bytes,
                    });
                }
            }
            ChunkEvent::Idle { poll_after } => {
                if let Some((volume_id, volume_time)) = current {
                    first_caught_up_at.get_or_insert(chunk_count);
                    // Decode the live edge once, as soon as a radial chunk is
                    // in hand.
                    if partial.is_none() && chunk_count >= 2 {
                        let summary =
                            decode_and_check(&bytes, site, volume_time).map_err(|message| {
                                format!(
                                    "partial volume {volume_id} ({chunk_count} chunks): {message}"
                                )
                            })?;
                        progress(
                            site,
                            &format!(
                                "realtime: volume {volume_id} partial, {chunk_count} chunks: {}",
                                summary.describe()
                            ),
                        );
                        partial = Some((volume_id, chunk_count, summary));
                    }
                }
                wait(deadline, timeout, poll_after, current, chunk_count)?;
            }
            ChunkEvent::Retry {
                after,
                attempt,
                kind,
                url,
                error,
            } => {
                let note = format!("{kind:?} {url}: {error} (attempt {attempt} in {after:?})");
                progress(site, &format!("realtime: retry {note}"));
                retries.push(note);
                wait(deadline, timeout, after, current, chunk_count)?;
            }
            ChunkEvent::VolumeAbandoned {
                volume_id,
                volume_time,
                last_chunk_id,
                next_volume_id,
                ..
            } => {
                let note = format!(
                    "volume {volume_id} ({}) after chunk {last_chunk_id}; continuing with {next_volume_id}",
                    volume_time.to_rfc3339_opts(SecondsFormat::Secs, true)
                );
                progress(site, &format!("realtime: abandoned {note}"));
                abandoned.push(note);
            }
            other => progress(site, &format!("realtime: ignoring event {other:?}")),
        }
    }
}

fn wait(
    deadline: Instant,
    timeout: Duration,
    delay: Duration,
    current: Option<(u16, DateTime<Utc>)>,
    chunk_count: usize,
) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        let position = match current {
            Some((volume_id, _)) => format!("volume {volume_id} at {chunk_count} chunks"),
            None => "no chunk taken".to_owned(),
        };
        return Err(format!(
            "no End chunk within {} s ({position})",
            timeout.as_secs()
        ));
    }
    std::thread::sleep(delay.min(remaining));
    Ok(())
}

// ---------------------------------------------------------------------------
// Decode and checks

struct VolumeSummary {
    vcp: u16,
    cuts: usize,
    radials: usize,
    elevations: (f32, f32),
    moments: BTreeSet<String>,
    volume_time: DateTime<Utc>,
    key_time_difference_secs: i64,
    decode: Duration,
}

impl VolumeSummary {
    fn describe(&self) -> String {
        let moments = self.moments.iter().cloned().collect::<Vec<_>>().join(" ");
        format!(
            "VCP {}, {} cuts ({:.1}-{:.1} deg), {} radials, moments {moments}, \
             volume time {} (key {:+} s), decode {:.2} s",
            self.vcp,
            self.cuts,
            self.elevations.0,
            self.elevations.1,
            self.radials,
            self.volume_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            self.key_time_difference_secs,
            self.decode.as_secs_f64()
        )
    }
}

fn decode_and_check(
    bytes: &[u8],
    site: &str,
    key_time: DateTime<Utc>,
) -> Result<VolumeSummary, String> {
    let decode_started = Instant::now();
    let volume = recast_radar_io::decode_supported_volume_bytes(bytes)
        .map_err(|err| format!("decode failed: {err}"))?;
    let decode = decode_started.elapsed();

    if !volume.site.id.trim().eq_ignore_ascii_case(site) {
        return Err(format!(
            "decoded site is {:?}, expected {site}",
            volume.site.id
        ));
    }
    let Some(vcp) = volume.vcp.as_ref().map(|vcp| vcp.pattern) else {
        return Err("decoded volume has no VCP".to_owned());
    };
    let key_time_difference_secs = volume
        .volume_time
        .signed_duration_since(key_time)
        .num_seconds();
    if key_time_difference_secs.abs() > MAX_KEY_TIME_DIFFERENCE_SECS {
        return Err(format!(
            "decoded volume time {} is {key_time_difference_secs} s from the key time {}",
            volume
                .volume_time
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            key_time.to_rfc3339_opts(SecondsFormat::Secs, true)
        ));
    }
    if volume.cuts.is_empty() {
        return Err("decoded volume has no cuts".to_owned());
    }

    let mut radials = 0usize;
    let mut moments = BTreeSet::new();
    let mut elevations = (f32::INFINITY, f32::NEG_INFINITY);
    for (index, cut) in volume.cuts.iter().enumerate() {
        if cut.radials.is_empty() {
            return Err(format!(
                "cut {index} ({:.2} deg) has no radials",
                cut.elevation_deg
            ));
        }
        if cut.moments.is_empty() {
            return Err(format!(
                "cut {index} ({:.2} deg) has no moments",
                cut.elevation_deg
            ));
        }
        radials += cut.radials.len();
        moments.extend(
            cut.moments
                .keys()
                .map(|moment| moment.short_name().to_owned()),
        );
        elevations.0 = elevations.0.min(cut.elevation_deg);
        elevations.1 = elevations.1.max(cut.elevation_deg);
    }

    Ok(VolumeSummary {
        vcp,
        cuts: volume.cuts.len(),
        radials,
        elevations,
        moments,
        volume_time: volume.volume_time,
        key_time_difference_secs,
        decode,
    })
}

// ---------------------------------------------------------------------------
// Output helpers

fn megabytes(bytes: usize) -> String {
    format!("{:.2} MB", bytes as f64 / 1_000_000.0)
}

fn minutes(duration: chrono::Duration) -> String {
    format!("{:.1} min", duration.num_seconds() as f64 / 60.0)
}

fn write_step_summary(reports: &[SiteReport]) {
    let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") else {
        return;
    };
    let mut markdown = String::from(
        "## Live decode\n\n| Site | Archive | Real-time (partial) | Real-time (complete) |\n|---|---|---|---|\n",
    );
    for report in reports {
        let archive = match &report.archive {
            Ok(archive) => format!(
                "{} ({}, age {}): {}",
                archive.key,
                megabytes(archive.bytes),
                minutes(archive.age),
                archive.summary.describe()
            ),
            Err(message) => format!("**FAILED**: {message}"),
        };
        let (partial, complete) = match &report.realtime {
            Ok(realtime) => (
                realtime.partial.as_ref().map_or_else(
                    || "none".to_owned(),
                    |(volume_id, chunks, summary)| {
                        format!(
                            "volume {volume_id}, {chunks} chunks: {}",
                            summary.describe()
                        )
                    },
                ),
                format!(
                    "volume {}, {} chunks: {}",
                    realtime.volume_id,
                    realtime.complete_chunks,
                    realtime.complete.describe()
                ),
            ),
            Err(message) => (String::new(), format!("**FAILED**: {message}")),
        };
        let _ = writeln!(
            markdown,
            "| {} | {} | {} | {} |",
            report.site,
            table_cell(&archive),
            table_cell(&partial),
            table_cell(&complete)
        );
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(markdown.as_bytes()));
    if let Err(err) = written {
        eprintln!("could not write the step summary to {path}: {err}");
    }
}

fn table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
