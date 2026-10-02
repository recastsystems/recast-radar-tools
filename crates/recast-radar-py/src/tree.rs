//! The FM301 group tree of a volume as Python dictionaries, with every field
//! buffer moved into NumPy (design note `docs/design/fm301-model.md` 12.2).
//!
//! 1. Build the FM301 view (with the NEXRAD xradar attributes when the
//!    input is Level II) and detach it with `VolumeView::layout`.
//! 2. Consume the volume: each field's `Vec` goes to `PyArray::from_vec`
//!    and is reshaped to `[nrays, native_gates]`. NumPy owns the memory from
//!    then on; nothing is copied.
//! 3. Walk the layout. A field variable carries its moved buffer plus the
//!    mapping the Python side applies lazily (ray permutation, gate padding
//!    and repetition). When `zero_copy` is true the buffer already is the
//!    variable.
//!
//! The dictionaries (`_open_tree`, `Volume._tree`) are an internal format
//! between this module and `recast_radar/_tree.py`:
//!
//! ```text
//! {"tree": group, "source_format": str, "format_name": str, "label": str | None,
//!  "pyart_names": {field: {"config": str, "reader": str}}, "warnings": [str],
//!  "flavor": str, "first_dim": str}
//! group = {"name": str, "dims": [(str, int)], "attrs": [(str, value)],
//!          "variables": [variable], "children": [group]}
//! variable = {"name": str, "dims": [str], "attrs": [(str, value)], "kind": kind, "data": ...}
//! kind "array": data is a NumPy array (text: a list of str)
//! kind "scalar": data is a NumPy scalar;  kind "text": data is a str
//! kind "field": data is the moved [nrays, native_gates] buffer, plus
//!   "rows" (uint32 array or None), "start", "stride", "native_gates",
//!   "out_gates", "nrays", "fill", "zero_copy", "range_folded", "undetect"
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use numpy::PyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use recast_radar_cli::open::Loaded;
use recast_radar_core::fm301::{
    self, DataRef, FieldSource, FirstDim, Flavor, GroupLayout, Passthrough, RowOrder, ViewOptions,
    ViewWarning, VolumeLayout,
};
use recast_radar_core::model::{
    Coding, FieldName, IntCoding, PackedInt, PyartNames, Scalar, SourceFormat, Volume,
};
use recast_radar_io::FormatMetadata;
use recast_radar_io_nexrad::NexradVolume;

use crate::source::{self, LoadOptions, Source};
use crate::values;

/// Parse the view options from their Python spellings.
pub(crate) fn view_options(
    flavor: &str,
    first_dim: &str,
    passthrough: &str,
) -> PyResult<ViewOptions> {
    let flavor = match flavor {
        "xradar" => Flavor::Xradar012,
        "wmo" | "fm301" => Flavor::Wmo2022,
        other => {
            return Err(PyValueError::new_err(format!(
                "flavor must be \"xradar\" or \"wmo\", not {other:?}"
            )));
        }
    };
    let first_dim = match first_dim {
        "auto" => FirstDim::Auto,
        "time" => FirstDim::Time,
        other => {
            return Err(PyValueError::new_err(format!(
                "first_dim must be \"auto\" or \"time\", not {other:?}"
            )));
        }
    };
    if flavor == Flavor::Wmo2022 && first_dim != FirstDim::Time {
        return Err(PyValueError::new_err(
            "flavor=\"wmo\" needs first_dim=\"time\": an FM301 time coordinate must be monotonic",
        ));
    }
    let passthrough = match passthrough {
        "flavor" => Passthrough::Flavor,
        "all" => Passthrough::All,
        other => {
            return Err(PyValueError::new_err(format!(
                "passthrough must be \"flavor\" or \"all\", not {other:?}"
            )));
        }
    };
    Ok(ViewOptions {
        flavor,
        first_dim,
        passthrough,
    })
}

/// The snake-case name of a source format, as Python sees it.
pub(crate) fn source_format_id(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::NexradLevel2 => "nexrad_level2",
        SourceFormat::NexradLevel3 => "nexrad_level3",
        SourceFormat::OdimH5 => "odim_h5",
        SourceFormat::CfRadial1 => "cfradial1",
        SourceFormat::CfRadial2 => "cfradial2",
        SourceFormat::Dorade => "dorade",
        SourceFormat::JmaGrib2 => "jma_grib2",
        SourceFormat::MeteoFranceBufr => "meteofrance_bufr",
        SourceFormat::Simulated => "simulated",
        _ => "unknown",
    }
}

