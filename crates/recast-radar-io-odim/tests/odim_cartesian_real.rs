//! ODIM_H5 Cartesian MAX decoding of real IMGW POLRAD products (KDP,
//! PhiDP, RhoHV and ZDR): dataset-level metadata, grid geometry, missing
//! values and physical units.

use chrono::{TimeZone, Utc};
use recast_radar_io_odim::odim_cartesian::{
    OdimCartesianProjection, OdimCartesianQuantity, PROJ_SPHERE_RADIUS_M,
    decode_odim_h5_cartesian_max,
};

const KDP: &[u8] = include_bytes!("data/imgw_polrad/2026071100150601KDP.max.h5");
const PHIDP: &[u8] = include_bytes!("data/imgw_polrad/2026071100150601PhiDP.max.h5");
const RHOHV: &[u8] = include_bytes!("data/imgw_polrad/2026071100150601RhoHV.max.h5");
const ZDR: &[u8] = include_bytes!("data/imgw_polrad/2026071100150601ZDR.max.h5");

fn finite_range(values: &[f32]) -> (usize, f32, f32) {
    values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(
            (0, f32::INFINITY, f32::NEG_INFINITY),
            |(count, low, high), value| (count + 1, low.min(value), high.max(value)),
        )
}

#[test]
fn imgw_max_decodes_dataset_level_metadata_geometry_and_missing_values() {
    let grid = decode_odim_h5_cartesian_max(KDP).expect("IMGW KDP MAX decodes");

    assert_eq!(grid.odim_version.as_deref(), Some("H5rd 2.3"));
    assert_eq!(grid.dataset, "dataset1");
    assert_eq!(grid.product, "MAX");
    assert_eq!(grid.quantity_code, "KDP");
    assert_eq!(
        grid.quantity,
        OdimCartesianQuantity::SpecificDifferentialPhase
    );
    assert_eq!(grid.units.as_deref(), Some("deg/km"));
    assert_eq!(
        grid.start_time,
        Utc.with_ymd_and_hms(2026, 7, 11, 0, 15, 6).unwrap()
    );
    assert_eq!(grid.end_time, Some(grid.start_time));

    assert_eq!(grid.site.id, "RAM");
    assert_eq!(grid.site.source, "WMO:12514");
    assert!((grid.site.latitude_deg - 50.151_328).abs() < 1.0e-8);
    assert!((grid.site.longitude_deg - 18.725_094).abs() < 1.0e-8);
    assert!((grid.site.height_m.unwrap() - 357.1).abs() < 1.0e-8);

    let OdimCartesianProjection::AzimuthalEquidistantSphere {
        center_latitude_deg,
        center_longitude_deg,
        radius_m,
        projdef,
    } = &grid.projection
    else {
        panic!(
            "projection {:?} is not the azimuthal equidistant sphere",
            grid.projection
        );
    };
    assert!((*center_latitude_deg - 50.1513).abs() < 1.0e-8);
    assert!((*center_longitude_deg - 18.7251).abs() < 1.0e-8);
    assert_eq!(*radius_m, PROJ_SPHERE_RADIUS_M);
    assert!(projdef.contains("+proj=aeqd"));
    assert!(projdef.contains("+ellps=sphere"));

    assert_eq!((grid.geometry.width, grid.geometry.height), (500, 500));
    assert!((grid.geometry.x_spacing_m - 1_001.953_064_117_074_4).abs() < 1.0e-9);
    assert!((grid.geometry.y_spacing_m - 998.636_495_766_002).abs() < 1.0e-9);
    assert_eq!(grid.geometry.min_height_m, Some(500.0));
    assert_eq!(grid.geometry.max_height_m, Some(18_000.0));
    assert!(
        grid.geometry.corners.upper_left.latitude_deg
            > grid.geometry.corners.lower_left.latitude_deg
    );
    assert_eq!(grid.values().len(), 250_000);
    assert_eq!(grid.encoding.nodata, Some(255.0));
    assert_eq!(grid.encoding.undetect, Some(0.0));

    let (finite, low, high) = finite_range(&grid.values());
    assert_eq!(finite, 1_934);
    assert!((low - -0.719_441_1).abs() < 1.0e-6, "low={low}");
    assert!((high - 0.936_317).abs() < 1.0e-6, "high={high}");
    assert_eq!(
        grid.values().iter().filter(|value| value.is_nan()).count(),
        248_066
    );
    assert!((grid.value_at(231, 93).unwrap() - -0.103_838_73).abs() < 1.0e-6);
    assert!(grid.geometry.cell_center_offset_m(0, 0).unwrap().0 < 0.0);
    assert!(grid.geometry.cell_center_offset_m(0, 0).unwrap().1 > 0.0);
}

#[test]
fn all_released_imgw_dual_pol_max_quantities_decode_with_physical_units() {
    let cases = [
        (
            KDP,
            OdimCartesianQuantity::SpecificDifferentialPhase,
            "deg/km",
            1_934,
            -0.719_441_1,
            0.936_317,
        ),
        (
            PHIDP,
            OdimCartesianQuantity::DifferentialPhase,
            "deg",
            1_934,
            0.0,
            360.0,
        ),
        (
            RHOHV,
            OdimCartesianQuantity::CorrelationCoefficient,
            "1",
            21_131,
            0.003_952_569,
            1.0,
        ),
        (
            ZDR,
            OdimCartesianQuantity::DifferentialReflectivity,
            "dB",
            19_623,
            -8.0,
            12.0,
        ),
    ];

    for (bytes, quantity, units, expected_finite, expected_low, expected_high) in cases {
        let grid = decode_odim_h5_cartesian_max(bytes).expect("IMGW MAX decodes");
        assert_eq!(grid.quantity, quantity);
        assert_eq!(grid.units.as_deref(), Some(units));
        let (finite, low, high) = finite_range(&grid.values());
        assert_eq!(
            finite, expected_finite,
            "{} finite count",
            grid.quantity_code
        );
        assert!(
            (low - expected_low).abs() < 1.0e-5,
            "{} low={low}",
            grid.quantity_code
        );
        assert!(
            (high - expected_high).abs() < 1.0e-5,
            "{} high={high}",
            grid.quantity_code
        );
    }
}

