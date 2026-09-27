//! Processing, rendering, and the bundled CLI, using shared Rust implementations.

use numpy::PyArray1;
use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use recast_radar_cli::{CliError, InputArgs, RenderArgs, process};
use std::path::PathBuf;

use crate::volume::PyVolume;

fn error(err: CliError) -> PyErr {
    match err {
        CliError::Usage(message) => PyValueError::new_err(message),
        CliError::Io { path, source } => {
            PyOSError::new_err(format!("{}: {source}", path.display()))
        }
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

#[pyfunction(name = "_products")]
fn products() -> PyResult<String> {
    serde_json::to_string(&process::products()).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction(name = "_process", signature = (volume, options, previous=None))]
fn process_volume(
    py: Python<'_>,
    volume: &PyVolume,
    options: &str,
    previous: Option<&PyVolume>,
) -> PyResult<(PyVolume, String)> {
    let options: process::ProcessOptions =
        serde_json::from_str(options).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let (loaded, report) = py
        .detach(|| process::process(volume.loaded(), &options, previous.map(PyVolume::loaded)))
        .map_err(error)?;
    let report =
        serde_json::to_string(&report).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok((PyVolume::new(loaded), report))
}

#[pyfunction(name = "_render", signature = (volume, *, output=None, sweep=None, field=None, size=1024, range_fraction=94, dealias=false, palette=None))]
#[allow(clippy::too_many_arguments)]
fn render<'py>(
    py: Python<'py>,
    volume: &PyVolume,
    output: Option<PathBuf>,
    sweep: Option<usize>,
    field: Option<String>,
    size: u32,
    range_fraction: u8,
    dealias: bool,
    palette: Option<PathBuf>,
) -> PyResult<Bound<'py, PyAny>> {
    let args = RenderArgs {
        file: PathBuf::new(),
        output: PathBuf::new(),
        sweep,
        field,
        size,
        range_fraction,
        dealias,
        palette,
        all_sweeps: false,
        input: InputArgs::default(),
    };
    let pixels = py
        .detach(|| {
            let image =
                recast_radar_cli::render::image_from_volume(&volume.loaded().volume, &args)?;
            if let Some(path) = output {
                recast_radar_cli::render::save_png(&image, &path)?;
            }
            Ok::<_, CliError>(image.into_raw())
        })
        .map_err(error)?;
    PyArray1::from_vec(py, pixels).call_method1("reshape", ((size, size, 4),))
}

#[pyfunction(name = "_cli")]
fn cli(py: Python<'_>, args: Vec<String>) -> u8 {
    py.detach(|| {
        recast_radar_cli::exit_status_with_args(
            std::iter::once("recast-radar".to_owned()).chain(args),
        )
    })
}

/// Register the private primitives used by `processing.py` and `_cli.py`.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(products, module)?)?;
    module.add_function(wrap_pyfunction!(process_volume, module)?)?;
    module.add_function(wrap_pyfunction!(render, module)?)?;
    module.add_function(wrap_pyfunction!(cli, module)?)?;
    Ok(())
}
