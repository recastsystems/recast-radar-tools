//! NumPy buffers for the shared section and grid frontends.
use numpy::PyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use recast_radar_cli::mapping;

use crate::volume::PyVolume;

#[pyfunction(name = "_section")]
fn section<'py>(py: Python<'py>, volume: &PyVolume, options: &str) -> PyResult<Bound<'py, PyDict>> {
    let options: mapping::SectionOptions =
        serde_json::from_str(options).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let section = py
        .detach(|| mapping::section(&volume.loaded().volume, &options))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let dict = PyDict::new(py);
    dict.set_item("field", section.field)?;
    dict.set_item("units", section.units)?;
    dict.set_item("height_m", PyArray1::from_vec(py, section.height_m))?;
    dict.set_item("distance_m", PyArray1::from_vec(py, section.distance_m))?;
    dict.set_item(
        "values",
        PyArray1::from_vec(py, section.values).call_method1("reshape", (section.shape,))?,
    )?;
    Ok(dict)
}

#[pyfunction(name = "_grid")]
fn grid<'py>(
    py: Python<'py>,
    volumes: Vec<PyRef<'py, PyVolume>>,
    options: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let options: mapping::GridOptions =
        serde_json::from_str(options).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let refs: Vec<_> = volumes.iter().map(|v| &v.loaded().volume).collect();
    let grid = py
        .detach(|| mapping::grid(&refs, &options))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let dict = PyDict::new(py);
    dict.set_item("x_m", PyArray1::from_vec(py, grid.x_m))?;
    dict.set_item("y_m", PyArray1::from_vec(py, grid.y_m))?;
    dict.set_item("z_m", PyArray1::from_vec(py, grid.z_m))?;
    dict.set_item(
        "origin",
        (
            grid.origin.latitude_deg,
            grid.origin.longitude_deg,
            grid.origin.altitude_m,
        ),
    )?;
    dict.set_item(
        "roi_m",
        PyArray1::from_vec(py, grid.roi_m).call_method1("reshape", (grid.shape,))?,
    )?;
    let fields = PyDict::new(py);
    let units = PyDict::new(py);
    for field in grid.fields {
        let unit = refs
            .iter()
            .flat_map(|v| &v.sweeps)
            .find_map(|s| s.field(&field.name).and_then(mapping::field_units));
        units.set_item(field.name.as_str(), unit)?;
        fields.set_item(
            field.name.as_str(),
            PyArray1::from_vec(py, field.values).call_method1("reshape", (grid.shape,))?,
        )?;
    }
    dict.set_item("fields", fields)?;
    dict.set_item("units", units)?;
    Ok(dict)
}

/// Register native section and grid adapters.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(section, module)?)?;
    module.add_function(wrap_pyfunction!(grid, module)?)?;
    Ok(())
}
