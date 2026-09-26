//! Network primitives behind `recast_radar.fetch` (feature `net`).
//!
//! Each function wraps one `recast_radar_data` call, runs it with the GIL
//! released (so Python threads can download in parallel) and returns plain
//! Python values. Selection, pacing and writing files are done in
//! `recast_radar/fetch.py`.

use chrono::{DateTime, NaiveDate, Utc};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use recast_radar_cli::frames;
use recast_radar_data::international::{
    ArchiveFrames, FramePlan, IntlProvider, IntlSite, intl_providers,
};
use recast_radar_data::{LEVEL2_ARCHIVE_BUCKET, LEVEL2_CHUNKS_BUCKET, S3Object, level3, polling};

use crate::errors::FetchError;

fn fetch_error(err: impl std::fmt::Display) -> PyErr {
    FetchError::new_err(err.to_string())
}

fn date(text: &str) -> PyResult<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|err| PyValueError::new_err(format!("date {text:?} is not YYYY-MM-DD: {err}")))
}

fn iso(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

fn object_dict<'py>(
    py: Python<'py>,
    object: &S3Object,
    url: String,
    time: Option<DateTime<Utc>>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("key", &object.key)?;
    dict.set_item("name", object.key.rsplit('/').next().unwrap_or(&object.key))?;
    dict.set_item("size", object.size)?;
    dict.set_item("time", time.map(iso))?;
    dict.set_item("url", url)?;
    Ok(dict)
}

fn level2_list<'py>(py: Python<'py>, objects: &[S3Object]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for object in objects {
        let url = format!(
            "https://{LEVEL2_ARCHIVE_BUCKET}.s3.amazonaws.com/{}",
            object.key
        );
        let time = recast_radar_data::level2_object_time_utc(object);
        list.append(object_dict(py, object, url, time)?)?;
    }
    Ok(list)
}

fn level3_list<'py>(py: Python<'py>, objects: &[S3Object]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for object in objects {
        let url = level3::level3_object_url(&object.key);
        let time = level3::level3_key_time(&object.key);
        list.append(object_dict(py, object, url, time)?)?;
    }
    Ok(list)
}

/// Level II volumes of `site` on `date` (YYYY-MM-DD) in the AWS archive.
#[pyfunction]
fn _level2_objects<'py>(py: Python<'py>, site: &str, date: &str) -> PyResult<Bound<'py, PyList>> {
    let site = site.to_ascii_uppercase();
    let date = self::date(date)?;
    let objects = py
        .detach(|| recast_radar_data::level2_objects_for_date(&site, date))
        .map_err(fetch_error)?;
    level2_list(py, &objects)
}

/// The newest `count` Level II volumes of `site`, looking back `days_back`
/// days, newest first.
#[pyfunction]
fn _recent_level2_objects<'py>(
    py: Python<'py>,
    site: &str,
    days_back: i64,
    count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let site = site.to_ascii_uppercase();
    let objects = py
        .detach(|| recast_radar_data::recent_level2_objects(&site, days_back, count))
        .map_err(fetch_error)?;
    level2_list(py, &objects)
}

/// Level III products `product` of `site` on `date` in the AWS archive.
#[pyfunction]
fn _level3_objects<'py>(
    py: Python<'py>,
    site: &str,
    product: &str,
    date: &str,
) -> PyResult<Bound<'py, PyList>> {
    let date = self::date(date)?;
    let objects = py
        .detach(|| level3::level3_objects_for_date(site, product, date))
        .map_err(fetch_error)?;
    level3_list(py, &objects)
}

/// The newest `count` Level III products, newest first.
#[pyfunction]
fn _recent_level3_objects<'py>(
    py: Python<'py>,
    site: &str,
    product: &str,
    days_back: i64,
    count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let objects = py
        .detach(|| level3::recent_level3_objects(site, product, days_back, count))
        .map_err(fetch_error)?;
    level3_list(py, &objects)
}

/// The newest real-time Level II volume of `site` in the chunks bucket:
/// `{"site", "volume_id", "volume_time", "complete", "total_size",
/// "chunks": [{"key", "name", "size", "url", "type", "chunk_id"}]}`.
#[pyfunction]
fn _realtime_volume<'py>(py: Python<'py>, site: &str) -> PyResult<Bound<'py, PyDict>> {
    let site = site.to_ascii_uppercase();
    let volume = py
        .detach(|| recast_radar_data::latest_realtime_level2_volume(&site))
        .map_err(fetch_error)?;
    let dict = PyDict::new(py);
    dict.set_item("site", &volume.site)?;
    dict.set_item("volume_id", volume.volume_id)?;
    dict.set_item("volume_time", iso(volume.volume_time))?;
    dict.set_item("complete", volume.complete)?;
    dict.set_item("total_size", volume.total_size)?;
    let chunks = PyList::empty(py);
    for chunk in &volume.chunks {
        let url = format!(
            "https://{LEVEL2_CHUNKS_BUCKET}.s3.amazonaws.com/{}",
            chunk.object.key
        );
        let entry = object_dict(py, &chunk.object, url, chunk.object.last_modified)?;
        entry.set_item("type", chunk.chunk_type.label())?;
        entry.set_item("chunk_id", chunk.chunk_id)?;
        chunks.append(entry)?;
    }
    dict.set_item("chunks", chunks)?;
    Ok(dict)
}

