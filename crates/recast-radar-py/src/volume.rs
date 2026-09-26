//! `recast_radar.Volume`: a decoded volume that stays in Rust, and the
//! functions that make one.
//!
//! `recast_radar.open` moves a volume's buffers into NumPy and keeps
//! nothing. A `Volume` keeps the Rust model instead, for the writers and for
//! repeated conversions; each `to_datatree`/`to_pyart` works on a copy.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use recast_radar_cli::open::{Loaded, source_format_name};
use recast_radar_core::model::{RangeCoord, Volume, merge_volumes};
use recast_radar_io::{FormatMetadata, SupportedVolumeFormat};

use crate::errors::decode_error;
use crate::source::{self, LoadOptions, Source};
use crate::tree::{self, source_format_id};

/// A decoded radar volume held in Rust.
///
/// Made by `recast_radar.read`, `read_all` and `merge`. Converting it
/// (`to_datatree`, `to_pyart`) copies the field buffers, so the volume stays
/// usable; `recast_radar.open` is the zero-copy path.
#[pyclass(module = "recast_radar", name = "Volume", frozen)]
pub struct PyVolume {
    loaded: Loaded,
}

impl PyVolume {
    /// Wrap a decoded volume.
    pub(crate) fn new(loaded: Loaded) -> Self {
        Self { loaded }
    }

    /// The decoded volume and its metadata.
    pub(crate) fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    fn volume(&self) -> &Volume {
        &self.loaded.volume
    }
}

fn datetime<'py>(py: Python<'py>, iso: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import("datetime")?
        .getattr("datetime")?
        .call_method1("fromisoformat", (iso,))
}

fn iso(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
        .replace('Z', "+00:00")
}

