//! Write one netCDF-4 file that uses every structure the netCDF-4 writer
//! emits (groups, dimensions with and without coordinate variables, a
//! variable named like a dimension, string variables, packed chunked data,
//! `char`, `string` and numeric attributes), for checking with netCDF-C,
//! netCDF4-python and xarray (`tools/hdf5_writer_check.py netcdf4 <file>`).
//!
//! ```text
//! cargo run --release -p recast-radar-hdf5 --example nc4_write_demo -- out.nc
//! ```

use recast_radar_hdf5::write::netcdf4::{NcAttr, NcStorage, NcVariable, NcWriter};
use recast_radar_hdf5::write::{CharSet, Data};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: nc4_write_demo <out.nc>")?;
    let mut nc = NcWriter::new();
    let root = nc.root();
    nc.add_dim(root, "sweep", 2)?;
    nc.add_attr(root, "Conventions", NcAttr::Text("CF-1.8".into()))?;
    nc.add_attr(root, "empty", NcAttr::Text(String::new()))?;
    nc.add_attr(
        root,
        "strings",
        NcAttr::Strings(vec!["a".into(), "bc".into()]),
    )?;
    nc.add_attr(root, "centre", NcAttr::Numbers(Data::U16(vec![7])))?;
    nc.add_variable(
        root,
        NcVariable {
            name: "sweep_group_name".into(),
            dims: vec!["sweep".into()],
            data: Data::VarStrings {
                values: vec!["sweep_0".into(), "sweep_1".into()],
                charset: CharSet::Utf8,
            },
            attrs: Vec::new(),
            storage: NcStorage::Contiguous,
        },
    )?;
    nc.add_variable(
        root,
        NcVariable {
            name: "latitude".into(),
            dims: Vec::new(),
            data: Data::F64(vec![35.333]),
            attrs: vec![("units".into(), NcAttr::Text("degrees_north".into()))],
            storage: NcStorage::Contiguous,
        },
    )?;
    for index in 0..2u32 {
        let sweep = nc.add_group(root, &format!("sweep_{index}"))?;
        nc.add_dim(sweep, "time", 3)?;
        nc.add_dim(sweep, "range", 4)?;
        nc.add_variable(
            sweep,
            NcVariable {
                name: "time".into(),
                dims: vec!["time".into()],
                data: Data::F64(vec![0.0, 1.0, 2.0]),
                attrs: vec![
                    ("standard_name".into(), NcAttr::Text("time".into())),
                    (
                        "units".into(),
                        NcAttr::Text("seconds since 2024-03-15T00:02:17Z".into()),
                    ),
                ],
                storage: NcStorage::Contiguous,
            },
        )?;
        nc.add_variable(
            sweep,
            NcVariable {
                name: "range".into(),
                dims: vec!["range".into()],
                data: Data::F32(vec![125.0, 375.0, 625.0, 875.0]),
                attrs: vec![("units".into(), NcAttr::Text("metres".into()))],
                storage: NcStorage::Contiguous,
            },
        )?;
        nc.add_variable(
            sweep,
            NcVariable {
                name: "DBZH".into(),
                dims: vec!["time".into(), "range".into()],
                data: Data::U8((0..12).collect()),
                attrs: vec![
                    ("scale_factor".into(), NcAttr::Numbers(Data::F64(vec![0.5]))),
                    ("add_offset".into(), NcAttr::Numbers(Data::F64(vec![-33.0]))),
                    ("_FillValue".into(), NcAttr::Numbers(Data::U8(vec![0]))),
                    ("flag_values".into(), NcAttr::Numbers(Data::U8(vec![1]))),
                    ("flag_meanings".into(), NcAttr::Text("range_folded".into())),
                ],
                storage: NcStorage::Chunked {
                    chunk: vec![3, 4],
                    shuffle: true,
                    deflate: Some(4),
                },
            },
        )?;
        nc.add_variable(
            sweep,
            NcVariable {
                name: "sweep_mode".into(),
                dims: Vec::new(),
                data: Data::VarStrings {
                    values: vec!["azimuth_surveillance".into()],
                    charset: CharSet::Utf8,
                },
                attrs: Vec::new(),
                storage: NcStorage::Contiguous,
            },
        )?;
        let monitoring = nc.add_group(sweep, "monitoring")?;
        nc.add_dim(monitoring, "time", 3)?;
        nc.add_variable(
            monitoring,
            NcVariable {
                name: "zdr_offset".into(),
                dims: vec!["time".into()],
                data: Data::F32(vec![0.1, 0.2, 0.3]),
                attrs: vec![("units".into(), NcAttr::Text("dB".into()))],
                storage: NcStorage::Contiguous,
            },
        )?;
    }
    let calibration = nc.add_group(root, "radar_calibration")?;
    nc.add_dim(calibration, "calib", 1)?;
    nc.add_variable(
        calibration,
        NcVariable {
            name: "time".into(),
            dims: vec!["calib".into()],
            data: Data::F64(vec![0.0]),
            attrs: Vec::new(),
            storage: NcStorage::Contiguous,
        },
    )?;
    std::fs::write(&path, nc.finish()?)?;
    Ok(())
}