/// Download `url` (radar volumes up to the data crate's size cap, with its
/// retry policy).
#[pyfunction]
fn _fetch_bytes<'py>(py: Python<'py>, url: &str) -> PyResult<Bound<'py, PyBytes>> {
    let bytes = py
        .detach(|| recast_radar_data::fetch_volume_bytes(url))
        .map_err(|err| fetch_error(format!("{url}: {err}")))?;
    Ok(PyBytes::new(py, &bytes))
}

fn find_provider(id: &str) -> PyResult<Box<dyn IntlProvider>> {
    let wanted = id.to_ascii_lowercase();
    let providers = intl_providers();
    let known: Vec<&str> = providers.iter().map(|provider| provider.id()).collect();
    let message = format!("unknown provider {id:?} (known: {})", known.join(", "));
    providers
        .into_iter()
        .find(|provider| provider.id() == wanted)
        .ok_or_else(|| PyValueError::new_err(message))
}

fn site_dict<'py>(py: Python<'py>, site: &IntlSite) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("provider", site.provider_id)?;
    dict.set_item("id", &site.site_id)?;
    dict.set_item("name", &site.label)?;
    dict.set_item("country", site.country)?;
    dict.set_item("latitude", site.latitude_deg)?;
    dict.set_item("longitude", site.longitude_deg)?;
    Ok(dict)
}

/// Every international provider: `{"id", "name", "country", "sites",
/// "recent", "archive"}`.
#[pyfunction]
fn _intl_providers(py: Python<'_>) -> PyResult<Bound<'_, PyList>> {
    let list = PyList::empty(py);
    for provider in intl_providers() {
        let dict = PyDict::new(py);
        dict.set_item("id", provider.id())?;
        dict.set_item("name", provider.label())?;
        dict.set_item("country", provider.country())?;
        dict.set_item("sites", provider.static_sites().len())?;
        dict.set_item("recent", provider.supports_recent())?;
        dict.set_item("archive", provider.supports_archive())?;
        list.append(dict)?;
    }
    Ok(list)
}

/// A provider's sites: the built-in list, or the provider's live catalog
/// with `online=True`.
#[pyfunction]
fn _intl_sites<'py>(py: Python<'py>, provider: &str, online: bool) -> PyResult<Bound<'py, PyList>> {
    let provider = find_provider(provider)?;
    let sites = if online {
        py.detach(|| provider.list_sites()).map_err(fetch_error)?
    } else {
        provider.static_sites()
    };
    let list = PyList::empty(py);
    for site in &sites {
        list.append(site_dict(py, site)?)?;
    }
    Ok(list)
}

/// Frames as `{"identity", "time", "merge", "urls", "names"}`: `time` is
/// the scan time read from the identity (ISO 8601, or `None`), `names` the
/// file names the parts are saved under
/// (`recast_radar_cli::frames::part_file_names`).
fn frame_list<'py>(py: Python<'py>, plans: &[FramePlan]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for plan in plans {
        let dict = PyDict::new(py);
        dict.set_item("identity", &plan.identity)?;
        dict.set_item("time", frames::frame_time(&plan.identity).map(iso))?;
        dict.set_item("merge", plan.merge)?;
        let urls: Vec<&str> = plan.parts.iter().map(|part| part.url.as_str()).collect();
        dict.set_item("names", frames::part_file_names(&plan.identity, &urls))?;
        dict.set_item("urls", urls)?;
        list.append(dict)?;
    }
    Ok(list)
}

/// The newest `count` frames of a provider's site, oldest first (see
/// [`frame_list`]). A frame with `merge` true is split into parts of one
/// scan.
#[pyfunction]
fn _intl_frames<'py>(
    py: Python<'py>,
    provider: &str,
    site: &str,
    count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let provider = find_provider(provider)?;
    let plans = py
        .detach(|| {
            if count > 1 {
                provider.recent(site, count)
            } else {
                provider.latest(site).map(|plan| vec![plan])
            }
        })
        .map_err(fetch_error)?;
    frame_list(py, &plans)
}

/// `ValueError` naming the providers with an archive unless `provider`
/// has one.
fn require_archive(provider: &dyn IntlProvider) -> PyResult<()> {
    provider.archive_source().map(|_| ()).ok_or_else(|| {
        let with_archive: Vec<&str> = intl_providers()
            .iter()
            .filter(|provider| provider.supports_archive())
            .map(|provider| provider.id())
            .collect();
        PyValueError::new_err(format!(
            "{:?} has no archive (providers with one: {})",
            provider.id(),
            with_archive.join(", ")
        ))
    })
}

