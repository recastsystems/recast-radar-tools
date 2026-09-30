//! FM301 view over real volumes (docs/design/fm301-model.md section 12):
//! layout, padding, ray order and flavor rules. `tests/fm301_conformance.rs`
//! compares the view against the xradar and Py-ART goldens (F.4).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_core::fm301::{
    self, DataRef, FirstDim, Flavor, Passthrough, RowOrder, Values, ViewError, ViewOptions,
    ViewWarning,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, FieldData, FieldName, GateMapping, RangeCoord,
    RayAlignment, Scalar, Sweep, SweepError, SweepMode, Volume,
};

/// Decode a corpus file. A file that is neither committed nor cached and
/// cannot be downloaded skips the calling test only when
/// `RECAST_RADAR_TESTDATA_OFFLINE` is set or the file is not redistributed;
/// otherwise the test fails, so the download-only cases never pass vacuously.
fn volume(id: &str) -> Option<Volume> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(err) if err.is_offline() && (offline_requested() || not_redistributed(id)) => {
            eprintln!("skipping: {err}");
            return None;
        }
        Err(err) => panic!(
            "{err} (set {}=1 to skip files that cannot be fetched)",
            recast_radar_testdata::OFFLINE_ENV
        ),
    };
    let bytes = std::fs::read(path).unwrap();
    Some(recast_radar_io::read_supported_volume_bytes(&bytes).unwrap())
}

/// The manifest tags `id` `not-redistributed`: it runs only from a cached
/// copy.
fn not_redistributed(id: &str) -> bool {
    recast_radar_testdata::entry(id)
        .is_some_and(|entry| entry.tags.iter().any(|tag| tag == "not-redistributed"))
}

/// `RECAST_RADAR_TESTDATA_OFFLINE` is set (as `recast-radar-testdata` reads
/// it).
fn offline_requested() -> bool {
    std::env::var_os(recast_radar_testdata::OFFLINE_ENV)
        .is_some_and(|value| !value.is_empty() && value != "0")
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

/// Dataset variables whose field buffer is already the FM301 array
/// (`DataRef::is_zero_copy`), and all dataset variables, under `first_dim`.
fn zero_copy_fields(volume: &Volume, first_dim: FirstDim) -> (usize, usize) {
    let options = ViewOptions {
        first_dim,
        ..ViewOptions::XRADAR
    };
    let layout = fm301::volume_view(volume, options, None).unwrap().layout();
    let fields: Vec<&DataRef> = layout
        .root
        .children
        .iter()
        .filter(|group| group.name.starts_with("sweep_"))
        .flat_map(|group| &group.variables)
        .map(|variable| &variable.data)
        .filter(|data| matches!(data, DataRef::Field { .. }))
        .collect();
    let zero_copy = fields.iter().filter(|data| data.is_zero_copy()).count();
    (zero_copy, fields.len())
}

/// Whether a moved field buffer is already the variable depends on
/// `first_dim` (design note 12.2): storage keeps the source's ray order.
/// `FirstDim::Time` needs no permutation for acquisition-ordered storage
/// (NEXRAD, CfRadial, most DORADE) or when every ray time is equal (JMA, and
/// ODIM files without per-ray times such as dkrom); truncated and coarse
/// fields are mapped either way. `FirstDim::Auto` needs none for
/// azimuth-ordered storage (ODIM, and the NOXP sweepfile, whose RYIB times
/// decrease). JMA storage starts at an arbitrary azimuth.
#[test]
fn zero_copy_fields_depend_on_first_dim() {
    // (id, (zero-copy, fields) under Time, the same under Auto)
    type Case<'a> = (&'a str, (usize, usize), (usize, usize));
    let cases: [Case; 10] = [
        ("l2-ktlx-20240315-000217", (76, 104), (0, 104)),
        ("l2-kilx-20260418-013553", (95, 125), (0, 125)),
        ("l2-kpah-20080415-235014", (8, 15), (0, 15)),
        (
            "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
            (3, 3),
            (0, 3),
        ),
        ("dorade-dow6-20211230-222139-rhi-head41", (32, 32), (0, 32)),
        ("odim-norst-20170421-0908-pvol", (0, 6), (6, 6)),
        ("odim-iesha-20260305-0115-pvol", (0, 30), (30, 30)),
        ("dorade-noxp-20090501-190244-ppi", (0, 9), (9, 9)),
        ("odim-dkrom-20260820-1130-pvol", (80, 80), (80, 80)),
        ("jma-n5-20191012-090000-rs47773", (26, 26), (0, 26)),
    ];
    for (id, time, auto) in cases {
        let Some(volume) = volume(id) else {
            continue;
        };
        assert_eq!(
            zero_copy_fields(&volume, FirstDim::Time),
            time,
            "{id}: time"
        );
        assert_eq!(
            zero_copy_fields(&volume, FirstDim::Auto),
            auto,
            "{id}: auto"
        );
    }
    // NOXP: RYIB seconds 44, 43, 42 within the sweep, so acquisition order
    // reverses storage order. The time reference is the earliest ray, before
    // the SSWB start (19:02:44Z), so no ray time is negative.
    if let Some(noxp) = volume("dorade-noxp-20090501-190244-ppi") {
        let times = &noxp.sweeps[0].rays.time_s;
        assert!(
            times.first() > times.last(),
            "{:?}",
            (times.first(), times.last())
        );
        assert!(times.windows(2).all(|pair| pair[0] >= pair[1]));
        assert_eq!(
            noxp.time_reference.to_rfc3339(),
            "2009-05-01T19:02:42+00:00"
        );
        assert!(times.iter().all(|time| *time >= 0.0), "{times:?}");
        assert_eq!(
            noxp.time_coverage.map(|coverage| coverage.start),
            Some(noxp.time_reference)
        );
    }
}

