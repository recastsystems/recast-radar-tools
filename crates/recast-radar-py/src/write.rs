//! Writing volumes, through the same backend registry as the command-line
//! tool (`recast_radar_cli::backend::Backends::builtin`).
//!
//! `Backends::builtin` registers the Level II, CfRadial 1, ODIM_H5 and FM301
//! writers and the polling-directory publisher. A format whose writer a
//! build leaves out raises `UnavailableError` (a `NotImplementedError`)
//! before anything is written.
//!
//! Every write returns the writer's report beside its result: the fields
//! and sweeps left out, and notes (codings coarser than the source, radials
//! reordered, a missing Nyquist velocity), which the Python package turns
//! into `WriteWarning`s.

use std::fs;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use pyo3::exceptions::{PyFileExistsError, PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use recast_radar_cli::backend::{
    BackendError, Backends, FieldMapping, Level2Compression, Level2Quantization, OutputFormat,
    PublishRequest, SitePosition, VolumeEdits, WriteInput, WriteOptions, WriteReport,
};

use crate::errors::{UnavailableError, UnrepresentableError};
use crate::volume::PyVolume;

fn output_format(name: &str) -> PyResult<OutputFormat> {
    let wanted = name.to_ascii_lowercase().replace('_', "-");
    let format = match wanted.as_str() {
        "level2" | "nexrad" | "ar2v" => OutputFormat::Level2,
        "cfradial1" | "cfradial" => OutputFormat::CfRadial1,
        "odim" | "odim-h5" => OutputFormat::OdimH5,
        "fm301" | "cfradial2" => OutputFormat::Fm301,
        _ => {
            let known: Vec<&str> = OutputFormat::ALL
                .iter()
                .map(|format| format.cli_name())
                .collect();
            return Err(PyValueError::new_err(format!(
                "unknown format {name:?} (known: {})",
                known.join(", ")
            )));
        }
    };
    Ok(format)
}

fn compression(name: &str) -> PyResult<Level2Compression> {
    match name {
        "bzip2" => Ok(Level2Compression::Bzip2),
        "none" => Ok(Level2Compression::None),
        other => Err(PyValueError::new_err(format!(
            "compression must be \"bzip2\" or \"none\", not {other:?}"
        ))),
    }
}

fn quantization(name: &str) -> PyResult<Level2Quantization> {
    match name {
        "precise" => Ok(Level2Quantization::Precise),
        "compatible" => Ok(Level2Quantization::Compatible),
        "standard" => Ok(Level2Quantization::Standard),
        other => Err(PyValueError::new_err(format!(
            "quantization must be \"precise\", \"compatible\" or \"standard\", not {other:?}"
        ))),
    }
}

/// The writer options and volume edits every write takes, from the keyword
/// arguments of `recast_radar.write` and its siblings.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriteArgs {
    options: WriteOptions,
    edits: VolumeEdits,
}

impl WriteArgs {
    #[allow(clippy::too_many_arguments)]
    fn new(
        compression: &str,
        site: Option<String>,
        quantization: &str,
        nyquist_velocity: Option<f32>,
        unambiguous_range: Option<f32>,
        drop_negative_range_gates: bool,
        sweeps: Option<Vec<usize>>,
        sweeps_in_time_order: bool,
        sweeps_by_elevation: bool,
        fields: Option<Vec<String>>,
        field_map: Option<Vec<(String, String)>>,
        position: Option<(f64, f64, f64)>,
        strict: bool,
    ) -> PyResult<Self> {
        let mut options = WriteOptions::new(self::compression(compression)?, site_override(site)?);
        options.level2_quantization = self::quantization(quantization)?;
        options.nyquist_velocity_mps = nyquist_velocity;
        options.unambiguous_range_m = unambiguous_range;
        options.drop_negative_range_gates = drop_negative_range_gates;
        options.strict = strict;
        options.level2_field_map = field_map
            .unwrap_or_default()
            .iter()
            .map(|(field, moment)| FieldMapping::new(field, moment))
            .collect::<Result<_, _>>()
            .map_err(PyValueError::new_err)?;
        let mut edits = VolumeEdits::default();
        edits.sweeps = sweeps;
        edits.sweeps_in_time_order = sweeps_in_time_order;
        edits.sweeps_by_elevation = sweeps_by_elevation;
        edits.fields = fields;
        edits.position = match position {
            Some((latitude_deg, longitude_deg, altitude_m)) => {
                let position = SitePosition {
                    latitude_deg,
                    longitude_deg,
                    altitude_m,
                };
                if !position.is_valid() {
                    return Err(PyValueError::new_err(format!(
                        "position {position:?}: latitude within 90 degrees, longitude within 360 \
                         and a finite height"
                    )));
                }
                Some(position)
            }
            None => None,
        };
        Ok(Self { options, edits })
    }
}