/// The provider's archive (`ArchiveFrames` is not `Sync`, so it is looked
/// up again on the thread that runs without the GIL).
fn archive(provider: &dyn IntlProvider) -> Result<&dyn ArchiveFrames, String> {
    provider
        .archive_source()
        .ok_or_else(|| format!("{} has no archive", provider.id()))
}

/// Every archived frame of a site on the UTC date `date`, oldest first.
#[pyfunction]
fn _intl_archive_day<'py>(
    py: Python<'py>,
    provider: &str,
    site: &str,
    date: &str,
) -> PyResult<Bound<'py, PyList>> {
    let provider = find_provider(provider)?;
    require_archive(provider.as_ref())?;
    let date = self::date(date)?;
    let plans = py
        .detach(|| archive(provider.as_ref())?.day_plans(site, date))
        .map_err(fetch_error)?;
    frame_list(py, &plans)
}

/// The `count` archived frames of a site nearest `when` (ISO 8601, UTC),
/// oldest first; the archive is searched
/// `recast_radar_cli::frames::archive_search_span(count)` either side.
#[pyfunction]
fn _intl_archive_nearest<'py>(
    py: Python<'py>,
    provider: &str,
    site: &str,
    when: &str,
    count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let provider = find_provider(provider)?;
    require_archive(provider.as_ref())?;
    let target = DateTime::parse_from_rfc3339(when)
        .map_err(|err| PyValueError::new_err(format!("time {when:?}: {err}")))?
        .with_timezone(&Utc);
    let span = frames::archive_search_span(count);
    let plans = py
        .detach(|| {
            archive(provider.as_ref())?.window_plans(site, target - span, target + span, usize::MAX)
        })
        .map_err(fetch_error)?;
    let times: Vec<_> = plans
        .iter()
        .map(|plan| frames::frame_time(&plan.identity))
        .collect();
    let keep = frames::nearest(&times, target, count);
    let chosen: Vec<FramePlan> = plans
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.binary_search(index).is_ok())
        .map(|(_, plan)| plan)
        .collect();
    frame_list(py, &chosen)
}

/// The sites a GR2Analyst polling server lists in its `config.cfg`.
#[pyfunction]
fn _polling_sites(py: Python<'_>, url: &str) -> PyResult<Vec<String>> {
    py.detach(|| polling::fetch_site_config(url))
        .map_err(fetch_error)
}

/// A polling site's `dir.list`: `(size, name)` pairs, oldest first.
#[pyfunction]
fn _polling_dir_list(py: Python<'_>, url: &str, site: &str) -> PyResult<Vec<(u64, String)>> {
    let entries = py
        .detach(|| polling::fetch_dir_list(url, site))
        .map_err(fetch_error)?;
    Ok(entries
        .into_iter()
        .map(|entry| (entry.size, entry.name))
        .collect())
}

/// The URL of a file in a polling site's folder.
#[pyfunction]
fn _polling_file_url(url: &str, site: &str, name: &str) -> String {
    polling::site_file_url(url, site, name)
}

/// The built-in NEXRAD site table: `{"id", "name", "latitude",
/// "longitude"}`.
#[pyfunction]
fn _nexrad_sites(py: Python<'_>) -> PyResult<Bound<'_, PyList>> {
    let list = PyList::empty(py);
    for site in recast_radar_data::fallback_sites() {
        let dict = PyDict::new(py);
        dict.set_item("id", &site.level2_id)?;
        dict.set_item("name", &site.name)?;
        dict.set_item("latitude", site.latitude_deg)?;
        dict.set_item("longitude", site.longitude_deg)?;
        list.append(dict)?;
    }
    Ok(list)
}

/// Add this module's functions.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_level2_objects, module)?)?;
    module.add_function(wrap_pyfunction!(_recent_level2_objects, module)?)?;
    module.add_function(wrap_pyfunction!(_level3_objects, module)?)?;
    module.add_function(wrap_pyfunction!(_recent_level3_objects, module)?)?;
    module.add_function(wrap_pyfunction!(_realtime_volume, module)?)?;
    module.add_function(wrap_pyfunction!(_fetch_bytes, module)?)?;
    module.add_function(wrap_pyfunction!(_intl_providers, module)?)?;
    module.add_function(wrap_pyfunction!(_intl_sites, module)?)?;
    module.add_function(wrap_pyfunction!(_intl_frames, module)?)?;
    module.add_function(wrap_pyfunction!(_intl_archive_day, module)?)?;
    module.add_function(wrap_pyfunction!(_intl_archive_nearest, module)?)?;
    module.add_function(wrap_pyfunction!(_polling_sites, module)?)?;
    module.add_function(wrap_pyfunction!(_polling_dir_list, module)?)?;
    module.add_function(wrap_pyfunction!(_polling_file_url, module)?)?;
    module.add_function(wrap_pyfunction!(_nexrad_sites, module)?)?;
    Ok(())
}
