//! `fetch`: downloads through `recast-radar-data`.
//!
//! Every download is written through a temporary file and renamed into
//! place, so an interrupted run never leaves a truncated volume under the
//! final name. A file that already exists with the listed size is kept and
//! not downloaded again; for international frames, whose listings give no
//! size, a non-empty file under the part's name is kept (the names stand for
//! one upstream file, see [`frames::part_file_names`]).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use recast_radar_data::international::{FramePlan, IntlProvider, intl_providers};

use crate::frames::{self, frame_time};
use recast_radar_data::{LEVEL2_ARCHIVE_BUCKET, LEVEL2_CHUNKS_BUCKET, S3Object, level3, polling};
use serde_json::json;

use crate::output::{human_bytes, write_file_atomically};
use crate::{
    ChunksFetchArgs, CliError, FetchArgs, FetchSource, IntlFetchArgs, Level2FetchArgs,
    Level3FetchArgs, PollingFetchArgs, SitesFetchArgs,
};

/// Real-time chunks downloaded at once.
const CHUNK_DOWNLOADS: usize = 4;

/// Each chunk's bytes or download error, by chunk index.
type ChunkResults = Mutex<Vec<Option<Result<Vec<u8>, String>>>>;

pub(crate) fn run(args: FetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    match args.source {
        FetchSource::Level2(args) => level2(&args, out),
        FetchSource::Chunks(args) => chunks(&args, out),
        FetchSource::Level3(args) => level3_products(&args, out),
        FetchSource::Intl(args) => intl(&args, out),
        FetchSource::Polling(args) => polling_server(&args, out),
        FetchSource::Sites(args) => sites(&args, out),
    }
}

fn failed(err: impl std::fmt::Display) -> CliError {
    CliError::Failed(err.to_string())
}

/// The `count` objects nearest to `target` (in chronological order), or the
/// first `count` of the day when there is no target.
fn select_objects(
    mut objects: Vec<S3Object>,
    time_of: impl Fn(&S3Object) -> Option<DateTime<Utc>>,
    target: Option<DateTime<Utc>>,
    count: usize,
) -> Vec<S3Object> {
    if let Some(target) = target {
        objects.sort_by_key(|object| {
            time_of(object).map_or(i64::MAX, |time| (time - target).num_seconds().abs())
        });
    }
    objects.truncate(count);
    objects.sort_by(|a, b| a.key.cmp(&b.key));
    objects
}

fn target_time(date: Option<NaiveDate>, time: Option<NaiveTime>) -> Option<DateTime<Utc>> {
    Some(date?.and_time(time?).and_utc())
}

/// A name that stays inside its directory: not empty, not `.` or `..`, no
/// path separators or drive prefixes.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

/// Download `url` to `dir/name` unless a file of `expected_size` bytes is
/// already there.
fn download(
    url: &str,
    dir: &Path,
    name: &str,
    expected_size: Option<u64>,
    out: &mut dyn Write,
) -> Result<PathBuf, CliError> {
    if !is_plain_name(name) {
        return Err(CliError::Failed(format!(
            "{url}: `{name}` is not a safe file name"
        )));
    }
    let path = dir.join(name);
    if let (Some(size), Ok(metadata)) = (expected_size, fs::metadata(&path))
        && metadata.len() == size
    {
        writeln!(out, "have  {} ({})", path.display(), human_bytes(size))?;
        return Ok(path);
    }
    let bytes = recast_radar_data::fetch_volume_bytes(url)
        .map_err(|err| CliError::Failed(format!("{url}: {err}")))?;
    if let Some(size) = expected_size
        && bytes.len() as u64 != size
    {
        return Err(CliError::Failed(format!(
            "{url}: downloaded {} bytes, the listing says {size}",
            bytes.len()
        )));
    }
    write_file_atomically(&path, &bytes)?;
    writeln!(
        out,
        "wrote {} ({})",
        path.display(),
        human_bytes(bytes.len() as u64)
    )?;
    Ok(path)
}

