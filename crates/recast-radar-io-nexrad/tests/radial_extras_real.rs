//! The per-radial Level II values FM301 has no coordinate for reach the
//! model and the FM301 view (`src/radial_extras.rs`).
//!
//! Every committed Level II fixture (Message 1 files from 1991 to 2005 and
//! Message 31 files from Build 10 to Build 24) and the committed KIWA
//! real-time chunks are decoded, and every ray's values are read twice: from
//! the model (`Sweep::extra_vars`, `Sweep::monitoring`, `Sweep::other` and the
//! typed sweep items) and from the FM301 view with every passthrough item
//! (`Passthrough::All`). Both must equal what this test reads from the file
//! bytes itself: the messages are framed with `messages::RawMessages`, and
//! every value is read at its ICD offset here, without the decoder's
//! parsers:
//!
//! - The message header channel byte (Table II halfword 2, high byte) of
//!   both messages, and the rest of the message header as stored: size
//!   (halfword 1), sequence number (3), generation date (4) and time (5-6),
//!   segment count (7) and segment number (8).
//! - Message 31 (ICD 2620002 Table XVII-A): status byte 21, azimuth number
//!   bytes 10-11, spacing byte 20, cut sector byte 23, spot blanking byte 28,
//!   indexing byte 29, and the VOL, ELV and RAD blocks found through the
//!   pointer table (Tables XVII-E, XVII-F, XVII-H).
//! - Message 1 (Table III, offsets as MetPy and Py-ART read them): status
//!   bytes 12-13, azimuth number 10-11, cut sector 30-31, calibration
//!   constant 32-35, atmospheric attenuation 62-63, TOVER 64-65 and spot
//!   blanking 66-67.
//!
//! Radials are matched to rays in file order by their azimuth (the exact
//! float of Message 31, the binary angle of Message 1) and elevation number,
//! so a radial the decoder skips cannot shift the comparison.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use recast_radar_core::fm301::{self, FirstDim, Flavor, Passthrough, ViewOptions, VolumeView};
use recast_radar_core::model::{ArrayBuf, AttrValue, Scalar, Sweep, Volume};
use recast_radar_io_nexrad::messages::{self, RawMessages};
use recast_radar_io_nexrad::read_volume_from_bytes;

/// Every committed Level II fixture.
const COMMITTED: &[&str] = &[
    "l2-ktlx-19910605-162126-trim",
    "l2-ktlx-19990504-002218-trim",
    "l2-ktlx-20030508-221041-trim",
    "l2-klix-20050829-130035-trim",
    "l2-kdmx-20080525-205148-trim",
    "l2-ktlx-20130520-201643-trim",
    "l2-koax-20140616-205305-trim",
    "l2-kewx-20160413-022531-trim",
    "l2-kdvn-20200810-180401-trim",
    "l2-klix-20210829-180425-trim",
    "l2-kbox-20220129-150537-trim",
    "l2-tstl-20230331-230314-trim",
    "l2-pgua-20230524-030945-trim",
    "l2-kmtx-20240301-212827-trim",
    "l2-ktlx-20240315-000217-trim",
    "l2-kilx-20260418-013553-trim",
];

const ALL_PASSTHROUGH: ViewOptions = ViewOptions {
    flavor: Flavor::Wmo2022,
    first_dim: FirstDim::Time,
    passthrough: Passthrough::All,
};

fn be_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be_i16(bytes: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be_f32(bytes: &[u8], at: usize) -> f32 {
    f32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The RAD block values of one Message 31 radial.
#[derive(Clone, Copy, Debug)]
struct Rad {
    noise_h: f32,
    noise_v: f32,
    flags: u16,
    calibration: Option<(f32, f32)>,
}

/// The VOL block values of one Message 31 radial.
#[derive(Clone, Copy, Debug)]
struct Vol {
    major: u8,
    minor: u8,
    latitude: f32,
    longitude: f32,
    site_height: i16,
    feedhorn: u16,
    calibration: f32,
    tx_h: f32,
    tx_v: f32,
    system_zdr: f32,
    phidp: f32,
    vcp: u16,
    processing: u16,
    zdr_bias_raw: Option<u16>,
}

/// The message header of a radial's message (Table II) as stored, beyond
/// the type and channel byte.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Header {
    size: u16,
    sequence: u16,
    date: u16,
    milliseconds: u32,
    segments: u16,
    segment_number: u16,
}

impl Header {
    /// The header at `offset` in `records`.
    fn at(records: &[u8], offset: usize) -> Self {
        Self {
            size: be_u16(records, offset),
            sequence: be_u16(records, offset + 4),
            date: be_u16(records, offset + 6),
            milliseconds: u32::from_be_bytes(records[offset + 8..offset + 12].try_into().unwrap()),
            segments: be_u16(records, offset + 12),
            segment_number: be_u16(records, offset + 14),
        }
    }
}

/// One radial as read from the file bytes by this test.
#[derive(Clone, Debug)]
struct FileRadial {
    message_1: bool,
    /// Message header byte 2 (Table II halfword 2, high byte).
    channels: u8,
    /// The rest of the message header.
    header: Header,
    azimuth_bits: u32,
    elevation_number: u16,
    status: u16,
    azimuth_number: u16,
    cut_sector: u16,
    spot_blanking: u16,
    spacing: Option<u8>,
    indexing: Option<u8>,
    rad: Option<Rad>,
    vol: Option<Vol>,
    elv: Option<(i16, f32)>,
    legacy: Option<(f32, i16, i16)>,
    /// Message 31 bytes 0-3, trimmed.
    identifier: Option<String>,
}

/// A Message 31 block by its four-byte type and name, found through the
/// pointer table.
fn block<'a>(body: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let count = usize::from(be_u16(body, 30));
    (0..count)
        .map(|index| u32::from_be_bytes(body[32 + 4 * index..36 + 4 * index].try_into().unwrap()))
        .map(|pointer| pointer as usize)
        .filter(|&pointer| pointer != 0 && pointer + 4 <= body.len())
        .find(|&pointer| &body[pointer..pointer + 4] == name)
        .map(|pointer| &body[pointer..])
}