/// A writer's report for Python: `(left_out, notes)`.
type Report = (Vec<String>, Vec<String>);

fn report(report: WriteReport) -> Report {
    (report.left_out, report.notes)
}

fn site_override(site: Option<String>) -> PyResult<Option<String>> {
    match site {
        Some(site) if site.trim().is_empty() || !site.is_ascii() => Err(PyValueError::new_err(
            format!("site {site:?} must be a non-empty ASCII identifier"),
        )),
        other => Ok(other),
    }
}

fn backend_error(err: BackendError) -> PyErr {
    if err.is_unavailable() {
        return UnavailableError::new_err(err.to_string());
    }
    match err {
        BackendError::Unrepresentable { .. } => UnrepresentableError::new_err(err.to_string()),
        BackendError::Io(io) => PyOSError::new_err(io.to_string()),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Encode `volume` in `format` into memory, optionally gzip-wrapped.
fn encode(
    py: Python<'_>,
    volume: &PyVolume,
    format: OutputFormat,
    args: &WriteArgs,
    gzip: bool,
) -> PyResult<(Vec<u8>, WriteReport)> {
    let backends = Backends::builtin();
    let writer = backends.writer(format).map_err(backend_error)?;
    let loaded = volume.loaded();
    let source_name = loaded.volume.provenance.source_path.clone();
    py.detach(|| {
        let edited = args
            .edits
            .apply(&loaded.volume)
            .map_err(PyValueError::new_err)?;
        let input = WriteInput {
            volume: &edited,
            metadata: &loaded.metadata,
            source_name: source_name.as_deref(),
        };
        let mut bytes = Vec::new();
        let written = if gzip {
            let mut encoder = GzEncoder::new(&mut bytes, Compression::default());
            let written = writer.write(&input, &args.options, &mut encoder);
            written.and_then(|report| {
                encoder.finish()?;
                Ok(report)
            })
        } else {
            writer.write(&input, &args.options, &mut bytes)
        };
        let report = written.map_err(backend_error)?;
        Ok((bytes, report))
    })
}

/// Write `bytes` to `path` through a temporary file in the same directory,
/// so a failed write never leaves a truncated file under the final name.
fn write_atomically(path: &Path, bytes: &[u8], overwrite: bool) -> PyResult<()> {
    if !overwrite && path.exists() {
        return Err(PyFileExistsError::new_err(format!(
            "{} exists (pass overwrite=True to replace it)",
            path.display()
        )));
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_owned());
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if let Err(err) = result {
        let _ = fs::remove_file(&temp);
        return Err(PyOSError::new_err(format!("{}: {err}", path.display())));
    }
    Ok(())
}

/// `Volume.write`: encode and write a file; returns the path and the
/// writer's report.
#[pyfunction]
#[pyo3(signature = (
    volume, path, format, *, compression="bzip2", gzip=false, site=None, overwrite=false,
    quantization="standard", nyquist_velocity=None, unambiguous_range=None,
    drop_negative_range_gates=false, sweeps=None, sweeps_in_time_order=false, sweeps_by_elevation=false,
    fields=None, field_map=None, position=None,
    strict=false
))]
#[allow(clippy::too_many_arguments)]
fn _write(
    py: Python<'_>,
    volume: &PyVolume,
    path: PathBuf,
    format: &str,
    compression: &str,
    gzip: bool,
    site: Option<String>,
    overwrite: bool,
    quantization: &str,
    nyquist_velocity: Option<f32>,
    unambiguous_range: Option<f32>,
    drop_negative_range_gates: bool,
    sweeps: Option<Vec<usize>>,
    sweeps_in_time_order: bool,
    sweeps_by_elevation: bool,
    fields: Option<Vec<String>>,
    field_map: Option<Vec<(String, String)>>,
    position: Option<(f64, f64, f64)>,
    strict: bool,
) -> PyResult<(PathBuf, Report)> {
    let format = output_format(format)?;
    let args = WriteArgs::new(
        compression,
        site,
        quantization,
        nyquist_velocity,
        unambiguous_range,
        drop_negative_range_gates,
        sweeps,
        sweeps_in_time_order,
        sweeps_by_elevation,
        fields,
        field_map,
        position,
        strict,
    )?;
    let (bytes, written) = encode(py, volume, format, &args, gzip)?;
    py.detach(|| write_atomically(&path, &bytes, overwrite))?;
    Ok((path, report(written)))
}

