//! Real-feed survey helper: print the international providers' site tables,
//! the newest frame plan for chosen sites, or download those plans' parts
//! into a local cache, as JSON lines.
//!
//! - `sites` prints every provider's embedded site table (no network).
//! - `plan PROVIDER:SITE...` calls [`IntlProvider::latest`] for each pair (a
//!   catalog probe only: listings and HEAD requests) and prints the plan's
//!   identity, merge flag and part URLs. Besides the ids of
//!   [`intl_providers`], `dwd-dbzh` is DWD with its filtered DBZH as well
//!   ([`DwdProvider::filtered_reflectivity`]), `dwd-full` adds the dual-pol
//!   moments too ([`DwdProvider::dual_pol`]), `ord-complete` is ORD planning
//!   the last complete cycle ([`OrdProvider::complete_cycles`]), and
//!   `ord-vscan` is ORD planning only the velocity scan of a site that scans
//!   reflectivity and velocity separately
//!   ([`OrdProvider::velocity_scan_only`]; Belgium's `bejab` is one); they
//!   cache under their own names.
//! - `fetch DIR [--parts N] PROVIDER:SITE...` does the same, then downloads
//!   the plan's parts (the first `N` only, with `--parts`) with
//!   [`recast_radar_data::fetch_volume_bytes`] into
//!   `DIR/<provider>/<site>/<file>`, skipping parts already there, writes the
//!   plan (identity, part URLs, and the downloaded file names in plan order)
//!   to `DIR/<provider>/<site>/frame.json`, and prints one line per part with
//!   its local path.
//! - `imgw DIR SITE...` lists the newest IMGW POLRAD CMAX cycle of each site
//!   ([`recast_radar_data::grid_products::imgw::imgw_polrad_latest_cycle`], one
//!   listing request) and downloads its ZDR and KDP files into
//!   `DIR/imgw/<site>/`.
//! - `poll DIR SITE_URL...` reads each GR2Analyst-style site directory's
//!   `dir.list` with
//!   [`recast_radar_data::polling::latest_volume_or_single_site`] (a polling
//!   root whose `grlevel2.cfg` names one site, like the Laredo EWR feed,
//!   stands for that site) and downloads the newest volume into `DIR/<site>/`.
//!
//! Requests go one after another with a pause between them, to stay a light
//! client of the national servers. `docs/testdata/feeds-survey.md` was built
//! with this program and `examples/feeds_survey.rs`, which decodes the parts.
//!
//! Usage:
//!   cargo run --release -p recast-radar-data --example feeds_plans -- sites
//!   cargo run --release -p recast-radar-data --example feeds_plans -- plan dwd:boo ord:nlhrw
//!   cargo run --release -p recast-radar-data --example feeds_plans -- fetch C:/corpus/feeds dwd:boo
//!   cargo run --release -p recast-radar-data --example feeds_plans -- fetch C:/corpus/feeds --parts 1 ord:frlep
//!   cargo run --release -p recast-radar-data --example feeds_plans -- imgw C:/corpus/feeds ram
//!   cargo run --release -p recast-radar-data --example feeds_plans -- poll C:/corpus/feeds/polling https://mesonet-nexrad.agron.iastate.edu/level2/raw/FWLX

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use recast_radar_data::grid_products::imgw::{
    ImgwPolradQuantity, ImgwPolradSite, imgw_polrad_latest_cycle,
};
use recast_radar_data::international::{
    DwdProvider, FramePlan, IntlProvider, OrdProvider, intl_providers,
};
use serde_json::{Value, json};

/// Minimum spacing between two requests this program starts itself.
const PAUSE: Duration = Duration::from_millis(1500);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sites") => print_sites(),
        Some("plan") if args.len() > 1 => run(&args[1..], None, None),
        Some("fetch") if args.len() > 2 => {
            let (max_parts, pairs) = match (args[2].as_str(), args.get(3)) {
                ("--parts", Some(count)) => match count.parse::<usize>() {
                    Ok(count) => (Some(count), &args[4..]),
                    Err(_) => usage(),
                },
                _ => (None, &args[2..]),
            };
            run(pairs, Some(Path::new(&args[1])), max_parts);
        }
        Some("imgw") if args.len() > 2 => imgw(Path::new(&args[1]), &args[2..]),
        Some("poll") if args.len() > 2 => poll(Path::new(&args[1]), &args[2..]),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: feeds_plans sites | plan PROVIDER:SITE... | fetch DIR [--parts N] PROVIDER:SITE... | imgw DIR SITE... | poll DIR SITE_URL..."
    );
    std::process::exit(2);
}