/// Every radial of the real file (or chunk set) `ids`, in file order.
fn radials(ids: &[&str]) -> Vec<FileRadial> {
    fn message_31(body: &[u8]) -> FileRadial {
        let rad = block(body, b"RRAD").map(|rad| {
            let size = usize::from(be_u16(rad, 4));
            Rad {
                noise_h: be_f32(rad, 8),
                noise_v: be_f32(rad, 12),
                flags: be_u16(rad, 18),
                calibration: (size >= 28).then(|| (be_f32(rad, 20), be_f32(rad, 24))),
            }
        });
        let vol = block(body, b"RVOL").map(|vol| {
            let size = usize::from(be_u16(vol, 4));
            Vol {
                major: vol[6],
                minor: vol[7],
                latitude: be_f32(vol, 8),
                longitude: be_f32(vol, 12),
                site_height: be_i16(vol, 16),
                feedhorn: be_u16(vol, 18),
                calibration: be_f32(vol, 20),
                tx_h: be_f32(vol, 24),
                tx_v: be_f32(vol, 28),
                system_zdr: be_f32(vol, 32),
                phidp: be_f32(vol, 36),
                vcp: be_u16(vol, 40),
                processing: be_u16(vol, 42),
                zdr_bias_raw: (size >= 52).then(|| be_u16(vol, 44)),
            }
        });
        let elv = block(body, b"RELV").map(|elv| (be_i16(elv, 6), be_f32(elv, 8)));
        FileRadial {
            message_1: false,
            channels: 0,
            header: Header::default(),
            azimuth_bits: be_f32(body, 12).to_bits(),
            elevation_number: u16::from(body[22]),
            status: u16::from(body[21]),
            azimuth_number: be_u16(body, 10),
            cut_sector: u16::from(body[23]),
            spot_blanking: u16::from(body[28]),
            spacing: Some(body[20]),
            indexing: Some(body[29]),
            rad,
            vol,
            elv,
            legacy: None,
            identifier: Some(
                String::from_utf8_lossy(&body[0..4])
                    .trim_matches(char::from(0))
                    .trim()
                    .to_owned(),
            ),
        }
    }

    fn message_1(body: &[u8]) -> FileRadial {
        FileRadial {
            message_1: true,
            channels: 0,
            header: Header::default(),
            azimuth_bits: (f32::from(be_u16(body, 8)) * 360.0 / 65_536.0).to_bits(),
            elevation_number: be_u16(body, 16).max(1),
            status: be_u16(body, 12),
            azimuth_number: be_u16(body, 10),
            cut_sector: be_u16(body, 30),
            spot_blanking: be_u16(body, 66),
            spacing: None,
            indexing: None,
            rad: None,
            vol: None,
            elv: None,
            legacy: Some((be_f32(body, 32), be_i16(body, 62), be_i16(body, 64))),
            identifier: None,
        }
    }

    let bytes = common::load_all(ids).expect("the file is available");
    let records = messages::record_bytes(&bytes).expect("decompress the records");
    let mut out = Vec::new();
    for message in RawMessages::new(&records) {
        let Ok(message) = message else {
            continue;
        };
        let body = &message.body;
        let channels = records[message.offset + 2];
        let header = Header::at(&records, message.offset);
        match records[message.offset + 3] {
            31 if body.len() >= 72 => out.push(FileRadial {
                channels,
                header,
                ..message_31(body)
            }),
            1 if body.len() >= 100 => out.push(FileRadial {
                channels,
                header,
                ..message_1(body)
            }),
            _ => {}
        }
    }
    out
}

