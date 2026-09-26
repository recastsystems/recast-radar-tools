//! Fuzz regression inputs for the volume writers (`fuzz/`, target
//! `writers`; testdata/fuzz/manifest.toml): mutations of real seeds
//! (libFuzzer's, or the stable `fuzz-tools mutate`) whose volumes a writer
//! turned into a file its own readers refused or read back differently.
//!
//! - `fuzz-writers-l2-sweep-without-gates`: a cut of one radial without
//!   moments (a sweep of rays and no gates); the FM301 writer wrote it and
//!   the CfRadial 2 reader refused a sweep without gates.
//! - `fuzz-writers-l2-one-gate-sweep`: a volume of one-gate radials; the
//!   CfRadial 1 writer wrote a one-gate `range`, which its reader refuses (it
//!   needs two centres to state the spacing).
//! - `fuzz-writers-l2-odim-rstart-beyond-20-km`: a radial whose first gate
//!   is centred 27,649 m out; the ODIM writer wrote `rstart` in km and the
//!   ODIM reader took a km value over 20 as metres.
//! - `fuzz-writers-dorade-ray-without-time`: one ray of a COW2 sweep without
//!   a time; the ODIM writer dropped `startazT`/`stopazT` for the whole
//!   dataset, and every other ray read back at the whole-second start.
//! - `fuzz-writers-l2-empty-field-name`: a moment named U+0001; the
//!   CfRadial writers call its variable `_`.
//! - `fuzz-writers-odim-gate-spacing-below-float`: a sweep with 1.4e-100 m
//!   gates; the CfRadial 1 per-ray layout wrote a float spacing of 0, which
//!   readers take as fill.
//! - `fuzz-writers-dorade-absent-rows-without-fill`: a field without a fill
//!   code and with an absent ray; the CfRadial 2 file had no `_FillValue`,
//!   and the absent ray read back as values.
//! - `fuzz-writers-cfradial1-ray-time-near-float-max`: a ray time of about
//!   -1.8e308 s; the ODIM reader's start/stop mean overflowed to -inf.
//!
//! Every writer must now either refuse with its typed error or write a file
//! that reads back through the router with the same rays per sweep and the
//! same data (the `writers` harness), and each input's own case is checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::model::Volume;
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_io_cfradial::{
    CfWriteError, Cfradial1Options, Cfradial2Options, RangeLayout, write_cfradial1, write_cfradial2,
};
use recast_radar_io_odim::{OdimWriteOptions, write_odim_h5_volume};

#[path = "common/compare.rs"]
mod compare;

const WITHOUT_GATES: &str = "fuzz-writers-l2-sweep-without-gates";
const ONE_GATE: &str = "fuzz-writers-l2-one-gate-sweep";

fn volume(id: &str) -> Volume {
    let bytes = recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{id}: {err}"));
    read_supported_volume_bytes(&bytes).unwrap_or_else(|err| panic!("{id}: {err}"))
}

/// Rays per sweep, sorted (the CfRadial 1 writer stores sweeps in time
/// order).
fn rays(volume: &Volume) -> Vec<usize> {
    let mut rays: Vec<usize> = volume.sweeps.iter().map(|sweep| sweep.nrays()).collect();
    rays.sort_unstable();
    rays
}

/// Every writer's output for `volume`: `Ok(read back)` or the writer's
/// refusal as text. A file that does not read back fails the test.
fn outputs(id: &str, volume: &Volume) -> Vec<(&'static str, Result<Volume, String>)> {
    let cf1 = |layout| {
        write_cfradial1(
            volume,
            &Cfradial1Options::default().with_range_layout(layout),
        )
        .map_err(|err| err.to_string())
    };
    let written: Vec<(&'static str, Result<Vec<u8>, String>)> = vec![
        ("CfRadial 1", cf1(RangeLayout::Auto)),
        ("CfRadial 1 (per sweep)", cf1(RangeLayout::PerSweep)),
        ("CfRadial 1 (per ray)", cf1(RangeLayout::PerRay)),
        (
            "CfRadial 2",
            write_cfradial2(volume, &Cfradial2Options::default()).map_err(|err| err.to_string()),
        ),
        (
            "ODIM_H5",
            write_odim_h5_volume(volume, &OdimWriteOptions::default())
                .map_err(|err| err.to_string()),
        ),
        (
            "ODIM_H5 (every quantity)",
            write_odim_h5_volume(
                volume,
                &OdimWriteOptions::default().with_every_quantity(true),
            )
            .map_err(|err| err.to_string()),
        ),
    ];
    written
        .into_iter()
        .map(|(writer, result)| {
            let read = result.map(|bytes| {
                let read = read_supported_volume_bytes(&bytes).unwrap_or_else(|err| {
                    panic!("{id}: {writer} output does not read back: {err}")
                });
                assert_eq!(rays(&read), rays(volume), "{id}: {writer} rays per sweep");
                read
            });
            (writer, read)
        })
        .collect()
}

