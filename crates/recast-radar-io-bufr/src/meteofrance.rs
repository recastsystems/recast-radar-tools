//! Meteo-France polar radar files (originating centre 85, data category 6):
//! the PAG (Doppler) and PAM (dual-polarization) products, one elevation
//! per file.
//!
//! Each message of a file is one image: the radar description of WMO
//! sequence 3-21-011 and Meteo-France's 3-21-192/3-21-196 (site, antenna,
//! calibration, the elevation as 0-02-135), the image geometry (0-30-021
//! gates per ray, 0-30-022 rays, 0-55-233 gate length, 0-05-196 azimuth
//! step), then the pixel codes as one run of 0-30-001 under a data width
//! operator. What a code means:
//!
//! - reflectivity: the 3-21-193 table (0-30-001 code, 0-21-216 lower and
//!   upper bound), each code the lower bound of its class. Its 0-21-216 has
//!   a reference value of 0 and cannot hold the negative classes, which
//!   Meteo-France codes as 2 to 10 for -9 to -1 dBZ (code - 11, as from
//!   code 11 up); codes 0 and 1 are the noise floor (no echo);
//! - reflectivity standard deviation (PAG "sigma polaire"): a list of
//!   0-21-216 values, one per code;
//! - radial velocity: 0-49-241 (lowest velocity) plus 0-49-231 (step) per
//!   code;
//! - the PAM fields after reflectivity carry no table: the 16-bit one is
//!   PHIDP (code = degrees), the first 8-bit one RHOHV (0.3 + code / 100)
//!   and the second ZDR (-10 + code / 10 dB);
//!
//! and a code with every bit set (255 in 8 bits, 65535 in PHIDP's 16) is
//! no data in every field. Images that are not polar (a PAM file's
//! Cartesian product and its rain accumulation) are left out.
//!
//! Rows are rays from north, clockwise, at `step` degrees (0.5 for 720
//! rays, 1 for 360), each ray centred half a step past its start; the
//! first gate is centred half a gate past 0-06-194 (the range where
//! processing starts). A file holds no ray times: every ray of an image is
//! at the image's time (0-04-001 to 0-04-006).

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use recast_radar_core::model::{
    AttrValue, Field, FieldData, FieldName, FloatCoding, FollowMode, GateMapping, RangeCoord,
    Scalar, SourceFormat, Sweep, SweepMode, Volume,
};

use crate::BufrError;
use crate::decode::{Item, decode_message};
use crate::message::{Message, messages};
use crate::tables::Tables;

/// Meteo-France's originating centre.
const CENTRE: u16 = 85;
/// Table A data category: radar data.
const RADAR: u8 = 6;
/// Most rays and gates of one image.
const MAX_RAYS: usize = 4096;
const MAX_GATES: usize = 8192;

/// What an image holds.
#[derive(Clone, Debug, PartialEq)]
enum Quantity {
    /// Reflectivity, with its class table (code, lower, upper).
    Reflectivity(Vec<(u32, f64, f64)>),
    /// A value per code (PAG sigma).
    Table(Vec<f64>),
    /// Radial velocity: lowest value and step.
    Velocity { minimum: f64, step: f64 },
    /// A PAM field without a table, told apart by order and width.
    Untabled,
}

/// One decoded polar image.
#[derive(Clone, Debug)]
struct Image {
    time: DateTime<Utc>,
    station: Option<u32>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    altitude: Option<f64>,
    antenna_height: Option<f64>,
    elevation: f64,
    rays: usize,
    gates: usize,
    gate_m: f64,
    start_m: f64,
    frequency_hz: Option<f64>,
    beam_width_deg: Option<f64>,
    pulse_width_s: Option<f64>,
    antenna_gain_db: Option<f64>,
    quantity: Quantity,
    width: u32,
    codes: Vec<u32>,
}