/// `name` of `group` as materialized values.
fn values(group: &fm301::Group<'_>, name: &str) -> ArrayBuf {
    group
        .variable(name)
        .unwrap_or_else(|| panic!("{} has no {name}", group.name))
        .values
        .materialize()
        .unwrap()
}

/// A per-ray extra variable follows the view's ray order, and the view does
/// not trust its `shape`: the model's fields are public, so a caller can set
/// any shape. Shape `[nrays, u32::MAX]` over `nrays` values made the view
/// reserve `nrays × u32::MAX` values to reorder the rows, and the process
/// aborted. The values here are the sweep's own azimuths.
#[test]
fn a_per_ray_extra_variable_follows_the_ray_order_whatever_its_shape() {
    let Some(mut volume) = volume("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    let nrays = u32::try_from(volume.sweeps[0].nrays()).unwrap();
    let acquired = ArrayBuf::F32(volume.sweeps[0].rays.azimuth_deg.clone());
    volume.sweeps[0].extra_vars.push(ExtraVariable {
        name: "azimuth_copy".into(),
        dims: vec!["time".into()],
        shape: vec![nrays],
        values: acquired.clone(),
        attrs: Vec::new(),
    });
    let view = fm301::volume_view(&volume, ViewOptions::XRADAR, None).unwrap();
    let sweep = view.group("sweep_0").unwrap();
    let sorted = values(sweep, "azimuth");
    assert_ne!(sorted, acquired, "the view keeps acquisition order");
    assert_eq!(values(sweep, "azimuth_copy"), sorted);

    // A shape that does not describe the values: the rows cannot be found,
    // and the values are kept in source order.
    for shape in [
        vec![nrays, u32::MAX],
        vec![nrays, u32::MAX, u32::MAX, u32::MAX],
    ] {
        let Some(extra) = volume.sweeps[0].extra_vars.last_mut() else {
            panic!("the extra variable is gone");
        };
        extra.dims = (0..shape.len())
            .map(|index| match index {
                0 => "time".into(),
                _ => format!("dim_{index}").into(),
            })
            .collect();
        extra.shape = shape;
        let view = fm301::volume_view(&volume, ViewOptions::XRADAR, None).unwrap();
        let sweep = view.group("sweep_0").unwrap();
        assert_eq!(values(sweep, "azimuth_copy"), acquired);
    }
}

/// Unmodelled source metadata (`Volume::attrs.other`, `Sweep::other`) is
/// written only with `Passthrough::All`; xradar 0.12 writes none of it
/// (design note 0 item 10, 12.3). Typed site constants and per-ray
/// calibration indices are written either way.
#[test]
fn passthrough_all_adds_the_verbatim_source_metadata() {
    let all = ViewOptions {
        passthrough: Passthrough::All,
        ..TIME_ORDER
    };
    let cases = [
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        "odim-iesha-20260305-0115-pvol",
        "dorade-noxp-20090525-203211-sector",
    ];
    for id in cases {
        let Some(volume) = volume(id) else {
            continue;
        };
        assert!(!volume.attrs.other.is_empty(), "{id}: no root metadata");
        let flavor = fm301::volume_view(&volume, TIME_ORDER, None).unwrap();
        let everything = fm301::volume_view(&volume, all, None).unwrap();
        for (name, value) in &volume.attrs.other {
            assert_eq!(everything.root.attr(name), Some(value), "{id}: {name}");
            assert_eq!(flavor.root.attr(name), None, "{id}: {name}");
        }
        // The view writes per-ray attribute arrays in its own ray order: the
        // volume with its rays in that order holds them as the view shows.
        let mut ordered = volume.clone();
        fm301::order_rays_for_view(&mut ordered, all).unwrap();
        for (index, sweep) in ordered.sweeps.iter().enumerate() {
            let path = format!("sweep_{index}");
            let (flavor, everything) = (
                flavor.group(&path).unwrap(),
                everything.group(&path).unwrap(),
            );
            for (name, value) in &sweep.other {
                assert_eq!(everything.attr(name), Some(value), "{id} {path}: {name}");
                assert_eq!(flavor.attr(name), None, "{id} {path}: {name}");
            }
            assert_eq!(
                flavor.variables.len(),
                everything.variables.len(),
                "{id} {path}"
            );
        }
    }

    // CfRadial: the file's global attributes without a typed slot.
    if let Some(dow8) = volume("cfrad1-dow8-20211011-223602-rhi-trim3-classic") {
        assert_eq!(dow8.attrs.other.len(), 9, "{:?}", dow8.attrs.other);
    }

    // ODIM: root and dataset `how` attributes, `what/object`, dataset
    // `what/product` and the `what/version` the Xradar flavor writes as
    // `version` (xradar itself writes "None"); two calibration entries
    // (datasets 1-9 at 2.0 microseconds, dataset 10 at 1.2).
    if let Some(iesha) = volume("odim-iesha-20260305-0115-pvol") {
        let view = fm301::volume_view(&iesha, all, None).unwrap();
        assert_eq!(view.root.attr("software"), Some(&text("RAINBOW 5.61.14")));
        assert_eq!(view.root.attr("object"), Some(&text("PVOL")));
        assert_eq!(view.root.attr("version"), Some(&text("H5rad 2.3")));
        assert_eq!(
            view.group("sweep_0").unwrap().attr("product"),
            Some(&text("SCAN"))
        );
        let flavor = fm301::volume_view(&iesha, TIME_ORDER, None).unwrap();
        assert_eq!(flavor.root.attr("version"), Some(&text("None")));
        let wmo = ViewOptions {
            flavor: Flavor::Wmo2022,
            ..all
        };
        let wmo = fm301::volume_view(&iesha, wmo, None).unwrap();
        assert_eq!(wmo.root.attr("source_version"), Some(&text("H5rad 2.3")));
        assert_eq!(wmo.root.attr("version"), None);
        assert_eq!(
            view.group("sweep_0").unwrap().attr("NEZH"),
            Some(&AttrValue::Scalar(Scalar::F64(-47.7944)))
        );
        assert_eq!(
            view.group("sweep_9").unwrap().attr("NEZH"),
            Some(&AttrValue::Scalar(Scalar::F64(-43.3579)))
        );
        let calibration = view.group("radar_calibration").unwrap();
        assert_eq!(calibration.dim("calib"), Some(2));
        assert_eq!(
            values(calibration, "calib_index"),
            ArrayBuf::I32(vec![0, 1])
        );
        assert_eq!(
            values(calibration, "radar_constant_h"),
            ArrayBuf::F32(vec![67.949, 70.167])
        );
        assert_eq!(
            values(calibration, "pulse_width"),
            ArrayBuf::F32(vec![2e-6, 1.2e-6])
        );
        for (index, expected) in [(0, 0), (8, 0), (9, 1)] {
            let sweep = view.group(&format!("sweep_{index}")).unwrap();
            let rays = sweep.dim("time").unwrap();
            assert_eq!(
                values(sweep, "r_calib_index"),
                ArrayBuf::I32(vec![expected; rays]),
                "sweep {index}"
            );
        }
    }

    // DORADE: VOLD text, the RADD calibration entry and the RYIB transmit
    // power (300 kW) per ray.
    if let Some(noxp) = volume("dorade-noxp-20090525-203211-sector") {
        let view = fm301::volume_view(&noxp, all, None).unwrap();
        assert_eq!(view.root.attr("gen_facility"), Some(&text("NOXPRVP")));
        let calibration = view.group("radar_calibration").unwrap();
        assert_eq!(calibration.dim("calib"), Some(1));
        assert_eq!(
            values(calibration, "radar_constant_h"),
            ArrayBuf::F32(vec![63.71])
        );
        let parameters = view.group("radar_parameters").unwrap();
        assert_eq!(
            parameters.variable("radar_beam_width_h").unwrap().values,
            Values::Scalar(Scalar::F32(0.879_999_94))
        );
        let sweep = view.group("sweep_0").unwrap();
        let rays = sweep.dim("time").unwrap();
        let ArrayBuf::F32(power) = values(sweep, "measured_transmit_power_h") else {
            panic!("float32 transmit power");
        };
        assert_eq!(power.len(), rays);
        assert!(power.iter().all(|dbm| (dbm - 84.771_21).abs() < 1e-3));
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

/// Calibration, monitoring and site variables carry their units in both
/// flavors (FM301 Tables 301-11 and 301-14a; xradar shows the CfRadial files'
/// units), vertical coordinates say `positive = up`, and the table entries
/// of `/radar_calibration` are float32 like the CfRadial source.
#[test]
fn calibration_and_monitoring_variables_carry_units() {
    let cases = [
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        "dorade-noxp-20090501-190244-ppi",
        "odim-iesha-20260305-0115-pvol",
        "l2-ktlx-20240315-000217-trim",
    ];
    let mut counted = Vec::new();
    for id in cases {
        let Some(volume) = volume(id) else {
            continue;
        };
        let (mut calibration_variables, mut monitoring_variables) = (0usize, 0usize);
        for flavor in [Flavor::Xradar012, Flavor::Wmo2022] {
            let options = ViewOptions {
                flavor,
                first_dim: FirstDim::Time,
                passthrough: Passthrough::All,
            };
            let view = fm301::volume_view(&volume, options, None).unwrap();
            let unit = |variable: &fm301::Variable<'_>| match variable.attr("units") {
                Some(AttrValue::Text(units)) => Some(units.to_string()),
                _ => None,
            };
            if let Some(calibration) = view.group("radar_calibration") {
                for variable in &calibration.variables {
                    if variable.name == "calib_index" {
                        continue;
                    }
                    calibration_variables += 1;
                    let units = unit(variable).unwrap_or_else(|| {
                        panic!("{id} {flavor:?}: {} has no units", variable.name)
                    });
                    let expected: &[&str] = match &*variable.name {
                        "time" => &[],
                        "pulse_width" => &["seconds", "s"],
                        "system_phidp" => &["degrees", "degree"],
                        "k_squared_water" | "receiver_slope_hc" | "receiver_slope_vc"
                        | "receiver_slope_hx" | "receiver_slope_vx" => &["", "1"],
                        name if name.starts_with("base_1km") => &["dBZ"],
                        name if name.contains("dbm")
                            || name.starts_with("noise")
                            || name.starts_with("xmit_power")
                            || name.starts_with("sun_power")
                            || name.starts_with("test_power") =>
                        {
                            &["dBm"]
                        }
                        _ => &["dB"],
                    };
                    if variable.name == "time" {
                        assert!(units.starts_with("seconds since "), "{id}: {units}");
                    } else {
                        assert!(
                            expected.contains(&units.as_str()),
                            "{id} {flavor:?}: {} units {units:?}",
                            variable.name
                        );
                    }
                    if let Values::Owned(array) = &variable.values
                        && variable.name != "time"
                        && id.starts_with("cfrad1")
                    {
                        assert_eq!(array.dtype(), "float32", "{id}: {}", variable.name);
                    }
                }
            }
            for group in view
                .root
                .children
                .iter()
                .filter(|g| g.name.starts_with("sweep_"))
            {
                let monitoring = group
                    .child("monitoring")
                    .map(|child| child.variables.iter().collect::<Vec<_>>())
                    .unwrap_or_default();
                let xradar_monitoring = group
                    .variables
                    .iter()
                    .filter(|v| v.name.starts_with("measured_transmit_power"));
                for variable in monitoring.into_iter().chain(xradar_monitoring) {
                    monitoring_variables += 1;
                    assert_eq!(
                        unit(variable).as_deref(),
                        Some("dBm"),
                        "{id} {flavor:?}: {}",
                        variable.name
                    );
                }
                if let Some(ratio) = group.variable("prt_ratio") {
                    let expected = if flavor == Flavor::Wmo2022 { "1" } else { "" };
                    assert_eq!(unit(ratio).as_deref(), Some(expected), "{id}");
                }
                let elevation = group.variable("elevation").unwrap();
                assert_eq!(elevation.attr("positive"), Some(&text("up")), "{id}");
                for name in ["altitude", "altitude_agl"] {
                    if let Some(variable) = group.variable(name) {
                        assert_eq!(variable.attr("positive"), Some(&text("up")), "{id} {name}");
                    }
                }
            }
            for name in ["altitude", "altitude_agl"] {
                if let Some(variable) = view.root.variable(name) {
                    assert_eq!(variable.attr("positive"), Some(&text("up")), "{id} {name}");
                }
            }
        }
        counted.push((id, calibration_variables, monitoring_variables));
    }
    eprintln!("(case, calibration variables, monitoring variables): {counted:?}");
    if !offline_requested() {
        // Both flavors: IRENE and DOW8 carry the Table 301-14a entries the
        // files set (DOW8 also k_squared_water, i0_dbm_* and dynamic_range_db_*)
        // and measure the transmit power per ray; NOXP has DORADE's radar
        // constant, powers, gains and system gain, and the RYIB transmit
        // power per ray; iesha per-dataset radar constants and pulse widths;
        // KTLX the Message 31 VOL calibration constant, system ZDR and
        // initial system PhiDP, and the VOL transmitter powers of every
        // radial.
        let expected = [
            ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", true, true),
            ("cfrad1-dow8-20211011-223602-rhi-trim3-classic", true, true),
            ("dorade-noxp-20090501-190244-ppi", true, true),
            ("odim-iesha-20260305-0115-pvol", true, false),
            ("l2-ktlx-20240315-000217-trim", true, true),
        ];
        for ((id, calibration, monitoring), (expected_id, has_calibration, has_monitoring)) in
            counted.iter().zip(expected)
        {
            assert_eq!(*id, expected_id);
            assert_eq!(*calibration > 0, has_calibration, "{id}: {calibration}");
            assert_eq!(*monitoring > 0, has_monitoring, "{id}: {monitoring}");
        }
    }
}

/// 32-bit integer storage (CfRadial `int` fields; no corpus file has one):
/// DOW8's int16 DBZHC widened to int32 keeps its physical values, and the
/// view writes it as an int32 variable, borrowed in acquisition order, with
/// its packing and fill in the packed type.
#[test]
fn int32_fields_stay_packed() {
    let Some(mut volume) = volume("cfrad1-dow8-20211011-223602-rhi-trim3-classic") else {
        return;
    };
    let name = FieldName::parse("DBZHC");
    let field = volume.sweeps[0].field_mut(&name).unwrap();
    let before = field.to_physical();
    let FieldData::I16 { values, coding } = &field.data else {
        panic!("DBZHC is int16");
    };
    let widened = FieldData::I32 {
        values: values.iter().map(|value| i32::from(*value)).collect(),
        coding: recast_radar_core::model::IntCoding {
            transform: coding.transform,
            fill_value: coding.fill_value.map(i32::from),
            undetect: coding.undetect.map(i32::from),
            range_folded: coding.range_folded.map(i32::from),
            valid_range: coding
                .valid_range
                .map(|[lo, hi]| [i32::from(lo), i32::from(hi)]),
        },
    };
    field.data = widened;
    let after = field.to_physical();
    assert_eq!(before.len(), after.len());
    assert!(
        before
            .iter()
            .zip(&after)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    assert_eq!(field.data.dtype(), "int32");
    volume.seal().unwrap();

    let view = fm301::volume_view(&volume, TIME_ORDER, None).unwrap();
    let variable = view.group("sweep_0").unwrap().variable("DBZHC").unwrap();
    assert!(
        matches!(variable.values, Values::Borrowed(_)),
        "{:?}",
        variable.values
    );
    let encoded = variable.values.materialize().unwrap();
    assert_eq!(encoded.dtype(), "int32");
    assert_eq!(
        variable.attr("_FillValue"),
        Some(&AttrValue::Scalar(Scalar::I32(-32768)))
    );
    assert!(matches!(
        variable.attr("scale_factor"),
        Some(AttrValue::Scalar(Scalar::F32(_)))
    ));
    let layout = view.layout();
    let data = &layout
        .root
        .child("sweep_0")
        .unwrap()
        .variable("DBZHC")
        .unwrap()
        .data;
    assert!(data.is_zero_copy());
}

/// Every variable of a view as materialized values with its dims and
/// attributes, and the view's warnings, keyed by group path.
fn view_contents(volume: &Volume, options: ViewOptions) -> Vec<String> {
    fn walk(group: &fm301::Group<'_>, path: &str, out: &mut Vec<String>) {
        let path = format!("{path}/{}", group.name);
        out.push(format!(
            "{path} dims {:?} attrs {:?}",
            group.dims, group.attrs
        ));
        for variable in &group.variables {
            let values = match &variable.values {
                Values::Scalar(scalar) => format!("{scalar:?}"),
                Values::Text(text) => text.to_string(),
                other => {
                    // Length and an FNV-1a hash of every value's bits.
                    let array = other.materialize().unwrap();
                    let hash = (0..array.len()).fold(0xcbf2_9ce4_8422_2325_u64, |hash, index| {
                        let bits = array.get_f64(index).map_or(u64::MAX, f64::to_bits);
                        (hash ^ bits).wrapping_mul(0x0000_0100_0000_01b3)
                    });
                    format!("{} values, hash {hash:016x}", array.len())
                }
            };
            out.push(format!(
                "{path}/{} dims {:?} attrs {:?} = {values}",
                variable.name, variable.dims, variable.attrs
            ));
        }
        for child in &group.children {
            walk(child, &path, out);
        }
    }
    let view = fm301::volume_view(volume, options, None).unwrap();
    let mut out = vec![format!("warnings {:?}", view.warnings)];
    walk(&view.root, "", &mut out);
    out
}

/// The view options the ray-ordering tests run under: the xradar default,
/// `first_dim="time"` and the WMO flavor, each also with every verbatim
/// source item (`Passthrough::All`, which writes `Sweep::other`).
const VIEW_OPTIONS: [ViewOptions; 6] = [
    ViewOptions::XRADAR,
    TIME_ORDER,
    ViewOptions::WMO,
    ViewOptions {
        passthrough: Passthrough::All,
        ..ViewOptions::XRADAR
    },
    ViewOptions {
        passthrough: Passthrough::All,
        ..TIME_ORDER
    },
    ViewOptions {
        passthrough: Passthrough::All,
        ..ViewOptions::WMO
    },
];

/// `order_rays_for_view` puts storage in the view's ray order: the view
/// shows exactly the same variables, values, attributes and warnings as
/// before, and every field whose gates are the sweep's range gates becomes
/// zero-copy under that view (design note 12.2: none of the Level II,
/// CfRadial or JMA fields is under the xradar default before). Covers
/// Level II (SAILS, legacy resolution), CfRadial and DORADE RHIs, ODIM with
/// per-ray times, the NOXP sweepfile with decreasing ray times and JMA,
/// under the xradar default, `first_dim="time"` and the WMO flavor, with the
/// flavor's items and with every verbatim source item.
#[test]
fn ordering_rays_for_the_view_keeps_it_and_makes_fields_zero_copy() {
    // (id, zero-copy fields after ordering for the xradar default, fields)
    let cases = [
        ("l2-ktlx-20240315-000217", 76, 104),
        ("l2-kpah-20080415-235014", 8, 15),
        ("cfrad1-dow8-20211011-223602-rhi-trim3-classic", 3, 3),
        ("dorade-dow6-20211230-222139-rhi-head41", 32, 32),
        ("odim-norst-20170421-0908-pvol", 6, 6),
        ("dorade-noxp-20090501-190244-ppi", 9, 9),
        ("jma-n5-20191012-090000-rs47773", 26, 26),
    ];
    for (id, zero_copy_after, fields) in cases {
        let Some(volume) = volume(id) else {
            continue;
        };
        for options in VIEW_OPTIONS {
            let before = view_contents(&volume, options);
            let mut ordered = volume.clone();
            let unordered = fm301::order_rays_for_view(&mut ordered, options).unwrap();
            assert!(unordered.is_empty(), "{id} {options:?}: {unordered:?}");
            let after = view_contents(&ordered, options);
            assert_eq!(before.len(), after.len(), "{id} {options:?}");
            for (b, a) in before.iter().zip(&after) {
                assert!(
                    b == a,
                    "{id} {options:?}:\n before {b:.300}\n after  {a:.300}"
                );
            }
            // A second ordering finds nothing to move.
            let mut again = ordered.clone();
            fm301::order_rays_for_view(&mut again, options).unwrap();
            assert!(again == ordered, "{id} {options:?}: ordering is idempotent");
        }
        let mut ordered = volume.clone();
        fm301::order_rays_for_view(&mut ordered, ViewOptions::XRADAR).unwrap();
        assert_eq!(
            zero_copy_fields(&ordered, FirstDim::Auto),
            (zero_copy_after, fields),
            "{id}: zero-copy under the xradar default after ordering"
        );
    }
}

/// A per-ray array among a sweep's verbatim attributes (an ODIM `how` array
/// no typed slot holds) moves with its rays in `order_rays_for_view`, and the
/// view writes it in its own ray order, so that entry `i` belongs to the
/// view's ray `i` whether or not storage was reordered first.
///
/// Input: `odim-au02-20260921-0000-pvol-subset`, two sweeps of a RAINBOW
/// PVOL whose `how` groups each carry six 360-value per-ray arrays that
/// io-odim keeps verbatim in `Sweep::other` (h5py: `TXpower`, `dataflag`,
/// `noisepowerh`, `noisepowerv`, `numpulses`, `startT`). Rays are stored in
/// azimuth order and acquisition starts at ray `a1gate` (225 and 307), so
/// every view order is a rotation of storage. Each stored ray is identified by its
/// azimuth, which is unique within a sweep. The field-level case copies the
/// file's own `TXpower` into the first field's attributes, where an ODIM
/// data-level `how` array would be kept (none of the corpus files has one).
#[test]
fn per_ray_attribute_arrays_move_with_their_rays() {
    const PER_RAY: [&str; 6] = [
        "TXpower",
        "dataflag",
        "noisepowerh",
        "noisepowerv",
        "numpulses",
        "startT",
    ];
    fn find<'v>(attrs: &'v [(Box<str>, AttrValue)], name: &str) -> &'v AttrValue {
        attrs
            .iter()
            .find(|(key, _)| &**key == name)
            .map(|(_, value)| value)
            .unwrap_or_else(|| panic!("no {name}"))
    }
    fn array(value: &AttrValue) -> ArrayBuf {
        match value {
            AttrValue::Array(array) => array.clone(),
            other => panic!("{other:?} is not an array"),
        }
    }
    /// The stored ray of `sweep` with this azimuth.
    fn source_ray(sweep: &Sweep, azimuth: f32) -> usize {
        let matches: Vec<usize> = (0..sweep.nrays())
            .filter(|&ray| sweep.rays.azimuth_deg[ray].to_bits() == azimuth.to_bits())
            .collect();
        assert_eq!(matches.len(), 1, "azimuth {azimuth} is not unique");
        matches[0]
    }

    let Some(mut volume) = volume("odim-au02-20260921-0000-pvol-subset") else {
        return;
    };
    assert_eq!(volume.sweeps.len(), 2);
    for sweep in &mut volume.sweeps {
        let nrays = sweep.nrays();
        assert_eq!(nrays, 360);
        for name in PER_RAY {
            assert_eq!(
                find(&sweep.other, name).ray_alignment(name, nrays),
                RayAlignment::PerRay,
                "{name}"
            );
        }
        let tx_power = find(&sweep.other, "TXpower").clone();
        sweep.fields[0]
            .attrs
            .other
            .push(("TXpower".into(), tx_power));
    }

    let mut rotated = 0;
    for options in VIEW_OPTIONS {
        let mut ordered = volume.clone();
        let unordered = fm301::order_rays_for_view(&mut ordered, options).unwrap();
        assert!(unordered.is_empty(), "{options:?}: {unordered:?}");
        let everything = ViewOptions {
            passthrough: Passthrough::All,
            ..options
        };
        let before_view = fm301::volume_view(&volume, everything, None).unwrap();
        let after_view = fm301::volume_view(&ordered, everything, None).unwrap();
        for (index, (source, moved)) in volume.sweeps.iter().zip(&ordered.sweeps).enumerate() {
            if source.rays.azimuth_deg != moved.rays.azimuth_deg {
                rotated += 1;
            }
            let path = format!("sweep_{index}");
            // Stored ray of the source for each ray of the reordered sweep,
            // and for each ray of the view.
            let rays: Vec<usize> = moved
                .rays
                .azimuth_deg
                .iter()
                .map(|azimuth| source_ray(source, *azimuth))
                .collect();
            let (before_group, after_group) = (
                before_view.group(&path).unwrap(),
                after_view.group(&path).unwrap(),
            );
            let view_rays: Vec<usize> = match before_group
                .variable("azimuth")
                .unwrap()
                .values
                .materialize()
                .unwrap()
            {
                ArrayBuf::F32(azimuths) => azimuths
                    .iter()
                    .map(|azimuth| source_ray(source, *azimuth))
                    .collect(),
                other => panic!("azimuth is {}", other.dtype()),
            };
            for name in PER_RAY {
                let was = array(find(&source.other, name));
                let now = array(find(&moved.other, name));
                for (ray, &from) in rays.iter().enumerate() {
                    assert_eq!(
                        now.get_f64(ray),
                        was.get_f64(from),
                        "{options:?} {path} {name}[{ray}]"
                    );
                }
                for group in [before_group, after_group] {
                    let written = array(group.attr(name).unwrap());
                    for (ray, &from) in view_rays.iter().enumerate() {
                        assert_eq!(
                            written.get_f64(ray),
                            was.get_f64(from),
                            "{options:?} {path} view {name}[{ray}]"
                        );
                    }
                }
            }
            let was = array(find(&source.fields[0].attrs.other, "TXpower"));
            let now = array(find(&moved.fields[0].attrs.other, "TXpower"));
            for (ray, &from) in rays.iter().enumerate() {
                assert_eq!(now.get_f64(ray), was.get_f64(from), "{options:?} {path}");
            }
        }
        assert!(
            view_contents(&volume, everything) == view_contents(&ordered, everything),
            "{options:?}: the view changed"
        );
    }
    // Every option moves both sweeps: time order is a rotation by `a1gate`,
    // and azimuth order a rotation by one ray, because the first stored ray
    // spans 359.5-0.5 deg (h5py `startazA`/`stopazA`) and is centred at
    // 359.99 deg.
    assert_eq!(rotated, 12);
}

/// `order_rays_for_view` checks every sweep before it moves any rows: a
/// per-ray item with the wrong length in the last sweep of KTLX 2024 is an
/// error, and the sweeps before it, which it would otherwise rotate, are
/// left in storage order too.
#[test]
fn ordering_rays_for_the_view_changes_nothing_when_a_sweep_fails() {
    let Some(volume) = volume("l2-ktlx-20240315-000217") else {
        return;
    };
    let mut ordered = volume.clone();
    fm301::order_rays_for_view(&mut ordered, ViewOptions::XRADAR).unwrap();
    assert!(
        ordered.sweeps[0].rays.azimuth_deg != volume.sweeps[0].rays.azimuth_deg,
        "the first sweep is reordered when nothing fails"
    );

    let mut broken = volume.clone();
    let last = broken.sweeps.len() - 1;
    let nrays = broken.sweeps[last].nrays();
    broken.sweeps[last].ray_vars.nyquist_velocity_mps = Some(vec![10.0; nrays + 1]);
    let before = broken.clone();
    assert_eq!(
        fm301::order_rays_for_view(&mut broken, ViewOptions::XRADAR),
        Err(SweepError::RayLength {
            what: "nyquist_velocity".to_owned(),
            len: nrays + 1,
            nrays,
        })
    );
    assert!(broken == before, "a failed ordering moved rows");
}

/// An array attribute with one entry per ray under a name not known to be
/// per ray is neither moved nor left behind: `permute_rays` refuses the
/// sweep, `order_rays_for_view` leaves that sweep in storage order (and
/// orders the others), and the view writes the array as stored, the same
/// before and after. The Melbourne subset's `how/TXpower` stands in for such
/// an array under a name of no ODIM feed (on the second sweep only, and on
/// the first field of the first sweep).
#[test]
fn unknown_per_ray_length_attributes_keep_their_sweep_in_storage_order() {
    let Some(mut volume) = volume("odim-au02-20260921-0000-pvol-subset") else {
        return;
    };
    let nrays = volume.sweeps[1].nrays();
    let (_, tx_power) = volume.sweeps[1]
        .other
        .iter()
        .find(|(name, _)| &**name == "TXpower")
        .cloned()
        .unwrap();
    assert_eq!(
        tx_power.ray_alignment("vendor_array", nrays),
        RayAlignment::Unknown
    );
    volume.sweeps[1]
        .other
        .push(("vendor_array".into(), tx_power.clone()));
    let mut field_case = volume.sweeps[0].clone();
    field_case.fields[0]
        .attrs
        .other
        .push(("vendor_array".into(), tx_power.clone()));
    let field_name = field_case.fields[0].name.as_str().to_owned();
    assert_eq!(
        field_case.unknown_ray_attribute(),
        Some(format!("{field_name}/vendor_array"))
    );
    let before = field_case.clone();
    let order: Vec<u32> = (0..nrays as u32).rev().collect();
    assert_eq!(
        field_case.permute_rays(&order),
        Err(SweepError::UnknownRayAttribute {
            name: format!("{field_name}/vendor_array")
        })
    );
    assert_eq!(field_case, before, "a refused sweep is unchanged");

    for options in VIEW_OPTIONS {
        let everything = ViewOptions {
            passthrough: Passthrough::All,
            ..options
        };
        let mut ordered = volume.clone();
        let unordered = fm301::order_rays_for_view(&mut ordered, options).unwrap();
        assert!(
            unordered
                .iter()
                .all(|kept| kept.sweep == 1 && kept.attribute == "vendor_array"),
            "{options:?}: {unordered:?}"
        );
        assert!(
            ordered.sweeps[1] == volume.sweeps[1],
            "{options:?}: the sweep with the unknown array moved"
        );
        assert!(
            view_contents(&volume, everything) == view_contents(&ordered, everything),
            "{options:?}: the view changed"
        );
        let view = fm301::volume_view(&ordered, everything, None).unwrap();
        let written = view.group("sweep_1").unwrap().attr("vendor_array").unwrap();
        assert!(*written == tx_power, "{options:?}: not written as stored");
    }
    // The first sweep, without such an array, is still ordered for the
    // view (a rotation under every option, as in the test above), and the
    // sweep left in storage order is reported with the array that kept it
    // there.
    let mut ordered = volume.clone();
    let unordered = fm301::order_rays_for_view(&mut ordered, ViewOptions::XRADAR).unwrap();
    assert!(ordered.sweeps[0].rays.azimuth_deg != volume.sweeps[0].rays.azimuth_deg);
    let kept: Vec<(usize, &str)> = unordered
        .iter()
        .map(|kept| (kept.sweep, kept.attribute.as_str()))
        .collect();
    assert_eq!(kept, [(1, "vendor_array")]);
}
