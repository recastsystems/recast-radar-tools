//! The netCDF-4 data model of real files against netCDF4-python (the
//! netCDF-C library).
//!
//! Goldens: `testdata/golden/netcdf4/<id>.json`, written by
//! `tools/netcdf4_golden.py`. Each test opens one corpus file with
//! [`NcFile`] and compares every group netCDF-C shows, in its order: the
//! dimensions defined there (name, length, unlimited), the child groups,
//! every attribute (text, or numeric type and values), and every variable
//! (name, netCDF type, dimension names, shape, attributes, and a SHA-256 of
//! its values in the script's canonical encoding). `_NCProperties` and the
//! data model are file-level checks.

use recast_radar_hdf5::Values;
use recast_radar_hdf5::netcdf4::{NcFile, NcType};
use serde_json::Value;

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("netcdf4")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn number(value: f64) -> Value {
    if value.is_nan() {
        Value::from("NaN")
    } else if value.is_infinite() {
        Value::from(if value > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        Value::from(value)
    }
}

fn same(have: &Value, want: &Value) -> bool {
    match (have.as_f64(), want.as_f64()) {
        (Some(a), Some(b)) => a == b || (a - b).abs() <= f64::EPSILON * b.abs(),
        _ => have == want,
    }
}

/// numpy dtype name of a numeric attribute (what netCDF4-python returns).
fn numpy_name(values: &Values) -> &'static str {
    match values {
        Values::I8(_) => "int8",
        Values::U8(_) => "uint8",
        Values::I16(_) => "int16",
        Values::U16(_) => "uint16",
        Values::I32(_) => "int32",
        Values::U32(_) => "uint32",
        Values::I64(_) => "int64",
        Values::U64(_) => "uint64",
        Values::F32(_) => "float32",
        Values::F64(_) => "float64",
        _ => "other",
    }
}

/// SHA-256 of the canonical bytes of `tools/netcdf4_golden.py`, and the
/// leading items.
fn canonical(values: &Values, nc_type: NcType) -> (String, Vec<Value>) {
    macro_rules! numbers {
        ($v:expr, $f:expr) => {
            (
                $v.iter().flat_map(|x| x.to_le_bytes()).collect(),
                $v.iter().take(8).map($f).collect(),
            )
        };
    }
    let (bytes, head): (Vec<u8>, Vec<Value>) = match values {
        Values::I8(v) => numbers!(v, |x| Value::from(*x)),
        Values::U8(v) => numbers!(v, |x| Value::from(*x)),
        Values::I16(v) => numbers!(v, |x| Value::from(*x)),
        Values::U16(v) => numbers!(v, |x| Value::from(*x)),
        Values::I32(v) => numbers!(v, |x| Value::from(*x)),
        Values::U32(v) => numbers!(v, |x| Value::from(*x)),
        Values::I64(v) => numbers!(v, |x| Value::from(*x)),
        Values::U64(v) => numbers!(v, |x| Value::from(*x)),
        Values::F32(v) => numbers!(v, |x| number(f64::from(*x))),
        Values::F64(v) => numbers!(v, |x| number(*x)),
        Values::FixedStrings { bytes, .. } if nc_type == NcType::Char => (
            bytes.clone(),
            bytes
                .iter()
                .take(8)
                // numpy's S1 drops a NUL byte: b'' decodes to "".
                .map(|byte| match byte {
                    0 => Value::from(""),
                    byte => Value::from(char::from(*byte).to_string()),
                })
                .collect(),
        ),
        _ => {
            let strings = values.strings().unwrap_or_default();
            let mut bytes = Vec::new();
            for text in &strings {
                bytes.extend_from_slice(text.as_bytes());
                bytes.push(0);
            }
            let head = strings.into_iter().take(8).map(Value::from).collect();
            (bytes, head)
        }
    };
    (recast_radar_testdata::sha256_hex(&bytes), head)
}

