//! Every object, attribute, link, chunk and dataset value of real HDF5 files
//! against h5py (the HDF Group's C library).
//!
//! Goldens: `testdata/golden/hdf5/<id>.json`, written by
//! `tools/hdf5_golden.py`. Each test opens one corpus file and compares the
//! whole object graph: paths reached through hard links, object kinds,
//! object header addresses and versions, every attribute (datatype class,
//! shape and value, hashed for long arrays), group links, and for datasets
//! the shape, maximum shape, layout, chunk shape, chunk index, filters, every
//! stored chunk (element offsets, filter mask, file offset, stored size) and a
//! SHA-256 of all values in the canonical encoding the script documents.

use std::collections::{BTreeMap, BTreeSet};

use recast_radar_hdf5::{
    Attribute, ByteOrder, ChunkIndexKind, Datatype, H5File, LinkTarget, ObjectKind, StorageLayout,
    UNLIMITED, Values,
};
use serde_json::Value;

fn golden(id: &str) -> Value {
    let path = recast_radar_testdata::testdata_dir()
        .join("golden")
        .join("hdf5")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn sha256(bytes: &[u8]) -> String {
    recast_radar_testdata::sha256_hex(bytes)
}

fn class_name(datatype: &Datatype) -> &'static str {
    match datatype {
        Datatype::Integer { .. } => "integer",
        Datatype::Float { .. } => "float",
        Datatype::FixedString { .. } => "string",
        Datatype::VarLenString { .. } => "vlen_string",
        Datatype::Bitfield { .. } => "bitfield",
        Datatype::Opaque { .. } => "opaque",
        Datatype::Compound { .. } => "compound",
        Datatype::Reference { .. } => "reference",
        Datatype::Enum { .. } => "enum",
        Datatype::VarLenSequence { .. } => "vlen",
        Datatype::Array { .. } => "array",
        Datatype::Other { class: 2, .. } => "time",
        _ => "other",
    }
}

/// True for variable-length strings and sequences, and for compounds and
/// arrays that hold one.
fn has_variable_length(datatype: &Datatype) -> bool {
    match datatype {
        Datatype::VarLenString { .. } | Datatype::VarLenSequence { .. } => true,
        Datatype::Compound { members, .. } => members
            .iter()
            .any(|member| has_variable_length(&member.datatype)),
        Datatype::Array { base, .. } => has_variable_length(base),
        _ => false,
    }
}

fn check_type(datatype: &Datatype, expected: &Value, context: &str) {
    assert_eq!(
        class_name(datatype),
        expected["class"].as_str().unwrap_or_default(),
        "{context}: datatype class ({datatype:?})"
    );
    let size = expected["size"].as_u64().unwrap_or(0) as usize;
    // HDF5 reports the in-memory size of variable-length data (and so of a
    // compound holding some), not the stored size.
    if !has_variable_length(datatype) && !matches!(datatype, Datatype::Reference { .. }) {
        assert_eq!(datatype.size(), size, "{context}: datatype size");
    }
    match datatype {
        Datatype::Integer { signed, order, .. } => {
            assert_eq!(
                Some(*signed),
                expected["signed"].as_bool(),
                "{context}: signedness"
            );
            check_order(*order, expected, context);
        }
        Datatype::Float { order, .. } | Datatype::Bitfield { order, .. } => {
            check_order(*order, expected, context);
        }
        Datatype::Compound { members, .. } => {
            let expected_members = expected["members"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                members.len(),
                expected_members.len(),
                "{context}: compound members"
            );
            for (member, want) in members.iter().zip(&expected_members) {
                assert_eq!(member.name, want["name"].as_str().unwrap_or_default());
                assert_eq!(
                    member.offset as u64,
                    want["offset"].as_u64().unwrap_or(u64::MAX)
                );
                check_type(
                    &member.datatype,
                    &want["type"],
                    &format!("{context}.{}", member.name),
                );
            }
        }
        Datatype::VarLenSequence { base, .. } | Datatype::Array { base, .. } => {
            check_type(base, &expected["base"], &format!("{context}[]"));
        }
        Datatype::Enum { base, members } => {
            check_type(base, &expected["base"], &format!("{context} enum base"));
            let want = expected["members"].as_object().cloned().unwrap_or_default();
            assert_eq!(members.len(), want.len(), "{context}: enum members");
            for member in members {
                assert_eq!(
                    Some(member.value as i64),
                    want.get(&member.name).and_then(Value::as_i64),
                    "{context}: enum member {}",
                    member.name
                );
            }
        }
        _ => {}
    }
}