/// Decode a Meteo-France PAG or PAM file (gzip members, or the expanded
/// BUFR) into one volume: a sweep per image geometry.
pub fn read_meteofrance_volume(bytes: &[u8]) -> Result<Volume, BufrError> {
    let compressed = !bytes.starts_with(crate::message::MAGIC);
    let expanded = crate::container::expand(bytes)?;
    let messages = messages(&expanded)?;
    let mut images = Vec::new();
    let mut skipped = 0usize;
    for message in &messages {
        if message.centre != CENTRE || message.category != RADAR {
            return Err(BufrError::Unsupported(format!(
                "BUFR from centre {} category {}: only Meteo-France (centre 85) radar data \
                 (category 6) is read",
                message.centre, message.category
            )));
        }
        match image(message)? {
            Some(image) => images.push(image),
            None => skipped += 1,
        }
    }
    if images.is_empty() {
        return Err(BufrError::Unsupported(
            "no polar radar image in the BUFR messages (Cartesian products are not read)".into(),
        ));
    }
    let mut volume = build_volume(images)?;
    volume.provenance.decode.message_count = messages.len();
    volume.provenance.decode.skipped_message_count = skipped;
    volume.provenance.compression = compressed.then(|| "gzip".to_owned());
    if let Some(first) = messages.first() {
        volume.provenance.source_version = Some(format!(
            "BUFR edition {}, master table {} version {}, local table version {}",
            first.edition, first.master_table, first.master_version, first.local_version
        ));
    }
    Ok(volume)
}

/// First value of `descriptor` in `items`.
fn first(items: &[Item], descriptor: u32) -> Option<f64> {
    items
        .iter()
        .find(|item| item.descriptor() == descriptor)?
        .number()
}

/// The polar image of one message, or `None` for a message that holds none.
fn image(message: &Message<'_>) -> Result<Option<Image>, BufrError> {
    let tables = Tables::for_message(message.centre, message.local_version);
    let items = decode_message(
        &message.descriptors,
        message.data,
        message.subsets,
        message.compressed,
        tables,
    )?;
    // Cartesian images give their corner distance (0-05-192, 0-06-192).
    let polar = items
        .iter()
        .all(|item| !matches!(item.descriptor(), 5192 | 6192));
    let (Some(elevation), Some(gates), Some(rays)) = (
        first(&items, 2135),
        first(&items, 30021),
        first(&items, 30022),
    ) else {
        return Ok(None);
    };
    if !polar {
        return Ok(None);
    }
    let (rays, gates) = (rays as usize, gates as usize);
    if !(1..=MAX_RAYS).contains(&rays) || !(1..=MAX_GATES).contains(&gates) {
        return Err(BufrError::Limit(format!(
            "image of {rays} rays and {gates} gates (at most {MAX_RAYS} and {MAX_GATES})"
        )));
    }
    let Some((width, codes)) = items.iter().rev().find_map(|item| match item {
        Item::Run {
            descriptor: 30001,
            width,
            codes,
            ..
        } if codes.len() == rays * gates => Some((*width, codes)),
        _ => None,
    }) else {
        return Err(BufrError::Format(format!(
            "image of {rays} x {gates} has no run of that many pixel codes"
        )));
    };
    let gate_m = first(&items, 55233)
        .or_else(|| first(&items, 25001))
        .filter(|gate| *gate > 0.0)
        .ok_or_else(|| BufrError::Format("image without a gate length (0-55-233)".into()))?;
    let time = image_time(&items)
        .or(message.time)
        .ok_or_else(|| BufrError::Format("image without a time (0-04-001 to 0-04-006)".into()))?;
    let quantity = quantity(&items);
    let station = match (first(&items, 1001), first(&items, 1002)) {
        (Some(block), Some(number)) => Some(block as u32 * 1000 + number as u32),
        _ => None,
    };
    Ok(Some(Image {
        time,
        station,
        latitude: first(&items, 5001),
        longitude: first(&items, 6001),
        altitude: first(&items, 7002),
        antenna_height: first(&items, 2102),
        elevation,
        rays,
        gates,
        gate_m,
        start_m: first(&items, 6194).unwrap_or(0.0),
        frequency_hz: first(&items, 2121),
        beam_width_deg: first(&items, 2106),
        pulse_width_s: first(&items, 2126),
        antenna_gain_db: first(&items, 2105),
        quantity,
        width,
        codes: codes.clone(),
    }))
}

/// The image's own time: the first 0-04-001 to 0-04-006 (the calibration
/// date comes later).
fn image_time(items: &[Item]) -> Option<DateTime<Utc>> {
    let year = first(items, 4001)? as i32;
    let month = first(items, 4002)? as u32;
    let day = first(items, 4003)? as u32;
    let hour = first(items, 4004)? as u32;
    let minute = first(items, 4005)? as u32;
    let second = first(items, 4006).unwrap_or(0.0) as u32;
    Some(
        NaiveDate::from_ymd_opt(year, month, day)?
            .and_hms_opt(hour, minute, second)?
            .and_utc(),
    )
}

