//! Level II writer on real non-NEXRAD volumes: ODIM_H5 (Belgium, Denmark,
//! Ireland, Norway, Spain), CfRadial 1 (ARM X-SAPR, the SMART-R2 in Irene),
//! DORADE (COW2, NOXP) and JMA GRIB2 (Takayasu reflectivity and velocity),
//! each decoded, written as Level II and decoded again.
//!
//! What must come back:
//! - geometry identical: the same rays (from the earliest when a sweep
//!   stores them from another azimuth, as ODIM does) with bit-identical
//!   azimuths and elevations, ray times to the millisecond, every moment's
//!   first gate and
//!   spacing (to the metre Message 31 holds: only NOXP 2009-06-10, whose
//!   first gate is at 37.5 m, moves, by half a metre) and gate count, and
//!   fixed angles within half a Message 5 angle code (360/65536 degrees);
//! - every value within the quantisation step the writer reports (and to
//!   float rounding where it reports an exact coding), missing and undetect
//!   gates as below threshold, and nothing clipped under any policy (the
//!   writer refuses a value its coding cannot hold);
//! - under the default policy (`Precise`), no value coded more coarsely
//!   than its source stores it: exact for float sources on a grid and for
//!   integer sources whose sweeps share one coding, else within half the
//!   finest source step; under `Compatible`, NEXRAD's word sizes;
//! - the station location (latitude and longitude to f32, height to the
//!   metre), the transmitter frequency to the MHz, and the Nyquist velocity
//!   to 0.01 m/s.
//!
//! RHI volumes are refused with a typed error.

use recast_radar_core::model::{
    Field, FieldData, FieldName, Gate, LinearTransform, Sweep, Volume, merge_volumes,
};
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_io_nexrad::write::{
    Moment, MomentReport, Quantization, SourceMetadata, WriteError, WriteOptions, WriteSummary,
    write_volume_with_source,
};

/// Half of one Message 5 angle code.
const HALF_ANGLE_CODE_DEG: f32 = 180.0 / 65_536.0;

/// The default policy: `Precise`.
fn default_options() -> WriteOptions {
    let options = WriteOptions::default();
    assert_eq!(options.quantization, Quantization::Precise);
    options
}

fn compatible() -> WriteOptions {
    let mut options = WriteOptions::default();
    options.quantization = Quantization::Compatible;
    options
}

fn corpus(id: &str) -> Vec<u8> {
    recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{id}: {err}"))
}

fn decoded(id: &str) -> Volume {
    read_supported_volume_bytes(&corpus(id)).unwrap_or_else(|err| panic!("{id}: {err}"))
}

/// The NOXP DORADE sweeps name their dual-polarization fields `DB_ZDR`,
/// `DB_PHIDP`, `DB_RHOHV` (and, in 2009-05-01, every field `DB_*`), which
/// no name table classifies; map them explicitly.
fn noxp_options() -> WriteOptions {
    let mut options = WriteOptions::default();
    options.field_map = vec![
        (FieldName::parse("DB_DBZ2"), Moment::Ref),
        (FieldName::parse("DB_VEL2"), Moment::Vel),
        (FieldName::parse("DB_WIDTH"), Moment::Sw),
        (FieldName::parse("DB_ZDR2"), Moment::Zdr),
        (FieldName::parse("DB_ZDR"), Moment::Zdr),
        (FieldName::parse("DB_PHIDP"), Moment::Phi),
        (FieldName::parse("DB_RHOHV"), Moment::Rho),
    ];
    options
}

/// Write `volume` and decode the Level II bytes.
fn through_level2(id: &str, volume: &Volume, options: &WriteOptions) -> (Volume, WriteSummary) {
    let (bytes, summary) = write_volume_with_source(volume, SourceMetadata::default(), options)
        .unwrap_or_else(|err| panic!("{id}: write: {err}"));
    let again = recast_radar_io_nexrad::read_volume_from_bytes(&bytes)
        .unwrap_or_else(|err| panic!("{id}: reread: {err}"));
    (again, summary)
}

/// Ray time in milliseconds since 1970, rounded to the millisecond.
fn ray_ms(volume: &Volume, sweep: &Sweep, ray: usize) -> i64 {
    volume.time_reference.timestamp_millis() + (sweep.rays.time_s[ray] * 1000.0).round() as i64
}

/// The source ray of each written radial of source sweep `sweep`: the
/// summary's list, else the rays in storage order.
fn written_order(summary: &WriteSummary, sweep: usize, nrays: usize) -> Vec<usize> {
    summary
        .written_rays
        .iter()
        .find(|rays| rays.sweep == sweep)
        .map_or_else(|| (0..nrays).collect(), |rays| rays.rays.clone())
}

