//! Level III volumes ([`read_level3_volume`]) against Py-ART 2.3.0's
//! `read_nexrad_level3` (`tools/level3_pyart_golden.py`,
//! `testdata/level3/golden-pyart.json`), for the 112 corpus files Py-ART
//! reads. Py-ART converts the first data packet only; it is the volume's
//! first sweep.
//!
//! Equal: dimensions, every radial start azimuth (the SHA-256 of Py-ART's
//! `float32` azimuths), radar location (altitude 0.3048 m per foot), volume
//! time, and every physical value of every product except those listed in
//! [`VALUES_DIFFER`] (the SHA-256 of Py-ART's `float32` field with masked
//! gates as NaN, besides its count, minimum, maximum and mean). Documented
//! differences
//! (the conventions of `recast_radar_io_level3::volume`):
//!
//! - azimuth: Py-ART reports the start angle, the volume the radial centre
//!   (the start angle is the per-ray `level3_start_angle`);
//! - range: Py-ART starts at 0 and steps by the packet's display scale
//!   factor (999 for a 1 km bin, 249.75 for 250 m), the volume places bin
//!   centres by the ICD bin size (generic components agree: both use the
//!   component's range to the first bin and bin size);
//! - elevation: Py-ART gives 0 where the product has no elevation and for
//!   the TDWR products, the volume NaN and halfword 30 / 10;
//! - ray time: Py-ART 0 for every ray, the volume the elevation delay of
//!   halfword 50.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use chrono::{DateTime, Utc};
use common::{Json, PhysicalSummary};
use recast_radar_core::model::{ArrayBuf, RangeCoord};
use recast_radar_io_level3::read_level3_volume;

/// Products whose Py-ART values differ from the volume's, with the reason
/// asserted in the test:
///
/// - 155: Py-ART maps levels from 2 like reflectivity (63.5-78.5 m/s); the
///   ICD's levels 129-152 give 0-11.5 m/s (the volume and MetPy);
/// - 170, 172-175: Py-ART scales the `0.01 in` levels to inches (the volume
///   and MetPy keep hundredths, the ICD's unit); 174 and 175 also give every
///   gate a value, where the ICD's level 0 is "no data";
/// - 32: Py-ART's values are one increment (1 dBZ) above the ICD's level `N`
///   = `hw31/10 + (N - 2) * hw32/10` (the volume and MetPy);
/// - 176: Py-ART masks a rate of 0 in/h; the ICD's 16-bit levels have no flag
///   value, so the volume (and MetPy) give 0.
const VALUES_DIFFER: [i16; 8] = [32, 155, 170, 172, 173, 174, 175, 176];

fn theirs_sha(file: &Json) -> String {
    file.get("values")
        .get("sha256")
        .as_str()
        .unwrap()
        .to_owned()
}

fn num(json: &Json) -> f64 {
    json.as_f64().unwrap_or_else(|| panic!("number: {json:?}"))
}

