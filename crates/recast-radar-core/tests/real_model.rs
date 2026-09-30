//! Model values of real volumes: fields, sweeps and per-ray instrument
//! variables.
//!
//! Expected values: `testdata/golden/core/model.json`, written by
//! `tools/core_golden.py` with Py-ART 2.2.5 (raw moment codes and data-block
//! headers), MetPy 1.7.1 (sweep layouts), netCDF4 1.7.4 (per-ray instrument
//! variables) and a GRIB2 section walker (JMA sweep ladder).

// A panic is how a test fails (clippy.toml), in helpers too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    array, as_f64, as_opt_f64, as_str, as_usize, assert_close, cfradial, field, field_names,
    golden, jma, level2, nexrad_field, odim, raw_code, raw_row_sums, time,
};
use recast_radar_core::model::{Coding, SweepError};
use recast_radar_core::{
    ArrayBuf, Field, FieldData, FieldName, Gate, GateMapping, LinearTransform, Polarization,
    Quantity, RangeCoord,
};
use serde_json::Value;

/// ICD scale and offset of a Level II field, with its sentinel codes.
fn nexrad_coding(field: &Field) -> (f32, f32, Option<u16>, Option<u16>, u32) {
    let (transform, fill, folded, bits) = match &field.data {
        FieldData::U8 { coding, .. } => (
            coding.transform,
            coding.fill_value.map(u16::from),
            coding.range_folded.map(u16::from),
            8,
        ),
        FieldData::U16 { coding, .. } => {
            (coding.transform, coding.fill_value, coding.range_folded, 16)
        }
        other => panic!(
            "Level II fields are unsigned integers, not {}",
            other.dtype()
        ),
    };
    let LinearTransform::IcdScaleOffset { scale, offset } = transform else {
        panic!("Level II fields use the ICD transform");
    };
    (scale, offset, fill, folded, bits)
}

/// Every physical cell of a decoded u8/u16 field against Py-ART's raw codes:
/// header fields, per-row code sums, probes, and the count of cells the
/// model reports as no data (codes 0 and 1).
fn assert_field_matches_raw_codes(field: &Field, range: &RangeCoord, expected: &Value, what: &str) {
    let (rows, gates) = field.shape();
    assert_eq!(rows, as_usize(&expected["rays"]), "{what}: rows");
    assert_eq!(gates, as_usize(&expected["gates"]), "{what}: gates");
    let (first_gate_m, spacing_m) = field.native_geometry(range).expect("geometry");
    assert_eq!(
        first_gate_m,
        as_f64(&expected["first_gate_m"]),
        "{what}: first gate centre"
    );
    assert_eq!(
        spacing_m,
        as_f64(&expected["gate_spacing_m"]),
        "{what}: gate spacing"
    );
    let (scale, offset, fill, folded, bits) = nexrad_coding(field);
    assert_eq!(
        bits,
        as_usize(&expected["word_size"]) as u32,
        "{what}: word size"
    );
    assert_eq!(
        f64::from(scale),
        as_f64(&expected["scale"]),
        "{what}: scale"
    );
    assert_eq!(
        f64::from(offset),
        as_f64(&expected["offset"]),
        "{what}: offset"
    );
    assert_eq!(fill, Some(0), "{what}: Level II code 0 is below threshold");
    assert_eq!(folded, Some(1), "{what}: Level II code 1 is range folded");
    // Row r is ray r of the sweep, every row is present, and storage is
    // exactly rows x gates.
    assert!(field.absent_rows.is_empty(), "{what}: absent rows");
    assert_eq!(field.data.len(), rows * gates, "{what}: storage length");

    let sums: Vec<u64> = array(&expected["raw_row_sums"])
        .iter()
        .map(|v| v.as_u64().expect("row sum"))
        .collect();
    assert_eq!(raw_row_sums(field), sums, "{what}: raw row sums");

    let mut no_data = 0usize;
    for row in 0..rows {
        for gate in 0..gates {
            if field.value(row, gate).is_none() {
                no_data += 1;
            }
        }
    }
    let padding: usize = array(&expected["gates_per_ray"])
        .iter()
        .map(|count| gates - as_usize(count))
        .sum();
    assert_eq!(
        no_data,
        as_usize(&expected["code_0_count"]) + as_usize(&expected["code_1_count"]) + padding,
        "{what}: no-data cells are exactly the code 0 and code 1 cells plus the padding of short rays"
    );
    assert!(field.value(0, gates).is_none());
    assert!(field.value(rows, 0).is_none());

    for probe in array(&expected["probes"]) {
        let row = as_usize(&probe["ray"]);
        let gate = as_usize(&probe["gate"]);
        let scaled = field.value(row, gate);
        match as_opt_f64(&probe["scaled"]) {
            Some(value) => assert_close(
                f64::from(scaled.unwrap_or(f32::NAN)),
                value,
                1e-4,
                &format!("{what}: ray {row} gate {gate}"),
            ),
            None => assert_eq!(scaled, None, "{what}: ray {row} gate {gate} has no data"),
        }
        if let Some(raw) = probe["raw"].as_u64() {
            assert_eq!(
                u64::from(raw_code(field, row, gate)),
                raw,
                "{what}: raw code at ray {row} gate {gate}"
            );
        }
    }
}