fn assert_values(
    id: &str,
    source: &Sweep,
    written: &Sweep,
    order: &[usize],
    report: &MomentReport,
) {
    let at = format!(
        "{id}: sweep {} {} <- {}",
        report.sweep, report.moment, report.field
    );
    let field = source
        .field(&report.field)
        .unwrap_or_else(|| panic!("{at}: source field"));
    let name = FieldName::from_nexrad_block(report.moment.name().as_bytes());
    let back = written
        .field(&name)
        .unwrap_or_else(|| panic!("{at}: written field"));

    let Some((first, spacing)) = field.native_geometry(&source.range) else {
        panic!("{at}: source geometry");
    };
    let Some((first_back, spacing_back)) = back.native_geometry(&written.range) else {
        panic!("{at}: written geometry");
    };
    assert_eq!(first.round(), first_back, "{at}: first gate");
    assert_eq!(spacing.round(), spacing_back, "{at}: gate spacing");
    assert_eq!(field.ngates, back.ngates, "{at}: gate count");
    let absent: Vec<u32> = order
        .iter()
        .enumerate()
        .filter(|(_, ray)| field.is_absent(**ray))
        .map(|(radial, _)| radial as u32)
        .collect();
    assert_eq!(absent, back.absent_rows, "{at}: absent rays");

    let step = 1.0 / report.scale;
    assert!(
        report.max_abs_error <= step / 2.0 * (1.0 + 1e-4),
        "{at}: error {} above half the {step} step",
        report.max_abs_error
    );
    let mut values = 0usize;
    for (radial, &ray) in order.iter().enumerate() {
        for gate in 0..field.ngates as usize {
            let a = field.gate(ray, gate).unwrap_or(Gate::Missing);
            let b = back.gate(radial, gate).unwrap_or(Gate::Missing);
            match (a, b) {
                (Gate::Value(x), Gate::Value(y)) if x.is_finite() => {
                    let slack = 1e-5 * x.abs().max(1.0);
                    let allowed = if report.exact {
                        slack
                    } else {
                        report.max_abs_error + slack
                    };
                    assert!(
                        (x - y).abs() <= allowed,
                        "{at}: ray {ray} gate {gate}: {x} came back as {y}"
                    );
                    values += 1;
                }
                (Gate::Value(x), Gate::Undetect) if !x.is_finite() => {}
                (Gate::Missing | Gate::Undetect, Gate::Undetect | Gate::Missing) => {}
                (Gate::RangeFolded, Gate::RangeFolded) => {}
                (a, b) => panic!("{at}: ray {ray} gate {gate}: {a:?} came back as {b:?}"),
            }
        }
    }
    assert!(
        values > 0 || field.to_physical().iter().all(|v| v.is_nan()),
        "{at}"
    );
}

/// The `Compatible` policy's codings: NEXRAD word sizes (16 bits only for
/// ZDR and PHI), one coding per moment for the whole volume.
fn assert_compatible_codings(id: &str, summary: &WriteSummary) {
    for report in &summary.moments {
        if !matches!(report.moment, Moment::Zdr | Moment::Phi) {
            assert_eq!(report.word_size, 8, "{id}: {report:?}");
        }
        assert!(
            summary
                .moments
                .iter()
                .filter(|other| other.moment == report.moment)
                .all(|other| (other.scale, other.offset) == (report.scale, report.offset)),
            "{id}: {} codings differ between sweeps",
            report.moment
        );
    }
}

/// The default policy's promise: no value coded more coarsely than its
/// source stores it. Float sources (on a grid, as every fixture here is)
/// come back exact; integer sources exact when every sweep's field of the
/// moment has the same coding, else within half the finest source step.
fn assert_no_coarser_than_source(id: &str, volume: &Volume, summary: &WriteSummary) {
    for report in &summary.moments {
        let fields: Vec<&Field> = summary
            .moments
            .iter()
            .filter(|m| m.moment == report.moment)
            .map(|m| {
                volume.sweeps[m.sweep]
                    .field(&m.field)
                    .unwrap_or_else(|| panic!("{id}: {m:?}"))
            })
            .collect();
        let float = fields
            .iter()
            .any(|f| matches!(f.data, FieldData::F32 { .. } | FieldData::F64 { .. }));
        if float {
            assert!(report.exact, "{id}: float source not exact: {report:?}");
            continue;
        }
        let transforms: Vec<Option<LinearTransform>> =
            fields.iter().map(|f| f.data.transform()).collect();
        if transforms.windows(2).all(|pair| pair[0] == pair[1]) {
            assert!(
                report.exact,
                "{id}: one source coding, not exact: {report:?}"
            );
        }
        let finest = transforms
            .iter()
            .flatten()
            .filter_map(|t| t.scale_factor().map(f64::abs))
            .fold(f64::INFINITY, f64::min);
        assert!(
            f64::from(report.max_abs_error) <= finest / 2.0 * (1.0 + 1e-4),
            "{id}: {report:?} against a {finest} source step"
        );
    }
}