#[test]
fn volumes_match_pyart() {
    let path = common::testdata_dir().join("level3/golden-pyart.json");
    let golden = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(golden.get("pyart").as_str(), Some("2.3.0"));
    let mut compared = 0;
    let mut values_equal = 0;
    let mut arrays_equal = 0;
    let mut azimuths_equal = 0;
    let mut failures = Vec::new();
    for file in golden.get("files").items() {
        if !file.get("error").is_null() {
            continue;
        }
        let id = file.get("id").as_str().unwrap();
        let code = i16::try_from(file.get("product").int("product")).unwrap();
        let volume = read_level3_volume(&common::entry(id).bytes()).unwrap();
        let sweep = &volume.sweeps[0];
        let field = &sweep.fields[0];
        let mut problems = Vec::new();

        let (nrays, ngates) = (
            usize::try_from(file.get("nrays").int("nrays")).unwrap(),
            usize::try_from(file.get("ngates").int("ngates")).unwrap(),
        );
        // Py-ART keeps the pad byte of radials with an odd bin count as one
        // more gate (N1Q, N2Q, NBQ, N3U of KTLX 2013).
        let (rows, cols) = field.shape();
        let padded = cols % 2 == 1 && ngates == cols + 1;
        if rows != nrays || (cols != ngates && !padded) {
            problems.push(format!(
                "shape {:?} vs Py-ART {nrays} x {ngates}",
                field.shape()
            ));
        }
        let ngates = cols;

        // Start azimuths: the raw start angle, or centre minus half the width.
        let starts: Vec<f32> = match sweep
            .extra_vars
            .iter()
            .find(|v| &*v.name == "level3_start_angle")
        {
            Some(v) => match &v.values {
                ArrayBuf::F32(s) => s.clone(),
                other => panic!("{other:?}"),
            },
            None => {
                let width = sweep
                    .extra_vars
                    .iter()
                    .find(|v| &*v.name == "level3_width")
                    .unwrap();
                let ArrayBuf::F32(width) = &width.values else {
                    panic!("width");
                };
                sweep
                    .rays
                    .azimuth_deg
                    .iter()
                    .zip(width)
                    .map(|(a, w)| (a - 0.5 * w).rem_euclid(360.0))
                    .collect()
            }
        };
        // Every start azimuth: Py-ART's float32 array, little endian.
        let azimuth_bytes: Vec<u8> = starts.iter().flat_map(|a| a.to_le_bytes()).collect();
        if common::sha256_hex(&azimuth_bytes) == file.get("azimuth_sha256").as_str().unwrap() {
            azimuths_equal += 1;
        } else {
            problems.push("start azimuths differ from Py-ART's (SHA-256)".into());
        }
        let expected: Vec<f64> = file.get("azimuth").items().iter().map(num).collect();
        let ours: Vec<f64> = starts[..4]
            .iter()
            .chain(starts.last())
            .map(|&a| f64::from(a))
            .collect();
        if ours
            .iter()
            .zip(&expected)
            .any(|(a, b)| (a - b).abs() > 1e-3)
        {
            problems.push(format!("start azimuths {ours:?} vs Py-ART {expected:?}"));
        }

        // Range: Py-ART's convention for radial packets, the same geometry
        // for generic components.
        let range: Vec<f64> = file.get("range").items().iter().map(num).collect();
        let RangeCoord::Uniform {
            first_center_m,
            spacing_m,
            ..
        } = sweep.range
        else {
            panic!("uniform range");
        };
        if code == 176 {
            if (range[0] - first_center_m).abs() > 1e-6
                || (range[1] - range[0] - spacing_m).abs() > 1e-6
            {
                problems.push(format!("generic range {range:?}"));
            }
        } else if range[0] != 0.0 || (first_center_m - 0.5 * spacing_m).abs() > 1e-9 {
            problems.push(format!("range {range:?} vs first centre {first_center_m}"));
        }

        // Location and time.
        for (ours, key, tol) in [
            (volume.location.latitude_deg, "latitude", 1e-9),
            (volume.location.longitude_deg, "longitude", 1e-9),
            (volume.location.altitude_m, "altitude", 1e-6),
        ] {
            if ours.is_none_or(|v| (v - num(file.get(key))).abs() > tol) {
                problems.push(format!("{key} {ours:?} vs {:?}", file.get(key)));
            }
        }
        let units = file.get("time_units").as_str().unwrap();
        let reference: DateTime<Utc> = units
            .strip_prefix("seconds since ")
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .unwrap()
            .with_timezone(&Utc);
        if reference != volume.time_reference {
            problems.push(format!("time {reference} vs {}", volume.time_reference));
        }

        // Elevation.
        let elevation = num(file.get("elevation"));
        let ours = f64::from(sweep.fixed_angle_deg);
        let tdwr = (180..=187).contains(&code);
        if elevation != 0.0 && (ours - elevation).abs() > 1e-5 {
            problems.push(format!("elevation {ours} vs Py-ART {elevation}"));
        }
        if elevation == 0.0 && !(ours.is_nan() || ours == 0.0 || tdwr) {
            problems.push(format!("elevation {ours} where Py-ART has none"));
        }

        // Physical values.
        let values: Vec<f32> = (0..nrays)
            .flat_map(|ray| (0..ngates).map(move |gate| (ray, gate)))
            .map(|(ray, gate)| field.value(ray, gate).unwrap_or(f32::NAN))
            .collect();
        let summary = PhysicalSummary::of_f32(&values);
        // Every value: Py-ART's float32 array with masked gates as NaN (and
        // its pad gate of odd-length radials), C order, little endian.
        let mut bytes = Vec::with_capacity(4 * nrays * (ngates + 1));
        for ray in 0..nrays {
            for v in &values[ray * ngates..(ray + 1) * ngates] {
                let v = if v.is_nan() { f32::NAN } else { *v };
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            if padded {
                bytes.extend_from_slice(&f32::NAN.to_le_bytes());
            }
        }
        let arrays_match = common::sha256_hex(&bytes) == theirs_sha(file);
        let theirs = file.get("values");
        let count = usize::try_from(theirs.get("count").int("count")).unwrap();
        if VALUES_DIFFER.contains(&code) {
            // The documented difference, asserted.
            let max = num(theirs.get("max"));
            let ok = match code {
                155 => max > 60.0 && summary.max.is_some_and(|m| m < 20.0),
                32 => {
                    count as u64 == summary.finite
                        && summary.max.is_some_and(|m| (m + 1.0 - max).abs() < 1e-4)
                }
                176 => {
                    let nonzero = values
                        .iter()
                        .filter(|v| v.is_finite() && **v != 0.0)
                        .count();
                    count == nonzero && summary.max.is_some_and(|m| (m - max).abs() < 1e-4)
                }
                170 | 172 | 173 => {
                    count as u64 == summary.finite
                        && summary
                            .max
                            .is_some_and(|m| (m / 100.0 - max).abs() <= 1e-4 * max.abs().max(1.0))
                }
                174 | 175 => {
                    count == nrays * ngates
                        && summary
                            .max
                            .is_some_and(|m| (m / 100.0 - max).abs() <= 1e-4 * max.abs().max(1.0))
                }
                _ => false,
            };
            if !ok {
                problems.push(format!(
                    "documented difference not found: {summary:?} vs {theirs:?}"
                ));
            }
        } else {
            let expected = PhysicalSummary {
                finite: count as u64,
                masked: (nrays * ngates - count) as u64,
                min: theirs.get("min").as_f64(),
                max: theirs.get("max").as_f64(),
                mean: theirs.get("mean").as_f64(),
            };
            for mismatch in summary.mismatches(&expected) {
                problems.push(format!("values vs Py-ART: {mismatch}"));
            }
            if arrays_match {
                arrays_equal += 1;
            } else {
                problems.push("values differ from Py-ART's array (SHA-256)".into());
            }
            values_equal += 1;
        }

        if !problems.is_empty() {
            failures.push(format!(
                "{id} (product {code}):\n    {}",
                problems.join("\n    ")
            ));
        }
        compared += 1;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(compared, 112);
    assert_eq!((values_equal, arrays_equal, azimuths_equal), (94, 94, 112));
    eprintln!(
        "{compared} files compared: every value equal for {arrays_equal}, every start \
         azimuth for {azimuths_equal}"
    );
}