#[test]
fn a_sweep_without_gates_reads_back_from_every_writer() {
    let volume = volume(WITHOUT_GATES);
    let empty = volume
        .sweeps
        .iter()
        .position(|sweep| sweep.range.ngates() == 0 && sweep.nrays() > 0)
        .expect("a sweep of rays without gates");
    for (writer, read) in outputs(WITHOUT_GATES, &volume) {
        match (writer, read) {
            ("CfRadial 2", Ok(read)) => {
                assert_eq!(read.sweeps[empty].range.ngates(), 0, "{writer}");
                assert_eq!(read.sweeps[empty].nrays(), volume.sweeps[empty].nrays());
            }
            ("CfRadial 2", Err(err)) => panic!("{writer} refused: {err}"),
            // ODIM refuses a sweep without gates (typed); CfRadial 1 gives
            // it a row of fill.
            (_, _) => {}
        }
    }
}

#[test]
fn a_one_gate_volume_reads_back_from_every_writer() {
    let volume = volume(ONE_GATE);
    assert!(
        volume.sweeps.iter().all(|sweep| sweep.range.ngates() <= 1)
            && volume.sweeps.iter().any(|sweep| sweep.range.ngates() == 1),
        "{ONE_GATE}: one-gate sweeps"
    );
    for (writer, read) in outputs(ONE_GATE, &volume) {
        if !writer.starts_with("CfRadial 1") {
            continue;
        }
        // The file's `range` has a second gate of fill; `n_points` storage
        // keeps each ray to its own gate, so the sweep reads back as it was.
        let read = read.unwrap_or_else(|err| panic!("{writer} refused: {err}"));
        let mut gates: Vec<usize> = read.sweeps.iter().map(|s| s.range.ngates()).collect();
        let mut want: Vec<usize> = volume
            .sweeps
            .iter()
            .map(|s| s.range.ngates().max(1))
            .collect();
        gates.sort_unstable();
        want.sort_unstable();
        assert_eq!(gates, want, "{writer}: gates per sweep");
    }
}

const RSTART_BEYOND_20_KM: &str = "fuzz-writers-l2-odim-rstart-beyond-20-km";

/// The ODIM writer states `rstart` in km (ODIM_H5/V2_3); the reader takes a
/// pre-v2.4 `rstart` over 20 as metres (producers that wrote metres), so a
/// sweep starting 27.6 km out read back 27 km short. The writer names itself
/// in root `how/software`, whose files the reader takes as km.
#[test]
fn an_odim_sweep_starting_beyond_20_km_reads_back_in_place() {
    use recast_radar_io_odim::hdf5::H5File;

    let volume = volume(RSTART_BEYOND_20_KM);
    let first = volume.sweeps[0].range.center_m(0).unwrap();
    assert!((first - 27_649.0).abs() < 1.0, "{first}");
    let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default()).unwrap();
    let file = H5File::open(&written).unwrap();
    assert_eq!(
        file.attr("/how", "software").and_then(|a| a.as_str()),
        Some("recast-radar-tools".to_owned())
    );
    let rstart = file
        .attr("/dataset1/where", "rstart")
        .and_then(|a| a.as_f64())
        .unwrap();
    assert!(rstart > 20.0, "rstart {rstart} km");
    let read = read_supported_volume_bytes(&written).unwrap();
    let back = read.sweeps[0].range.center_m(0).unwrap();
    assert!(
        (back - first).abs() <= 0.51,
        "{first} m read back as {back} m"
    );
    // Every file a writer writes reads back with the same rays (`outputs`).
    let written = outputs(RSTART_BEYOND_20_KM, &volume);
    assert!(
        written.iter().all(|(_, read)| read.is_ok()),
        "a writer refused"
    );
}

const RAY_WITHOUT_TIME: &str = "fuzz-writers-dorade-ray-without-time";

/// One ray without a time (NaN) leaves the others their sub-second times in
/// ODIM_H5: `startazT`/`stopazT` hold NaN for that ray only.
#[test]
fn an_odim_sweep_keeps_ray_times_beside_a_ray_without_one() {
    let volume = volume(RAY_WITHOUT_TIME);
    let times = &volume.sweeps[0].rays.time_s;
    let unknown = times.iter().filter(|time| time.is_nan()).count();
    assert_eq!(unknown, 1, "{times:?}");
    assert!(
        times
            .iter()
            .any(|time| time.is_finite() && time.fract() != 0.0),
        "{times:?}"
    );
    let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default()).unwrap();
    let read = read_supported_volume_bytes(&written).unwrap();
    // Every ray's angles and time, and every gate (the harness check).
    compare::assert_matches(
        &volume,
        &read,
        &compare::odim_expect(RAY_WITHOUT_TIME.to_owned(), &volume),
    );
    let back = &read.sweeps[0].rays.time_s;
    assert_eq!(back.iter().filter(|time| time.is_nan()).count(), 1);
    let written = outputs(RAY_WITHOUT_TIME, &volume);
    assert!(
        written.iter().all(|(_, read)| read.is_ok()),
        "a writer refused"
    );
}

const CONTROL_CHARACTER_NAME: &str = "fuzz-writers-l2-empty-field-name";