/// Every check of the module documentation.
fn assert_round_trip(
    id: &str,
    source: &Volume,
    options: &WriteOptions,
    site: &str,
) -> WriteSummary {
    let (again, summary) = through_level2(id, source, options);
    if options.quantization == Quantization::Compatible {
        assert_compatible_codings(id, &summary);
    }
    assert_eq!(summary.icao, site, "{id}: site identifier");
    assert_eq!(again.attrs.instrument_name, site, "{id}");
    // Message 31 places gates to the metre: NOXP's 2009-06-10 first gate at
    // 37.5 m moves half a metre; every other source is whole metres.
    assert!(
        summary.max_range_error_m <= 0.5,
        "{id}: range error {}",
        summary.max_range_error_m
    );
    let written: Vec<&Sweep> = source.sweeps.iter().filter(|s| s.nrays() > 0).collect();
    assert_eq!(again.sweeps.len(), written.len(), "{id}: sweeps");

    let location = source.location;
    assert_eq!(
        again.location.latitude_deg,
        location.latitude_deg.map(|v| f64::from(v as f32)),
        "{id}: latitude"
    );
    assert_eq!(
        again.location.longitude_deg,
        location.longitude_deg.map(|v| f64::from(v as f32)),
        "{id}: longitude"
    );
    assert_eq!(
        again.location.altitude_m,
        Some(location.altitude_m.unwrap_or(0.0).round()),
        "{id}: altitude"
    );
    let mhz = |hz: &[f64]| hz.first().map(|hz| (hz / 1e6).round());
    assert_eq!(
        mhz(&again.radar_parameters.frequency_hz),
        mhz(&source.radar_parameters.frequency_hz),
        "{id}: frequency"
    );

    for (index, (sa, sb)) in written.iter().zip(&again.sweeps).enumerate() {
        let at = format!("{id}: sweep {index}");
        let Some(source_index) = source
            .sweeps
            .iter()
            .position(|sweep| std::ptr::eq(sweep, *sa))
        else {
            panic!("{at}: source sweep");
        };
        // Every ray of these sources has data: the written radials are the
        // source's rays, from the earliest when the sweep stores them from
        // another azimuth.
        let order = written_order(&summary, source_index, sa.nrays());
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert!(
            sorted.iter().copied().eq(0..sa.nrays()),
            "{at}: every ray once"
        );
        assert_eq!(sb.nrays(), order.len(), "{at}: rays");
        for (radial, &ray) in order.iter().enumerate() {
            assert_eq!(
                sa.rays.azimuth_deg[ray].to_bits(),
                sb.rays.azimuth_deg[radial].to_bits(),
                "{at}: radial {radial} azimuth"
            );
            assert_eq!(
                sa.rays.elevation_deg[ray].to_bits(),
                sb.rays.elevation_deg[radial].to_bits(),
                "{at}: radial {radial} elevation"
            );
            assert_eq!(
                ray_ms(source, sa, ray),
                ray_ms(&again, sb, radial),
                "{at}: radial {radial} time"
            );
        }
        if order.first() != Some(&0) {
            // Written from the earliest ray: times run forward.
            assert!(
                (1..sb.nrays()).all(|r| ray_ms(&again, sb, r - 1) <= ray_ms(&again, sb, r)),
                "{at}: radial times"
            );
        }
        assert!(
            (sa.fixed_angle_deg - sb.fixed_angle_deg).abs() <= HALF_ANGLE_CODE_DEG,
            "{at}: fixed angle {} vs {}",
            sa.fixed_angle_deg,
            sb.fixed_angle_deg
        );
        assert_eq!(sb.elevation_number, Some(index as u16 + 1), "{at}");
        match (
            &sa.ray_vars.nyquist_velocity_mps,
            &sb.ray_vars.nyquist_velocity_mps,
        ) {
            (Some(a), Some(b)) => {
                for (&ray, y) in order.iter().zip(b) {
                    let x = &a[ray];
                    if x.is_finite() && *x > 0.0 {
                        assert!((x - y).abs() <= 0.005 + 1e-6, "{at}: Nyquist {x} vs {y}");
                    } else {
                        assert!(y.is_nan(), "{at}: Nyquist {x} vs {y}");
                    }
                }
            }
            (None, None) => {}
            (a, b) => assert!(
                a.as_ref()
                    .is_some_and(|v| v.iter().all(|x| !(x.is_finite() && *x > 0.0)))
                    && b.is_none(),
                "{at}: Nyquist {a:?} vs {b:?}"
            ),
        }
        for report in summary.moments.iter().filter(|m| m.sweep == source_index) {
            assert_values(id, sa, sb, &order, report);
        }
    }
    summary
}