fn quantity(items: &[Item]) -> Quantity {
    if let (Some(minimum), Some(step)) = (first(items, 49241), first(items, 49231)) {
        return Quantity::Velocity { minimum, step };
    }
    // 3-21-193: after an extended delayed replication factor, triples of
    // (0-30-001 code, 0-21-216 lower, 0-21-216 upper).
    if let Some(at) = items.iter().position(|item| item.descriptor() == 31002) {
        let mut classes = Vec::new();
        let mut rest = &items[at + 1..];
        while let [code, lower, upper, tail @ ..] = rest {
            if code.descriptor() != 30001
                || lower.descriptor() != 21216
                || upper.descriptor() != 21216
            {
                break;
            }
            if let (Some(code), Some(lower), Some(upper)) =
                (code.number(), lower.number(), upper.number())
            {
                classes.push((code as u32, lower, upper));
            }
            rest = tail;
        }
        if !classes.is_empty() {
            return Quantity::Reflectivity(classes);
        }
    }
    // A replicated 0-21-216: one value per code.
    if let Some(Item::Run {
        codes,
        scale,
        reference,
        width,
        ..
    }) = items.iter().find(|item| {
        matches!(
            item,
            Item::Run {
                descriptor: 21216,
                ..
            }
        )
    }) {
        let missing = (1u64 << *width) - 1;
        return Quantity::Table(
            codes
                .iter()
                .map(|&code| {
                    if u64::from(code) == missing {
                        f64::NAN
                    } else {
                        crate::decode::physical(u64::from(code), *reference, *scale)
                    }
                })
                .collect(),
        );
    }
    Quantity::Untabled
}

/// The field an image becomes: its name and the value of every code.
fn field_of(image: &Image, untabled_index: usize) -> (FieldName, Vec<f32>) {
    let size = 1usize << image.width.min(16);
    // All bits set: no data (255 in 8 bits, 65535 for the 16-bit PHIDP).
    let no_data = |code: usize| code + 1 == size;
    let mut lookup = vec![f32::NAN; size];
    let name = match &image.quantity {
        Quantity::Reflectivity(classes) => {
            for &(code, lower, upper) in classes {
                if let Some(slot) = lookup.get_mut(code as usize)
                    && upper > lower
                {
                    *slot = lower as f32;
                }
            }
            // The negative classes the table's 0-21-216 cannot hold.
            let first_class = classes.iter().find(|(_, lower, upper)| upper > lower);
            if first_class.is_some_and(|&(code, lower, _)| code == 11 && lower == 0.0) {
                for (code, slot) in lookup.iter_mut().enumerate().take(11).skip(2) {
                    *slot = code as f32 - 11.0;
                }
            }
            FieldName::Dbzh
        }
        Quantity::Table(values) => {
            for (slot, value) in lookup.iter_mut().zip(values) {
                *slot = *value as f32;
            }
            FieldName::parse("DBZH_SD")
        }
        Quantity::Velocity { minimum, step } => {
            for (code, slot) in lookup.iter_mut().enumerate() {
                *slot = (minimum + step * code as f64) as f32;
            }
            FieldName::Vradh
        }
        Quantity::Untabled if image.width > 8 => {
            for (code, slot) in lookup.iter_mut().enumerate().take(360) {
                *slot = code as f32;
            }
            FieldName::Phidp
        }
        Quantity::Untabled if untabled_index == 0 => {
            for (code, slot) in lookup.iter_mut().enumerate().take(79) {
                *slot = 0.3 + code as f32 / 100.0;
            }
            FieldName::Rhohv
        }
        Quantity::Untabled => {
            for (code, slot) in lookup.iter_mut().enumerate().take(200) {
                *slot = -10.0 + code as f32 / 10.0;
            }
            FieldName::Zdr
        }
    };
    for (code, slot) in lookup.iter_mut().enumerate() {
        if no_data(code) {
            *slot = f32::NAN;
        }
    }
    let values = image
        .codes
        .iter()
        .map(|&code| lookup.get(code as usize).copied().unwrap_or(f32::NAN))
        .collect();
    (name, values)
}