/// A per-ray variable of the sweep as `f64` values, from the model and from
/// the view, which must agree.
fn column(view: &VolumeView<'_>, index: usize, sweep: &Sweep, name: &str) -> Option<Vec<f64>> {
    let model = sweep.extra_vars.iter().find(|v| &*v.name == name);
    let group = view.group(&format!("sweep_{index}")).unwrap();
    let viewed = group.variable(name);
    assert_eq!(
        model.is_some(),
        viewed.is_some(),
        "sweep {index} {name}: model and view disagree on presence"
    );
    let model = model?;
    assert_eq!(model.dims, vec![Box::<str>::from("time")], "{name} dims");
    let viewed = viewed.unwrap().values.materialize().unwrap();
    assert_eq!(viewed, model.values, "sweep {index} {name}: view differs");
    Some(values_f64(&model.values))
}

fn values_f64(values: &ArrayBuf) -> Vec<f64> {
    (0..values.len())
        .map(|i| values.get_f64(i).unwrap())
        .collect()
}

/// A sweep attribute from the model and the view (Passthrough::All).
fn sweep_attr(view: &VolumeView<'_>, index: usize, sweep: &Sweep, name: &str) -> Option<f64> {
    let model = sweep
        .other
        .iter()
        .find(|(key, _)| &**key == name)
        .map(|(_, value)| value.clone());
    let viewed = view
        .group(&format!("sweep_{index}"))
        .unwrap()
        .attr(name)
        .cloned();
    assert_eq!(model, viewed, "sweep {index} {name}: model and view differ");
    model.and_then(|value| value.as_f64())
}

fn same_f32(actual: f64, expected: f32) -> bool {
    (actual as f32).to_bits() == expected.to_bits() || (actual.is_nan() && expected.is_nan())
}

fn dbm(kw: f32) -> f32 {
    10.0 * kw.log10() + 60.0
}