#[test]
fn odim_volumes_come_back_within_the_quantisation_step() {
    for (id, site) in [
        ("odim-bejab-20190606-0000-pvol", "BJAB"),
        ("odim-bewid-20130429-0430-pvol-dbzh-scan1", "BWID"),
        ("odim-norst-20170421-0908-pvol", "NRST"),
        ("odim-espdg-20260707-1927-pvol-dbzh-vradh", "EPDG"),
        ("odim-iesha-20260305-0115-pvol", "ISHA"),
        ("odim-dkrom-20260820-1130-pvol", "DROM"),
        ("odim-bejab-20260612-1450-dbzh", "BJAB"),
        ("odim-bejab-20260612-1450-vrad", "BJAB"),
        ("odim-nohur-20260612-1445-dbzh", "NHUR"),
        ("odim-nohur-20260612-1446-vradh", "NHUR"),
    ] {
        let volume = decoded(id);
        let summary = assert_round_trip(id, &volume, &compatible(), site);
        assert!(!summary.moments.is_empty(), "{id}");
        // 8-bit ODIM data fits 8-bit codes exactly when every sweep of the
        // moment has the same gain and offset, under both policies. The
        // Norwegian vertical scan's velocities have a quarter of the other
        // sweeps' step: no one 8-bit grid holds both, so Compatible's shared
        // coding rounds, where the default stays within half the finest
        // step.
        let default = assert_round_trip(id, &volume, &default_options(), site);
        assert_no_coarser_than_source(id, &volume, &default);
        for (compatible, finer) in summary.moments.iter().zip(&default.moments) {
            if finer.word_size == 8 {
                assert_eq!(
                    (compatible.scale, compatible.offset),
                    (finer.scale, finer.offset),
                    "{id}: 8-bit codings differ between the policies"
                );
            }
        }
        // ODIM reflectivity in 0.5 dB steps from -32 dBZ lies on the
        // typical REF coding.
        for report in summary.moments.iter().filter(|m| m.moment == Moment::Ref) {
            assert_eq!(
                (report.word_size, report.scale, report.offset),
                (8, 2.0, 66.0),
                "{id}: REF coding"
            );
            assert!(report.exact, "{id}");
        }
    }
    // Total power, LDR and the other quantities without a Message 31 moment
    // are left out and reported.
    let volume = decoded("odim-dkrom-20260820-1130-pvol");
    let (_, summary) = through_level2("dkrom", &volume, &default_options());
    let skipped: Vec<&str> = summary
        .skipped_fields
        .iter()
        .filter(|s| s.sweep == 0)
        .map(|s| s.field.as_str())
        .collect();
    assert_eq!(skipped, ["TH", "LDR"]);
}

#[test]
fn cfradial_volumes_come_back_within_the_quantisation_step() {
    for (id, site) in [
        ("cfrad1-xsapr-sgp-20110520-ppi-classic", "XSAP"),
        ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", "CPOL"),
    ] {
        let volume = decoded(id);
        assert_round_trip(id, &volume, &compatible(), site);
        // Float and 8-bit CfRadial data on 0.01 dB and 0.5 dB grids: exact
        // under the default policy, which may use 16-bit words for any
        // moment.
        let summary = assert_round_trip(id, &volume, &default_options(), site);
        assert_no_coarser_than_source(id, &volume, &summary);
        assert!(
            summary.moments.iter().all(|m| m.exact),
            "{id}: {:?}",
            summary.moments
        );
    }
}