/// Hurricane Irene SMART-R2 sweeps (CfRadial with per-ray `prt`,
/// `unambiguous_range` and `n_samples`): the decoded `(time)` variables have
/// one entry per ray and carry netCDF4's values; a vector one entry short or
/// long breaks the sweep invariant `Sweep::seal` checks; Level II Message 31
/// has no PRT or sample-count variables.
#[test]
fn ray_instrument_variables_are_optional_but_must_align() {
    let expected = golden();
    let irene = &expected["irene"];
    let path =
        recast_radar_testdata::require_file!("cfrad1-irene-sr2-20110827-120420-sur-sweeps01");
    let volume = cfradial(&path);
    assert_eq!(volume.sweeps.len(), array(&irene["sweeps"]).len());
    for (index, (sweep, want)) in volume
        .sweeps
        .iter()
        .zip(array(&irene["sweeps"]))
        .enumerate()
    {
        let rays = sweep.nrays();
        assert_eq!(rays, as_usize(&want["rays"]), "sweep {index} rays");
        let vars = &sweep.ray_vars;
        let prt_s = vars.prt_s.as_ref().expect("CfRadial prt");
        let range_m = vars
            .unambiguous_range_m
            .as_ref()
            .expect("unambiguous range");
        let samples = vars.n_samples.as_ref().expect("n_samples");
        assert_eq!(prt_s.len(), rays);
        assert_eq!(range_m.len(), rays);
        assert_eq!(samples.len(), rays);
        let prt: Vec<f64> = array(&want["prt_s_unique"]).iter().map(as_f64).collect();
        let range_km: Vec<f64> = array(&want["unambiguous_range_km_unique"])
            .iter()
            .map(as_f64)
            .collect();
        let sample_counts: Vec<i64> = array(&want["n_samples_unique"])
            .iter()
            .map(|v| v.as_i64().expect("n_samples"))
            .collect();
        for ray in 0..rays {
            let value = f64::from(prt_s[ray]);
            assert!(
                prt.iter().any(|v| (v - value).abs() < 1e-12),
                "sweep {index} ray {ray}: prt {value} not in {prt:?}"
            );
            let km = f64::from(range_m[ray]) / 1000.0;
            assert!(
                range_km.iter().any(|v| (v - km).abs() < 1e-4),
                "sweep {index} ray {ray}: range {km} km not in {range_km:?}"
            );
            assert!(
                sample_counts.contains(&i64::from(samples[ray])),
                "sweep {index} ray {ray}: n_samples {} not in {sample_counts:?}",
                samples[ray]
            );
        }
        // The file has no CfRadial `independent_samples` variable.
        assert!(!irene["has_independent_samples"].as_bool().unwrap());
        assert_eq!(vars.independent_samples, None);
    }

    // Misalign the real prt vector of sweep 1 by one entry each way.
    let mut sweep = volume.sweeps[0].clone();
    let nrays = sweep.nrays();
    let prt_s = sweep.ray_vars.prt_s.as_mut().expect("prt");
    let first = prt_s[0];
    prt_s.pop();
    assert_eq!(
        sweep.seal(),
        Err(SweepError::RayLength {
            what: "prt".to_owned(),
            len: nrays - 1,
            nrays,
        })
    );
    let prt_s = sweep.ray_vars.prt_s.as_mut().expect("prt");
    prt_s.push(first);
    prt_s.push(first);
    assert_eq!(
        sweep.seal(),
        Err(SweepError::RayLength {
            what: "prt".to_owned(),
            len: nrays + 1,
            nrays,
        })
    );
    // Absent means the source supplied none.
    sweep.ray_vars.prt_s = None;
    assert_eq!(sweep.seal(), Ok(()));

    // Archive II Message 31 carries the Nyquist velocity and unambiguous range
    // (RAD block); the sample count comes from the Message 5 cut (15 pulses on
    // the surveillance cut, 64 on the Doppler cut). This file has no Message
    // 32 PRF table, so no PRT, and no independent samples.
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let level2 = level2(&path);
    for (sweep, pulses) in level2.sweeps.iter().zip([15, 64]) {
        let vars = &sweep.ray_vars;
        assert_eq!(vars.prt_s, None);
        assert_eq!(vars.n_samples, Some(vec![pulses; sweep.nrays()]));
        assert_eq!(vars.independent_samples, None);
        let nyquist = vars.nyquist_velocity_mps.as_ref().expect("RAD Nyquist");
        assert_eq!(nyquist.len(), sweep.nrays());
    }
}