/// `Volume.to_bytes`: encode into memory; returns the bytes and the
/// writer's report.
#[pyfunction]
#[pyo3(signature = (
    volume, format, *, compression="bzip2", gzip=false, site=None, quantization="standard",
    nyquist_velocity=None, unambiguous_range=None, drop_negative_range_gates=false, sweeps=None,
    sweeps_in_time_order=false, sweeps_by_elevation=false,
    fields=None, field_map=None, position=None, strict=false
))]
#[allow(clippy::too_many_arguments)]
fn _to_bytes<'py>(
    py: Python<'py>,
    volume: &PyVolume,
    format: &str,
    compression: &str,
    gzip: bool,
    site: Option<String>,
    quantization: &str,
    nyquist_velocity: Option<f32>,
    unambiguous_range: Option<f32>,
    drop_negative_range_gates: bool,
    sweeps: Option<Vec<usize>>,
    sweeps_in_time_order: bool,
    sweeps_by_elevation: bool,
    fields: Option<Vec<String>>,
    field_map: Option<Vec<(String, String)>>,
    position: Option<(f64, f64, f64)>,
    strict: bool,
) -> PyResult<(Bound<'py, PyBytes>, Report)> {
    let format = output_format(format)?;
    let args = WriteArgs::new(
        compression,
        site,
        quantization,
        nyquist_velocity,
        unambiguous_range,
        drop_negative_range_gates,
        sweeps,
        sweeps_in_time_order,
        sweeps_by_elevation,
        fields,
        field_map,
        position,
        strict,
    )?;
    let (bytes, written) = encode(py, volume, format, &args, gzip)?;
    Ok((PyBytes::new(py, &bytes), report(written)))
}

/// One real-time chunk for Python: key, kind letter, number, bytes.
type ChunkRow<'py> = (String, char, u16, Bound<'py, PyBytes>);

/// `recast_radar.write_chunks`: the volume as NEXRAD real-time chunks,
/// `[(key, kind, number, bytes)]` in order, where `key` is the chunk's
/// object key in the chunks bucket (`SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-K`)
/// and `kind` its letter (`S`, `I` or `E`), and the writer's report.
#[pyfunction]
#[pyo3(signature = (
    volume, *, site=None, quantization="standard", nyquist_velocity=None, unambiguous_range=None,
    drop_negative_range_gates=false, sweeps=None, sweeps_in_time_order=false, sweeps_by_elevation=false,
    fields=None, field_map=None, position=None,
    strict=false
))]
#[allow(clippy::too_many_arguments)]
fn _write_chunks<'py>(
    py: Python<'py>,
    volume: &PyVolume,
    site: Option<String>,
    quantization: &str,
    nyquist_velocity: Option<f32>,
    unambiguous_range: Option<f32>,
    drop_negative_range_gates: bool,
    sweeps: Option<Vec<usize>>,
    sweeps_in_time_order: bool,
    sweeps_by_elevation: bool,
    fields: Option<Vec<String>>,
    field_map: Option<Vec<(String, String)>>,
    position: Option<(f64, f64, f64)>,
    strict: bool,
) -> PyResult<(Vec<ChunkRow<'py>>, Report)> {
    let backends = Backends::builtin();
    let writer = backends
        .writer(OutputFormat::Level2)
        .map_err(backend_error)?;
    if !writer.supports_chunks() {
        return Err(backend_error(BackendError::ChunksUnavailable(
            OutputFormat::Level2,
        )));
    }
    let args = WriteArgs::new(
        "bzip2",
        site,
        quantization,
        nyquist_velocity,
        unambiguous_range,
        drop_negative_range_gates,
        sweeps,
        sweeps_in_time_order,
        sweeps_by_elevation,
        fields,
        field_map,
        position,
        strict,
    )?;
    let loaded = volume.loaded();
    let source_name = loaded.volume.provenance.source_path.clone();
    let chunked = py.detach(|| {
        let edited = args
            .edits
            .apply(&loaded.volume)
            .map_err(PyValueError::new_err)?;
        let input = WriteInput {
            volume: &edited,
            metadata: &loaded.metadata,
            source_name: source_name.as_deref(),
        };
        writer
            .write_chunks(&input, &args.options)
            .map_err(backend_error)
    })?;
    let rows = chunked
        .chunks
        .iter()
        .map(|chunk| {
            (
                chunked.chunk_key(chunk),
                chunk.kind.letter(),
                chunk.number,
                PyBytes::new(py, &chunk.bytes),
            )
        })
        .collect();
    Ok((rows, report(chunked.report)))
}

