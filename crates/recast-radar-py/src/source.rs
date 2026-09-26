//! Reading an input: a path or the bytes of one file, decoded with the
//! GIL released.
//!
//! Decoding goes through `recast_radar_cli::open`, the same code as the
//! `recast-radar` command, so both front ends read the same files the same
//! way: format detection by content, Level III products, mobile-radar ZIP
//! archives (paths only), JMA station selection.

use std::path::PathBuf;

use pyo3::exceptions::{PyFileNotFoundError, PyTypeError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::{PyByteArray, PyBytes, PyMemoryView, PyString};
use recast_radar_cli::CliError;
use recast_radar_cli::open::{self, Contents, Loaded, OpenOptions};

use crate::errors::decode_error;

/// What to read: a file or a buffer.
pub(crate) enum Source {
    /// A file on disk (`str` or `os.PathLike`).
    Path(PathBuf),
    /// The bytes of one file (`bytes`; `bytearray` and `memoryview` are
    /// copied into one).
    Bytes(PyBackedBytes),
}

impl Source {
    /// Interpret a Python argument.
    pub(crate) fn from_py(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        if value.is_instance_of::<PyBytes>() || value.is_instance_of::<PyByteArray>() {
            return Ok(Self::Bytes(value.extract::<PyBackedBytes>()?));
        }
        if value.is_instance_of::<PyMemoryView>() {
            let bytes = value.call_method0("tobytes")?;
            return Ok(Self::Bytes(bytes.extract::<PyBackedBytes>()?));
        }
        if value.is_instance_of::<PyString>() {
            return Ok(Self::Path(PathBuf::from(value.extract::<String>()?)));
        }
        if value.hasattr("__fspath__")? {
            let path = value.call_method0("__fspath__")?;
            if path.is_instance_of::<PyBytes>() {
                return Err(PyTypeError::new_err(
                    "byte-string paths are not supported; pass a str or pathlib.Path",
                ));
            }
            return Ok(Self::Path(PathBuf::from(path.extract::<String>()?)));
        }
        Err(PyTypeError::new_err(format!(
            "expected a path (str or os.PathLike) or the file's bytes, got {}",
            value.get_type().name()?
        )))
    }

    /// A short name for messages and provenance.
    pub(crate) fn name(&self) -> Option<String> {
        match self {
            Self::Path(path) => Some(path.display().to_string()),
            Self::Bytes(_) => None,
        }
    }
}

/// Options for [`load`].
#[derive(Clone, Debug, Default)]
pub(crate) struct LoadOptions {
    /// JMA tars: the station to decode.
    pub station: Option<String>,
    /// JMA tars: every station.
    pub all_stations: bool,
}

impl LoadOptions {
    pub(crate) fn open_options(&self) -> OpenOptions {
        let mut options = OpenOptions::default();
        options.station = self.station.clone();
        options.all_stations = self.all_stations;
        // Level II metadata messages: the FM301 view's xradar attributes and,
        // later, byte-identical Level II round trips need them.
        options.metadata = true;
        options
    }
}

pub(crate) fn cli_error(err: CliError) -> PyErr {
    match err {
        CliError::Io { path, source } if source.kind() == std::io::ErrorKind::NotFound => {
            PyFileNotFoundError::new_err(format!("{}: {source}", path.display()))
        }
        CliError::Io { path, source } => {
            pyo3::exceptions::PyOSError::new_err(format!("{}: {source}", path.display()))
        }
        other => decode_error(other.to_string()),
    }
}

/// Decode every volume of `source`, with the GIL released.
///
/// Level III products decode to their data array as a one-sweep volume. An
/// input that holds no volume (a Level III product without a data array, a
/// Level II real-time chunk) raises `DecodeError`.
pub(crate) fn load(
    py: Python<'_>,
    source: &Source,
    options: &LoadOptions,
) -> PyResult<Vec<Loaded>> {
    let open_options = options.open_options();
    let decoded = py.detach(|| match source {
        Source::Path(path) => open::open_path(path, &open_options)
            .map(|input| input.contents)
            .map_err(Failure::Cli),
        Source::Bytes(bytes) => open::open_bytes(bytes, &open_options).map_err(Failure::Decode),
    });
    let contents = decoded.map_err(|failure| match failure {
        Failure::Cli(err) => cli_error(err),
        Failure::Decode(message) => decode_error(message),
    })?;
    let what = source.name().unwrap_or_else(|| "input".to_owned());
    let volumes = match contents {
        Contents::Volumes(volumes) => volumes,
        Contents::Level3(level3) => match level3.volume {
            Ok(volume) => vec![Loaded {
                label: None,
                volume,
                metadata: recast_radar_io::FormatMetadata::None,
            }],
            Err(reason) => {
                return Err(decode_error(format!(
                    "{what}: a Level III product with no data array to read ({reason}); recast_radar.dump() gives its packets and tables"
                )));
            }
        },
        Contents::Level2Records(summary) => {
            return Err(decode_error(format!(
                "{what}: Level II records ({} message(s)) but no complete volume, \
                 for example a real-time chunk other than the first",
                summary.messages.len()
            )));
        }
        _ => return Err(decode_error(format!("{what}: holds no radar volume"))),
    };
    if volumes.is_empty() {
        return Err(decode_error(format!("{what}: holds no radar volume")));
    }
    let mut volumes = volumes;
    if let Source::Path(path) = source {
        for loaded in &mut volumes {
            if loaded.volume.provenance.source_path.is_none() {
                loaded.volume.provenance.source_path = Some(path.display().to_string());
            }
        }
    }
    Ok(volumes)
}

/// Decode `source` as `recast-radar dump --json` does and return that
/// document as JSON text, with the GIL released.
pub(crate) fn dump_json(
    py: Python<'_>,
    source: &Source,
    options: &LoadOptions,
    data: bool,
    rays: bool,
) -> PyResult<String> {
    let open_options = options.open_options();
    let result = py.detach(|| {
        let input = match source {
            Source::Path(path) => open::open_path(path, &open_options).map_err(Failure::Cli)?,
            Source::Bytes(bytes) => open::Input {
                path: PathBuf::from("<bytes>"),
                size: bytes.len() as u64,
                contents: open::open_bytes(bytes, &open_options).map_err(Failure::Decode)?,
            },
        };
        let document =
            recast_radar_cli::dump::document(&input, data, rays).map_err(Failure::Cli)?;
        serde_json::to_string(&document).map_err(|err| Failure::Decode(err.to_string()))
    });
    result.map_err(|failure| match failure {
        Failure::Cli(err) => cli_error(err),
        Failure::Decode(message) => decode_error(message),
    })
}

/// Why a decode failed, carried out of the GIL-free closure.
enum Failure {
    Cli(CliError),
    Decode(String),
}

/// Pick volume `index` of `volumes`.
pub(crate) fn select(mut volumes: Vec<Loaded>, index: usize, what: &str) -> PyResult<Loaded> {
    let count = volumes.len();
    if index >= count {
        return Err(pyo3::exceptions::PyIndexError::new_err(format!(
            "{what}: volume {index} requested, but the input holds {count} volume(s)"
        )));
    }
    Ok(volumes.swap_remove(index))
}