#[test]
fn dorade_sweeps_come_back_within_the_quantisation_step() {
    let volume = decoded("dorade-cow2-20260521-225514-sur-head24");
    let summary = assert_round_trip("cow2", &volume, &compatible(), "COW2");
    let moments: Vec<Moment> = summary.moments.iter().map(|m| m.moment).collect();
    assert_eq!(
        moments,
        [Moment::Ref, Moment::Vel, Moment::Zdr, Moment::Rho]
    );
    // 0.01 steps of 16-bit DORADE fields: exact 16-bit codings under the
    // default policy.
    let summary = assert_round_trip("cow2", &volume, &default_options(), "COW2");
    assert_no_coarser_than_source("cow2", &volume, &summary);
    assert!(summary.moments.iter().all(|m| m.exact && m.word_size == 16));

    // 2009-05-01: every field is named DB_*, so nothing maps without the
    // explicit mapping; the other sweeps name REF, VEL and SW DZ, VR, SW.
    let volume = decoded("dorade-noxp-20090501-190244-ppi");
    let refused =
        write_volume_with_source(&volume, SourceMetadata::default(), &WriteOptions::default());
    assert!(matches!(refused, Err(WriteError::NoMoments)), "{refused:?}");
    let mapped = assert_round_trip("noxp 190244", &volume, &noxp_options(), "NOXP");
    assert_eq!(mapped.moments.len(), 6, "{:?}", mapped.moments);
    assert_no_coarser_than_source("noxp 190244", &volume, &mapped);
    for id in [
        "dorade-noxp-20090525-203211-sector",
        "dorade-noxp-20090610-003210-ppi-head6",
    ] {
        let volume = decoded(id);
        let automatic = assert_round_trip(id, &volume, &default_options(), "NOXP");
        let moments: Vec<Moment> = automatic.moments.iter().map(|m| m.moment).collect();
        assert_eq!(moments, [Moment::Ref, Moment::Vel, Moment::Sw], "{id}");
        assert_no_coarser_than_source(id, &volume, &automatic);
        assert_round_trip(id, &volume, &compatible(), "NOXP");
        let mapped = assert_round_trip(id, &volume, &noxp_options(), "NOXP");
        assert_eq!(mapped.moments.len(), 6, "{id}: {:?}", mapped.moments);
        assert_no_coarser_than_source(id, &volume, &mapped);
    }
}

#[test]
fn jma_volumes_come_back_within_the_quantisation_step() {
    let reflectivity = decoded("jma-n5-20191012-090000-rs47773");
    let summary = assert_round_trip("jma n5", &reflectivity, &default_options(), "TAKA");
    // Every sweep written: 512 radials per cut.
    assert_eq!(summary.sweeps, reflectivity.sweeps.len());
    assert_no_coarser_than_source("jma n5", &reflectivity, &summary);
    assert_round_trip("jma n5", &reflectivity, &compatible(), "TAKA");
    let velocity = decoded("jma-n6-20191012-090000-rs47773");
    let summary = assert_round_trip("jma n6", &velocity, &default_options(), "TAKA");
    assert_no_coarser_than_source("jma n6", &velocity, &summary);
    assert_round_trip("jma n6", &velocity, &compatible(), "TAKA");

    // Reflectivity and velocity merged by elevation into one volume.
    let (merged, _) = merge_volumes(vec![reflectivity, velocity]).unwrap();
    let summary = assert_round_trip("jma merged", &merged, &default_options(), "TAKA");
    assert_no_coarser_than_source("jma merged", &merged, &summary);
    assert!(summary.moments.iter().any(|m| m.moment == Moment::Vel));
    assert!(summary.moments.iter().any(|m| m.moment == Moment::Ref));
}

/// `Quantization::Standard` writes NOAA's current codings (KTLX 2024,
/// KILX 2026) where every value of the moment lies within them, and never
/// clips: DMI Romo's RHOHV (steps of 0.0028 from 0) reaches below the
/// typical RHO coding's 0.208 floor, so RHO takes the coding `Compatible`
/// chooses instead (a value outside a moment's coding would refuse the
/// write: `WriteError::ValueOutsideCoding`).
#[test]
fn standard_quantisation_uses_the_typical_codings_without_clipping() {
    let id = "odim-dkrom-20260820-1130-pvol";
    let volume = decoded(id);
    let mut options = WriteOptions::default();
    options.quantization = Quantization::Standard;
    let (again, summary) = through_level2(id, &volume, &options);
    let (_, compatible_summary) = through_level2(id, &volume, &compatible());
    let typical = |moment: Moment| match moment {
        Moment::Ref => (8, 2.0, 66.0),
        Moment::Vel | Moment::Sw => (8, 2.0, 129.0),
        Moment::Zdr => (16, 32.0, 418.0),
        Moment::Phi => (16, 2.8361, 2.0),
        Moment::Rho => (8, 300.0, -60.5),
        Moment::Cfp => (8, 1.0, 8.0),
        other => panic!("{other}"),
    };
    for report in &summary.moments {
        let coding = (report.word_size, report.scale, report.offset);
        if coding == typical(report.moment) {
            // Values are within half a step of the typical coding.
            let step = 1.0 / report.scale;
            assert!(
                report.max_abs_error <= step / 2.0 * (1.0 + 1e-4),
                "{report:?}"
            );
        } else {
            let fallback = compatible_summary
                .moments
                .iter()
                .find(|m| m.moment == report.moment && m.sweep == report.sweep)
                .unwrap();
            assert_eq!(
                coding,
                (fallback.word_size, fallback.scale, fallback.offset),
                "{report:?}"
            );
        }
    }
    let coding_of = |moment: Moment| {
        summary
            .moments
            .iter()
            .find(|m| m.moment == moment && m.sweep == 0)
            .map(|m| (m.word_size, m.scale, m.offset))
            .unwrap()
    };
    assert_eq!(coding_of(Moment::Ref), typical(Moment::Ref));
    assert_ne!(coding_of(Moment::Rho), typical(Moment::Rho));
    assert_eq!(again.sweeps.len(), volume.sweeps.len());
}

