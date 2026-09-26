//! CfRadial 1.x in netCDF-4 containers, decoded natively.
//!
//! The corpus holds two real files in both containers: the Py-ART X-SAPR
//! PPI (published netCDF-4, and a raw variable-for-variable NETCDF3_CLASSIC
//! copy) and the Radx DOW8 RHI (published netCDF-4, 8 fields, and a
//! classic copy keeping 3 fields byte-identical). Each netCDF-4 file must
//! decode to exactly the volume its classic twin decodes to, apart from the
//! container note (`provenance.compression`, `_NCProperties`) and, for
//! DOW8, the five fields and their attributes the classic copy dropped.

use recast_radar_core::model::{FieldData, Volume};
use recast_radar_io_cfradial::read_cfradial1_volume;

fn corpus(id: &str) -> Option<Vec<u8>> {
    match recast_radar_testdata::bytes(id) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            None
        }
        Err(err) => panic!("{err}"),
    }
}

/// The netCDF-4 decode with its container note replaced by the classic
/// one.
fn as_classic(mut volume: Volume) -> Volume {
    assert_eq!(
        volume.provenance.compression.as_deref(),
        Some("cfradial1-netcdf4")
    );
    volume.provenance.compression = Some("cfradial1-netcdf3".to_owned());
    volume
        .attrs
        .other
        .retain(|(name, _)| &**name != "_NCProperties");
    volume
}

#[test]
fn xsapr_netcdf4_decodes_exactly_like_its_classic_twin() {
    let (Some(netcdf4), Some(classic)) = (
        corpus("cfrad1-xsapr-sgp-20110520-ppi-netcdf4"),
        corpus("cfrad1-xsapr-sgp-20110520-ppi-classic"),
    ) else {
        return;
    };
    assert_eq!(&netcdf4[1..4], b"HDF");
    let native = read_cfradial1_volume(&netcdf4).expect("netCDF-4 decode");
    let twin = read_cfradial1_volume(&classic).expect("classic decode");
    assert_eq!(
        twin.provenance.compression.as_deref(),
        Some("cfradial1-netcdf3")
    );
    // netCDF-C 4.1-era file: no _NCProperties.
    assert!(
        !native
            .attrs
            .other
            .iter()
            .any(|(name, _)| &**name == "_NCProperties")
    );
    assert_eq!(as_classic(native), twin);
}

/// Every attribute list of `volume` in name order.
fn attrs_by_name(volume: &mut Volume) {
    fn sort<T>(attrs: &mut [(Box<str>, T)]) {
        attrs.sort_by(|a, b| a.0.cmp(&b.0));
    }
    sort(&mut volume.attrs.other);
    for entry in &mut volume.variable_attrs {
        sort(&mut entry.attrs);
    }
    for extra in &mut volume.extra_vars {
        sort(&mut extra.attrs);
    }
    for sweep in &mut volume.sweeps {
        sort(&mut sweep.other);
        for extra in &mut sweep.extra_vars {
            sort(&mut extra.attrs);
        }
        for field in &mut sweep.fields {
            sort(&mut field.attrs.other);
        }
    }
}

