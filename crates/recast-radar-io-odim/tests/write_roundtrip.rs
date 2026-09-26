//! The ODIM_H5 writer against the ODIM_H5 reader on real volumes: every
//! corpus volume (eleven writers: RMI, met.no, AEMET, Met Eireann, DMI,
//! SMHI, FMI, DWD, ARPA Lombardia, h5py rebuilds) read, written and read
//! again gives the same model. Ray times pass through seconds since 1970
//! as float64, so they agree to a microsecond; everything else is equal.
//!
//! Independent readers (h5py, xradar, Py-ART, wradlib) check the written
//! files in `tools/writer_check.py`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::Volume;
use recast_radar_io_odim::{OdimWriteOptions, read_odim_h5_volume, write_odim_h5_volume};
use serde_json::Value as Json;

/// The first path where two JSON trees differ.
fn first_difference(a: &Json, b: &Json, path: &str) -> Option<String> {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            for (key, value) in x {
                let child = format!("{path}.{key}");
                match y.get(key) {
                    Some(other) => {
                        if let Some(found) = first_difference(value, other, &child) {
                            return Some(found);
                        }
                    }
                    None => return Some(format!("{child}: missing on the right")),
                }
            }
            y.keys()
                .find(|key| !x.contains_key(*key))
                .map(|key| format!("{path}.{key}: missing on the left"))
        }
        (Json::Array(x), Json::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: length {} != {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(index, (p, q))| first_difference(p, q, &format!("{path}[{index}]")))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} != {b}")),
    }
}

/// Assert `second` equals `first`, ray times to 1 µs.
fn assert_same(first: &Volume, second: &Volume, id: &str) {
    let mut second = second.clone();
    assert_eq!(first.sweeps.len(), second.sweeps.len(), "{id}: sweeps");
    for (index, (a, b)) in first.sweeps.iter().zip(&mut second.sweeps).enumerate() {
        assert_eq!(
            a.rays.time_s.len(),
            b.rays.time_s.len(),
            "{id}: sweep {index} rays"
        );
        for (ray, (x, y)) in a.rays.time_s.iter().zip(&b.rays.time_s).enumerate() {
            assert!(
                (x - y).abs() <= 1e-6 || (x.is_nan() && y.is_nan()),
                "{id}: sweep {index} ray {ray} time {x} != {y}"
            );
        }
        b.rays.time_s.clone_from(&a.rays.time_s);
    }
    second.time_coverage = first.time_coverage;
    if *first != second {
        let a = serde_json::to_value(first).unwrap();
        let b = serde_json::to_value(&second).unwrap();
        panic!(
            "{id}: the volume read back differs at {}",
            first_difference(&a, &b, "volume").unwrap_or_else(|| "(no JSON difference)".into())
        );
    }
}

fn round_trip(id: &str) {
    let bytes = recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{id}: {err}"));
    let first = read_odim_h5_volume(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"));
    let written = write_odim_h5_volume(&first, &OdimWriteOptions::default())
        .unwrap_or_else(|err| panic!("{id}: write: {err}"));
    let second =
        read_odim_h5_volume(&written).unwrap_or_else(|err| panic!("{id}: read back: {err}"));
    assert_same(&first, &second, id);
    // Uncompressed planes read back the same.
    let plain = write_odim_h5_volume(&first, &OdimWriteOptions::default().with_deflate(None))
        .unwrap_or_else(|err| panic!("{id}: write: {err}"));
    let third = read_odim_h5_volume(&plain).unwrap_or_else(|err| panic!("{id}: read back: {err}"));
    assert_same(&first, &third, id);
}

macro_rules! round_trips {
    ($($name:ident => $id:literal,)*) => {$(
        #[test]
        fn $name() {
            round_trip($id);
        }
    )*};
}

round_trips! {
    bejab => "odim-bejab-20190606-0000-pvol",
    bewid_bool_quality_planes => "odim-bewid-20130429-0430-pvol-dbzh-scan1",
    norst => "odim-norst-20170421-0908-pvol",
    espdg_float64_planes => "odim-espdg-20260707-1927-pvol-dbzh-vradh",
    iesha => "odim-iesha-20260305-0115-pvol",
    dkrom => "odim-dkrom-20260820-1130-pvol",
    dkrom_h5latest => "odim-dkrom-20260820-1130-pvol-h5latest-trim",
    seang_int16_quality_how_subgroups => "odim-seang-20260924-2130-qcvol-dataset1-trim",
    fianj_quality_legends => "odim-fianj-20260924-2130-pvol-dataset1-trim",
    deboo_root_how_subgroups => "odim-deboo-20260924-2130-sweep-th-00",
    itdes_class_legend => "odim-itdes-20260924-2135-pvol-class",
}

/// Every kept attribute goes back to the group it came from: FMI writes
/// `type` (and on its CLASS quality group `legend`) in plane `what` groups,
/// which the ODIM_H5 tables do not list there.
#[test]
fn kept_attributes_return_to_their_group() {
    use recast_radar_io_odim::hdf5::H5File;

    let id = "odim-fianj-20260924-2130-pvol-dataset1-trim";
    let bytes = recast_radar_testdata::bytes(id).unwrap();
    let volume = read_odim_h5_volume(&bytes).unwrap();
    let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default()).unwrap();
    let names = |file: &H5File<'_>, path: &str| -> Vec<String> {
        file.attrs(path)
            .iter()
            .map(|attr| attr.name().to_owned())
            .collect()
    };
    let source = H5File::open(&bytes).unwrap();
    let file = H5File::open(&written).unwrap();
    // The source's DBZH plane is data2, the written one data1.
    for (source_path, written_path) in [
        ("/dataset1/data2/what", "/dataset1/data1/what"),
        ("/dataset1/data2/how", "/dataset1/data1/how"),
        (
            "/dataset1/data2/quality2/what",
            "/dataset1/data1/quality2/what",
        ),
        ("/dataset1/quality2/what", "/dataset1/quality2/what"),
    ] {
        let mut expected = names(&source, source_path);
        let mut got = names(&file, written_path);
        expected.sort();
        got.sort();
        assert_eq!(got, expected, "{written_path}");
    }
    assert!(names(&file, "/dataset1/data1/what").contains(&"type".to_owned()));
}