/// IMGW POLRAD CMAX (ODIM_H5 Cartesian `IMAGE` products): the newest cycle of
/// each site, and its ZDR and KDP files (two of the cycle's four) into
/// `DIR/imgw/<site>/`.
fn imgw(cache: &Path, sites: &[String]) {
    let mut pacer = Pacer(None);
    for code in sites {
        let Some(site) = ImgwPolradSite::from_code(code) else {
            println!(
                "{}",
                json!({"site": code, "ok": false, "error": "unknown IMGW POLRAD site"})
            );
            continue;
        };
        pacer.wait();
        let cycle = match imgw_polrad_latest_cycle(site) {
            Ok(cycle) => cycle,
            Err(error) => {
                println!("{}", json!({"site": code, "ok": false, "error": error}));
                continue;
            }
        };
        println!(
            "{}",
            json!({
                "site": code,
                "ok": true,
                "identity": cycle.identity,
                "observed_at": cycle.observed_at.to_rfc3339(),
                "files": cycle.files.iter().map(|file| file.filename.clone()).collect::<Vec<_>>(),
            })
        );
        let dir = cache.join("imgw").join(sanitize(site.code()));
        let pair = format!("imgw:{}", site.code());
        let wanted = [ImgwPolradQuantity::Zdr, ImgwPolradQuantity::Kdp];
        for (index, quantity) in wanted.into_iter().enumerate() {
            if let Some(file) = cycle.file(quantity) {
                println!(
                    "{}",
                    fetch_part(&pair, index, &file.download_url, &dir, &mut pacer)
                );
            }
        }
    }
}

/// GR2Analyst-style polling directories (`recast_radar_data::polling`):
/// read each site directory's `dir.list` (or, for a root naming one site,
/// that site's), then download its newest volume into
/// `DIR/<last URL segment of the site directory>/`.
fn poll(cache: &Path, poll_urls: &[String]) {
    let mut pacer = Pacer(None);
    for poll_url in poll_urls {
        pacer.wait();
        let found = match recast_radar_data::polling::latest_volume_or_single_site(poll_url) {
            Ok(found) => found,
            Err(error) => {
                println!(
                    "{}",
                    json!({"site_url": poll_url, "ok": false, "error": error.to_string()})
                );
                continue;
            }
        };
        let site_url = found.site_url.as_str();
        println!(
            "{}",
            json!({
                "site_url": site_url,
                "ok": true,
                "newest": found.entry.name,
                "listed_size": found.entry.size,
            })
        );
        let site = site_url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("site");
        let dir = cache.join(sanitize(site));
        println!("{}", fetch_part(site_url, 0, &found.url, &dir, &mut pacer));
    }
}

fn print_sites() {
    for provider in intl_providers() {
        for site in provider.static_sites() {
            println!(
                "{}",
                json!({
                    "provider": provider.id(),
                    "site": site.site_id,
                    "label": site.label,
                    "country": site.country,
                    "lat": site.latitude_deg,
                    "lon": site.longitude_deg,
                })
            );
        }
    }
}

/// Spaces what this program starts at least [`PAUSE`] apart: each provider
/// `latest` call, each IMGW cycle listing, each poll and each download. The
/// requests inside one call are not spaced: a DWD `latest` makes its station,
/// `hdf5/` and sweep listing requests back to back (8 for one frame with the
/// filtered DBZH), and `latest_volume_or_single_site` up to three.
struct Pacer(Option<Instant>);

impl Pacer {
    fn wait(&mut self) {
        if let Some(previous) = self.0 {
            let elapsed = previous.elapsed();
            if elapsed < PAUSE {
                std::thread::sleep(PAUSE - elapsed);
            }
        }
        self.0 = Some(Instant::now());
    }
}