fn object_name(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

fn list_objects(objects: &[S3Object], out: &mut dyn Write) -> Result<(), CliError> {
    for object in objects {
        writeln!(out, "{:>10}  {}", object.size, object_name(&object.key))?;
    }
    Ok(())
}

fn level2(args: &Level2FetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let site = args.site.to_ascii_uppercase();
    let count = args.count as usize;
    let objects = match args.date {
        Some(date) => {
            let objects =
                recast_radar_data::level2_objects_for_date(&site, date).map_err(failed)?;
            if args.list && args.time.is_none() {
                objects
            } else {
                select_objects(
                    objects,
                    recast_radar_data::level2_object_time_utc,
                    target_time(args.date, args.time),
                    count,
                )
            }
        }
        None => {
            let mut objects =
                recast_radar_data::recent_level2_objects(&site, 1, count).map_err(failed)?;
            objects.reverse();
            objects
        }
    };
    if objects.is_empty() {
        return Err(CliError::Failed(format!("no Level II volumes for {site}")));
    }
    if args.list {
        return list_objects(&objects, out);
    }
    for object in &objects {
        let url = format!(
            "https://{LEVEL2_ARCHIVE_BUCKET}.s3.amazonaws.com/{}",
            object.key
        );
        download(
            &url,
            &args.output,
            object_name(&object.key),
            Some(object.size),
            out,
        )?;
    }
    Ok(())
}

fn level3_products(args: &Level3FetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let count = args.count as usize;
    let objects = match args.date {
        Some(date) => {
            let objects =
                level3::level3_objects_for_date(&args.site, &args.product, date).map_err(failed)?;
            if args.list && args.time.is_none() {
                objects
            } else {
                select_objects(
                    objects,
                    |object| level3::level3_key_time(&object.key),
                    target_time(args.date, args.time),
                    count,
                )
            }
        }
        None => {
            let mut objects = level3::recent_level3_objects(&args.site, &args.product, 1, count)
                .map_err(failed)?;
            objects.reverse();
            objects
        }
    };
    if objects.is_empty() {
        return Err(CliError::Failed(format!(
            "no {} products for {}",
            args.product.to_ascii_uppercase(),
            level3::level3_site_id(&args.site)
        )));
    }
    if args.list {
        return list_objects(&objects, out);
    }
    for object in &objects {
        download(
            &level3::level3_object_url(&object.key),
            &args.output,
            object_name(&object.key),
            Some(object.size),
            out,
        )?;
    }
    Ok(())
}

fn chunks(args: &ChunksFetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let site = args.site.to_ascii_uppercase();
    let volume = recast_radar_data::latest_realtime_level2_volume(&site).map_err(failed)?;
    writeln!(
        out,
        "{} volume {} started {}: {} chunk(s), {}, {}",
        volume.site,
        volume.volume_id,
        volume.volume_time.format("%Y-%m-%dT%H:%M:%SZ"),
        volume.chunks.len(),
        human_bytes(volume.total_size),
        if volume.complete {
            "complete"
        } else {
            "in progress"
        }
    )?;
    if args.list {
        for chunk in &volume.chunks {
            writeln!(
                out,
                "{:>10}  {}  {}",
                chunk.object.size,
                chunk.chunk_type.label(),
                chunk.object.key
            )?;
        }
        return Ok(());
    }

    let next = AtomicUsize::new(0);
    let results: ChunkResults = Mutex::new(vec![None; volume.chunks.len()]);
    thread::scope(|scope| {
        for _ in 0..CHUNK_DOWNLOADS.min(volume.chunks.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(chunk) = volume.chunks.get(index) else {
                        break;
                    };
                    let url = format!(
                        "https://{LEVEL2_CHUNKS_BUCKET}.s3.amazonaws.com/{}",
                        chunk.object.key
                    );
                    let result = recast_radar_data::fetch_volume_bytes(&url)
                        .map_err(|err| format!("{url}: {err}"));
                    let failed = result.is_err();
                    if let Ok(mut results) = results.lock() {
                        results[index] = Some(result);
                    }
                    if failed {
                        break;
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|p| p.into_inner());

    let capacity = usize::try_from(volume.total_size)
        .unwrap_or(usize::MAX)
        .min(crate::open::MAX_INPUT_BYTES as usize);
    let mut assembled = Vec::with_capacity(capacity);
    let chunk_dir = args.output.join(format!(
        "{}_{:03}_{}",
        volume.site,
        volume.volume_id,
        volume.volume_time.format("%Y%m%d_%H%M%S")
    ));
    for (chunk, result) in volume.chunks.iter().zip(results) {
        let bytes = match result {
            Some(Ok(bytes)) => bytes,
            Some(Err(message)) => return Err(CliError::Failed(message)),
            None => {
                return Err(CliError::Failed(format!(
                    "chunk {} was not downloaded",
                    chunk.object.key
                )));
            }
        };
        if bytes.len() as u64 != chunk.object.size {
            return Err(CliError::Failed(format!(
                "chunk {}: {} bytes, the listing says {}",
                chunk.object.key,
                bytes.len(),
                chunk.object.size
            )));
        }
        if args.keep_chunks {
            write_file_atomically(&chunk_dir.join(object_name(&chunk.object.key)), &bytes)?;
        }
        assembled.extend_from_slice(&bytes);
    }
    let name = format!(
        "{}{}_V06{}",
        volume.site,
        volume.volume_time.format("%Y%m%d_%H%M%S"),
        if volume.complete { "" } else { ".part" }
    );
    let path = args.output.join(name);
    write_file_atomically(&path, &assembled)?;
    writeln!(
        out,
        "wrote {} ({})",
        path.display(),
        human_bytes(assembled.len() as u64)
    )?;
    if args.keep_chunks {
        writeln!(out, "chunks in {}", chunk_dir.display())?;
    }
    Ok(())
}

fn find_provider(id: &str) -> Result<Box<dyn IntlProvider>, CliError> {
    let providers = intl_providers();
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    let wanted = id.to_ascii_lowercase();
    let known = ids.join(", ");
    providers
        .into_iter()
        .find(|provider| provider.id() == wanted)
        .ok_or_else(|| CliError::Usage(format!("unknown provider `{id}` (known: {known})")))
}

/// The frames `fetch intl` works on: the newest ones, or with `--date`
/// archived ones (the first `-n` of the day, or with `--time` the `-n`
/// nearest that time), oldest first.
fn intl_plans(
    provider: &dyn IntlProvider,
    site: &str,
    args: &IntlFetchArgs,
) -> Result<Vec<FramePlan>, CliError> {
    let count = args.count as usize;
    let Some(date) = args.date else {
        return if count > 1 {
            provider.recent(site, count).map_err(failed)
        } else {
            Ok(vec![provider.latest(site).map_err(failed)?])
        };
    };
    let Some(archive) = provider.archive_source() else {
        let with_archive: Vec<&str> = intl_providers()
            .iter()
            .filter(|provider| provider.supports_archive())
            .map(|provider| provider.id())
            .collect();
        return Err(CliError::Usage(format!(
            "`{}` has no archive, so --date does not apply (providers with one: {})",
            provider.id(),
            with_archive.join(", ")
        )));
    };
    let Some(time) = args.time else {
        let mut plans = archive.day_plans(site, date).map_err(failed)?;
        if !args.list {
            plans.truncate(count);
        }
        return Ok(plans);
    };
    let target = date.and_time(time).and_utc();
    let span = frames::archive_search_span(count);
    let plans = archive
        .window_plans(site, target - span, target + span, usize::MAX)
        .map_err(failed)?;
    let times: Vec<_> = plans
        .iter()
        .map(|plan| frame_time(&plan.identity))
        .collect();
    let keep = frames::nearest(&times, target, count);
    Ok(plans
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.binary_search(index).is_ok())
        .map(|(_, plan)| plan)
        .collect())
}

fn intl(args: &IntlFetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let Some(provider_id) = &args.provider else {
        writeln!(
            out,
            "{:<16} {:<28} {:<14} {:>5}  recent  archive",
            "provider", "name", "country", "sites"
        )?;
        for provider in intl_providers() {
            writeln!(
                out,
                "{:<16} {:<28} {:<14} {:>5}  {:<6}  {}",
                provider.id(),
                provider.label(),
                provider.country(),
                provider.static_sites().len(),
                if provider.supports_recent() {
                    "yes"
                } else {
                    "no"
                },
                if provider.supports_archive() {
                    "yes"
                } else {
                    "no"
                },
            )?;
        }
        return Ok(());
    };
    let provider = find_provider(provider_id)?;
    if args.list_sites {
        let sites = if args.online {
            provider.list_sites().map_err(failed)?
        } else {
            provider.static_sites()
        };
        for site in sites {
            writeln!(
                out,
                "{:<16} {:<30} {:>9} {:>10}",
                site.site_id,
                site.label,
                site.latitude_deg
                    .map_or("-".to_owned(), |v| format!("{v:.4}")),
                site.longitude_deg
                    .map_or("-".to_owned(), |v| format!("{v:.4}")),
            )?;
        }
        return Ok(());
    }
    let Some(site) = &args.site else {
        return Err(CliError::Usage(format!(
            "name a site of `{}` (see --list-sites)",
            provider.id()
        )));
    };
    let plans = intl_plans(provider.as_ref(), site, args)?;
    if plans.is_empty() {
        return Err(CliError::Failed(format!(
            "no frames for `{site}` of `{}`",
            provider.id()
        )));
    }
    for plan in &plans {
        let urls: Vec<&str> = plan.parts.iter().map(|part| part.url.as_str()).collect();
        let names = frames::part_file_names(&plan.identity, &urls);
        writeln!(
            out,
            "frame {}{}: {} part(s){}",
            plan.identity,
            frame_time(&plan.identity)
                .map(|time| time.format(" (%Y-%m-%dT%H:%M:%SZ)").to_string())
                .unwrap_or_default(),
            plan.parts.len(),
            if plan.merge { ", merge" } else { "" }
        )?;
        if args.list {
            for part in &plan.parts {
                writeln!(out, "  {}", part.url)?;
            }
            continue;
        }
        let mut paths = Vec::new();
        for (part, name) in plan.parts.iter().zip(&names) {
            let path = args.output.join(name);
            if fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0) {
                writeln!(out, "have  {}", path.display())?;
                paths.push(path);
                continue;
            }
            paths.push(download(&part.url, &args.output, name, None, out)?);
        }
        if plan.merge && paths.len() > 1 {
            let joined: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            writeln!(
                out,
                "  parts of one scan; decode them together with --merge, for example:\n  recast-radar convert --merge --to level2 -o OUT {}",
                joined.join(" ")
            )?;
        }
        if provider.id() == "jma" {
            writeln!(
                out,
                "  a JMA tar holds every station; pass --station {site} to info, dump, render or convert"
            )?;
        }
    }
    Ok(())
}

/// Spaces requests to one server at least `interval` apart.
struct Pace {
    interval: Duration,
    last: Option<Instant>,
}

impl Pace {
    fn wait(&mut self) {
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < self.interval {
                thread::sleep(self.interval - elapsed);
            }
        }
        self.last = Some(Instant::now());
    }
}

fn polling_server(args: &PollingFetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let mut pace = Pace {
        interval: Duration::from_millis(args.interval_ms),
        last: None,
    };
    if let Some(site) = &args.site
        && !site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CliError::Usage(format!(
            "site `{site}` must be letters, digits, `_` or `-`"
        )));
    }
    let Some(site) = &args.site else {
        pace.wait();
        let sites = polling::fetch_site_config(&args.url).map_err(failed)?;
        for site in sites {
            writeln!(out, "{site}")?;
        }
        return Ok(());
    };
    pace.wait();
    let entries = polling::fetch_dir_list(&args.url, site).map_err(failed)?;
    if entries.is_empty() {
        return Err(CliError::Failed(format!(
            "{}: empty dir.list",
            polling::dir_list_url(&args.url, site)
        )));
    }
    if args.list {
        for entry in &entries {
            writeln!(out, "{:>10}  {}", entry.size, entry.name)?;
        }
        return Ok(());
    }
    // A listing can name files that are not volumes: one still being
    // written (`.tmp`, `.part`), a state or text file. Those are listed by
    // --list and never downloaded.
    let volumes: Vec<&polling::DirListEntry> = entries
        .iter()
        .filter(|entry| polling::is_volume_name(&entry.name))
        .collect();
    if volumes.is_empty() {
        return Err(CliError::Failed(format!(
            "{}: no volume listed",
            polling::dir_list_url(&args.url, site)
        )));
    }
    let start = volumes.len().saturating_sub(args.count as usize);
    let dir = args.output.join(site);
    for entry in &volumes[start..] {
        let path = dir.join(&entry.name);
        if fs::metadata(&path).is_ok_and(|m| m.len() == entry.size) {
            writeln!(out, "have  {}", path.display())?;
            continue;
        }
        pace.wait();
        // Polling servers rewrite a volume while it grows, so dir.list can
        // lag the file: the listed size is a hint, not a check.
        let path = download(
            &polling::site_file_url(&args.url, site, &entry.name),
            &dir,
            &entry.name,
            None,
            out,
        )?;
        if let Ok(metadata) = fs::metadata(&path)
            && metadata.len() != entry.size
        {
            writeln!(
                out,
                "note: dir.list said {} bytes; the file had changed to {}",
                entry.size,
                metadata.len()
            )?;
        }
    }
    Ok(())
}