fn parse_source_format(id: &str) -> SourceFormat {
    match id {
        "nexrad_level2" => SourceFormat::NexradLevel2,
        "nexrad_level3" => SourceFormat::NexradLevel3,
        "odim_h5" => SourceFormat::OdimH5,
        "cfradial1" => SourceFormat::CfRadial1,
        "cfradial2" => SourceFormat::CfRadial2,
        "dorade" => SourceFormat::Dorade,
        "jma_grib2" => SourceFormat::JmaGrib2,
        "meteofrance_bufr" => SourceFormat::MeteoFranceBufr,
        "simulated" => SourceFormat::Simulated,
        _ => SourceFormat::Unknown,
    }
}

/// A moved field buffer and the coding values the encoded attributes do
/// not single out.
struct MovedField<'py> {
    array: Bound<'py, PyAny>,
    range_folded: Option<Scalar>,
    undetect: Option<Scalar>,
}

fn int_sentinels<T: PackedInt>(
    coding: &IntCoding<T>,
    wrap: fn(T) -> Scalar,
) -> (Option<Scalar>, Option<Scalar>) {
    (coding.range_folded.map(wrap), coding.undetect.map(wrap))
}

fn sentinels(coding: &Coding) -> (Option<Scalar>, Option<Scalar>) {
    match coding {
        Coding::U8(coding) => int_sentinels(coding, Scalar::U8),
        Coding::U16(coding) => int_sentinels(coding, Scalar::U16),
        Coding::I8(coding) => int_sentinels(coding, Scalar::I8),
        Coding::I16(coding) => int_sentinels(coding, Scalar::I16),
        Coding::I32(coding) => int_sentinels(coding, Scalar::I32),
        Coding::F32(coding) => (None, coding.undetect.map(Scalar::F32)),
        Coding::F64(coding) => (None, coding.undetect.map(Scalar::F64)),
    }
}

/// Build the layout, then move every field buffer into NumPy.
fn layout_and_move<'py>(
    py: Python<'py>,
    loaded: Loaded,
    options: ViewOptions,
) -> PyResult<(
    VolumeLayout,
    HashMap<FieldSource, MovedField<'py>>,
    VolumeInfo,
)> {
    let Loaded {
        label,
        volume,
        metadata,
    } = loaded;
    let view_error = |err: fm301::ViewError| PyValueError::new_err(err.to_string());
    let (layout, volume) = match metadata {
        FormatMetadata::Nexrad(metadata) => {
            let nexrad = NexradVolume {
                volume,
                metadata: *metadata,
            };
            let layout = fm301::volume_view(&nexrad.volume, options, Some(&nexrad))
                .map_err(view_error)?
                .layout();
            (layout, nexrad.volume)
        }
        _ => {
            let layout = fm301::volume_view(&volume, options, None)
                .map_err(view_error)?
                .layout();
            (layout, volume)
        }
    };
    let info = VolumeInfo::of(&volume, label);

    let mut moved = HashMap::new();
    for (sweep_index, sweep) in volume.sweeps.into_iter().enumerate() {
        for (field_index, field) in sweep.fields.into_iter().enumerate() {
            let parts = field.into_parts();
            let (buffer, coding) = parts.data.into_array();
            let (range_folded, undetect) = sentinels(&coding);
            let array =
                values::field_array(py, buffer, parts.nrays as usize, parts.ngates as usize)?;
            let source = FieldSource {
                sweep: u32::try_from(sweep_index).unwrap_or(u32::MAX),
                field: u32::try_from(field_index).unwrap_or(u32::MAX),
            };
            moved.insert(
                source,
                MovedField {
                    array,
                    range_folded,
                    undetect,
                },
            );
        }
    }
    Ok((layout, moved, info))
}

/// Volume facts the Python side needs after the fields have moved.
struct VolumeInfo {
    source_format: SourceFormat,
    label: Option<String>,
    /// Field name → (config name, reader name).
    pyart_names: Vec<(String, String, String)>,
}

impl VolumeInfo {
    fn of(volume: &Volume, label: Option<String>) -> Self {
        let source_format = volume.provenance.source_format;
        let mut pyart_names: Vec<(String, String, String)> = Vec::new();
        for sweep in &volume.sweeps {
            for field in &sweep.fields {
                let name = field.name.as_str();
                if pyart_names.iter().any(|(known, _, _)| known == name) {
                    continue;
                }
                pyart_names.push((
                    name.to_owned(),
                    field
                        .name
                        .pyart_name(PyartNames::Config, source_format)
                        .into_owned(),
                    field
                        .name
                        .pyart_name(PyartNames::Reader, source_format)
                        .into_owned(),
                ));
            }
        }
        Self {
            source_format,
            label,
            pyart_names,
        }
    }
}