#[test]
fn dow8_radx_netcdf4_decodes_like_its_three_field_classic_copy() {
    let (Some(netcdf4), Some(classic)) = (
        corpus("cfrad1-dow8-20211011-223602-rhi"),
        corpus("cfrad1-dow8-20211011-223602-rhi-trim3-classic"),
    ) else {
        return;
    };
    let native = read_cfradial1_volume(&netcdf4).expect("netCDF-4 decode");
    let twin = read_cfradial1_volume(&classic).expect("classic decode");
    // Radx wrote it with a netCDF-C older than 4.4.1: no _NCProperties
    // (netCDF4-python golden nc_properties = null).
    assert!(
        !native
            .attrs
            .other
            .iter()
            .any(|(name, _)| &**name == "_NCProperties")
    );
    // Eight fields in the published file, in file order; the classic copy
    // kept DBZHC, VEL and WIDTH.
    let names: Vec<&str> = native.sweeps[0]
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(names.len(), 8, "{names:?}");
    for kept in ["DBZHC", "VEL", "WIDTH"] {
        assert!(names.contains(&kept), "{kept} in {names:?}");
    }
    let mut native = as_classic(native);
    for sweep in &mut native.sweeps {
        sweep
            .fields
            .retain(|field| matches!(field.name.as_str(), "DBZHC" | "VEL" | "WIDTH"));
    }
    // Attributes keep each file's order, and the classic copy (written by
    // netCDF4-python) put `_FillValue` first: compare them by name.
    let mut twin = twin;
    attrs_by_name(&mut native);
    attrs_by_name(&mut twin);
    // The classic copy's global `field_names`-style attributes and root
    // variables are the published ones, so everything else must agree.
    assert_eq!(native.sweeps.len(), twin.sweeps.len());
    for (have, want) in native.sweeps.iter().zip(&twin.sweeps) {
        assert_eq!(have.rays, want.rays);
        assert_eq!(have.range, want.range);
        assert_eq!(have.ray_vars, want.ray_vars);
        assert_eq!(have.platform_track, want.platform_track);
        assert_eq!(have.fields, want.fields);
        assert_eq!(have.extra_vars, want.extra_vars);
    }
    assert_eq!(native.attrs, twin.attrs);
    assert_eq!(native.location, twin.location);
    assert_eq!(native.radar_parameters, twin.radar_parameters);
    assert_eq!(native.radar_calibration, twin.radar_calibration);
    assert_eq!(native, twin);
}

#[test]
fn spol_netcdf4_keeps_int16_storage() {
    let Some(bytes) = corpus("cfrad1-spol-20080604-002217-sur") else {
        return;
    };
    let volume = read_cfradial1_volume(&bytes).expect("S-Pol netCDF-4 decode");
    // netCDF4-python: 9 sweeps, 4343 rays x 996 gates, DBZ and VR stored
    // as short with scale_factor/add_offset (tools/netcdf4_golden.py,
    // testdata/golden/netcdf4/cfrad1-spol-20080604-002217-sur.json).
    assert_eq!(volume.sweeps.len(), 9);
    assert_eq!(volume.sweeps.iter().map(|s| s.nrays()).sum::<usize>(), 4343);
    for sweep in &volume.sweeps {
        assert_eq!(sweep.range.ngates(), 996);
        for field in &sweep.fields {
            assert!(
                matches!(field.data, FieldData::I16 { .. }),
                "{} is {:?}",
                field.name.as_str(),
                std::mem::discriminant(&field.data)
            );
        }
    }
}