fn sites(args: &SitesFetchArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let sites = recast_radar_data::fallback_sites();
    if args.json {
        let rows: Vec<_> = sites
            .iter()
            .map(|site| {
                json!({
                    "id": site.level2_id,
                    "name": site.name,
                    "latitude_deg": site.latitude_deg,
                    "longitude_deg": site.longitude_deg,
                })
            })
            .collect();
        serde_json::to_writer_pretty(&mut *out, &rows).map_err(std::io::Error::from)?;
        writeln!(out)?;
        return Ok(());
    }
    for site in sites {
        writeln!(
            out,
            "{:<5} {:<32} {:>9} {:>10}",
            site.level2_id,
            site.name.as_deref().unwrap_or("-"),
            site.latitude_deg
                .map_or("-".to_owned(), |v| format!("{v:.4}")),
            site.longitude_deg
                .map_or("-".to_owned(), |v| format!("{v:.4}")),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_objects_are_chosen_and_returned_in_time_order() {
        let objects: Vec<S3Object> = [
            "2024/03/15/KTLX/KTLX20240315_000217_V06",
            "2024/03/15/KTLX/KTLX20240315_000712_V06",
            "2024/03/15/KTLX/KTLX20240315_001206_V06",
        ]
        .into_iter()
        .map(|key| S3Object {
            key: key.to_owned(),
            size: 1,
            last_modified: None,
        })
        .collect();
        let target = NaiveDate::from_ymd_opt(2024, 3, 15)
            .and_then(|d| d.and_hms_opt(0, 11, 0))
            .map(|t| t.and_utc());
        let chosen = select_objects(
            objects,
            recast_radar_data::level2_object_time_utc,
            target,
            2,
        );
        let keys: Vec<&str> = chosen.iter().map(|o| object_name(&o.key)).collect();
        assert_eq!(keys, ["KTLX20240315_000712_V06", "KTLX20240315_001206_V06"]);
    }
}