/// Converts layout groups, sharing one NumPy permutation per sweep.
struct Converter<'py> {
    py: Python<'py>,
    moved: HashMap<FieldSource, MovedField<'py>>,
    permutations: HashMap<usize, Bound<'py, PyAny>>,
}

impl<'py> Converter<'py> {
    fn permutation(&mut self, order: &Arc<[u32]>) -> Bound<'py, PyAny> {
        let key = order.as_ptr() as usize;
        self.permutations
            .entry(key)
            .or_insert_with(|| PyArray1::from_slice(self.py, order).into_any())
            .clone()
    }

    fn group(
        &mut self,
        group: GroupLayout,
        inherited_dims: &[(String, usize)],
    ) -> PyResult<Bound<'py, PyDict>> {
        let py = self.py;
        let dict = PyDict::new(py);
        let mut dims: Vec<(String, usize)> = inherited_dims.to_vec();
        for (name, len) in &group.dims {
            dims.retain(|(known, _)| known != name);
            dims.push((name.clone(), *len));
        }
        dict.set_item("name", &group.name)?;
        let own_dims = PyList::empty(py);
        for (name, len) in &group.dims {
            own_dims.append((name.as_str(), *len))?;
        }
        dict.set_item("dims", own_dims)?;
        dict.set_item("attrs", values::attrs(py, &group.attrs)?)?;
        let variables = PyList::empty(py);
        for variable in group.variables {
            let var = PyDict::new(py);
            var.set_item("name", &variable.name)?;
            var.set_item("dims", PyList::new(py, &variable.dims)?)?;
            var.set_item("attrs", values::attrs(py, &variable.attrs)?)?;
            match variable.data {
                DataRef::Field {
                    source,
                    nrays,
                    native_gates,
                    mapping,
                    out_gates,
                    fill,
                    rows,
                } => {
                    let zero_copy = rows == RowOrder::Identity
                        && mapping.start == 0
                        && mapping.stride <= 1
                        && native_gates == out_gates;
                    let Some(field) = self.moved.get(&source) else {
                        return Err(PyValueError::new_err(format!(
                            "{}: the view names field {}/{} that the volume does not have",
                            variable.name, source.sweep, source.field
                        )));
                    };
                    var.set_item("kind", "field")?;
                    var.set_item("data", field.array.clone())?;
                    let range_folded = field.range_folded.map(|v| values::scalar(py, v));
                    let undetect = field.undetect.map(|v| values::scalar(py, v));
                    match rows {
                        RowOrder::Identity => var.set_item("rows", py.None())?,
                        RowOrder::Permutation(order) => {
                            var.set_item("rows", self.permutation(&order))?
                        }
                        other => {
                            return Err(PyValueError::new_err(format!(
                                "{}: ray order {other:?} is not supported by this binding",
                                variable.name
                            )));
                        }
                    }
                    var.set_item("start", mapping.start)?;
                    var.set_item("stride", mapping.stride.max(1))?;
                    var.set_item("native_gates", native_gates)?;
                    var.set_item("out_gates", out_gates)?;
                    var.set_item("nrays", nrays)?;
                    var.set_item("fill", values::typed_scalar(py, fill)?)?;
                    var.set_item("zero_copy", zero_copy)?;
                    var.set_item("range_folded", range_folded.transpose()?)?;
                    var.set_item("undetect", undetect.transpose()?)?;
                }
                DataRef::Array(array) => {
                    let shape: Vec<usize> = variable
                        .dims
                        .iter()
                        .map(|dim| {
                            dims.iter()
                                .rev()
                                .find(|(name, _)| name == dim)
                                .map_or(0, |(_, len)| *len)
                        })
                        .collect();
                    var.set_item("kind", "array")?;
                    var.set_item("data", values::shaped_array(py, array, &shape)?)?;
                }
                DataRef::Scalar(scalar) => {
                    var.set_item("kind", "scalar")?;
                    var.set_item("data", values::typed_scalar(py, scalar)?)?;
                }
                DataRef::Text(text) => {
                    var.set_item("kind", "text")?;
                    var.set_item("data", text)?;
                }
                other => {
                    return Err(PyValueError::new_err(format!(
                        "{}: view data {other:?} is not supported by this binding",
                        variable.name
                    )));
                }
            }
            variables.append(var)?;
        }
        dict.set_item("variables", variables)?;
        let children = PyList::empty(py);
        for child in group.children {
            children.append(self.group(child, &dims)?)?;
        }
        dict.set_item("children", children)?;
        Ok(dict)
    }
}