#[test]
fn imgw_dataset_level_what_is_required_not_data_plane_what() {
    let file = recast_radar_io_odim::hdf5::H5File::open(KDP).expect("fixture HDF5 opens");
    assert!(file.has_object("/dataset1/what"));
    assert!(!file.has_object("/dataset1/data1/what"));
    let grid = decode_odim_h5_cartesian_max(KDP).expect("dataset-level metadata decodes");
    assert_eq!(grid.quantity_code, "KDP");
}

/// The decode keeps every dataset (IMGW's `VSP` and `HSP` side projections
/// beside the `MAX`), every plane as stored and every attribute of every
/// group: checked against h5py's reading of the same file
/// (`testdata/golden/hdf5/odim-imgw-ram-20260711-0015-kdp-max.json`,
/// tools/hdf5_golden.py: every attribute, and each plane's shape and SHA-256
/// of its stored values).
#[test]
fn imgw_max_keeps_every_dataset_plane_and_attribute() {
    use recast_radar_core::model::{ArrayBuf, AttrValue};

    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("hdf5")
        .join("odim-imgw-ram-20260711-0015-kdp-max.json");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let objects = golden["objects"].as_object().unwrap();
    let grid = decode_odim_h5_cartesian_max(KDP).expect("IMGW MAX decodes");

    // Every attribute of the file, by path: the root's under `<group>.`,
    // each dataset's and plane's under `<member>.`.
    let mut expected = 0usize;
    for (object_path, object) in objects {
        for attribute in object["attributes"].as_array().unwrap() {
            let name = attribute["name"].as_str().unwrap();
            let parts: Vec<&str> = object_path.trim_start_matches('/').split('/').collect();
            let (attrs, key) = match parts.as_slice() {
                [""] => (&grid.attrs, name.to_owned()),
                [group] if !group.starts_with("dataset") => {
                    (&grid.attrs, format!("{group}.{name}"))
                }
                [dataset, rest @ ..] => {
                    let dataset = grid
                        .datasets
                        .iter()
                        .find(|candidate| candidate.name == *dataset)
                        .unwrap_or_else(|| panic!("{dataset} not kept"));
                    match rest {
                        [] => (&dataset.attrs, name.to_owned()),
                        [plane, member @ ..] if plane.starts_with("data") => {
                            let plane = dataset
                                .planes
                                .iter()
                                .find(|candidate| candidate.name == *plane)
                                .unwrap_or_else(|| panic!("{plane} not kept"));
                            let key = if member.is_empty() {
                                name.to_owned()
                            } else {
                                format!("{}.{name}", member.join("."))
                            };
                            (&plane.attrs, key)
                        }
                        member => (&dataset.attrs, format!("{}.{name}", member.join("."))),
                    }
                }
                [] => unreachable!(),
            };
            let value = attrs
                .iter()
                .find(|(have, _)| **have == *key)
                .map(|(_, value)| value)
                .unwrap_or_else(|| panic!("{object_path} @{name} not kept as {key}"));
            let want = &attribute["value"]["values"][0];
            match (value, want) {
                (AttrValue::Text(text), serde_json::Value::String(want)) => {
                    assert_eq!(&**text, want, "{object_path} @{name}");
                }
                (value, serde_json::Value::Number(want)) => {
                    assert_eq!(value.as_f64(), want.as_f64(), "{object_path} @{name}");
                }
                (value, want) => panic!("{object_path} @{name}: {value:?} against {want}"),
            }
            expected += 1;
        }
    }
    let kept = grid.attrs.len()
        + grid
            .datasets
            .iter()
            .map(|dataset| {
                dataset.attrs.len()
                    + dataset
                        .planes
                        .iter()
                        .map(|plane| plane.attrs.len())
                        .sum::<usize>()
            })
            .sum::<usize>();
    assert_eq!(kept, expected, "attributes kept against the file's");

    // Every plane in its stored type and codes.
    let products: Vec<&str> = grid
        .datasets
        .iter()
        .map(|dataset| match dataset.what("product") {
            Some(AttrValue::Text(product)) => &**product,
            other => panic!("{}: product {other:?}", dataset.name),
        })
        .collect();
    assert_eq!(products, ["MAX", "VSP", "HSP"]);
    for dataset in &grid.datasets {
        for plane in &dataset.planes {
            let path = format!("/{}/{}/data", dataset.name, plane.name);
            let want = &objects[&path]["dataset"];
            let shape: Vec<usize> = want["shape"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(plane.dims, shape, "{path}");
            let ArrayBuf::U8(codes) = &plane.raw else {
                panic!("{path}: stored as {}", plane.raw.dtype());
            };
            assert_eq!(
                recast_radar_testdata::sha256_hex(codes),
                want["value"]["sha256"].as_str().unwrap(),
                "{path}"
            );
        }
    }
    assert_eq!(grid.raw().map(ArrayBuf::len), Some(250_000));
}