/// KTLX 2024-03-15 surveillance sweep: the 8-bit DBZH field (1832 gates x 480
/// rays, scale 2, offset 66) scales every raw code as Py-ART reads it, with
/// codes 0 and 1 reported as no data.
#[test]
fn decoded_u8_reflectivity_field_scales_real_codes() {
    let expected = golden();
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let sweep = &volume.sweeps[0];
    let dbzh = field(sweep, &FieldName::Dbzh);
    assert!(matches!(dbzh.data, FieldData::U8 { .. }));
    assert_field_matches_raw_codes(
        dbzh,
        &sweep.range,
        &expected["grids"]["ktlx_2024_ref_sweep0"],
        "KTLX 2024 DBZH",
    );
    assert!(
        expected["grids"]["ktlx_2024_ref_sweep0"]["all_rays_same_gate_count"]
            .as_bool()
            .unwrap()
    );
}

/// KDMX 2008-05-25 sweep 11 (5.0 deg): the VEL data blocks shrink from 836
/// to 816 gates along the sweep, so the field keeps the longest row's gate
/// count and pads the short rows with the no-data code (Py-ART: per-radial
/// `ngates`).
#[test]
fn decoded_field_pads_rows_with_fewer_gates() {
    let expected = golden();
    let expected = &expected["grids"]["kdmx_2008_vel_sweep10"];
    let path = recast_radar_testdata::require_file!("l2-kdmx-20080525-205148");
    let volume = level2(&path);
    let sweep = &volume.sweeps[as_usize(&expected["sweep_index"])];
    assert_close(
        f64::from(sweep.rays.elevation_deg[0]),
        as_f64(&expected["elevation_deg"]),
        1e-6,
        "first ray elevation",
    );
    let vradh = field(sweep, &FieldName::Vradh);
    assert_field_matches_raw_codes(vradh, &sweep.range, expected, "KDMX VRADH sweep 11");

    let counts: Vec<usize> = array(&expected["gates_per_ray"])
        .iter()
        .map(as_usize)
        .collect();
    let longest = *counts.iter().max().unwrap();
    assert_eq!(vradh.shape().1, longest);
    assert_eq!(counts[0], longest, "the longest radial comes first");
    assert_eq!(
        counts.iter().filter(|&&c| c < longest).count(),
        as_usize(&expected["short_ray_count"])
    );
    assert_eq!(
        *counts.iter().min().unwrap(),
        as_usize(&expected["shortest_ray_gates"])
    );
    for (row, &count) in counts.iter().enumerate() {
        for gate in count..longest {
            assert_eq!(
                raw_code(vradh, row, gate),
                0,
                "ray {row} gate {gate} is padding"
            );
            assert_eq!(vradh.value(row, gate), None);
        }
    }
}

/// KTLX 2024-03-15 Doppler sweep: the 8-bit VRADH field (1192 gates, scale 2,
/// offset 129) with 342 range-folded (code 1) gates.
#[test]
fn decoded_u8_velocity_field_reports_range_folded_gates() {
    let expected = golden();
    let expected = &expected["grids"]["ktlx_2024_vel_sweep1"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let sweep = &volume.sweeps[1];
    let vradh = field(sweep, &FieldName::Vradh);
    assert_field_matches_raw_codes(vradh, &sweep.range, expected, "KTLX 2024 VRADH");
    assert!(as_usize(&expected["code_1_count"]) > 0);
    let (rows, gates) = vradh.shape();
    let folded = (0..rows)
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| raw_code(vradh, row, gate) == 1)
        .count();
    assert_eq!(folded, as_usize(&expected["code_1_count"]));
    let folded_gates = (0..rows)
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| vradh.gate(row, gate) == Some(recast_radar_core::Gate::RangeFolded))
        .count();
    assert_eq!(folded_gates, folded);
}