fn check_attributes(have: &[recast_radar_hdf5::Attribute], want: &Value, context: &str) -> usize {
    let want = want.as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = have.iter().map(|attribute| attribute.name()).collect();
    let want_names: Vec<&str> = want
        .iter()
        .map(|attribute| attribute["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, want_names, "{context}: attribute names in order");
    for (attribute, expected) in have.iter().zip(&want) {
        let context = format!("{context} @{}", attribute.name());
        let values = attribute.values();
        if expected["kind"] == "text" {
            let texts = if attribute.is_null() {
                vec![String::new()]
            } else {
                values
                    .strings()
                    .unwrap_or_else(|| panic!("{context}: text attribute is {values:?}"))
            };
            match &expected["value"] {
                Value::String(text) => {
                    assert_eq!(texts.len(), 1, "{context}: one string");
                    assert_eq!(&texts[0], text, "{context}");
                }
                Value::Array(items) => {
                    let items: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                    assert_eq!(texts, items, "{context}");
                }
                other => panic!("{context}: golden text {other}"),
            }
            assert!(
                matches!(
                    NcType::of(attribute.datatype()),
                    NcType::Char | NcType::String
                ),
                "{context}: text attribute type"
            );
        } else {
            assert_eq!(
                numpy_name(values),
                expected["kind"].as_str().unwrap_or_default(),
                "{context}: numeric type"
            );
            let items: Vec<Value> = (0..values.len())
                .map(|index| {
                    values
                        .get_i64(index)
                        .map(Value::from)
                        .or_else(|| {
                            (!matches!(values, Values::U64(_)))
                                .then(|| values.get_f64(index).map(number))
                                .flatten()
                        })
                        .unwrap_or(Value::Null)
                })
                .collect();
            let want_items = expected["value"].as_array().cloned().unwrap_or_default();
            assert_eq!(items.len(), want_items.len(), "{context}: length");
            for (have, want) in items.iter().zip(&want_items) {
                assert!(same(have, want), "{context}: {have} != {want}");
            }
        }
    }
    have.len()
}

fn check(id: &str) -> bool {
    let bytes = match recast_radar_testdata::bytes(id) {
        Ok(bytes) => bytes,
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            return false;
        }
        Err(err) => panic!("{err}"),
    };
    let golden = golden(id);
    assert_eq!(
        golden["sha256"].as_str(),
        Some(recast_radar_testdata::sha256_hex(&bytes).as_str())
    );
    let file = NcFile::open(&bytes).unwrap_or_else(|err| panic!("{id}: open: {err}"));
    assert_eq!(
        file.nc_properties(),
        golden["nc_properties"].as_str(),
        "{id}: _NCProperties"
    );
    assert_eq!(
        file.is_classic_model(),
        golden["data_model"] == "NETCDF4_CLASSIC",
        "{id}: data model"
    );
    let want_groups = golden["groups"].as_array().cloned().unwrap_or_default();
    let have_paths: Vec<&str> = file.groups().iter().map(|g| g.path.as_str()).collect();
    let want_paths: Vec<&str> = want_groups
        .iter()
        .map(|g| g["path"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(have_paths, want_paths, "{id}: groups, depth first");
    let (mut variables, mut attributes) = (0usize, 0usize);
    for (group, expected) in file.groups().iter().zip(&want_groups) {
        let context = format!("{id} {}", group.path);
        let dims: Vec<(String, u64, bool)> = group
            .dims
            .iter()
            .map(|id| {
                let dim = file.dim(*id).unwrap_or_else(|| panic!("dimension id {id}"));
                (dim.name.clone(), dim.len as u64, dim.unlimited)
            })
            .collect();
        let want_dims: Vec<(String, u64, bool)> = expected["dims"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|d| {
                (
                    d["name"].as_str().unwrap_or_default().to_owned(),
                    d["len"].as_u64().unwrap_or(u64::MAX),
                    d["unlimited"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        assert_eq!(dims, want_dims, "{context}: dimensions");
        let children: Vec<&str> = group.groups.iter().map(String::as_str).collect();
        let want_children: Vec<&str> = expected["groups"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(children, want_children, "{context}: child groups");
        attributes += check_attributes(&group.attributes, &expected["attributes"], &context);

        let want_vars = expected["variables"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let names: Vec<&str> = group.variables.iter().map(|v| v.name.as_str()).collect();
        let want_names: Vec<&str> = want_vars
            .iter()
            .map(|v| v["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names, want_names, "{context}: variables in order");
        for (variable, want) in group.variables.iter().zip(&want_vars) {
            let context = format!("{context}/{}", variable.name);
            assert_eq!(
                variable.nc_type.name(),
                want["type"].as_str().unwrap_or_default(),
                "{context}: type"
            );
            let dim_names: Vec<&str> = variable
                .dims
                .iter()
                .map(|id| {
                    file.dim(*id)
                        .unwrap_or_else(|| panic!("dimension id {id}"))
                        .name
                        .as_str()
                })
                .collect();
            let want_dim_names: Vec<&str> = want["dims"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert_eq!(dim_names, want_dim_names, "{context}: dimensions");
            let shape: Vec<u64> = file.shape(variable).iter().map(|n| *n as u64).collect();
            let want_shape: Vec<u64> = want["shape"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            assert_eq!(shape, want_shape, "{context}: shape");
            attributes += check_attributes(&variable.attributes, &want["attributes"], &context);
            let values = file
                .read(variable)
                .unwrap_or_else(|err| panic!("{context}: read: {err}"));
            assert_eq!(
                values.len() as u64,
                want["value"]["len"].as_u64().unwrap_or(u64::MAX),
                "{context}: value count"
            );
            let (hash, head) = canonical(&values, variable.nc_type);
            let want_head = want["value"]["head"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for (have, want) in head.iter().zip(&want_head) {
                assert!(same(have, want), "{context}: head {have} != {want}");
            }
            assert_eq!(
                hash,
                want["value"]["sha256"].as_str().unwrap_or_default(),
                "{context}: value hash"
            );
            variables += 1;
        }
    }
    eprintln!(
        "{id}: {} groups, {variables} variables, {attributes} attributes match netCDF4-python",
        want_groups.len()
    );
    true
}

macro_rules! golden_tests {
    ($($name:ident => $id:literal,)*) => {
        $(
            #[test]
            fn $name() {
                check($id);
            }
        )*
    };
}

golden_tests! {
    xsapr_cfradial1 => "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    dow8_cfradial1_radx => "cfrad1-dow8-20211011-223602-rhi",
    spol_cfradial1 => "cfrad1-spol-20080604-002217-sur",
    spol_cfradial2 => "cfrad2-spol-20080604-002217-sur",
    irene_cfradial2_radx => "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    iesha_cfradial2_radx_int32 => "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    xsapr_cfradial2_xradar => "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    dow8_cfradial2_xradar => "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
    odim_dkrom_phony_dims => "odim-dkrom-20260820-1130-pvol",
    odim_imgw_square_phony_dims => "odim-imgw-ram-20260711-0015-kdp-max",
}