/// Checks one volume; returns the number of rays compared.
fn check_volume(ids: &[&str]) -> usize {
    let id = ids.join("+");
    let bytes = common::load_all(ids).expect("the file is available");
    let volume: Volume = read_volume_from_bytes(&bytes).expect("decode");
    let view = fm301::volume_view(&volume, ALL_PASSTHROUGH, None).expect("view");
    let file_radials = radials(ids);
    let mut next = 0usize;
    let mut compared = 0usize;
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        // Match every ray to the next file radial with its azimuth and
        // elevation number.
        let number = sweep.elevation_number.unwrap_or(0);
        let mut matched: Vec<&FileRadial> = Vec::with_capacity(sweep.nrays());
        for azimuth in &sweep.rays.azimuth_deg {
            let found = file_radials[next..]
                .iter()
                .position(|radial| {
                    radial.azimuth_bits == azimuth.to_bits() && radial.elevation_number == number
                })
                .unwrap_or_else(|| panic!("{id} sweep {index}: no radial at azimuth {azimuth}"));
            matched.push(&file_radials[next + found]);
            next += found + 1;
        }
        let legacy = matched.iter().any(|radial| radial.message_1);

        let status = column(&view, index, sweep, "nexrad_radial_status").expect("status");
        let azimuth_number = column(&view, index, sweep, "nexrad_azimuth_number").unwrap();
        let sector = column(&view, index, sweep, "nexrad_cut_sector_number").unwrap();
        let spot = column(&view, index, sweep, "nexrad_spot_blanking_status").unwrap();
        let dtype = |name: &str| {
            sweep
                .extra_vars
                .iter()
                .find(|v| &*v.name == name)
                .unwrap()
                .values
                .dtype()
        };
        let header_dtype = if legacy { "uint16" } else { "uint8" };
        assert_eq!(dtype("nexrad_radial_status"), header_dtype, "{id}");
        // Every stored status is described by the CF flags: a meaning whose
        // mask and value match it.
        let status_attr = |key: &str| {
            let variable = sweep
                .extra_vars
                .iter()
                .find(|v| &*v.name == "nexrad_radial_status")
                .unwrap();
            match &variable.attrs.iter().find(|(k, _)| &**k == key).unwrap().1 {
                AttrValue::Array(values) => values_f64(values),
                other => panic!("{key}: {other:?}"),
            }
        };
        let (masks, values) = (status_attr("flag_masks"), status_attr("flag_values"));
        assert_eq!(masks.len(), 7);
        for stored in &status {
            let stored = *stored as u16;
            assert!(
                masks
                    .iter()
                    .zip(&values)
                    .any(|(mask, value)| stored & (*mask as u16) == *value as u16),
                "{id} {index}: status {stored} has no flag meaning"
            );
        }
        assert_eq!(dtype("nexrad_azimuth_number"), "uint16", "{id}");
        for (ray, radial) in matched.iter().enumerate() {
            let what = format!("{id} sweep {index} ray {ray}");
            assert_eq!(status[ray], f64::from(radial.status), "{what} status");
            assert_eq!(
                azimuth_number[ray],
                f64::from(radial.azimuth_number),
                "{what} azimuth number"
            );
            assert_eq!(sector[ray], f64::from(radial.cut_sector), "{what} sector");
            assert_eq!(spot[ray], f64::from(radial.spot_blanking), "{what} spot");
        }

        // The message header of every radial, per ray and as stored.
        let header_columns = [
            ("nexrad_message_size", "uint16"),
            ("nexrad_message_sequence_number", "uint16"),
            ("nexrad_message_date", "uint16"),
            ("nexrad_message_milliseconds", "uint32"),
            ("nexrad_message_segments", "uint16"),
            ("nexrad_message_segment_number", "uint16"),
        ]
        .map(|(name, expected_dtype)| {
            assert_eq!(dtype(name), expected_dtype, "{id} {name}");
            column(&view, index, sweep, name).unwrap_or_else(|| panic!("{id} {index} {name}"))
        });
        for (ray, radial) in matched.iter().enumerate() {
            let header = radial.header;
            let stored = [
                f64::from(header.size),
                f64::from(header.sequence),
                f64::from(header.date),
                f64::from(header.milliseconds),
                f64::from(header.segments),
                f64::from(header.segment_number),
            ];
            for (column, expected) in header_columns.iter().zip(stored) {
                assert_eq!(column[ray], expected, "{id} sweep {index} ray {ray} header");
            }
        }

        // RAD block columns.
        let any_rad = matched.iter().any(|radial| radial.rad.is_some());
        let noise_h = column(&view, index, sweep, "nexrad_horizontal_noise_level");
        let noise_v = column(&view, index, sweep, "nexrad_vertical_noise_level");
        let flags = column(&view, index, sweep, "nexrad_radial_flags");
        assert_eq!(
            noise_h.is_some(),
            any_rad,
            "{id} sweep {index} RAD presence"
        );
        if let (Some(noise_h), Some(noise_v), Some(flags)) = (noise_h, noise_v, flags) {
            for (ray, radial) in matched.iter().enumerate() {
                let rad = radial.rad.expect("a RAD block on every radial");
                assert!(same_f32(noise_h[ray], rad.noise_h), "{id} {index}/{ray}");
                assert!(same_f32(noise_v[ray], rad.noise_v), "{id} {index}/{ray}");
                assert_eq!(flags[ray], f64::from(rad.flags), "{id} {index}/{ray}");
            }
        }
        let any_calibration = matched
            .iter()
            .any(|radial| radial.rad.and_then(|rad| rad.calibration).is_some());
        let calibration_h = column(
            &view,
            index,
            sweep,
            "nexrad_horizontal_calibration_constant",
        );
        let calibration_v = column(&view, index, sweep, "nexrad_vertical_calibration_constant");
        assert_eq!(calibration_h.is_some(), any_calibration, "{id} {index}");
        if let (Some(h), Some(v)) = (calibration_h, calibration_v) {
            for (ray, radial) in matched.iter().enumerate() {
                let (expected_h, expected_v) = radial.rad.and_then(|rad| rad.calibration).unwrap();
                assert!(same_f32(h[ray], expected_h), "{id} {index}/{ray}");
                assert!(same_f32(v[ray], expected_v), "{id} {index}/{ray}");
            }
        }

        // VOL transmitter powers as FM301 monitoring variables, in dBm.
        let any_vol = matched.iter().any(|radial| radial.vol.is_some());
        let monitoring = view
            .group(&format!("sweep_{index}/monitoring"))
            .map(|group| {
                let read = |name: &str| {
                    values_f64(&group.variable(name).unwrap().values.materialize().unwrap())
                };
                (
                    read("radar_measured_transmit_power_h"),
                    read("radar_measured_transmit_power_v"),
                )
            });
        assert_eq!(monitoring.is_some(), any_vol, "{id} {index} monitoring");
        if let Some((power_h, power_v)) = monitoring {
            let model = sweep.monitoring.as_deref().unwrap();
            let model_h = model.radar_measured_transmit_power_h_dbm.as_ref().unwrap();
            for (ray, radial) in matched.iter().enumerate() {
                let vol = radial.vol.expect("a VOL block on every radial");
                assert!(same_f32(power_h[ray], dbm(vol.tx_h)), "{id} {index}/{ray}");
                assert!(same_f32(power_v[ray], dbm(vol.tx_v)), "{id} {index}/{ray}");
                assert!(same_f32(f64::from(model_h[ray]), dbm(vol.tx_h)));
            }
        }

        // VOL calibration constant, system ZDR and initial system PhiDP:
        // each ray's calib_index names a radar_calibration entry holding
        // its radial's values.
        match &sweep.ray_vars.calib_index {
            None => assert!(!any_vol, "{id} {index}: VOL without calib_index"),
            Some(indices) => {
                let group = view.group(&format!("sweep_{index}")).unwrap();
                let viewed = values_f64(
                    &group
                        .variable("calib_index")
                        .expect("calib_index in the view")
                        .values
                        .materialize()
                        .unwrap(),
                );
                for (ray, radial) in matched.iter().enumerate() {
                    assert_eq!(viewed[ray], f64::from(indices[ray]), "{id} {index}/{ray}");
                    let Some(vol) = radial.vol else {
                        assert_eq!(indices[ray], -1, "{id} {index}/{ray}");
                        continue;
                    };
                    let entry = &volume.radar_calibration[usize::try_from(indices[ray]).unwrap()];
                    for (model, expected) in [
                        (entry.base_1km_hc_dbz, vol.calibration),
                        (entry.zdr_correction_db, vol.system_zdr),
                        (entry.system_phidp_deg, vol.phidp),
                    ] {
                        assert!(
                            same_f32(f64::from(model.unwrap()), expected),
                            "{id} {index}/{ray}: {model:?} vs {expected}"
                        );
                    }
                }
            }
        }

        // Message 1 columns.
        let legacy_calibration = column(&view, index, sweep, "nexrad_calibration_constant");
        let atmospheric = column(&view, index, sweep, "nexrad_atmospheric_attenuation");
        let tover = column(&view, index, sweep, "nexrad_tover");
        assert_eq!(legacy_calibration.is_some(), legacy, "{id} {index}");
        if let (Some(calibration), Some(atmospheric), Some(tover)) =
            (legacy_calibration, atmospheric, tover)
        {
            for (ray, radial) in matched.iter().enumerate() {
                let (expected_calibration, expected_atmospheric, expected_tover) =
                    radial.legacy.unwrap();
                assert!(same_f32(calibration[ray], expected_calibration));
                // The raw (packed) values; the view's `scale_factor` scales them.
                assert_eq!(atmospheric[ray], f64::from(expected_atmospheric));
                assert_eq!(tover[ray], f64::from(expected_tover));
            }
            let attr = |name: &str, key: &str| {
                sweep
                    .extra_vars
                    .iter()
                    .find(|v| &*v.name == name)
                    .unwrap()
                    .attrs
                    .iter()
                    .find(|(k, _)| &**k == key)
                    .map(|(_, v)| v.clone())
            };
            assert_eq!(
                attr("nexrad_atmospheric_attenuation", "scale_factor"),
                Some(AttrValue::Scalar(Scalar::F32(0.001)))
            );
            assert_eq!(
                attr("nexrad_tover", "scale_factor"),
                Some(AttrValue::Scalar(Scalar::F32(0.1)))
            );
        }

        // Sweep items: every radial of every sweep in the corpus has the
        // same values, so they are sweep attributes (a sweep whose radials
        // differ has per-ray variables instead; see
        // `radials_that_differ_keep_their_own_values`).
        let first = matched[0];
        for radial in &matched {
            assert_eq!(radial.channels, first.channels, "{id} {index}");
            assert_eq!(radial.spacing, first.spacing, "{id} {index}");
            assert_eq!(radial.indexing, first.indexing, "{id} {index}");
            let constant = |r: &FileRadial| {
                r.vol.map(|v| {
                    (
                        v.major,
                        v.minor,
                        v.latitude.to_bits(),
                        v.longitude.to_bits(),
                        v.site_height,
                        v.feedhorn,
                        v.vcp,
                        v.processing,
                        v.zdr_bias_raw,
                    )
                })
            };
            assert_eq!(constant(radial), constant(first), "{id} {index}");
            assert_eq!(
                radial.elv.map(|(a, c)| (a, c.to_bits())),
                first.elv.map(|(a, c)| (a, c.to_bits())),
                "{id} {index}"
            );
        }
        let group = view.group(&format!("sweep_{index}")).unwrap();
        assert_eq!(
            sweep_attr(&view, index, sweep, "nexrad_message_channels"),
            Some(f64::from(first.channels)),
            "{id} {index}: channels"
        );
        match first.spacing {
            Some(code) => {
                let expected = match code {
                    1 => Some(0.5),
                    2 => Some(1.0),
                    _ => None,
                };
                assert_eq!(sweep.rays_angle_resolution_deg, expected, "{id} {index}");
                let viewed = group.variable("rays_angle_resolution").map(|v| &v.values);
                assert_eq!(
                    viewed,
                    expected
                        .map(|deg| fm301::Values::Scalar(Scalar::F32(deg)))
                        .as_ref()
                );
                assert_eq!(
                    sweep_attr(&view, index, sweep, "nexrad_azimuthal_spacing_code"),
                    Some(f64::from(code))
                );
            }
            None => assert_eq!(sweep.rays_angle_resolution_deg, None),
        }
        match first.indexing {
            Some(raw) => {
                assert_eq!(sweep.rays_are_indexed, Some(raw != 0), "{id} {index}");
                assert_eq!(
                    group.variable("rays_are_indexed").map(|v| &v.values),
                    Some(&fm301::Values::Text(
                        if raw != 0 { "true" } else { "false" }.into()
                    ))
                );
                let angle = sweep_attr(&view, index, sweep, "nexrad_azimuth_indexing_angle_deg");
                assert!(same_f32(angle.unwrap(), f32::from(raw) * 0.01), "{id}");
            }
            None => assert_eq!(sweep.rays_are_indexed, None),
        }
        let attr = |name: &str| sweep_attr(&view, index, sweep, name);
        match first.vol {
            Some(vol) => {
                assert_eq!(
                    attr("nexrad_volume_block_version_major"),
                    Some(f64::from(vol.major))
                );
                assert_eq!(
                    attr("nexrad_volume_block_version_minor"),
                    Some(f64::from(vol.minor))
                );
                assert_eq!(
                    attr("nexrad_latitude_deg"),
                    Some(f64::from(vol.latitude)),
                    "{id} {index}"
                );
                assert_eq!(
                    attr("nexrad_longitude_deg"),
                    Some(f64::from(vol.longitude)),
                    "{id} {index}"
                );
                assert_eq!(
                    attr("nexrad_site_height_m"),
                    Some(f64::from(vol.site_height))
                );
                assert_eq!(
                    attr("nexrad_feedhorn_height_m"),
                    Some(f64::from(vol.feedhorn))
                );
                assert_eq!(
                    attr("nexrad_volume_coverage_pattern"),
                    Some(f64::from(vol.vcp))
                );
                assert_eq!(
                    attr("nexrad_processing_status"),
                    Some(f64::from(vol.processing))
                );
                assert_eq!(
                    attr("nexrad_zdr_bias_estimate_raw"),
                    vol.zdr_bias_raw.map(f64::from),
                    "{id} {index}"
                );
            }
            None => assert_eq!(attr("nexrad_volume_coverage_pattern"), None),
        }
        match first.elv {
            Some((attenuation, calibration)) => {
                assert!(same_f32(
                    attr("nexrad_atmospheric_attenuation_db_per_km").unwrap(),
                    f32::from(attenuation) * 0.001
                ));
                assert!(same_f32(
                    attr("nexrad_elevation_calibration_constant_db").unwrap(),
                    calibration
                ));
            }
            None => assert_eq!(attr("nexrad_elevation_calibration_constant_db"), None),
        }
        compared += matched.len();
    }
    // The volume header date and time as stored (Table I bytes 12-19), and
    // the first Message 31 radial's radar identifier.
    let (normalized, _) = recast_radar_io_nexrad::normalize_archive_bytes(&bytes).unwrap();
    let root = |name: &str| {
        let model = volume
            .attrs
            .other
            .iter()
            .find(|(key, _)| &**key == name)
            .map(|(_, value)| value.clone());
        assert_eq!(
            model.as_ref(),
            view.root.attr(name),
            "{id} {name}: model and view"
        );
        model
    };
    let header_u32 = |at: usize| u32::from_be_bytes(normalized[at..at + 4].try_into().unwrap());
    assert_eq!(
        root("nexrad_volume_header_date"),
        Some(AttrValue::Scalar(Scalar::U32(header_u32(12)))),
        "{id}"
    );
    assert_eq!(
        root("nexrad_volume_header_milliseconds"),
        Some(AttrValue::Scalar(Scalar::U32(header_u32(16)))),
        "{id}"
    );
    let identifier = file_radials
        .iter()
        .find_map(|radial| radial.identifier.clone());
    assert_eq!(
        root("nexrad_radar_identifier"),
        identifier.map(AttrValue::text),
        "{id}"
    );
    compared
}