/// KTLX 2013-05-20 surveillance sweep: PHIDP is a 16-bit moment (scale
/// 2.8361, offset 2), decoded from big-endian words into u16 storage.
#[test]
fn decoded_u16_differential_phase_field_matches_big_endian_codes() {
    let expected = golden();
    let expected = &expected["grids"]["ktlx_2013_phi_sweep0"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-20130520-201643-trim");
    let volume = level2(&path);
    let sweep = &volume.sweeps[0];
    let phidp = field(sweep, &FieldName::Phidp);
    assert!(matches!(phidp.data, FieldData::U16 { .. }));
    assert_field_matches_raw_codes(phidp, &sweep.range, expected, "KTLX 2013 PHIDP");
    // ZDR and RHO of the same rays are 8-bit in this Build 13.2 file.
    for name in [FieldName::Zdr, FieldName::Rhohv] {
        let other = field(sweep, &name);
        assert!(matches!(other.data, FieldData::U8 { .. }), "{name}");
    }
}

/// The fields a sweep carries are the data blocks MetPy lists on its radials,
/// under their FM301 names: the surveillance sweep (REF, ZDR, PHI, RHO and,
/// from Build 19, CFP) and the Doppler sweep (REF, VEL, SW) of the KTLX 2024
/// and 2013 split cuts.
#[test]
fn sweep_tracks_available_fields() {
    let expected = golden();
    for id in [
        "l2-ktlx-20240315-000217-trim",
        "l2-ktlx-20130520-201643-trim",
    ] {
        let layout = &expected["level2_layouts"][id];
        let path = recast_radar_testdata::require_file!(id);
        let volume = level2(&path);
        assert_eq!(volume.attrs.instrument_name, as_str(&layout["icao"]));
        let sweeps = array(&layout["sweeps"]);
        assert_eq!(volume.sweeps.len(), sweeps.len(), "{id}: sweeps");
        for (index, (sweep, want)) in volume.sweeps.iter().zip(sweeps).enumerate() {
            let names: std::collections::BTreeSet<String> = array(&want["moments"])
                .iter()
                .map(|m| nexrad_field(as_str(m)).to_string())
                .collect();
            assert_eq!(field_names(sweep), names, "{id}: sweep {index} fields");
            assert_eq!(sweep.nrays(), as_usize(&want["rays"]));
            assert_close(
                f64::from(sweep.rays.elevation_deg[0]),
                as_f64(&want["elevation_deg"]),
                1e-6,
                &format!("{id}: sweep {index} first ray elevation"),
            );
            assert_close(
                f64::from(sweep.rays.azimuth_deg[0]),
                as_f64(&want["azimuth_first"]),
                1e-4,
                &format!("{id}: sweep {index} first azimuth"),
            );
            for field in &sweep.fields {
                assert_eq!(field.nrays as usize, sweep.nrays());
                assert!(field.absent_rows.is_empty());
            }
            assert!(sweep.field(&FieldName::Kdp).is_none());
        }
    }
}

/// The JMA Osaka N5 member carries 26 sweeps in four descending ladders that
/// repeat the 0.0, 0.3, 0.7, 1.2, 1.8, 2.5 and 5.0 deg tilts: every sweep
/// stays its own sweep (never elevation-merged), sorted lowest first with the
/// scan order kept among equal elevations and numbered 1..=26 (GRIB2
/// walker: product-definition elevation of every member section).
#[test]
fn volume_can_keep_repeated_elevation_sweeps_separate() {
    let expected = golden();
    let n5 = &expected["jma"]["n5"];
    let path = recast_radar_testdata::require_file!("jma-n5-20191012-090000-rs47773");
    let volume = jma(&path);
    assert_eq!(volume.attrs.instrument_name, as_str(&n5["station_id"]));
    let scan_order: Vec<(f32, usize)> = array(&n5["sweeps_scan_order"])
        .iter()
        .map(|s| (as_f64(&s["elevation_deg"]) as f32, as_usize(&s["radials"])))
        .collect();
    assert_eq!(volume.sweeps.len(), scan_order.len());
    let mut sorted = scan_order.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable, like the reader
    let decoded: Vec<(f32, usize)> = volume
        .sweeps
        .iter()
        .map(|sweep| (sweep.fixed_angle_deg, sweep.nrays()))
        .collect();
    assert_eq!(decoded, sorted);
    let numbers: Vec<Option<u16>> = volume
        .sweeps
        .iter()
        .map(|sweep| sweep.elevation_number)
        .collect();
    assert_eq!(numbers, (1..=26).map(Some).collect::<Vec<_>>());
    let sweep_numbers: Vec<u32> = volume.sweeps.iter().map(|s| s.sweep_number).collect();
    assert_eq!(sweep_numbers, (0..26).collect::<Vec<_>>());
    // Repeated tilts: 5.0 deg appears three times, 0.3 deg four times.
    let count = |elevation: f32| {
        volume
            .sweeps
            .iter()
            .filter(|sweep| (sweep.fixed_angle_deg - elevation).abs() < 1e-6)
            .count()
    };
    assert_eq!(count(5.0), 3);
    assert_eq!(count(0.3), 4);
    assert_eq!(count(0.0), 2);
}

/// Every raw code of the KTLX 2024 DBZH and VRADH fields resolves as the ICD
/// defines it: code 0 below threshold (`Undetect`, also the fill code), code
/// 1 range folded, any other code `(code - offset) / scale`; the 8-bit decode
/// table and `to_physical` agree with `value` on every gate, and gates past
/// the native extent are not gates.
#[test]
fn level2_codes_resolve_to_icd_values_and_sentinels() {
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    for (sweep_index, name, quantity) in [
        (0, FieldName::Dbzh, Quantity::Reflectivity),
        (1, FieldName::Vradh, Quantity::RadialVelocity),
    ] {
        let sweep = &volume.sweeps[sweep_index];
        let field = field(sweep, &name);
        assert_eq!(field.quantity, quantity);
        let (scale, offset, _, _, _) = nexrad_coding(field);
        let lut = field.lut8().expect("u8 field");
        let physical = field.to_physical();
        let (rows, gates) = field.shape();
        let mut seen = [false; 256];
        for row in 0..rows {
            for gate in 0..gates {
                let code = raw_code(field, row, gate);
                seen[usize::from(code)] = true;
                let want = match code {
                    0 => Gate::Undetect,
                    1 => Gate::RangeFolded,
                    code => Gate::Value((f32::from(code) - offset) / scale),
                };
                assert_eq!(field.gate(row, gate), Some(want), "{name} [{row}, {gate}]");
                let value = field.value(row, gate).unwrap_or(f32::NAN);
                assert_eq!(lut[usize::from(code)].to_bits(), value.to_bits());
                assert_eq!(physical[row * gates + gate].to_bits(), value.to_bits());
            }
            assert_eq!(field.gate(row, gates), None);
        }
        assert!(
            seen[0] && seen[2..].iter().any(|s| *s),
            "{name}: below-threshold and valid codes"
        );
        assert_eq!(field.gate(rows, 0), None);
    }
}

/// X-SAPR's float `reflectivity_horizontal` keeps the file's `_FillValue`
/// in storage (not NaN): the fill gate at ray 3, gate 37 (netCDF4) holds it
/// verbatim and reads as missing, like every other fill gate.
#[test]
fn float_field_keeps_the_source_fill_value_verbatim() {
    let path = recast_radar_testdata::require_file!("cfrad1-xsapr-sgp-20110520-ppi-classic");
    let volume = cfradial(&path);
    let sweep = &volume.sweeps[0];
    let field = field(sweep, &FieldName::parse("reflectivity_horizontal"));
    let FieldData::F32 { values, coding } = &field.data else {
        panic!(
            "reflectivity_horizontal is float32, not {}",
            field.data.dtype()
        );
    };
    let fill = coding.fill_value.expect("_FillValue");
    assert!(!fill.is_nan());
    let (rows, gates) = field.shape();
    assert_eq!(values[3 * gates + 37].to_bits(), fill.to_bits());
    assert_eq!(field.gate(3, 37), Some(Gate::Missing));
    assert!(field.to_physical()[3 * gates + 37].is_nan());
    let fills = values
        .iter()
        .filter(|v| v.to_bits() == fill.to_bits())
        .count();
    let nans = values.iter().filter(|v| v.is_nan()).count();
    let missing = (0..rows)
        .flat_map(|row| (0..gates).map(move |gate| (row, gate)))
        .filter(|&(row, gate)| field.gate(row, gate) == Some(Gate::Missing))
        .count();
    assert!(fills > 0);
    assert_eq!(missing, fills + nans);
}

/// Every CF-packed integer field of the Irene and DOW8 CfRadial files
/// decodes as `raw * scale_factor + add_offset` on every non-sentinel gate,
/// and `to_physical` matches `value`.
#[test]
fn cf_packed_fields_scale_every_gate() {
    let mut checked = 0;
    for id in [
        "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
    ] {
        let path = recast_radar_testdata::require_file!(id);
        let volume = cfradial(&path);
        for sweep in &volume.sweeps {
            for field in &sweep.fields {
                let Some(LinearTransform::CfScaleOffset {
                    scale_factor,
                    add_offset,
                    ..
                }) = field.data.transform()
                else {
                    continue;
                };
                let raw: Vec<f64> = match &field.data {
                    FieldData::I8 { values, .. } => values.iter().map(|&v| f64::from(v)).collect(),
                    FieldData::I16 { values, .. } => values.iter().map(|&v| f64::from(v)).collect(),
                    FieldData::I32 { values, .. } => values.iter().map(|&v| f64::from(v)).collect(),
                    FieldData::U8 { values, .. } => values.iter().map(|&v| f64::from(v)).collect(),
                    FieldData::U16 { values, .. } => values.iter().map(|&v| f64::from(v)).collect(),
                    _ => continue,
                };
                let physical = field.to_physical();
                let (rows, gates) = field.shape();
                let mut valid = 0;
                for row in 0..rows {
                    for gate in 0..gates {
                        let index = row * gates + gate;
                        let value = field.value(row, gate);
                        assert_eq!(
                            physical[index].to_bits(),
                            value.unwrap_or(f32::NAN).to_bits(),
                            "{id} {} [{row}, {gate}]",
                            field.name
                        );
                        if let Some(value) = value {
                            let want = raw[index] * scale_factor + add_offset;
                            assert_close(
                                f64::from(value),
                                want,
                                1e-5 * want.abs().max(1.0),
                                "packed value",
                            );
                            valid += 1;
                        }
                    }
                }
                assert!(valid > 0, "{id} {} has valid gates", field.name);
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "the CfRadial files have packed integer fields");
}

/// Moving a decoded field's values out (`into_parts`, `into_array`) keeps
/// the allocation: no copy on the way to array consumers.
#[test]
fn into_array_moves_the_decoded_buffer() {
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let mut volume = level2(&path);
    let field = volume.sweeps[0].fields.remove(0);
    let FieldData::U8 { values, .. } = &field.data else {
        panic!("Level II DBZH is u8");
    };
    let pointer = values.as_ptr();
    let parts = field.into_parts();
    let (array, coding) = parts.data.into_array();
    let ArrayBuf::U8(values) = array else {
        panic!("Level II DBZH is u8");
    };
    assert_eq!(values.as_ptr(), pointer);
    assert!(matches!(coding, Coding::U8(_)));
}

/// KTLX 1999-05-04 sweep 5 (Message 1): DBZH is 356 gates of 1 km from
/// centre 0 and VRADH 920 gates of 250 m from centre -375 (Py-ART message
/// headers). The sweep range is the 250 m grid from -375 m, long enough for
/// both (1424 gates); DBZH sits on it with stride 4 and keeps its native
/// geometry.
#[test]
fn message1_reflectivity_maps_onto_the_doppler_range() {
    let expected = golden();
    let want = &expected["ktlx_1999_sweep4"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218");
    let volume = level2(&path);
    let sweep = &volume.sweeps[as_usize(&want["sweep_index"])];
    let (ref_first, ref_spacing, ref_gates) = (
        as_f64(&want["ref"]["first_gate_m"]),
        as_f64(&want["ref"]["gate_spacing_m"]),
        as_usize(&want["ref"]["gates"]),
    );
    let (vel_first, vel_spacing, vel_gates) = (
        as_f64(&want["vel"]["first_gate_m"]),
        as_f64(&want["vel"]["gate_spacing_m"]),
        as_usize(&want["vel"]["gates"]),
    );
    let stride = (ref_spacing / vel_spacing) as usize;
    // Both first gates start at the same edge (-500 m).
    assert_eq!(ref_first - ref_spacing / 2.0, vel_first - vel_spacing / 2.0);
    assert_eq!(
        sweep.range,
        RangeCoord::Uniform {
            first_center_m: vel_first,
            spacing_m: vel_spacing,
            ngates: (ref_gates * stride).max(vel_gates) as u32,
        }
    );
    let dbzh = field(sweep, &FieldName::Dbzh);
    assert_eq!(dbzh.ngates as usize, ref_gates);
    assert_eq!(
        dbzh.gates,
        GateMapping {
            start: 0,
            stride: stride as u32,
        }
    );
    assert_eq!(
        dbzh.native_geometry(&sweep.range),
        Some((ref_first, ref_spacing))
    );
    for name in [FieldName::Vradh, FieldName::Wradh] {
        let doppler = field(sweep, &name);
        assert_eq!(doppler.ngates as usize, vel_gates);
        assert_eq!(doppler.gates, GateMapping::IDENTITY);
        assert_eq!(
            doppler.native_geometry(&sweep.range),
            Some((vel_first, vel_spacing))
        );
    }
    // The Message 1 Nyquist velocity (halfword 31; Py-ART 26.1 m/s on the
    // first radial) is on every ray.
    let nyquist = sweep
        .ray_vars
        .nyquist_velocity_mps
        .as_ref()
        .expect("Message 1 Nyquist velocity");
    assert_eq!(nyquist.len(), sweep.nrays());
    assert_close(
        f64::from(nyquist[0]),
        as_f64(&want["nyquist_mps"]),
        1e-4,
        "Nyquist velocity",
    );
    assert!(nyquist.iter().all(|v| *v > 0.0));
}

/// The time reference is the first radial's collection time floored to the
/// second (MetPy: 00:02:17.182 and 20:16:43.850), and ray times carry the
/// fraction.
#[test]
fn time_reference_is_the_first_radial_floored_to_the_second() {
    let expected = golden();
    for id in [
        "l2-ktlx-20240315-000217-trim",
        "l2-ktlx-20130520-201643-trim",
    ] {
        let layout = &expected["level2_layouts"][id];
        let path = recast_radar_testdata::require_file!(id);
        let volume = level2(&path);
        let first = time(as_str(&layout["first_radial_time"]));
        assert_eq!(
            volume.time_reference,
            time(as_str(&layout["time_reference"]))
        );
        assert_eq!(volume.time_reference.timestamp_subsec_nanos(), 0);
        assert!(first > volume.time_reference);
        let sweep = &volume.sweeps[0];
        let instant = volume.instant(sweep.rays.time_s[0]).expect("instant");
        assert_eq!(instant.timestamp_millis(), first.timestamp_millis(), "{id}");
    }
}

/// `Sweep::seal` on edits of the real KTLX 2024 Doppler sweep: a field cut
/// to its first row is padded with absent rows up to the ray count; a second
/// field with an existing name and a per-ray variable of the wrong length
/// are errors.
#[test]
fn seal_pads_trailing_absent_rows_and_checks_invariants() {
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = level2(&path);
    let sweep = &volume.sweeps[1];
    let nrays = sweep.nrays();

    let mut edited = sweep.clone();
    let index = edited.field_index(&FieldName::Vradh).expect("VRADH");
    let original = edited.fields[index].clone();
    let gates = original.ngates as usize;
    {
        let vradh = &mut edited.fields[index];
        let FieldData::U8 { values, .. } = &mut vradh.data else {
            panic!("VRADH is u8");
        };
        values.truncate(gates);
        vradh.nrays = 1;
    }
    assert_eq!(edited.seal(), Ok(()));
    let vradh = &edited.fields[index];
    assert_eq!(vradh.nrays as usize, nrays);
    assert_eq!(vradh.absent_rows, (1..nrays as u32).collect::<Vec<_>>());
    assert_eq!(vradh.data.len(), nrays * gates);
    for gate in 0..gates {
        assert_eq!(vradh.gate(0, gate), original.gate(0, gate));
        assert_eq!(vradh.gate(1, gate), Some(Gate::Missing));
        assert_eq!(raw_code(vradh, nrays - 1, gate), 0, "fill code");
    }

    let mut duplicated = sweep.clone();
    duplicated.fields.push(original.clone());
    assert_eq!(
        duplicated.seal(),
        Err(SweepError::DuplicateName {
            name: "VRADH".to_owned()
        })
    );

    let mut short = sweep.clone();
    short
        .ray_vars
        .nyquist_velocity_mps
        .as_mut()
        .expect("Nyquist")
        .truncate(1);
    assert_eq!(
        short.seal(),
        Err(SweepError::RayLength {
            what: "nyquist_velocity".to_owned(),
            len: 1,
            nrays,
        })
    );
}

/// `Sweep::find` prefers horizontal, then unspecified, then vertical, on
/// real gates: IESHA's DBZH sweep, its decoded field re-spelled DBZ and DBZV
/// so the same real rows sit under each of the three reflectivity names. No
/// corpus file carries all three at once, and a decoder's `standard_name`
/// would decide the classification before the name does, so the re-spelling
/// runs the name through `Quantity::classify` the way a decoder would.
#[test]
fn find_prefers_horizontal_then_unspecified_then_vertical() {
    let volume = common::odim(&recast_radar_testdata::require_file!(
        "odim-iesha-20260305-0115-pvol"
    ));
    let source = volume.sweeps[0]
        .field(&FieldName::Dbzh)
        .expect("IESHA sweep 0 DBZH")
        .clone();
    assert_eq!(source.polarization, Polarization::H);
    assert_eq!(source.quantity, Quantity::Reflectivity);

    // The same real rows under another name, classified from the name alone.
    let respelled = |name: FieldName| {
        let mut field = source.clone();
        let (quantity, polarization) = Quantity::classify(name.as_str(), None);
        field.name = name;
        field.quantity = quantity;
        field.polarization = polarization;
        field.attrs = Default::default();
        field
    };
    assert_eq!(
        respelled(FieldName::Dbz).polarization,
        Polarization::Unspecified
    );
    assert_eq!(respelled(FieldName::Dbzv).polarization, Polarization::V);

    let mut sweep = volume.sweeps[0].clone();
    sweep.fields.clear();
    sweep.fields.push(respelled(FieldName::Dbzv));
    assert_eq!(
        sweep.find(Quantity::Reflectivity).map(|f| &f.name),
        Some(&FieldName::Dbzv),
        "vertical alone"
    );
    sweep.fields.push(respelled(FieldName::Dbz));
    assert_eq!(
        sweep.find(Quantity::Reflectivity).map(|f| &f.name),
        Some(&FieldName::Dbz),
        "unspecified beats vertical"
    );
    sweep.fields.push(respelled(FieldName::Dbzh));
    assert_eq!(
        sweep.find(Quantity::Reflectivity).map(|f| &f.name),
        Some(&FieldName::Dbzh),
        "horizontal beats both"
    );
    assert_eq!(sweep.seal(), Ok(()));
    // The winner is the real field, gates and all.
    let found = sweep.find(Quantity::Reflectivity).expect("reflectivity");
    assert_eq!(found.data, source.data);
    assert_eq!((found.nrays, found.ngates), (source.nrays, source.ngates));
}

/// `to_physical` decodes per code (8-bit and large 16-bit fields through a
/// decode table) and must equal `value` on every gate of every field: the
/// full KTLX 2024 volume (u8 and large u16 fields, absent rows), KPAH 2008
/// (legacy resolution), dkrom (ODIM u8/u16 with CF gain and offset) and the
/// JMA TAKA station (float32).
#[test]
fn to_physical_equals_value_on_every_gate() {
    let mut volumes = Vec::new();
    for id in ["l2-ktlx-20240315-000217", "l2-kpah-20080415-235014"] {
        let path = recast_radar_testdata::require_file!(id);
        volumes.push((id, level2(&path)));
    }
    let path = recast_radar_testdata::require_file!("odim-dkrom-20260820-1130-pvol");
    volumes.push(("odim-dkrom-20260820-1130-pvol", odim(&path)));
    // The JMA file is not redistributed: checked only when cached.
    let jma_path = recast_radar_testdata::path_if_available("jma-n5-20191012-090000-rs47773");
    if let Some(path) = &jma_path {
        volumes.push(("jma-n5-20191012-090000-rs47773", jma(path)));
    }
    // Absent rows: KTLX 2024 sweep 0 with every field cut to its first half
    // of rows, which `seal` pads back as trailing absent rows.
    let mut edited = volumes[0].1.clone();
    edited.sweeps.truncate(1);
    let sweep = &mut edited.sweeps[0];
    for field in &mut sweep.fields {
        let keep = field.nrays as usize / 2;
        let len = keep * field.ngates as usize;
        match &mut field.data {
            FieldData::U8 { values, .. } => values.truncate(len),
            FieldData::U16 { values, .. } => values.truncate(len),
            other => panic!("Level II field {}", other.dtype()),
        }
        field.nrays = keep as u32;
    }
    assert_eq!(sweep.seal(), Ok(()));
    volumes.push(("l2-ktlx-20240315-000217 sweep 0 half rows", edited));

    let mut kinds = std::collections::BTreeSet::new();
    let mut large_u16 = 0;
    let mut absent = 0;
    for (id, volume) in &volumes {
        for (sweep_index, sweep) in volume.sweeps.iter().enumerate() {
            for field in &sweep.fields {
                kinds.insert(field.data.dtype());
                if matches!(&field.data, FieldData::U16 { values, .. } if values.len() >= 1 << 17) {
                    large_u16 += 1;
                }
                absent += field.absent_rows.len();
                let physical = field.to_physical();
                let (rows, gates) = field.shape();
                assert_eq!(
                    physical.len(),
                    rows * gates,
                    "{id} sweep {sweep_index} {}",
                    field.name
                );
                for row in 0..rows {
                    for gate in 0..gates {
                        let want = field.value(row, gate).unwrap_or(f32::NAN);
                        assert_eq!(
                            physical[row * gates + gate].to_bits(),
                            want.to_bits(),
                            "{id} sweep {sweep_index} {} [{row}, {gate}]",
                            field.name
                        );
                    }
                }
            }
        }
    }
    assert!(kinds.contains("uint8") && kinds.contains("uint16"));
    assert!(kinds.contains("float32") || jma_path.is_none());
    assert!(
        large_u16 > 0,
        "a u16 field large enough for the 16-bit table"
    );
    assert!(absent > 0, "absent rows");
}
