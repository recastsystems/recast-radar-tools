//! ODIM decoding does not depend on how the HDF5 container is laid out, and
//! fails loudly on attributes it cannot represent.

use recast_radar_core::model::{ArrayBuf, AttrValue};
use recast_radar_io_odim::{OdimError, read_odim_h5_volume};

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
}

/// The DMI Romo PVOL re-containered by HDF5 2.0 in the 1.10+ "latest"
/// format (superblock v3, dense attributes, every version-4 chunk index;
/// manifest entry `odim-dkrom-20260820-1130-pvol-h5latest-trim`, sweeps 1-2
/// cut to 120 gates) decodes to the same sweeps as the original file
/// (superblock v0, old-style groups, v1 B-tree chunks): same rays, same
/// pass-through attributes, and the same value at every kept gate of every
/// quantity.
#[test]
fn latest_format_container_decodes_like_its_source() {
    let source = read_odim_h5_volume(&corpus("odim-dkrom-20260820-1130-pvol")).expect("source");
    let latest = read_odim_h5_volume(&corpus("odim-dkrom-20260820-1130-pvol-h5latest-trim"))
        .expect("h5latest container decodes");
    assert_eq!(latest.sweeps.len(), 2);
    assert_eq!(latest.attrs, source.attrs);
    assert_eq!(latest.location, source.location);
    for (index, (have, want)) in latest.sweeps.iter().zip(&source.sweeps).enumerate() {
        assert_eq!(have.fixed_angle_deg, want.fixed_angle_deg, "sweep {index}");
        assert_eq!(have.rays, want.rays, "sweep {index} rays");
        assert_eq!(have.ray_vars, want.ray_vars, "sweep {index} ray variables");
        // /dataset1/how is stored densely without creation order in the
        // latest container (the derivation's phase change 0/0), so HDF5
        // keeps no attribute order there and the reader lists it by name;
        // every other group keeps the source order.
        if index == 0 {
            let sorted = |attrs: &[(Box<str>, recast_radar_core::model::AttrValue)]| {
                let mut attrs = attrs.to_vec();
                attrs.sort_by(|a, b| a.0.cmp(&b.0));
                attrs
            };
            assert_eq!(
                sorted(&have.other),
                sorted(&want.other),
                "sweep 0 pass-through attributes"
            );
        } else {
            assert_eq!(
                have.other, want.other,
                "sweep {index} pass-through attributes"
            );
        }
        assert_eq!(have.range.ngates(), 120);
        assert_eq!(have.fields.len(), want.fields.len(), "sweep {index} fields");
        assert_eq!(have.fields.len(), 8);
        for field in &want.fields {
            let trimmed = have
                .field(&field.name)
                .unwrap_or_else(|| panic!("sweep {index}: {:?} missing", field.name));
            for ray in 0..want.nrays() {
                for gate in 0..120 {
                    assert_eq!(
                        trimmed.value(ray, gate).map(f32::to_bits),
                        field.value(ray, gate).map(f32::to_bits),
                        "sweep {index} {:?} ray {ray} gate {gate}",
                        field.name
                    );
                }
            }
        }
    }
}

/// An attribute of a datatype ODIM does not use is kept, not dropped and not
/// fatal: the first attribute of the real RMI Jabbeke `/what` group (`date`,
/// a fixed-length NUL-terminated string, datatype class 3) relabelled as
/// opaque (class 5, empty tag), which the HDF5 reader keeps as raw bytes.
/// The volume decodes, `date` stays in the root passthrough as its stored
/// bytes, and the time reference falls back to the earliest ray.
#[test]
fn attribute_without_an_odim_value_is_kept_as_bytes() {
    let original = corpus("odim-bejab-20190606-0000-pvol");
    let file = recast_radar_io_odim::hdf5::H5File::open(&original).expect("real file opens");
    let what = file.object("/what").expect("/what");
    let first = &what.attributes()[0];
    assert!(first.datatype().is_string(), "{:?}", first.datatype());
    // Locate the attribute message: a version-1 attribute message is
    // version (1), reserved (0), name size including the NUL (u16),
    // datatype size, dataspace size (u16 each), then the name padded to 8
    // bytes and the datatype.
    let name = first.name().as_bytes();
    let name_size = (name.len() + 1) as u16;
    let header = (0..original.len() - 8 - name.len())
        .find(|&p| {
            original[p] == 1
                && original[p + 1] == 0
                && u16::from_le_bytes([original[p + 2], original[p + 3]]) == name_size
                && &original[p + 8..p + 8 + name.len()] == name
                && original[p + 8 + name.len()] == 0
        })
        .expect("attribute message in the file");
    let datatype_at = header + 8 + usize::from(name_size).div_ceil(8) * 8;
    assert_eq!(original[datatype_at] & 0x0F, 3, "string datatype class");

    let mut bytes = original.clone();
    bytes[datatype_at] = (bytes[datatype_at] & 0xF0) | 5;
    bytes[datatype_at + 1] = 0; // opaque tag length 0
    let volume = read_odim_h5_volume(&bytes).expect("an opaque attribute does not fail the decode");
    let kept = volume
        .attrs
        .other
        .iter()
        .find(|(name, _)| &**name == first.name())
        .map(|(_, value)| value.clone());
    assert_eq!(
        kept,
        Some(AttrValue::Array(ArrayBuf::U8(b"20190606\0".to_vec())))
    );
    let untouched = read_odim_h5_volume(&original).expect("real file decodes");
    assert!(volume.time_reference >= untouched.time_reference);
    assert_eq!(volume.sweeps.len(), untouched.sweeps.len());
    let _ = OdimError::LimitExceeded(String::new());
}