#[pymethods]
impl PyVolume {
    /// Source format: `"nexrad_level2"`, `"nexrad_level3"`, `"odim_h5"`,
    /// `"cfradial1"`, `"cfradial2"`, `"dorade"`, `"jma_grib2"` or
    /// `"unknown"`.
    #[getter]
    fn source_format(&self) -> &'static str {
        source_format_id(self.volume().provenance.source_format)
    }

    /// Display name of the source format, for example `"NEXRAD Level II"`.
    #[getter]
    fn format_name(&self) -> &'static str {
        source_format_name(self.volume())
    }

    /// Name of the part of the input this volume came from (a mobile-archive
    /// member, a JMA station), when the input holds several.
    #[getter]
    fn label(&self) -> Option<&str> {
        self.loaded.label.as_deref()
    }

    /// `instrument_name`: the NEXRAD ICAO, the ODIM source node, ...
    #[getter]
    fn instrument_name(&self) -> &str {
        &self.volume().attrs.instrument_name
    }

    /// Epoch of the ray times (UTC, whole seconds), as a `datetime`.
    #[getter]
    fn time_reference<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        datetime(py, &iso(self.volume().time_reference))
    }

    /// First and last ray time as `(start, end)` datetimes, when known.
    #[getter]
    fn time_coverage<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.volume().time_coverage {
            Some(coverage) => Ok(Some(
                (
                    datetime(py, &iso(coverage.start))?,
                    datetime(py, &iso(coverage.end))?,
                )
                    .into_pyobject(py)?
                    .into_any(),
            )),
            None => Ok(None),
        }
    }

    /// Latitude in degrees north, or `None` when the source has no location.
    #[getter]
    fn latitude(&self) -> Option<f64> {
        self.volume().location.latitude_deg
    }

    /// Longitude in degrees east, or `None`.
    #[getter]
    fn longitude(&self) -> Option<f64> {
        self.volume().location.longitude_deg
    }

    /// Antenna altitude in metres above mean sea level, or `None`.
    #[getter]
    fn altitude(&self) -> Option<f64> {
        self.volume().location.altitude_m
    }

    /// `scan_name` (NEXRAD: `"VCP-212"`), when known.
    #[getter]
    fn scan_name(&self) -> Option<&str> {
        self.volume().scan.name.as_deref()
    }

    /// The NEXRAD volume coverage pattern number, when known.
    #[getter]
    fn vcp(&self) -> Option<u16> {
        self.volume().scan.vcp_pattern
    }

    /// Number of sweeps.
    #[getter]
    fn nsweeps(&self) -> usize {
        self.volume().sweeps.len()
    }

    /// One dictionary per sweep: `index`, `fixed_angle` (degrees),
    /// `sweep_mode`, `nrays`, `ngates`, `range_start` and `gate_spacing`
    /// (metres; `None` for explicit ranges) and `fields` (names).
    #[getter]
    fn sweeps<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (index, sweep) in self.volume().sweeps.iter().enumerate() {
            let dict = PyDict::new(py);
            dict.set_item("index", index)?;
            dict.set_item("fixed_angle", sweep.fixed_angle_deg)?;
            dict.set_item("sweep_mode", sweep.sweep_mode.as_str())?;
            dict.set_item("nrays", sweep.nrays())?;
            dict.set_item("ngates", sweep.range.ngates())?;
            match &sweep.range {
                RangeCoord::Uniform {
                    first_center_m,
                    spacing_m,
                    ..
                } => {
                    dict.set_item("range_start", *first_center_m)?;
                    dict.set_item("gate_spacing", *spacing_m)?;
                }
                RangeCoord::Explicit { centers_m } => {
                    dict.set_item("range_start", centers_m.first().copied())?;
                    dict.set_item("gate_spacing", py.None())?;
                }
            }
            let fields: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
            dict.set_item("fields", fields)?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// Every field name, in order of first appearance.
    #[getter]
    fn field_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for sweep in &self.volume().sweeps {
            for field in &sweep.fields {
                if !names.iter().any(|name| name == field.name.as_str()) {
                    names.push(field.name.as_str().to_owned());
                }
            }
        }
        names
    }

    /// Whether NEXRAD Level II metadata messages (VCP, RDA status,
    /// adaptation data, ...) were decoded beside the volume.
    #[getter]
    fn has_level2_metadata(&self) -> bool {
        matches!(self.loaded.metadata, FormatMetadata::Nexrad(_))
    }

    /// Metadata the model has no slot for, as `recast-radar dump` gives it:
    /// `{"nexrad": {...}}` with the NEXRAD Level II metadata messages (2, 3,
    /// 5, 13, 15, 18, 32), each sweep's Message 31 constant blocks and the
    /// decode problems, or `None` for other formats.
    #[getter]
    fn format_metadata<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match recast_radar_cli::dump::format_metadata_json(&self.loaded.metadata) {
            Some(value) => {
                let json = py.import("json")?;
                Ok(Some(json.call_method1("loads", (value.to_string(),))?))
            }
            None => Ok(None),
        }
    }

    /// The FM301 tree dictionary of a copy of this volume (internal; use
    /// `to_datatree`).
    #[pyo3(signature = (*, flavor="xradar", first_dim="auto", passthrough="flavor"))]
    fn _tree<'py>(
        &self,
        py: Python<'py>,
        flavor: &str,
        first_dim: &str,
        passthrough: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let options = tree::view_options(flavor, first_dim, passthrough)?;
        let copy = py.detach(|| self.loaded.clone());
        tree::native_tree(py, copy, source_format_name(self.volume()), options)
    }

    /// `recast_radar.to_datatree(self, **options)`: the FM301 DataTree of a
    /// copy of this volume.
    #[pyo3(signature = (**options))]
    fn to_datatree<'py>(
        slf: &Bound<'py, Self>,
        options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call_package(slf, "to_datatree", (slf,), options)
    }

    /// `recast_radar.to_pyart(self, **options)`: a `pyart.core.Radar`.
    #[pyo3(signature = (**options))]
    fn to_pyart<'py>(
        slf: &Bound<'py, Self>,
        options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call_package(slf, "to_pyart", (slf,), options)
    }

    /// `recast_radar.write(self, path, format, **options)`.
    #[pyo3(signature = (path, format, **options))]
    fn write<'py>(
        slf: &Bound<'py, Self>,
        path: &Bound<'py, PyAny>,
        format: &Bound<'py, PyAny>,
        options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call_package(slf, "write", (slf, path, format), options)
    }

    /// `recast_radar.to_bytes(self, format, **options)`.
    #[pyo3(signature = (format, **options))]
    fn to_bytes<'py>(
        slf: &Bound<'py, Self>,
        format: &Bound<'py, PyAny>,
        options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call_package(slf, "to_bytes", (slf, format), options)
    }

    /// `recast_radar.publish(self, root, **options)`.
    #[pyo3(signature = (root, **options))]
    fn publish<'py>(
        slf: &Bound<'py, Self>,
        root: &Bound<'py, PyAny>,
        options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call_package(slf, "publish", (slf, root), options)
    }

    fn __len__(&self) -> usize {
        self.volume().sweeps.len()
    }

    fn __repr__(&self) -> String {
        let volume = self.volume();
        let fields = self.field_names();
        format!(
            "<recast_radar.Volume {} {} {}: {} sweep(s), fields {}>",
            source_format_name(volume),
            if volume.attrs.instrument_name.is_empty() {
                "?"
            } else {
                volume.attrs.instrument_name.as_str()
            },
            volume.time_reference.format("%Y-%m-%dT%H:%M:%SZ"),
            volume.sweeps.len(),
            fields.join(" ")
        )
    }
}

/// Call `recast_radar.<name>(*args, **options)`: the conversions and
/// writers are written in Python and take the volume as their first
/// argument.
fn call_package<'py>(
    slf: &Bound<'py, PyVolume>,
    name: &str,
    args: impl pyo3::call::PyCallArgs<'py>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    slf.py()
        .import("recast_radar")?
        .getattr(name)?
        .call(args, options)
}