#[test]
fn radial_values_reach_the_model_and_the_view() {
    let mut checked = 0usize;
    let mut kinds = (0usize, 0usize);
    for id in COMMITTED {
        if common::load(id).is_none() {
            continue;
        }
        let rays = check_volume(&[id]);
        assert!(rays > 0, "{id}: no rays compared");
        let message_1 = radials(&[id]).iter().any(|radial| radial.message_1);
        if message_1 {
            kinds.0 += 1;
        } else {
            kinds.1 += 1;
        }
        checked += 1;
    }
    let sources: Vec<Vec<&str>> = COMMITTED.iter().map(|id| vec![*id]).collect();
    common::assert_checked_every_available("radial extras", checked, &sources);
    assert_eq!(kinds, (4, 12), "Message 1 and Message 31 files");
}

#[test]
fn real_time_chunks_carry_the_same_values() {
    // The committed start chunk and the next two chunks of KIWA 2026-09-17
    // (Build 24.1, 52-byte VOL, 28-byte RAD).
    let ids = [
        "l2chunk-kiwa-307-20260917-003629-001-s",
        "l2chunk-kiwa-307-20260917-003629-002-i",
        "l2chunk-kiwa-307-20260917-003629-003-i",
    ];
    if common::load_all(&ids).is_none() {
        return;
    }
    assert!(check_volume(&ids) > 100);
}

