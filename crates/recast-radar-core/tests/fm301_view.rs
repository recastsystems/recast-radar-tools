//! FM301 view over real volumes (docs/design/fm301-model.md section 12):
//! layout, padding, ray order and flavor rules. `tests/fm301_conformance.rs`
//! compares the view against the xradar and Py-ART goldens (F.4).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_core::fm301::{
    self, DataRef, FirstDim, Flavor, Passthrough, RowOrder, Values, ViewError, ViewOptions,
    ViewWarning,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, FieldData, FieldName, GateMapping, RangeCoord, Scalar, SweepMode, Volume,
};

fn volume(id: &str) -> Option<Volume> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(err) if err.is_offline() => {
            eprintln!("skipping: {err}");
            return None;
        }
        Err(err) => panic!("{err}"),
    };
    let bytes = std::fs::read(path).unwrap();
    Some(recast_radar_io::read_supported_volume_bytes(&bytes).unwrap())
}

const TIME_ORDER: ViewOptions = ViewOptions {
    flavor: Flavor::Xradar012,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::Flavor,
};

fn text(value: &str) -> AttrValue {
    AttrValue::Text(value.into())
}

#[test]
fn nexrad_dual_pol_view_pads_short_moments_and_keeps_encoding() {
    let Some(volume) = volume("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    let view = fm301::volume_view(&volume, TIME_ORDER, None).unwrap();
    assert!(view.warnings.is_empty(), "{:?}", view.warnings);
    let root = &view.root;
    assert_eq!(root.dim("sweep"), Some(volume.sweeps.len()));
    assert_eq!(root.attr("instrument_name"), Some(&text("KTLX")));
    assert_eq!(
        root.attr("comment"),
        Some(&text("im/exported using xradar"))
    );

    let sweep = view.group("sweep_0").unwrap();
    let model = &volume.sweeps[0];
    assert_eq!(sweep.dim("time"), Some(model.nrays()));
    assert_eq!(sweep.dim("range"), Some(1832));
    let range = sweep.variable("range").unwrap();
    assert_eq!(
        range.attr("meters_to_center_of_first_gate"),
        Some(&AttrValue::Scalar(Scalar::F32(2125.0)))
    );

    let dbzh = sweep.variable("DBZH").unwrap();
    assert!(
        matches!(dbzh.values, Values::Borrowed(_)),
        "DBZH is zero-copy"
    );
    assert_eq!(
        dbzh.attr("scale_factor"),
        Some(&AttrValue::Scalar(Scalar::F64(0.5)))
    );
    assert_eq!(
        dbzh.attr("add_offset"),
        Some(&AttrValue::Scalar(Scalar::F64(-33.0)))
    );
    assert_eq!(
        dbzh.attr("_FillValue"),
        Some(&AttrValue::Scalar(Scalar::U8(0)))
    );
    assert_eq!(
        dbzh.attr("_Undetect"),
        Some(&AttrValue::Scalar(Scalar::U8(0)))
    );
    assert_eq!(dbzh.attr("flag_meanings"), Some(&text("range_folded")));
    assert_eq!(dbzh.attr("units"), Some(&text("dBZ")));

    // ZDR carries 1192 native gates on the 1832-gate range: padded with the
    // fill code on read, native gates unchanged.
    let zdr_field = model.field(&FieldName::Zdr).unwrap();
    assert_eq!(zdr_field.ngates, 1192);
    let zdr = sweep.variable("ZDR").unwrap();
    let Values::Mapped { out_gates, .. } = &zdr.values else {
        panic!("ZDR must be mapped");
    };
    assert_eq!(*out_gates, 1832);
    let ArrayBuf::U16(padded) = zdr.values.materialize().unwrap() else {
        panic!("ZDR stays uint16");
    };
    let FieldData::U16 { values: native, .. } = &zdr_field.data else {
        panic!("ZDR storage is u16");
    };
    assert_eq!(padded.len(), model.nrays() * 1832);
    for ray in [0, model.nrays() / 2, model.nrays() - 1] {
        let row = &padded[ray * 1832..(ray + 1) * 1832];
        assert_eq!(&row[..1192], &native[ray * 1192..(ray + 1) * 1192]);
        assert!(row[1192..].iter().all(|code| *code == 0));
    }

    // Layout: every dataset variable refers to its field; full-length fields in
    // storage order are zero-copy.
    let layout = view.layout();
    let sweep_layout = layout.root.child("sweep_0").unwrap();
    let dbzh_ref = &sweep_layout.variable("DBZH").unwrap().data;
    assert!(dbzh_ref.is_zero_copy());
    let DataRef::Field {
        native_gates,
        out_gates,
        ..
    } = &sweep_layout.variable("ZDR").unwrap().data
    else {
        panic!("ZDR layout refers to its field");
    };
    assert_eq!((*native_gates, *out_gates), (1192, 1832));
}

#[test]
fn xradar_auto_order_sorts_rays_by_azimuth_without_moving_data() {
    let Some(volume) = volume("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    let view = fm301::volume_view(&volume, ViewOptions::XRADAR, None).unwrap();
    let sweep = view.group("sweep_0").unwrap();
    assert_eq!(sweep.dim("azimuth"), Some(volume.sweeps[0].nrays()));
    let ArrayBuf::F32(azimuth) = sweep
        .variable("azimuth")
        .unwrap()
        .values
        .materialize()
        .unwrap()
    else {
        panic!("azimuth is float32");
    };
    assert!(azimuth.windows(2).all(|pair| pair[0] <= pair[1]));

    let dbzh = sweep.variable("DBZH").unwrap();
    let Values::Mapped {
        rows: RowOrder::Permutation(order),
        ..
    } = &dbzh.values
    else {
        panic!("acquisition order is not azimuth order");
    };
    let model = volume.sweeps[0].field(&FieldName::Dbzh).unwrap();
    let FieldData::U8 { values: native, .. } = &model.data else {
        panic!("DBZH storage is u8");
    };
    let ArrayBuf::U8(sorted) = dbzh.values.materialize().unwrap() else {
        panic!("DBZH stays uint8");
    };
    let gates = model.ngates as usize;
    for position in [0, order.len() - 1] {
        let source = order[position] as usize;
        assert_eq!(
            &sorted[position * gates..(position + 1) * gates],
            &native[source * gates..(source + 1) * gates]
        );
        assert_eq!(azimuth[position], volume.sweeps[0].rays.azimuth_deg[source]);
    }
}

#[test]
fn message_1_reflectivity_repeats_on_the_doppler_range() {
    let Some(volume) = volume("l2-klix-20050829-130035") else {
        return;
    };
    let (index, sweep) = volume
        .sweeps
        .iter()
        .enumerate()
        .find(|(_, sweep)| sweep.fields.iter().any(|field| field.gates.stride == 4))
        .expect("a Message 1 sweep with 1 km reflectivity and 250 m Doppler moments");
    let RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ..
    } = sweep.range
    else {
        panic!("uniform range");
    };
    assert_eq!((first_center_m, spacing_m), (-375.0, 250.0));
    let reflectivity = sweep.field(&FieldName::Dbzh).unwrap();
    assert_eq!(
        reflectivity.gates,
        GateMapping {
            start: 0,
            stride: 4
        }
    );
    assert_eq!(
        reflectivity.native_geometry(&sweep.range),
        Some((0.0, 1000.0))
    );

    let view = fm301::volume_view(&volume, TIME_ORDER, None).unwrap();
    let group = view.group(&format!("sweep_{index}")).unwrap();
    let variable = group.variable("DBZH").unwrap();
    assert_eq!(
        variable.attr("comment"),
        Some(&text(
            "native gate spacing 1000 m; values repeated on the 250 m range coordinate"
        ))
    );
    let ArrayBuf::U8(values) = variable.values.materialize().unwrap() else {
        panic!("uint8");
    };
    let FieldData::U8 { values: native, .. } = &reflectivity.data else {
        panic!("u8 storage");
    };
    let out_gates = sweep.range.ngates();
    let native_gates = reflectivity.ngates as usize;
    for ray in [0, sweep.nrays() - 1] {
        let row = &values[ray * out_gates..(ray + 1) * out_gates];
        for gate in 0..native_gates {
            let code = native[ray * native_gates + gate];
            assert!(row[gate * 4..gate * 4 + 4].iter().all(|v| *v == code));
        }
        assert!(row[native_gates * 4..].iter().all(|v| *v == 0));
    }
}

#[test]
fn wmo_flavor_uses_fm301_names_and_time_order() {
    let Some(volume) = volume("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    // The WMO flavor always orders rays by time, whatever `first_dim` says.
    let options = ViewOptions {
        first_dim: FirstDim::Auto,
        ..ViewOptions::WMO
    };
    let view = fm301::volume_view(&volume, options, None).unwrap();
    assert_eq!(
        view.root.attr("Conventions"),
        Some(&text("CF-1.8, WMO CF-1.0"))
    );
    assert_eq!(
        view.root.attr("wmo__cf_profile"),
        Some(&text("FM 301-2022"))
    );
    let sweep = view.group("sweep_0").unwrap();
    assert!(sweep.dim("time").is_some());
    assert!(sweep.variable("fixed_angle").is_some());
    assert!(sweep.variable("sweep_fixed_angle").is_none());
    let vradh = view.group("sweep_1").unwrap().variable("VRADH").unwrap();
    assert_eq!(vradh.attr("units"), Some(&text("m s-1")));
    assert_eq!(
        vradh.attr("coordinates"),
        Some(&text("elevation azimuth range"))
    );
}

#[test]
fn odim_and_cfradial_ray_dimensions_follow_xradar() {
    if let Some(dkrom) = volume("odim-dkrom-20260820-1130-pvol") {
        let view = fm301::volume_view(&dkrom, ViewOptions::XRADAR, None).unwrap();
        assert!(view.group("sweep_0").unwrap().dim("azimuth").is_some());
        assert_eq!(
            view.group("sweep_0")
                .unwrap()
                .variable("DBZH")
                .unwrap()
                .dims
                .first()
                .map(|dim| dim.as_ref()),
            Some("azimuth")
        );
        // Legacy ODIM decoding has no per-ray times: acquisition order warns.
        let timed = fm301::volume_view(&dkrom, TIME_ORDER, None).unwrap();
        assert!(
            timed
                .warnings
                .contains(&ViewWarning::NonMonotonicTime { sweep: 0 })
        );
    }
    if let Some(dow8) = volume("cfrad1-dow8-20211011-223602-rhi-trim3-classic") {
        assert_eq!(dow8.sweeps[0].sweep_mode, SweepMode::Rhi);
        let view = fm301::volume_view(&dow8, ViewOptions::XRADAR, None).unwrap();
        // xradar 0.12 keeps `azimuth` for the DOW8 CfRadial RHI (A.5).
        assert!(view.group("sweep_0").unwrap().dim("azimuth").is_some());
    }
}

#[test]
fn flag_values_that_do_not_fit_the_packed_type_are_an_error() {
    let Some(mut volume) = volume("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    let field = volume.sweeps[0].field_mut(&FieldName::Dbzh).unwrap();
    field.attrs.flag_values = vec![300];
    field.attrs.flag_meanings = vec!["too_big".into()];
    assert!(matches!(
        fm301::volume_view(&volume, ViewOptions::XRADAR, None),
        Err(ViewError::OutOfRange {
            attr: "flag_values",
            ..
        })
    ));
}