/// netCDF-4 user-defined types reach the model: a compound variable as one
/// variable per member (`pairs.ray`, `pairs.angle`), an enumerated variable
/// as its base integers, a variable-length one as `<name>.lengths` and its
/// values back to back, an opaque one as its bytes; a compound global
/// attribute as one attribute per member, an enumerated one as its integer,
/// an opaque one as its bytes. Expected values: netCDF4-python's reading of
/// the fixture (tools/derive_cfradial_user_types.py prints and checks them)
/// and, for the opaque ones netCDF4-python skips, netCDF-C's `ncdump`.
#[test]
fn netcdf4_user_defined_types_are_kept() {
    use recast_radar_core::model::{ArrayBuf, AttrValue};

    let Some(bytes) = corpus("cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types") else {
        return;
    };
    let volume = recast_radar_io_cfradial::read_cfradial_volume(&bytes).unwrap();
    let extra = |name: &str| {
        volume.sweeps[0]
            .extra_vars
            .iter()
            .find(|extra| &*extra.name == name)
            .unwrap_or_else(|| panic!("{name} not kept"))
            .values
            .clone()
    };
    assert_eq!(extra("pairs.ray"), ArrayBuf::I32(vec![0]));
    assert_eq!(
        extra("pairs.angle"),
        ArrayBuf::F64(vec![0.499_877_929_687_5])
    );
    assert_eq!(extra("echo_flag").get_f64(0), Some(1.0));
    let global = |name: &str| {
        volume
            .attrs
            .other
            .iter()
            .find(|(have, _)| &**have == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("{name} not kept"))
    };
    assert_eq!(global("sweep_pair.ray").as_f64(), Some(40.0));
    assert_eq!(
        global("sweep_pair.angle").as_f64(),
        Some(0.499_877_929_687_5)
    );
    assert_eq!(global("echo_state").as_f64(), Some(1.0));
    assert!(!matches!(global("sweep_pair.ray"), AttrValue::Text(_)));

    // A variable-length variable: the sequence lengths per sweep, and the
    // values back to back (netCDF-C's `ncdump` prints echo_rays = {0, 1,
    // ..., 30, 32, 34, ..., 39}: 38 rays).
    assert_eq!(extra("echo_rays.lengths"), ArrayBuf::I64(vec![38]));
    let echo_rays = volume
        .extra_vars
        .iter()
        .find(|extra| &*extra.name == "echo_rays")
        .expect("echo_rays values kept")
        .values
        .clone();
    let mut want: Vec<i32> = (0..=30).collect();
    want.extend([32, 34, 35, 36, 37, 38, 39]);
    assert_eq!(echo_rays, ArrayBuf::I32(want));
    // An opaque variable and attribute: their bytes (`ncdump`: 0X00F0FF3E,
    // the fixed angle 0.4998779296875 as little-endian float32).
    assert_eq!(
        extra("fixed_angle_bytes"),
        ArrayBuf::U8(vec![0x00, 0xf0, 0xff, 0x3e])
    );
    let first_bytes = global("first_fixed_angle_bytes");
    let AttrValue::Array(ArrayBuf::U8(bytes)) = &first_bytes else {
        panic!("first_fixed_angle_bytes: {first_bytes:?}");
    };
    assert_eq!(bytes, &[0x00, 0xf0, 0xff, 0x3e]);
}

/// Attributes keep the file's order (netCDF4-python's `ncattrs()`): a
/// kept variable's, and the global ones.
#[test]
fn attributes_keep_file_order() {
    let cases: [(&str, &str, &[&str]); 3] = [
        (
            "cfrad1-xsapr-sgp-20110520-ppi-classic",
            "azimuth",
            &["long_name", "units", "comment", "standard_name", "axis"],
        ),
        (
            "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
            "time_coverage_start",
            &["long_name", "units"],
        ),
        (
            "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
            "time_coverage_start",
            &["long_name", "comment"],
        ),
    ];
    for (id, name, order) in cases {
        let Some(bytes) = corpus(id) else {
            continue;
        };
        let volume = recast_radar_io_cfradial::read_cfradial_volume(&bytes).unwrap();
        let entry = volume
            .variable_attrs
            .iter()
            .find(|entry| &*entry.name == name)
            .unwrap_or_else(|| panic!("{id}: no attributes of {name}"));
        let names: Vec<&str> = entry.attrs.iter().map(|(key, _)| &**key).collect();
        assert_eq!(names, order, "{id} {name}");
    }
    // Global attributes without a slot keep the file's relative order.
    let file_order = [
        "comment",
        "title",
        "Conventions",
        "source",
        "version",
        "references",
        "instrument_name",
        "institution",
        "field_names",
        "history",
    ];
    let Some(bytes) = corpus("cfrad1-xsapr-sgp-20110520-ppi-classic") else {
        return;
    };
    let volume = recast_radar_io_cfradial::read_cfradial_volume(&bytes).unwrap();
    let positions: Vec<usize> = volume
        .attrs
        .other
        .iter()
        .filter_map(|(key, _)| file_order.iter().position(|name| *name == &**key))
        .collect();
    assert!(!positions.is_empty());
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{positions:?}"
    );
}
