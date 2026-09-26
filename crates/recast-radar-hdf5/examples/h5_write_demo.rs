//! Write one HDF5 file that uses every structure the writer emits, for
//! checking with independent readers (`tools/hdf5_writer_check.py hdf5
//! <file>`: h5py).
//!
//! ```text
//! cargo run --release -p recast-radar-hdf5 --example h5_write_demo -- out.h5
//! ```

use recast_radar_hdf5::write::{Data, Layout, NewDataset, Shape, Value, Writer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: h5_write_demo <out.h5>")?;
    let mut writer = Writer::new();
    let root = writer.root();
    writer.add_attribute(root, "Conventions", Value::text_nul("ODIM_H5/V2_4"))?;
    writer.add_attribute(root, "title", Value::text("writer demo"))?;
    writer.add_attribute(root, "empty", Value::text(""))?;
    writer.add_attribute(root, "gain", Value::scalar(Data::F64(vec![0.5])))?;
    writer.add_attribute(root, "flags", Value::vector(Data::I8(vec![-1, 2, 3])))?;
    writer.add_attribute(root, "count", Value::vector(Data::U64(vec![u64::MAX])))?;
    writer.add_attribute(
        root,
        "names",
        Value::var_texts(vec!["alpha".into(), String::new(), "γ".into()]),
    )?;
    let group = writer.add_group(root, "group")?;
    let rows = 300u64;
    let cols = 97u64;
    let values: Vec<u16> = (0..rows * cols).map(|i| (i * 7 % 65_521) as u16).collect();
    writer.add_dataset(
        group,
        "contiguous",
        NewDataset::new(
            Value::array(Data::U16(values.clone()), vec![rows, cols]),
            Layout::Contiguous,
        ),
    )?;
    writer.add_dataset(
        group,
        "chunked",
        NewDataset::new(
            Value::array(Data::U16(values.clone()), vec![rows, cols]),
            Layout::Chunked {
                chunk: vec![45, 40],
                shuffle: true,
                deflate: Some(6),
            },
        )
        .with_fill_value(Data::U16(vec![65_535])),
    )?;
    writer.add_dataset(
        group,
        "many_chunks",
        NewDataset::new(
            Value::array(Data::U16(values), vec![rows, cols]),
            Layout::chunked(vec![1, cols], Some(1)),
        ),
    )?;
    writer.add_dataset(
        group,
        "compact",
        NewDataset::new(Value::vector(Data::F64(vec![1.5, -2.0])), Layout::Compact),
    )?;
    writer.add_dataset(
        group,
        "scalar",
        NewDataset::new(Value::scalar(Data::I32(vec![-7])), Layout::Contiguous),
    )?;
    writer.add_dataset(
        group,
        "strings",
        NewDataset::new(
            Value::var_texts(vec!["azimuth_surveillance".into(), "rhi".into()]),
            Layout::Contiguous,
        ),
    )?;
    writer.add_dataset(
        group,
        "scalar_string",
        NewDataset::new(Value::var_text("fixed"), Layout::Contiguous),
    )?;
    writer.add_dataset(
        group,
        "unallocated",
        NewDataset::new(
            Value {
                data: Data::F32(Vec::new()),
                shape: Shape::Simple(vec![5]),
            },
            Layout::Unallocated,
        ),
    )?;
    let time = writer.add_dataset(
        root,
        "time",
        NewDataset::new(Value::vector(Data::F64(vec![0.0, 1.0])), Layout::Contiguous),
    )?;
    let field = writer.add_dataset(
        root,
        "field",
        NewDataset::new(
            Value::array(Data::I16(vec![1, 2, 3, 4]), vec![2, 2]),
            Layout::Contiguous,
        ),
    )?;
    writer.add_attribute(time, "CLASS", Value::text_nul("DIMENSION_SCALE"))?;
    writer.add_attribute(time, "NAME", Value::text_nul("time"))?;
    writer.add_attribute(
        time,
        "REFERENCE_LIST",
        Value::vector(Data::DimensionScaleRefs(vec![(field, 0)])),
    )?;
    writer.add_attribute(
        field,
        "DIMENSION_LIST",
        Value::vector(Data::ObjectRefLists(vec![vec![time], vec![]])),
    )?;
    writer.add_attribute(root, "target", Value::scalar(Data::ObjectRefs(vec![field])))?;
    for index in (0..20).rev() {
        writer.add_group(root, &format!("sweep_{index}"))?;
    }
    std::fs::write(&path, writer.finish()?)?;
    Ok(())
}