/// A field whose name is not a netCDF name reads back from CfRadial 1 and 2
/// under the name the writers give its variable, gate for gate.
#[test]
fn a_field_named_by_a_control_character_reads_back_as_underscore() {
    let volume = volume(CONTROL_CHARACTER_NAME);
    assert!(
        volume
            .sweeps
            .iter()
            .any(|sweep| sweep.fields.iter().any(|f| f.name.as_str() == "\u{1}")),
        "a field named U+0001"
    );
    let cf1 = write_cfradial1(&volume, &Cfradial1Options::default()).unwrap();
    let read = read_supported_volume_bytes(&cf1).unwrap();
    assert!(
        read.sweeps
            .iter()
            .any(|sweep| sweep.fields.iter().any(|f| f.name.as_str() == "_"))
    );
    compare::assert_matches(
        &volume,
        &read,
        &compare::cfradial1_expect("CfRadial 1".to_owned(), RangeLayout::Auto),
    );
    let cf2 = write_cfradial2(&volume, &Cfradial2Options::default()).unwrap();
    let read = read_supported_volume_bytes(&cf2).unwrap();
    compare::assert_matches(
        &volume,
        &read,
        &compare::cfradial2_expect("CfRadial 2".to_owned()),
    );
}

const SPACING_BELOW_FLOAT: &str = "fuzz-writers-odim-gate-spacing-below-float";

/// A sweep whose gate spacing a float cannot hold is refused by the per-ray
/// layout (its `ray_gate_spacing` would be 0, which readers take as fill);
/// the other layouts state the sweep's centres and read back.
#[test]
fn a_gate_spacing_below_float_is_refused_by_the_per_ray_layout() {
    use recast_radar_core::model::RangeCoord;

    let volume = volume(SPACING_BELOW_FLOAT);
    assert!(volume.sweeps.iter().any(|sweep| matches!(
        sweep.range,
        RangeCoord::Uniform { spacing_m, .. } if spacing_m > 0.0 && (spacing_m as f32) == 0.0
    )));
    assert!(matches!(
        write_cfradial1(
            &volume,
            &Cfradial1Options::default().with_range_layout(RangeLayout::PerRay)
        ),
        Err(CfWriteError::Unrepresentable(_))
    ));
    for layout in [RangeLayout::Auto, RangeLayout::PerSweep] {
        let written = write_cfradial1(
            &volume,
            &Cfradial1Options::default().with_range_layout(layout),
        )
        .unwrap();
        let read = read_supported_volume_bytes(&written).unwrap();
        compare::assert_matches(
            &volume,
            &read,
            &compare::cfradial1_expect(format!("CfRadial 1 ({layout:?})"), layout),
        );
    }
}

const ABSENT_ROWS_WITHOUT_FILL: &str = "fuzz-writers-dorade-absent-rows-without-fill";

/// An integer field without a fill code whose rows are not all provided:
/// the CfRadial 2 writer gives it a free code as `_FillValue` (as the
/// CfRadial 1 writer does), so the absent rows read back missing.
#[test]
fn absent_rows_of_a_field_without_a_fill_code_read_back_missing() {
    use recast_radar_core::model::FieldData;

    let volume = volume(ABSENT_ROWS_WITHOUT_FILL);
    let lacking = volume.sweeps.iter().any(|sweep| {
        sweep.fields.iter().any(|field| {
            !field.absent_rows.is_empty()
                && matches!(&field.data, FieldData::I16 { coding, .. } if coding.fill_value.is_none())
        })
    });
    assert!(lacking, "a field with absent rows and no fill code");
    let written = write_cfradial2(&volume, &Cfradial2Options::default()).unwrap();
    let read = read_supported_volume_bytes(&written).unwrap();
    compare::assert_matches(
        &volume,
        &read,
        &compare::cfradial2_expect("CfRadial 2".to_owned()),
    );
    let written = outputs(ABSENT_ROWS_WITHOUT_FILL, &volume);
    assert!(
        written.iter().all(|(_, read)| read.is_ok()),
        "a writer refused"
    );
}

const TIME_NEAR_FLOAT_MAX: &str = "fuzz-writers-cfradial1-ray-time-near-float-max";

/// A ray time near the double range: the ODIM reader's start/stop mean
/// overflowed to -inf; it is finite for any two finite values now.
#[test]
fn an_odim_ray_time_near_the_double_range_reads_back() {
    let volume = volume(TIME_NEAR_FLOAT_MAX);
    assert!(
        volume
            .sweeps
            .iter()
            .flat_map(|sweep| &sweep.rays.time_s)
            .any(|time| time.is_finite() && time.abs() > 1e307),
        "a ray time near the double range"
    );
    let written = write_odim_h5_volume(&volume, &OdimWriteOptions::default()).unwrap();
    let read = read_supported_volume_bytes(&written).unwrap();
    compare::assert_matches(
        &volume,
        &read,
        &compare::odim_expect(TIME_NEAR_FLOAT_MAX.to_owned(), &volume),
    );
}