/// Offsets of the Message 31 bodies (past the 16-byte message header) in
/// normalized Archive II bytes: 134 fixed 2432-byte metadata frames, then
/// variable-length Message 31 records.
fn message_31_bodies(bytes: &[u8]) -> Vec<usize> {
    let mut bodies = Vec::new();
    let mut cursor = 24;
    let mut frame = 0usize;
    while cursor + 28 <= bytes.len() {
        let size = usize::from(be_u16(bytes, cursor + 12)) * 2;
        let message_type = bytes[cursor + 15];
        if size == 0 && frame >= 134 {
            break;
        }
        if message_type == 31 && frame >= 134 {
            bodies.push(cursor + 28);
            cursor += 12 + size;
        } else {
            cursor += 2432;
        }
        frame += 1;
    }
    bodies
}

/// The values [`fabricated_differing_radials`] changes, as the file stores
/// them.
struct Stored {
    /// Radial 5's ELV atmospheric attenuation (0.001 dB/km).
    attenuation: i16,
    /// Radial 7's VOL calibration constant.
    calibration: f32,
    /// Radial 11's reflectivity TOVER (0.1 dB).
    tover: u16,
    /// Radial 13's VOL latitude.
    latitude: f32,
}

/// Edited real bytes, a synthetic input: the real KTLX 2024 volume
/// (normalized) with radial 5's ELV atmospheric attenuation lowered by 3,
/// radial 7's VOL calibration constant raised by 0.5, radial 9's radar
/// identifier made `XTLX`, radial 11's reflectivity TOVER raised by 7 and
/// radial 13's VOL latitude moved 0.01 degree north. No real file has a
/// sweep whose radials differ in these values: a scan of 96 real Level II
/// volumes (the corpus, the testdata cache and the committed fixtures) found
/// none. `testdata/synthetic-allowlist.toml` lists this
/// helper and its test as pending the owner's decision.
fn fabricated_differing_radials(
    file: &[u8],
) -> (Vec<u8>, recast_radar_io_nexrad::ArchiveCompression, Stored) {
    let (mut bytes, compression) = recast_radar_io_nexrad::normalize_archive_bytes(file).unwrap();
    let bodies = message_31_bodies(&bytes);
    assert_eq!(bodies.len(), 960);
    let block_at = |bytes: &[u8], body: usize, name: &[u8; 4]| {
        let block = block(&bytes[body..], name).unwrap();
        body + (bytes[body..].len() - block.len())
    };
    let elv = block_at(&bytes, bodies[5], b"RELV");
    let attenuation = be_i16(&bytes, elv + 6);
    bytes[elv + 6..elv + 8].copy_from_slice(&(attenuation - 3).to_be_bytes());
    let vol = block_at(&bytes, bodies[7], b"RVOL");
    let calibration = be_f32(&bytes, vol + 20);
    bytes[vol + 20..vol + 24].copy_from_slice(&(calibration + 0.5).to_be_bytes());
    // Radial 9's radar identifier (Table XVII-A bytes 0-3) and radial 11's
    // reflectivity TOVER (Table XVII-B bytes 14-15).
    assert_eq!(&bytes[bodies[9]..bodies[9] + 4], b"KTLX");
    bytes[bodies[9]..bodies[9] + 4].copy_from_slice(b"XTLX");
    let reflectivity = block_at(&bytes, bodies[11], b"DREF");
    let tover = be_u16(&bytes, reflectivity + 14);
    bytes[reflectivity + 14..reflectivity + 16].copy_from_slice(&(tover + 7).to_be_bytes());
    // Radial 13's VOL latitude (Table XVII-E bytes 8-11), 0.01 degree north.
    let moved = block_at(&bytes, bodies[13], b"RVOL");
    let latitude = be_f32(&bytes, moved + 8);
    bytes[moved + 8..moved + 12].copy_from_slice(&(latitude + 0.01).to_be_bytes());
    let stored = Stored {
        attenuation,
        calibration,
        tover,
        latitude,
    };
    (bytes, compression, stored)
}