fn build_volume(images: Vec<Image>) -> Result<Volume, BufrError> {
    let reference = images
        .iter()
        .map(|image| image.time)
        .min()
        .unwrap_or_default();
    let head = &images[0];
    let mut volume = Volume::new(
        head.station
            .map(|id| format!("{id:05}"))
            .unwrap_or_default(),
        reference,
    );
    volume.provenance.source_format = SourceFormat::MeteoFranceBufr;
    volume.attrs.institution = Some("Meteo-France".to_owned());
    volume.attrs.wmo.id = head.station.map(|id| format!("{id:05}"));
    volume.attrs.wmo.originating_centre = Some(CENTRE);
    volume.location.latitude_deg = head.latitude;
    volume.location.longitude_deg = head.longitude;
    // The antenna: station height plus its height above the tower base.
    volume.location.altitude_m = head
        .altitude
        .map(|altitude| altitude + head.antenna_height.unwrap_or(0.0));
    if let Some(frequency) = head.frequency_hz {
        volume.radar_parameters.frequency_hz = vec![frequency];
    }
    volume.radar_parameters.beam_width_h_deg = head.beam_width_deg.map(|w| w as f32);
    volume.radar_parameters.beam_width_v_deg = head.beam_width_deg.map(|w| w as f32);
    volume.radar_parameters.pulse_width_s = head.pulse_width_s.map(|w| w as f32);
    volume.radar_parameters.antenna_gain_h_db = head.antenna_gain_db.map(|g| g as f32);

    // One sweep per geometry, in file order.
    let mut untabled = 0usize;
    for image in &images {
        let (name, values) = field_of(image, untabled);
        if image.quantity == Quantity::Untabled && image.width <= 8 {
            untabled += 1;
        }
        let existing = volume.sweeps.iter_mut().find(|sweep| {
            sweep.nrays() == image.rays
                && (f64::from(sweep.fixed_angle_deg) - image.elevation).abs() < 1e-6
                && matches!(sweep.range, RangeCoord::Uniform { spacing_m, ngates, .. }
                    if (spacing_m - image.gate_m).abs() < 1e-9 && ngates as usize == image.gates)
        });
        let sweep = match existing {
            Some(sweep) => sweep,
            None => {
                let sweep = new_sweep(volume.sweeps.len(), image, reference);
                volume.sweeps.push(sweep);
                volume.sweeps.last_mut().unwrap_or_else(|| unreachable!())
            }
        };
        if let Quantity::Velocity { minimum, .. } = image.quantity {
            sweep.ray_vars.nyquist_velocity_mps = Some(vec![minimum.abs() as f32; image.rays]);
        }
        let mut field = Field::new(
            name,
            GateMapping::IDENTITY,
            image.gates as u32,
            FieldData::F32 {
                values,
                coding: FloatCoding::default(),
            },
        );
        field.attrs.other.push((
            "meteofrance_code_bits".into(),
            AttrValue::Scalar(Scalar::U32(image.width)),
        ));
        sweep
            .add_field(field)
            .map_err(|err| BufrError::Format(format!("Meteo-France sweep field: {err}")))?;
    }
    volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
    Ok(volume)
}

fn new_sweep(number: usize, image: &Image, reference: DateTime<Utc>) -> Sweep {
    let vertical = image.elevation >= 89.0;
    let mode = if vertical {
        SweepMode::VerticalPointing
    } else {
        SweepMode::AzimuthSurveillance
    };
    let mut sweep = Sweep::new(number as u32, mode, image.elevation as f32);
    sweep.follow_mode = Some(FollowMode::None);
    sweep.range = RangeCoord::Uniform {
        first_center_m: image.start_m + image.gate_m / 2.0,
        spacing_m: image.gate_m,
        ngates: image.gates as u32,
    };
    let step = 360.0 / image.rays as f64;
    let time_s = (image.time - reference)
        .max(TimeDelta::zero())
        .num_milliseconds() as f64
        / 1000.0;
    sweep.reserve_rays(image.rays);
    for ray in 0..image.rays {
        sweep.push_ray(
            time_s,
            ((ray as f64 + 0.5) * step) as f32,
            image.elevation as f32,
        );
    }
    sweep
}