fn flavor_name(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Xradar012 => "xradar",
        Flavor::Wmo2022 => "wmo",
        _ => "other",
    }
}

fn first_dim_name(first_dim: FirstDim) -> &'static str {
    match first_dim {
        FirstDim::Auto => "auto",
        FirstDim::Time => "time",
        _ => "other",
    }
}

/// The tree dictionary of `loaded` (see the module documentation). The
/// volume is consumed: its field buffers now belong to NumPy.
pub(crate) fn native_tree<'py>(
    py: Python<'py>,
    loaded: Loaded,
    format_name: &str,
    options: ViewOptions,
) -> PyResult<Bound<'py, PyDict>> {
    let (layout, moved, info) = layout_and_move(py, loaded, options)?;
    let VolumeLayout { root, warnings } = layout;
    let mut converter = Converter {
        py,
        moved,
        permutations: HashMap::new(),
    };
    let tree = converter.group(root, &[])?;
    let dict = PyDict::new(py);
    dict.set_item("tree", tree)?;
    dict.set_item("source_format", source_format_id(info.source_format))?;
    dict.set_item("format_name", format_name)?;
    dict.set_item("label", info.label)?;
    let names = PyDict::new(py);
    for (name, config, reader) in &info.pyart_names {
        let entry = PyDict::new(py);
        entry.set_item("config", config)?;
        entry.set_item("reader", reader)?;
        names.set_item(name, entry)?;
    }
    dict.set_item("pyart_names", names)?;
    let warnings: Vec<String> = warnings
        .iter()
        .map(|warning| match warning {
            ViewWarning::NonMonotonicTime { sweep } => format!(
                "sweep_{sweep}: ray times are not increasing (the source has no per-ray times)"
            ),
            other => format!("{other:?}"),
        })
        .collect();
    dict.set_item("warnings", warnings)?;
    dict.set_item("flavor", flavor_name(options.flavor))?;
    dict.set_item("first_dim", first_dim_name(options.first_dim))?;
    Ok(dict)
}

/// Decode `source` and return volume `volume` as a tree dictionary, moving
/// its buffers into NumPy (the zero-copy path behind `recast_radar.open`).
#[pyfunction]
#[pyo3(signature = (source, *, flavor="xradar", first_dim="auto", passthrough="flavor", station=None, volume=0))]
fn _open_tree<'py>(
    py: Python<'py>,
    source: &Bound<'py, PyAny>,
    flavor: &str,
    first_dim: &str,
    passthrough: &str,
    station: Option<String>,
    volume: usize,
) -> PyResult<Bound<'py, PyDict>> {
    let options = view_options(flavor, first_dim, passthrough)?;
    let source = Source::from_py(source)?;
    let what = source.name().unwrap_or_else(|| "input".to_owned());
    let load = LoadOptions {
        station,
        all_stations: false,
    };
    let loaded = source::select(source::load(py, &source, &load)?, volume, &what)?;
    let format_name = recast_radar_cli::open::source_format_name(&loaded.volume);
    native_tree(py, loaded, format_name, options)
}

/// The Py-ART field name of an FM301 field name: `mode` "config" gives the
/// `pyart.config` default (what Py-ART's algorithms look for), "reader" the
/// name the Py-ART reader for `source_format` produces (see
/// `Volume.source_format`).
#[pyfunction]
#[pyo3(signature = (name, mode="config", source_format="unknown"))]
fn pyart_field_name(name: &str, mode: &str, source_format: &str) -> PyResult<String> {
    let mode = match mode {
        "config" => PyartNames::Config,
        "reader" => PyartNames::Reader,
        other => {
            return Err(PyValueError::new_err(format!(
                "mode must be \"config\" or \"reader\", not {other:?}"
            )));
        }
    };
    Ok(FieldName::parse(name)
        .pyart_name(mode, parse_source_format(source_format))
        .into_owned())
}

/// Add this module's functions.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_open_tree, module)?)?;
    module.add_function(wrap_pyfunction!(pyart_field_name, module)?)?;
    Ok(())
}