fn check_order(order: ByteOrder, expected: &Value, context: &str) {
    let want = expected["order"].as_str().unwrap_or_default();
    let have = if order == ByteOrder::BigEndian {
        "big"
    } else {
        "little"
    };
    assert_eq!(have, want, "{context}: byte order");
}

/// The canonical encoding of `tools/hdf5_golden.py`, and JSON-comparable
/// items.
fn canonical_hash(values: &Values) -> Option<(String, Vec<Value>)> {
    macro_rules! numbers {
        ($v:expr) => {{
            let bytes = $v.iter().flat_map(|x| x.to_le_bytes()).collect();
            let items = $v
                .iter()
                .map(|x| number(*x as f64, i128::try_from(*x as i128).ok()))
                .collect();
            Some((bytes, items))
        }};
    }
    let (bytes, items): (Vec<u8>, Vec<Value>) = match values {
        Values::I8(v) => numbers!(v),
        Values::U8(v) => numbers!(v),
        Values::I16(v) => numbers!(v),
        Values::U16(v) => numbers!(v),
        Values::I32(v) => numbers!(v),
        Values::U32(v) => numbers!(v),
        Values::I64(v) => numbers!(v),
        Values::U64(v) => numbers!(v),
        Values::F32(v) => Some((
            v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            v.iter().map(|x| number(f64::from(*x), None)).collect(),
        )),
        Values::F64(v) => Some((
            v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            v.iter().map(|x| number(*x, None)).collect(),
        )),
        Values::FixedStrings { .. } | Values::VarStrings(_) => {
            let strings = values.strings()?;
            let mut bytes = Vec::new();
            for text in &strings {
                bytes.extend_from_slice(text.as_bytes());
                bytes.push(0);
            }
            Some((bytes, strings.into_iter().map(Value::String).collect()))
        }
        Values::References(v) => Some((
            v.iter()
                .flat_map(|x| x.unwrap_or(0).to_le_bytes())
                .collect(),
            v.iter().map(|x| Value::from(x.unwrap_or(0))).collect(),
        )),
        _ => None,
    }?;
    Some((recast_radar_testdata::sha256_hex(&bytes), items))
}