#[test]
fn rhi_volumes_are_refused() {
    for id in [
        "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
        "dorade-dow6-20211230-222139-rhi-head41",
    ] {
        let volume = decoded(id);
        let refused =
            write_volume_with_source(&volume, SourceMetadata::default(), &WriteOptions::default());
        match refused {
            Err(WriteError::UnsupportedSweepMode { sweep: 0, mode }) => assert_eq!(mode, "rhi"),
            other => panic!("{id}: {other:?}"),
        }
    }
}

/// JMA cuts of 512 radials, which are not a multiple of 120. By default
/// records, and so real-time chunks, hold 120 radials each and run on
/// across cuts; with `RecordLayout::WithinCuts` they
/// end with each cut (120, 120, 120, 120 and 32 radials a cut). Under both,
/// the chunks sent while the sweeps arrive one by one are the whole
/// volume's.
#[test]
fn record_layouts_and_streamed_chunks() {
    use recast_radar_io_nexrad::messages::{MessageBody, MessageWalker, record_bytes};
    use recast_radar_io_nexrad::write::RecordLayout;
    use recast_radar_io_nexrad::write::realtime::{ChunkWriter, write_realtime_chunks};

    let volume = decoded("jma-n6-20191012-090000-rs47773");
    assert!(volume.sweeps.iter().all(|sweep| sweep.nrays() == 512));
    // Elevation number of every radial of each radial chunk.
    let cuts_per_chunk = |chunks: &[recast_radar_io_nexrad::write::realtime::Chunk]| {
        chunks[1..]
            .iter()
            .map(|chunk| {
                let records = record_bytes(&chunk.bytes).unwrap();
                MessageWalker::new(&records)
                    .filter_map(|item| match item.unwrap().1 {
                        MessageBody::DigitalRadarDataGeneric(radial) => {
                            Some(radial.header.elevation_number)
                        }
                        _ => None,
                    })
                    .collect::<Vec<u8>>()
            })
            .collect::<Vec<_>>()
    };
    let streamed = |options: &WriteOptions| {
        let mut writer = ChunkWriter::new(&volume, options).unwrap();
        let mut chunks = Vec::new();
        for sweep in &volume.sweeps {
            let mut part = volume.clone();
            part.sweeps = vec![sweep.clone()];
            chunks.extend(writer.push(&part).unwrap());
        }
        chunks.push(writer.finish().unwrap().0);
        chunks
    };

    // Default: 120 radials a record whatever the cut; only the last holds
    // fewer (13 x 512 = 6656 = 55 x 120 + 56).
    let options = WriteOptions::default();
    assert_eq!(options.record_layout, RecordLayout::Continuous);
    let whole = write_realtime_chunks(&volume, &options).unwrap();
    let counts: Vec<usize> = cuts_per_chunk(&whole.chunks).iter().map(Vec::len).collect();
    let (last, full) = counts.split_last().unwrap();
    assert!(full.iter().all(|count| *count == 120), "{counts:?}");
    assert_eq!((full.len(), *last), (55, 56));
    assert!(
        cuts_per_chunk(&whole.chunks)
            .iter()
            .any(|numbers| numbers.first() != numbers.last()),
        "some record runs across cuts"
    );
    assert!(streamed(&options) == whole.chunks, "streamed chunks differ");

    // Within cuts: each cut's records end with it.
    let mut within = WriteOptions::default();
    within.record_layout = RecordLayout::WithinCuts;
    let whole = write_realtime_chunks(&volume, &within).unwrap();
    let mut per_cut: Vec<Vec<usize>> = vec![Vec::new(); volume.sweeps.len()];
    for numbers in cuts_per_chunk(&whole.chunks) {
        assert!(
            numbers.windows(2).all(|pair| pair[0] == pair[1]),
            "a chunk spans cuts"
        );
        per_cut[usize::from(numbers[0]) - 1].push(numbers.len());
    }
    assert!(
        per_cut
            .iter()
            .all(|counts| counts == &[120, 120, 120, 120, 32]),
        "{per_cut:?}"
    );
    assert!(streamed(&within) == whole.chunks, "streamed chunks differ");
}