/// `Volume.publish`: place the volume in a GR2Analyst polling directory.
/// Returns `{"site", "path", "dir_list", "removed", "left_out", "notes"}`.
#[pyfunction]
#[pyo3(signature = (
    volume, root, *, site=None, keep=30, compression="bzip2", update_site_config=true,
    quantization="standard", nyquist_velocity=None, unambiguous_range=None,
    drop_negative_range_gates=false, sweeps=None, sweeps_in_time_order=false, sweeps_by_elevation=false,
    fields=None, field_map=None, position=None,
    strict=false
))]
#[allow(clippy::too_many_arguments)]
fn _publish<'py>(
    py: Python<'py>,
    volume: &PyVolume,
    root: PathBuf,
    site: Option<String>,
    keep: usize,
    compression: &str,
    update_site_config: bool,
    quantization: &str,
    nyquist_velocity: Option<f32>,
    unambiguous_range: Option<f32>,
    drop_negative_range_gates: bool,
    sweeps: Option<Vec<usize>>,
    sweeps_in_time_order: bool,
    sweeps_by_elevation: bool,
    fields: Option<Vec<String>>,
    field_map: Option<Vec<(String, String)>>,
    position: Option<(f64, f64, f64)>,
    strict: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let backends = Backends::builtin();
    let publisher = backends.publisher().map_err(backend_error)?;
    let args = WriteArgs::new(
        compression,
        site,
        quantization,
        nyquist_velocity,
        unambiguous_range,
        drop_negative_range_gates,
        sweeps,
        sweeps_in_time_order,
        sweeps_by_elevation,
        fields,
        field_map,
        position,
        strict,
    )?;
    let mut request = PublishRequest::new(root);
    request.site = args.options.site_id.clone();
    request.keep = keep;
    request.options = args.options.clone();
    request.update_site_config = update_site_config;
    let loaded = volume.loaded();
    let source_name = loaded.volume.provenance.source_path.clone();
    let published = py.detach(|| {
        let edited = args
            .edits
            .apply(&loaded.volume)
            .map_err(PyValueError::new_err)?;
        let input = WriteInput {
            volume: &edited,
            metadata: &loaded.metadata,
            source_name: source_name.as_deref(),
        };
        publisher.publish(&input, &request).map_err(backend_error)
    })?;
    let dict = PyDict::new(py);
    dict.set_item("site", &published.site)?;
    dict.set_item("path", &published.path)?;
    dict.set_item("dir_list", &published.dir_list)?;
    dict.set_item("removed", &published.removed)?;
    dict.set_item("left_out", &published.report.left_out)?;
    dict.set_item("notes", &published.report.notes)?;
    Ok(dict)
}

/// Every output format and whether this build can write it
/// (`{"level2": True, "cfradial1": True, "odim": True, "fm301": True}`).
#[pyfunction]
fn writers() -> Vec<(&'static str, bool)> {
    let backends = Backends::builtin();
    OutputFormat::ALL
        .into_iter()
        .map(|format| (format.cli_name(), backends.writer(format).is_ok()))
        .collect()
}

/// Raise `UnavailableError` unless this build can write `format` (and
/// `ValueError` for an unknown format name), before any input is decoded.
#[pyfunction]
fn _require_writer(format: &str) -> PyResult<()> {
    let format = output_format(format)?;
    Backends::builtin()
        .writer(format)
        .map(|_| ())
        .map_err(backend_error)
}

/// Whether this build can publish to a GR2Analyst polling directory.
#[pyfunction]
fn publisher_available() -> bool {
    Backends::builtin().publisher().is_ok()
}

/// Add this module's functions.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_write, module)?)?;
    module.add_function(wrap_pyfunction!(_to_bytes, module)?)?;
    module.add_function(wrap_pyfunction!(_publish, module)?)?;
    module.add_function(wrap_pyfunction!(_write_chunks, module)?)?;
    module.add_function(wrap_pyfunction!(writers, module)?)?;
    module.add_function(wrap_pyfunction!(_require_writer, module)?)?;
    module.add_function(wrap_pyfunction!(publisher_available, module)?)?;
    Ok(())
}