fn number(value: f64, integer: Option<i128>) -> Value {
    if let Some(integer) = integer {
        if let Ok(small) = i64::try_from(integer) {
            return Value::from(small);
        }
        if let Ok(big) = u64::try_from(integer) {
            return Value::from(big);
        }
    }
    if value.is_nan() {
        Value::from("NaN")
    } else if value.is_infinite() {
        Value::from(if value > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        Value::from(value)
    }
}

fn same_item(have: &Value, want: &Value) -> bool {
    match (have.as_f64(), want.as_f64()) {
        (Some(a), Some(b)) => a == b || (a - b).abs() <= f64::EPSILON * b.abs(),
        _ => have == want,
    }
}

fn check_values(values: &Values, expected: &Value, context: &str) {
    let len = expected["len"].as_u64().unwrap_or(u64::MAX) as usize;
    if let Some(columns) = expected.get("compound").and_then(Value::as_object) {
        // The element count, not a member column's length (an array member
        // flattens: the Desio CLASS legend's `char[64]` key column holds 26 * 64).
        assert_eq!(values.len(), len, "{context}: compound element count");
        let Values::Compound { members: have, .. } = values else {
            panic!("{context}: expected compound values, got {values:?}");
        };
        assert_eq!(have.len(), columns.len(), "{context}: compound columns");
        for (name, column) in have {
            let want = columns
                .get(name)
                .unwrap_or_else(|| panic!("{context}: no golden column {name}"));
            check_values(column, want, &format!("{context}.{name}"));
        }
        return;
    }
    if let Some(sequences) = expected.get("sequences").and_then(Value::as_array) {
        let Values::Sequences(have) = values else {
            panic!("{context}: expected sequences, got {values:?}");
        };
        assert_eq!(have.len(), len, "{context}: sequence count");
        for (index, (sequence, want)) in have.iter().zip(sequences).enumerate() {
            check_values(sequence, want, &format!("{context}[{index}]"));
        }
        return;
    }
    assert_eq!(values.len(), len, "{context}: element count");
    let Some((hash, items)) = canonical_hash(values) else {
        panic!("{context}: no canonical form for {values:?}");
    };
    if let Some(want_hash) = expected.get("sha256").and_then(Value::as_str) {
        assert_eq!(hash, want_hash, "{context}: value hash");
    }
    let want = expected
        .get("values")
        .or_else(|| expected.get("head"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, (have, want)) in items.iter().zip(&want).enumerate() {
        assert!(
            same_item(have, want),
            "{context}[{index}]: {have} != {want}"
        );
    }
}

fn check_attribute(attribute: &Attribute, expected: &Value, context: &str) {
    let context = format!("{context} @{}", attribute.name());
    check_type(attribute.datatype(), &expected["type"], &context);
    let shape: Vec<usize> = expected["shape"]
        .as_array()
        .map(|dims| {
            dims.iter()
                .map(|d| d.as_u64().unwrap_or(0) as usize)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(attribute.dims(), &shape[..], "{context}: shape");
    assert_eq!(
        attribute.is_null(),
        expected["null"].as_bool().unwrap_or(false),
        "{context}: null"
    );
    if let Some(value) = expected.get("value") {
        check_values(attribute.values(), value, &context);
    }
}

/// `json.dumps(sorted([[offsets], mask, byte_offset, size], ...))`, with
/// empty offsets when h5py cannot report them (see the golden script).
fn chunk_info_json(
    chunks: &[recast_radar_hdf5::ChunkLocation],
    with_offsets: bool,
) -> (usize, String, Vec<String>) {
    let mut rows: Vec<(Vec<u64>, u32, u64, usize)> = chunks
        .iter()
        .map(|chunk| {
            let offsets = if with_offsets {
                chunk.offsets.clone()
            } else {
                Vec::new()
            };
            (
                offsets,
                chunk.filter_mask,
                chunk.file_offset,
                chunk.stored_size,
            )
        })
        .collect();
    rows.sort();
    let text: Vec<String> = rows
        .iter()
        .map(|(offsets, mask, offset, size)| {
            let offsets: Vec<String> = offsets.iter().map(u64::to_string).collect();
            format!("[[{}], {mask}, {offset}, {size}]", offsets.join(", "))
        })
        .collect();
    (rows.len(), format!("[{}]", text.join(", ")), text)
}

/// The golden script's name of a chunk index (HDF5's `H5D_chunk_index_t`).
fn chunk_index_name(index: ChunkIndexKind) -> &'static str {
    match index {
        ChunkIndexKind::BTreeV1 => "btree_v1",
        ChunkIndexKind::SingleChunk => "single_chunk",
        ChunkIndexKind::Implicit => "implicit",
        ChunkIndexKind::FixedArray => "fixed_array",
        ChunkIndexKind::ExtensibleArray => "extensible_array",
        ChunkIndexKind::BTreeV2 => "btree_v2",
        _ => "other",
    }
}

fn check_dataset(file: &H5File<'_>, path: &str, expected: &Value) {
    let info = file
        .dataset_info(path)
        .unwrap_or_else(|err| panic!("{path}: dataset info: {err}"));
    let shape: Vec<usize> = expected["shape"]
        .as_array()
        .map(|dims| {
            dims.iter()
                .map(|d| d.as_u64().unwrap_or(0) as usize)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(info.dims, shape, "{path}: shape");
    if let Some(maxshape) = expected["maxshape"]
        .as_array()
        .filter(|dims| !dims.is_empty())
    {
        let want: Vec<u64> = maxshape
            .iter()
            .map(|d| {
                if d.as_i64() == Some(-1) {
                    UNLIMITED
                } else {
                    d.as_u64().unwrap_or(0)
                }
            })
            .collect();
        let have = info
            .max_dims
            .clone()
            .unwrap_or_else(|| info.dims.iter().map(|d| *d as u64).collect());
        assert_eq!(have, want, "{path}: maxshape");
    }
    check_type(&info.datatype, &expected["type"], path);
    let layout = expected["layout"].as_str().unwrap_or_default();
    match &info.layout {
        StorageLayout::Compact => assert_eq!(layout, "compact", "{path}"),
        StorageLayout::Contiguous => assert_eq!(layout, "contiguous", "{path}"),
        StorageLayout::Chunked { chunk_dims, index } => {
            assert_eq!(layout, "chunked", "{path}");
            // HDF5's own H5Dget_chunk_index_type (the golden script calls it
            // through ctypes): a reader that took one index for another
            // could still find every chunk address.
            if let Some(want) = expected["chunk_index"].as_str() {
                assert_eq!(chunk_index_name(*index), want, "{path}: chunk index");
            }
            let want: Vec<usize> = expected["chunks"]
                .as_array()
                .map(|dims| {
                    dims.iter()
                        .map(|d| d.as_u64().unwrap_or(0) as usize)
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(chunk_dims, &want, "{path}: chunk shape");
            let filters: Vec<u64> = info.filters.iter().map(|f| u64::from(f.id)).collect();
            let want_filters: Vec<u64> = expected["filters"]
                .as_array()
                .map(|ids| ids.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            assert_eq!(filters, want_filters, "{path}: filters");
            let chunks = file
                .chunk_locations(path)
                .unwrap_or_else(|err| panic!("{path}: chunks: {err}"));
            let with_offsets = expected["chunk_offsets_reliable"].as_bool().unwrap_or(true);
            let (count, text, rows) = chunk_info_json(&chunks, with_offsets);
            assert_eq!(
                count as u64,
                expected["num_chunks"].as_u64().unwrap_or(u64::MAX),
                "{path}: chunk count"
            );
            if sha256(text.as_bytes()) != expected["chunk_info_sha256"].as_str().unwrap_or_default()
            {
                let want = expected["chunk_info"].to_string();
                panic!(
                    "{path}: chunk list differs\n ours: {:?}\n h5py: {want}",
                    &rows[..rows.len().min(4)]
                );
            }
        }
        StorageLayout::Virtual => assert_eq!(layout, "virtual", "{path}"),
        other => panic!("{path}: unexpected layout {other:?}"),
    }
    if let Some(value) = expected.get("value") {
        let dataset = file
            .dataset(path)
            .unwrap_or_else(|err| panic!("{path}: read: {err}"));
        check_values(&dataset.values, value, path);
    }
}

fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Group => "group",
        ObjectKind::Dataset => "dataset",
        ObjectKind::Datatype => "datatype",
        _ => "other",
    }
}

/// Open a corpus file and compare it with its golden; `false` when the
/// file is download-only and unavailable offline.
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
    let file = H5File::open(&bytes).unwrap_or_else(|err| panic!("{id}: open: {err}"));
    assert_eq!(
        u64::from(file.superblock_version()),
        golden["superblock_version"].as_u64().unwrap_or(u64::MAX),
        "{id}: superblock version"
    );
    assert_eq!(
        file.offset_size() as u64,
        golden["offset_size"].as_u64().unwrap_or(0)
    );

    let objects = golden["objects"].as_object().cloned().unwrap_or_default();
    let ours: BTreeSet<&str> = file.objects().map(|(path, _)| path).collect();
    let theirs: BTreeSet<&str> = objects.keys().map(String::as_str).collect();
    assert_eq!(ours, theirs, "{id}: object paths");

    let mut attribute_count = 0usize;
    let mut dataset_count = 0usize;
    for (path, expected) in &objects {
        let context = format!("{id} {path}");
        let object = file
            .object(path)
            .unwrap_or_else(|| panic!("{context}: missing"));
        assert_eq!(
            kind_name(object.kind()),
            expected["kind"].as_str().unwrap_or_default(),
            "{context}: kind"
        );
        assert_eq!(
            object.address(),
            expected["address"].as_u64().unwrap_or(u64::MAX),
            "{context}: address"
        );
        assert_eq!(
            u64::from(object.header_version()),
            expected["header_version"].as_u64().unwrap_or(0),
            "{context}: header version"
        );

        let want_attributes: BTreeMap<&str, &Value> = expected["attributes"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|a| (a["name"].as_str().unwrap_or_default(), a))
                    .collect()
            })
            .unwrap_or_default();
        let have_names: BTreeSet<&str> = object.attributes().iter().map(Attribute::name).collect();
        let want_names: BTreeSet<&str> = want_attributes.keys().copied().collect();
        assert_eq!(have_names, want_names, "{context}: attribute names");
        for attribute in object.attributes() {
            check_attribute(attribute, want_attributes[attribute.name()], &context);
            attribute_count += 1;
        }
        // h5py iterates in creation order when the object tracks it.
        if object
            .attributes()
            .iter()
            .all(|a| a.creation_order().is_some())
        {
            let have: Vec<&str> = object.attributes().iter().map(Attribute::name).collect();
            let want: Vec<&str> = expected["attributes"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|a| a["name"].as_str().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(have, want, "{context}: attribute creation order");
        }

        if let Some(links) = expected.get("links").and_then(Value::as_array) {
            let want: Vec<(&str, &str)> = links
                .iter()
                .map(|l| {
                    (
                        l["name"].as_str().unwrap_or_default(),
                        l["kind"].as_str().unwrap_or_default(),
                    )
                })
                .collect();
            let have: Vec<(&str, &str)> = object
                .links()
                .iter()
                .map(|link| {
                    let kind = match link.target {
                        LinkTarget::Hard(_) => "hard",
                        LinkTarget::Soft(_) => "soft",
                        LinkTarget::External { .. } => "external",
                        _ => "other",
                    };
                    (link.name.as_str(), kind)
                })
                .collect();
            assert_eq!(have, want, "{context}: links in iteration order");
        }
        if let Some(dataset) = expected.get("dataset") {
            check_dataset(&file, path, dataset);
            dataset_count += 1;
        }
    }
    eprintln!(
        "{id}: {} paths, {attribute_count} attributes, {dataset_count} datasets match h5py",
        objects.len()
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
    xsapr_netcdf4_cfradial1 => "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    spol_netcdf4_cfradial1 => "cfrad1-spol-20080604-002217-sur",
    spol_cfradial2 => "cfrad2-spol-20080604-002217-sur",
    dow8_netcdf4_cfradial1 => "cfrad1-dow8-20211011-223602-rhi",
    irene_cfradial2_radx => "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    iesha_cfradial2_radx_int32 => "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    xsapr_cfradial2_xradar => "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    dow8_cfradial2_xradar => "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
    odim_bejab => "odim-bejab-20190606-0000-pvol",
    odim_bewid_vlen_strings => "odim-bewid-20130429-0430-pvol-dbzh-scan1",
    odim_norst_superblock_v1 => "odim-norst-20170421-0908-pvol",
    odim_espdg_v2_headers => "odim-espdg-20260707-1927-pvol-dbzh-vradh",
    odim_imgw_cartesian => "odim-imgw-ram-20260711-0015-kdp-max",
    odim_iesha => "odim-iesha-20260305-0115-pvol",
    odim_dkrom => "odim-dkrom-20260820-1130-pvol",
    odim_seang_int16_how_subgroups => "odim-seang-20260924-2130-qcvol-dataset1-trim",
    odim_fianj_compound_vlen_legend => "odim-fianj-20260924-2130-pvol-dataset1-trim",
    odim_deboo_root_how_subgroups => "odim-deboo-20260924-2130-sweep-th-00",
    odim_itdes_compound_char_array_legend => "odim-itdes-20260924-2135-pvol-class",
    odim_dkrom_h5latest_every_chunk_index => "odim-dkrom-20260820-1130-pvol-h5latest-trim",
    odim_dkrom_paged_extensible_array_committed_type => "odim-dkrom-20260820-1130-pvol-h5edge-paged-ea",
    odim_dkrom_4_byte_lengths_filtered_link_heap => "odim-dkrom-20260820-1130-pvol-h5edge-len4",
}