/// The RAD block values of one Message 31 radial.
#[derive(Debug, Clone)]
struct RadialDump {
    nyquist_raw: u16,
    unambiguous_raw: u16,
}

fn radial_dumps(bytes: &[u8]) -> Vec<RadialDump> {
    use recast_radar_io_nexrad::messages::{MessageBody, MessageWalker, record_bytes};
    let records = record_bytes(bytes).unwrap_or_else(|err| panic!("records: {err}"));
    MessageWalker::new(&records)
        .filter_map(|item| match item.ok()?.1 {
            MessageBody::DigitalRadarDataGeneric(radial) => Some(RadialDump {
                nyquist_raw: radial.radial.map_or(0, |rad| rad.nyquist_velocity_raw),
                unambiguous_raw: radial.radial.map_or(0, |rad| rad.unambiguous_range_raw),
            }),
            _ => None,
        })
        .collect()
}

/// The JMA decoder leaves the Nyquist velocity and unambiguous range unset
/// (staggered PRF). Written as they are, the VEL radials carry 0 in the RAD
/// block and a note says so; `WriteOptions::nyquist_velocity_mps` and
/// `unambiguous_range_m` fill every radial whose source has none with the
/// values the caller knows for the radar. Values the RAD block cannot hold
/// are refused.
#[test]
fn missing_nyquist_velocities_are_noted_or_supplied() {
    let id = "jma-n6-20191012-090000-rs47773";
    let volume = decoded(id);
    assert!(
        volume
            .sweeps
            .iter()
            .all(|sweep| sweep.ray_vars.nyquist_velocity_mps.is_none())
    );
    let (bytes, summary) =
        write_volume_with_source(&volume, SourceMetadata::default(), &default_options()).unwrap();
    assert!(
        summary
            .notes
            .iter()
            .any(|note| note.contains("without a Nyquist velocity")),
        "{:?}",
        summary.notes
    );
    let radials = radial_dumps(&bytes);
    assert!(
        radials
            .iter()
            .all(|r| (r.nyquist_raw, r.unambiguous_raw) == (0, 0))
    );

    let mut options = default_options();
    options.nyquist_velocity_mps = Some(26.48);
    options.unambiguous_range_m = Some(150_000.0);
    let (bytes, summary) =
        write_volume_with_source(&volume, SourceMetadata::default(), &options).unwrap();
    assert!(summary.notes.is_empty(), "{:?}", summary.notes);
    let radials = radial_dumps(&bytes);
    assert_eq!(radials.len(), 13 * 512);
    assert!(
        radials
            .iter()
            .all(|r| (r.nyquist_raw, r.unambiguous_raw) == (2648, 1500))
    );
    let again = recast_radar_io_nexrad::read_volume_from_bytes(&bytes).unwrap();
    for sweep in &again.sweeps {
        let nyquist = sweep.ray_vars.nyquist_velocity_mps.as_deref().unwrap();
        assert!(
            nyquist.iter().all(|v| (v - 26.48).abs() < 1e-4),
            "{nyquist:?}"
        );
    }

    for (nyquist, range) in [
        (Some(0.0), None),
        (Some(f32::NAN), None),
        (Some(400.0), None),
        (None, Some(-1.0)),
        (None, Some(4.0e6)),
    ] {
        let mut options = default_options();
        options.nyquist_velocity_mps = nyquist;
        options.unambiguous_range_m = range;
        match write_volume_with_source(&volume, SourceMetadata::default(), &options) {
            Err(WriteError::InvalidOption(_)) => {}
            other => panic!("{nyquist:?} {range:?}: {:?}", other.map(|(_, s)| s)),
        }
    }
}

/// The sweeps of one scan cycle of a JMA 10-minute tar, in the order they
/// were collected: the tar holds two 5-minute cycles, each starting with its
/// 25-degree sweep (JMA collects from the top down, then up again).
fn jma_cycle(volume: &Volume, cycle: usize) -> Volume {
    let start = |sweep: &Sweep| sweep.rays.time_s.first().copied().unwrap_or(f64::NAN);
    let mut tops: Vec<f64> = volume
        .sweeps
        .iter()
        .filter(|sweep| (sweep.fixed_angle_deg - 25.0).abs() < 0.01)
        .map(start)
        .collect();
    tops.sort_by(f64::total_cmp);
    assert_eq!(tops.len(), 2, "two cycles");
    let (from, until) = match cycle {
        0 => (f64::NEG_INFINITY, tops[1]),
        _ => (tops[1], f64::INFINITY),
    };
    let mut sweeps: Vec<Sweep> = volume
        .sweeps
        .iter()
        .filter(|sweep| (from..until).contains(&start(sweep)))
        .cloned()
        .collect();
    sweeps.sort_by(|a, b| start(a).total_cmp(&start(b)));
    let mut part = volume.clone();
    part.sweeps = sweeps;
    part
}