/// Decode one volume of a radar file (a path or the file's bytes).
///
/// The format is detected from the contents. `station` picks a JMA station
/// (JMA id or station number); `volume` picks one volume of an input that
/// holds several (a mobile-radar ZIP archive).
#[pyfunction]
#[pyo3(signature = (source, *, station=None, volume=0))]
fn read(
    py: Python<'_>,
    source: &Bound<'_, PyAny>,
    station: Option<String>,
    volume: usize,
) -> PyResult<PyVolume> {
    let source = Source::from_py(source)?;
    let what = source.name().unwrap_or_else(|| "input".to_owned());
    let options = LoadOptions {
        station,
        all_stations: false,
    };
    let loaded = source::select(source::load(py, &source, &options)?, volume, &what)?;
    Ok(PyVolume::new(loaded))
}

/// Decode every volume of a radar file: each member of a mobile-radar ZIP
/// archive, each station of a JMA tar with `all_stations=True`, otherwise
/// the one volume.
#[pyfunction]
#[pyo3(signature = (source, *, station=None, all_stations=false))]
fn read_all(
    py: Python<'_>,
    source: &Bound<'_, PyAny>,
    station: Option<String>,
    all_stations: bool,
) -> PyResult<Vec<PyVolume>> {
    let source = Source::from_py(source)?;
    let options = LoadOptions {
        station,
        all_stations,
    };
    Ok(source::load(py, &source, &options)?
        .into_iter()
        .map(PyVolume::new)
        .collect())
}

/// Merge the parts of one scan (split ODIM products, DWD sweep files, ...)
/// into one volume. The first part is the base; later parts add fields to
/// sweeps at the same angle and add the other sweeps. The parts are copied.
#[pyfunction]
fn merge(py: Python<'_>, volumes: Vec<Py<PyVolume>>) -> PyResult<PyVolume> {
    if volumes.is_empty() {
        return Err(PyValueError::new_err("merge needs at least one volume"));
    }
    let parts: Vec<Volume> = volumes
        .iter()
        .map(|volume| volume.get().volume().clone())
        .collect();
    let (volume, _report) = py
        .detach(|| merge_volumes(parts))
        .map_err(|err| decode_error(err.to_string()))?;
    Ok(PyVolume::new(Loaded {
        label: None,
        volume,
        metadata: FormatMetadata::None,
    }))
}

/// The format of a radar file's bytes, from its first bytes: `"dorade"`,
/// `"odim_h5"`, `"cfradial"` (classic netCDF), `"cfradial_netcdf4"`
/// (netCDF-4 CfRadial 1), `"cfradial2"` (netCDF-4 CfRadial 2 / FM301),
/// `"jma_grib2_tar"`, `"nexrad_level3"` or `"nexrad_level2"` (the router's
/// fallback, also for bytes it does not recognise).
#[pyfunction]
fn sniff(data: &[u8]) -> &'static str {
    match recast_radar_io::sniff_supported_volume_format(data) {
        SupportedVolumeFormat::Dorade => "dorade",
        SupportedVolumeFormat::OdimH5 => "odim_h5",
        SupportedVolumeFormat::CfRadial => "cfradial",
        SupportedVolumeFormat::CfRadialNetcdf4 => "cfradial_netcdf4",
        SupportedVolumeFormat::CfRadial2 => "cfradial2",
        SupportedVolumeFormat::JmaGrib2Tar => "jma_grib2_tar",
        SupportedVolumeFormat::NexradLevel3 => "nexrad_level3",
        SupportedVolumeFormat::NexradLevel2 => "nexrad_level2",
        _ => "other",
    }
}

/// Add the class and functions.
/// Every decoded value of `source` as JSON text: what
/// `recast-radar dump --json` prints (internal; use `recast_radar.dump`).
#[pyfunction]
#[pyo3(signature = (source, *, data=false, rays=false, station=None, all_stations=false))]
fn _dump(
    py: Python<'_>,
    source: &Bound<'_, PyAny>,
    data: bool,
    rays: bool,
    station: Option<String>,
    all_stations: bool,
) -> PyResult<String> {
    let source = Source::from_py(source)?;
    let options = LoadOptions {
        station,
        all_stations,
    };
    source::dump_json(py, &source, &options, data, rays)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyVolume>()?;
    module.add_function(wrap_pyfunction!(_dump, module)?)?;
    module.add_function(wrap_pyfunction!(read, module)?)?;
    module.add_function(wrap_pyfunction!(read_all, module)?)?;
    module.add_function(wrap_pyfunction!(merge, module)?)?;
    module.add_function(wrap_pyfunction!(sniff, module)?)?;
    Ok(())
}