fn run(pairs: &[String], cache: Option<&Path>, max_parts: Option<usize>) {
    let providers = intl_providers();
    let mut pacer = Pacer(None);
    for pair in pairs {
        let Some((provider_id, site)) = pair.split_once(':') else {
            println!(
                "{}",
                json!({"pair": pair, "ok": false, "error": "expected PROVIDER:SITE"})
            );
            continue;
        };
        let variant = provider_variant(provider_id);
        let found = variant.as_deref().or_else(|| {
            providers
                .iter()
                .find(|p| p.id() == provider_id)
                .map(|p| &**p)
        });
        let Some(provider) = found else {
            println!(
                "{}",
                json!({"pair": pair, "ok": false, "error": "unknown provider"})
            );
            continue;
        };
        pacer.wait();
        let started = Instant::now();
        let plan = provider.latest(site);
        let probe_ms = started.elapsed().as_millis();
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                println!(
                    "{}",
                    json!({"pair": pair, "ok": false, "error": error, "probe_ms": probe_ms})
                );
                continue;
            }
        };
        println!("{}", plan_line(pair, &plan, probe_ms));
        if let Some(cache) = cache {
            let dir = cache.join(provider_id).join(sanitize(site));
            let count = plan.parts.len().min(max_parts.unwrap_or(usize::MAX));
            let parts = &plan.parts[..count];
            for (index, part) in parts.iter().enumerate() {
                println!("{}", fetch_part(pair, index, &part.url, &dir, &mut pacer));
            }
            println!("{}", write_frame(pair, &plan, count, &dir));
        }
    }
}

/// Provider options the survey plans under names of their own (the
/// provider's `id()` stays its own).
fn provider_variant(name: &str) -> Option<Box<dyn IntlProvider>> {
    match name {
        "dwd-dbzh" => Some(Box::new(DwdProvider::new().filtered_reflectivity(true))),
        "dwd-full" => Some(Box::new(
            DwdProvider::new()
                .dual_pol(true)
                .filtered_reflectivity(true),
        )),
        "ord-complete" => Some(Box::new(OrdProvider::new().complete_cycles(true))),
        "ord-vscan" => Some(Box::new(OrdProvider::new().velocity_scan_only(true))),
        _ => None,
    }
}

/// Record the plan in `dir/frame.json`: identity, merge flag, every part URL
/// in plan order, and the file names of the first `count` parts (the ones
/// downloaded), which `tools/feeds_survey/run_upstream_survey.py` merges in
/// that order.
fn write_frame(pair: &str, plan: &FramePlan, count: usize, dir: &Path) -> Value {
    let frame = json!({
        "pair": pair,
        "identity": plan.identity,
        "merge": plan.merge,
        "parts": plan.parts.iter().map(|part| part.url.clone()).collect::<Vec<_>>(),
        "files": plan.parts[..count]
            .iter()
            .map(|part| file_name(&part.url).display().to_string())
            .collect::<Vec<_>>(),
    });
    let path = dir.join("frame.json");
    let display = path.display().to_string();
    match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, frame.to_string())) {
        Ok(()) => json!({"pair": pair, "ok": true, "frame": display}),
        Err(error) => {
            json!({"pair": pair, "ok": false, "error": format!("write {display}: {error}")})
        }
    }
}

fn plan_line(pair: &str, plan: &FramePlan, probe_ms: u128) -> Value {
    json!({
        "pair": pair,
        "ok": true,
        "identity": plan.identity,
        "merge": plan.merge,
        "parts": plan.parts.iter().map(|part| part.url.clone()).collect::<Vec<_>>(),
        "probe_ms": probe_ms,
    })
}

fn fetch_part(pair: &str, index: usize, url: &str, dir: &Path, pacer: &mut Pacer) -> Value {
    let path = dir.join(file_name(url));
    let display = path.display().to_string();
    if path.is_file() {
        return json!({"pair": pair, "part": index, "url": url, "path": display, "cached": true});
    }
    pacer.wait();
    let started = Instant::now();
    let bytes = match recast_radar_data::fetch_volume_bytes(url) {
        Ok(bytes) => bytes,
        Err(error) => {
            return json!({"pair": pair, "part": index, "url": url, "ok": false, "error": error.to_string()});
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    if let Err(error) = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, &bytes)) {
        return json!({"pair": pair, "part": index, "url": url, "ok": false, "error": format!("write {display}: {error}")});
    }
    json!({
        "pair": pair,
        "part": index,
        "url": url,
        "ok": true,
        "path": display,
        "bytes": bytes.len(),
        "download_ms": elapsed_ms,
    })
}

/// Last path segment of a URL (query dropped), made safe as a file name.
fn file_name(url: &str) -> PathBuf {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last = without_query
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("part");
    PathBuf::from(sanitize(last))
}

fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "part".to_owned()
    } else {
        cleaned
    }
}