/// The real-time chunk writer never clips. Planned from the first 5-minute
/// cycle of JMA Okinawa's 2026-09-24 21:00Z tar (the previous volume of the
/// radar) under `Compatible`, whose 8-bit REF coding spans that cycle's
/// values (0 to 47.2 dBZ), the second cycle's sweeps are pushed one by one
/// in the order they were collected. Its 0.2-degree sweep holds 3 gates
/// above 47.2 dBZ: the push is refused with `ValueOutsideCoding`, nothing
/// sent, and the volume still ends where the last accepted sweep left it.
/// The whole-file writer, which chooses the coding from the values, writes
/// the whole cycle with every value within half its step. Under `Precise`
/// the plan's exact 16-bit grid holds every value of the second cycle.
#[test]
fn chunk_writer_refuses_values_outside_the_planned_coding() {
    use recast_radar_io_nexrad::write::realtime::ChunkWriter;

    let volume = decoded("jma-n5-20260924-210000-rs47937");
    let first = jma_cycle(&volume, 0);
    let second = jma_cycle(&volume, 1);
    assert_eq!((first.sweeps.len(), second.sweeps.len()), (17, 18));
    let single = |sweep: &Sweep| {
        let mut part = second.clone();
        part.sweeps = vec![sweep.clone()];
        part
    };

    let options = compatible();
    let mut writer = ChunkWriter::new(&first, &options).unwrap();
    let mut sent = Vec::new();
    let mut refusal = None;
    for (index, sweep) in second.sweeps.iter().enumerate() {
        match writer.push(&single(sweep)) {
            Ok(chunks) => sent.extend(chunks),
            Err(WriteError::ValueOutsideCoding {
                sweep: 0,
                moment: Moment::Ref,
                gates,
                low,
                high,
                planned: true,
                ..
            }) => {
                refusal = Some((index, gates, low, high));
                break;
            }
            Err(err) => panic!("{err}"),
        }
    }
    let (refused, gates, low, high) = refusal.unwrap();
    assert_eq!((refused, gates, low), (11, 3, 0.0));
    assert!((high - 47.2).abs() < 0.05, "{high}");
    assert!((second.sweeps[refused].fixed_angle_deg - 0.2).abs() < 0.01);
    // The 3 gates are the source's values above the planned coding.
    let field = second.sweeps[refused]
        .field(&FieldName::parse("DBZH"))
        .unwrap();
    let above = (0..field.nrays as usize)
        .flat_map(|ray| (0..field.ngates as usize).map(move |gate| (ray, gate)))
        .filter(|&(ray, gate)| {
            matches!(field.gate(ray, gate), Some(Gate::Value(value)) if value > high)
        })
        .count();
    assert_eq!(above, 3, "gates above the planned coding");

    // The refused push sent nothing: the volume ends after the 11 sweeps
    // accepted, every value within half a step of its coding.
    let (end, summary) = writer.finish().unwrap();
    assert_eq!(summary.sweeps, 11);
    let file: Vec<u8> = sent
        .iter()
        .chain([&end])
        .flat_map(|chunk| chunk.bytes.clone())
        .collect();
    let again = recast_radar_io_nexrad::read_volume_from_bytes(&file).unwrap();
    assert_eq!(again.sweeps.len(), 11);
    for report in &summary.moments {
        assert!(
            report.max_abs_error <= 0.5 / report.scale * (1.0 + 1e-4),
            "{report:?}"
        );
    }

    // Written whole, the second cycle takes a coding that holds its values.
    let mut whole = second.clone();
    whole.sweeps.truncate(17);
    let (_, summary) =
        write_volume_with_source(&whole, SourceMetadata::default(), &options).unwrap();
    assert_eq!(summary.sweeps, 17);
    for report in &summary.moments {
        assert!(
            report.max_abs_error <= 0.5 / report.scale * (1.0 + 1e-4),
            "{report:?}"
        );
    }

    // Under `Precise` the plan's exact grid holds the second cycle.
    let mut writer = ChunkWriter::new(&first, &default_options()).unwrap();
    for sweep in second.sweeps.iter().take(17) {
        writer.push(&single(sweep)).unwrap();
    }
    assert_eq!(writer.finish().unwrap().1.sweeps, 17);
}