/// A sweep whose radials differ (the edits of
/// [`fabricated_differing_radials`]): the sweep's items become per-ray
/// variables (the other rays keep the file's values), and the changed radial
/// gets a second calibration entry.
#[test]
fn radials_that_differ_keep_their_own_values() {
    let Some(file) = common::load("l2-ktlx-20240315-000217-trim") else {
        return;
    };
    let (bytes, compression, stored) = fabricated_differing_radials(&file);
    let Stored {
        attenuation,
        calibration,
        tover,
        latitude,
    } = stored;
    let volume = recast_radar_io_nexrad::read_normalized_volume_bytes(&bytes, compression).unwrap();
    let view = fm301::volume_view(&volume, ALL_PASSTHROUGH, None).unwrap();
    let sweep = &volume.sweeps[0];
    assert_eq!(sweep.nrays(), 480);
    assert_eq!(
        sweep_attr(&view, 0, sweep, "nexrad_atmospheric_attenuation_db_per_km"),
        None
    );
    let per_ray = column(&view, 0, sweep, "nexrad_atmospheric_attenuation_db_per_km").unwrap();
    for (ray, value) in per_ray.iter().enumerate() {
        let raw = if ray == 5 {
            attenuation - 3
        } else {
            attenuation
        };
        assert!(
            same_f32(*value, f32::from(raw) * 0.001),
            "ray {ray}: {value}"
        );
    }
    // The sweep's other items become per-ray variables with it: the site
    // height every radial shares repeats on every ray.
    let site_height = column(&view, 0, sweep, "nexrad_site_height_m").unwrap();
    assert!(site_height.windows(2).all(|pair| pair[0] == pair[1]));
    // Every radial's position is kept; the volume's is the first radial's.
    let latitudes = column(&view, 0, sweep, "nexrad_latitude_deg").unwrap();
    for (ray, value) in latitudes.iter().enumerate() {
        let expected = if ray == 13 { latitude + 0.01 } else { latitude };
        assert!(same_f32(*value, expected), "ray {ray}: {value}");
    }
    assert_eq!(volume.location.latitude_deg, Some(f64::from(latitude)));
    // The second sweep's radials all agree: attributes, as in the file.
    assert!(
        sweep_attr(
            &view,
            1,
            &volume.sweeps[1],
            "nexrad_atmospheric_attenuation_db_per_km"
        )
        .is_some()
    );

    assert_eq!(volume.radar_calibration.len(), 2);
    let indices = sweep.ray_vars.calib_index.as_ref().unwrap();
    for (ray, index) in indices.iter().enumerate() {
        assert_eq!(*index, i32::from(ray == 7), "ray {ray}");
    }
    assert_eq!(
        volume.radar_calibration[0].base_1km_hc_dbz,
        Some(calibration)
    );
    assert_eq!(
        volume.radar_calibration[1].base_1km_hc_dbz,
        Some(calibration + 0.5)
    );
    assert!(
        volume.sweeps[1]
            .ray_vars
            .calib_index
            .as_ref()
            .unwrap()
            .iter()
            .all(|i| *i == 0)
    );

    // The volume keeps the first radial's identifier; the sweep has every
    // radial's.
    let identifiers = sweep
        .extra_vars
        .iter()
        .find(|v| &*v.name == "nexrad_radar_identifier")
        .expect("per-ray identifiers");
    let ArrayBuf::Text(texts) = &identifiers.values else {
        panic!("{:?}", identifiers.values);
    };
    assert_eq!(texts.len(), sweep.nrays());
    for (ray, text) in texts.iter().enumerate() {
        let expected = if ray == 9 { "XTLX" } else { "KTLX" };
        assert_eq!(&**text, expected, "ray {ray}");
    }
    let viewed = view
        .group("sweep_0")
        .unwrap()
        .variable("nexrad_radar_identifier")
        .unwrap()
        .values
        .materialize()
        .unwrap();
    assert_eq!(viewed, identifiers.values);
    // The reflectivity field keeps radial 0's TOVER as its attribute, and
    // every radial's as a per-ray variable.
    let field = sweep
        .fields
        .iter()
        .find(|f| f.name.to_string() == "DBZH")
        .unwrap();
    assert!(
        field
            .attrs
            .other
            .iter()
            .any(|(name, value)| &**name == "nexrad_tover_db"
                && value.as_f64().map(|v| v as f32) == Some(f32::from(tover) * 0.1))
    );
    let per_ray = column(&view, 0, sweep, "nexrad_tover_db_DBZH").unwrap();
    for (ray, value) in per_ray.iter().enumerate() {
        let raw = if ray == 11 { tover + 7 } else { tover };
        assert!(same_f32(*value, f32::from(raw) * 0.1), "ray {ray}: {value}");
    }
    assert!(column(&view, 0, sweep, "nexrad_tover_db_ZDR").is_none());
}
