//! VAD Wind Profile (product 48) winds as typed values.
//!
//! [`VadWindProfile::from_product`] reads a decoded product 48:
//!
//! - **Tabular winds**: the "VAD Algorithm Output" pages of the Tabular
//!   Alphanumeric Block (ICD 2620003AE section 12.2.4 and Appendix C Format X
//!   sheet 1). Each page has a title line `VAD Algorithm Output MM/DD/YY HH:MM`,
//!   two column heading lines and up to 14 rows of `ALT U V W DIR SPD RMS DIV
//!   SRNG ELEV` for the current volume scan. Products before these pages
//!   existed (the 1990s corpus files) have only the parameter pages.
//! - **Time-height display winds**: the wind barbs (packet 4) of the Product
//!   Symbology Block on the grid drawn with text packets (section 12.2 and
//!   Format IVA): a `TIME` label with the columns' `HHMM` labels on its row,
//!   and altitude labels in thousands of feet MSL. Each barb belongs to the
//!   nearest column label (observed: 14 pixels right of the label's I) and the
//!   nearest altitude label (observed: 4 pixels below the label's J).
//! - **Adaptable parameters** (Format X sheets 2 and 3): VAD analysis slant
//!   range, azimuth limits, passes, RMS, symmetry and data points thresholds,
//!   selected altitudes and optimum slant range.
//!
//! Column and table times carry only hours and minutes. They are anchored to
//! the volume scan time: the nearest of the same instant on the previous, same
//! and next day, preferring the earlier on a tie, so a profile from 23:55
//! before a 00:06 volume scan falls on the previous day.
//!
//! This module replaced `recast_radar_io_nexrad::level3_vwp` (since removed),
//! whose `decode_level3_vwp` chose tabular winds when there were any and
//! display winds otherwise; [`VadWindProfile::source`] and
//! [`VadWindProfile::profiles`] make the same choice. `tests/vwp.rs` documents
//! where the two differ.

use chrono::{DateTime, Duration, NaiveTime, Utc};

use crate::packets::symbols::SymbolPacket;
use crate::{Level3Error, Level3Product, Packet, TextPage};

/// Product code of the VAD Wind Profile.
pub const VWP_PRODUCT_CODE: i16 = 48;

/// Title that starts each tabular wind page.
const TABLE_TITLE: &str = "VAD Algorithm Output";
/// Lines at the top of a tabular wind page before the rows: title, column
/// names, units (2620003AE section 12.2.4).
const TABLE_HEADER_LINES: usize = 3;
/// Most distant column label a wind barb is assigned to, in pixels.
const MAX_COLUMN_DISTANCE: i32 = 30;
/// Most distant altitude label a wind barb is assigned to, in pixels.
const MAX_ALTITUDE_DISTANCE: i32 = 20;
/// Effective earth radius for beam heights: 4/3 of 6371 km.
const EFFECTIVE_EARTH_RADIUS_KM: f64 = 4.0 / 3.0 * 6371.0;
/// Kilometres per nautical mile.
const KM_PER_NM: f64 = 1.852;
/// Kilometres per foot.
const KM_PER_FT: f64 = 0.000_304_8;

/// Where a set of winds comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VwpSource {
    /// The VAD Algorithm Output pages of the Tabular Alphanumeric Block.
    Tabular,
    /// The wind barbs of the time-height display in the Product Symbology Block.
    Symbology,
}

/// One wind of a profile.
///
/// Tabular winds have every field except [`color_level`](Self::color_level);
/// display winds have the altitude, height, direction, speed and color level.
#[derive(Debug, Clone, PartialEq)]
pub struct VadWind {
    /// Altitude in feet above mean sea level: the `ALT` column (hundreds of
    /// feet) or the display's altitude label (thousands of feet).
    pub altitude_ft_msl: i32,
    /// Height above the radar in km. Tabular winds: the height of the beam at
    /// the slant range and elevation of the VAD analysis (4/3 earth radius
    /// model). Display winds: the altitude minus the radar height of the
    /// Product Description Block.
    pub height_above_radar_km: f64,
    /// Direction the wind blows from, in degrees, as in the file. Observed:
    /// the table prints north as 360 and the barb as 0.
    pub direction_deg: f64,
    /// Wind speed in knots.
    pub speed_kt: f64,
    /// Eastward component in m/s (`U`, tabular winds).
    pub u_m_per_s: Option<f64>,
    /// Northward component in m/s (`V`, tabular winds).
    pub v_m_per_s: Option<f64>,
    /// Upward component in cm/s (`W`); `None` where the table prints `NA`
    /// (all but constant slant range estimates).
    pub w_cm_per_s: Option<f64>,
    /// RMS between the velocity points and the fitted VAD curve in knots
    /// (`RMS`, tabular winds).
    pub rms_kt: Option<f64>,
    /// Divergence in 1/s (`DIV`, printed in 10^-3/s); `None` where the table
    /// prints `NA`.
    pub divergence_per_s: Option<f64>,
    /// Slant range of the VAD analysis in nautical miles (`SRNG`, tabular winds).
    pub slant_range_nm: Option<f64>,
    /// Elevation angle of the VAD analysis in degrees (`ELEV`, tabular winds).
    pub elevation_deg: Option<f64>,
    /// Color level of the display's wind barb (display winds): the RMS
    /// category of [`rms_range_kt`](Self::rms_range_kt).
    pub color_level: Option<i16>,
}

impl VadWind {
    /// The RMS range of a display wind's color level in knots, lower bound
    /// inclusive (2620003AE section 12.2.2): level 1 is 0-4, 2 is 4-8, 3 is
    /// 8-12, 4 is 12-16 and 5 is 16 or more (upper bound infinite). `None` for
    /// tabular winds and other levels.
    pub fn rms_range_kt(&self) -> Option<(f64, f64)> {
        match self.color_level? {
            level @ 1..=4 => {
                let low = 4.0 * f64::from(level - 1);
                Some((low, low + 4.0))
            }
            5 => Some((16.0, f64::INFINITY)),
            _ => None,
        }
    }
}

/// The winds at one time: a column of the time-height display or the tabular
/// winds of the current volume scan.
#[derive(Debug, Clone, PartialEq)]
pub struct WindProfile {
    /// The time as printed, `HHMM` (the table's `HH:MM` without the colon).
    pub label_hhmm: String,
    /// The time anchored to the volume scan time (see the module documentation).
    pub valid_time: DateTime<Utc>,
    /// Winds by increasing [`VadWind::height_above_radar_km`]; equal heights
    /// keep file order.
    pub winds: Vec<VadWind>,
}

/// Adaptable parameters of the VAD algorithm (2620003AE Appendix C Format X
/// sheets 2 and 3). `None` or empty when the product has no such page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VadParameters {
    /// VAD analysis slant range in nautical miles.
    pub analysis_slant_range_nm: Option<f64>,
    /// Beginning azimuth angle in degrees.
    pub beginning_azimuth_deg: Option<f64>,
    /// Ending azimuth angle in degrees.
    pub ending_azimuth_deg: Option<f64>,
    /// Number of passes.
    pub passes: Option<u32>,
    /// RMS threshold in knots.
    pub rms_threshold_kt: Option<f64>,
    /// Symmetry threshold in knots.
    pub symmetry_threshold_kt: Option<f64>,
    /// Data points threshold.
    pub data_points_threshold: Option<u32>,
    /// Selected altitudes in feet as printed. Observed: unused slots print -666.
    pub altitudes_selected_ft: Vec<i32>,
    /// Optimum slant range in nautical miles.
    pub optimum_slant_range_nm: Option<f64>,
}

/// The typed contents of a VAD Wind Profile product.
#[derive(Debug, Clone, PartialEq)]
pub struct VadWindProfile {
    /// Tabular winds, one profile per page title time (all pages of a product
    /// share one), newest first. Empty when the product has no VAD Algorithm
    /// Output page.
    pub tabular: Vec<WindProfile>,
    /// Time-height display columns that hold at least one wind barb, newest first.
    pub display: Vec<WindProfile>,
    /// Adaptable parameters.
    pub parameters: VadParameters,
}

impl VadWindProfile {
    /// Reads the winds and parameters of a decoded product 48.
    ///
    /// # Errors
    ///
    /// [`Level3Error::UnexpectedProduct`] when the product is not product 48.
    pub fn from_product(product: &Level3Product) -> Result<Self, Level3Error> {
        let description = &product.description;
        if description.product_code != VWP_PRODUCT_CODE {
            return Err(Level3Error::UnexpectedProduct {
                expected: VWP_PRODUCT_CODE,
                found: description.product_code,
            });
        }
        let reference = description.volume_scan_time;
        let pages = product
            .tabular
            .as_ref()
            .map_or(&[][..], |tabular| &tabular.pages[..]);
        let mut tabular = Vec::new();
        let mut parameters = VadParameters::default();
        for page in pages {
            match table_title_time(page) {
                Some(label) => add_winds(&mut tabular, label, table_winds(page), reference),
                None => read_parameters(page, &mut parameters),
            }
        }
        let display = display_winds(product, f64::from(description.height_ft))
            .into_iter()
            .fold(Vec::new(), |mut profiles, (label, wind)| {
                add_winds(&mut profiles, label, vec![wind], reference);
                profiles
            });
        Ok(Self {
            tabular: finish(tabular),
            display: finish(display),
            parameters,
        })
    }

    /// Where [`profiles`](Self::profiles) come from: tabular winds when there
    /// are any, otherwise display winds; `None` when the product has no wind.
    pub fn source(&self) -> Option<VwpSource> {
        if self.tabular.iter().any(|p| !p.winds.is_empty()) {
            Some(VwpSource::Tabular)
        } else if self.display.iter().any(|p| !p.winds.is_empty()) {
            Some(VwpSource::Symbology)
        } else {
            None
        }
    }

    /// The profiles of [`source`](Self::source), newest first: the tabular
    /// winds (full VAD diagnostics for the current volume scan) when there are
    /// any, otherwise the time-height display columns.
    pub fn profiles(&self) -> &[WindProfile] {
        match self.source() {
            Some(VwpSource::Tabular) => &self.tabular,
            Some(VwpSource::Symbology) => &self.display,
            None => &[],
        }
    }
}

/// Adds winds to the profile labelled `label`, creating it when needed.
fn add_winds(
    profiles: &mut Vec<WindProfile>,
    label: String,
    winds: Vec<VadWind>,
    reference: DateTime<Utc>,
) {
    if let Some(profile) = profiles.iter_mut().find(|p| p.label_hhmm == label) {
        profile.winds.extend(winds);
    } else if let Some(valid_time) = anchor_hhmm(&label, reference) {
        profiles.push(WindProfile {
            label_hhmm: label,
            valid_time,
            winds,
        });
    }
}

/// Sorts winds by height and profiles newest first; drops empty profiles.
fn finish(mut profiles: Vec<WindProfile>) -> Vec<WindProfile> {
    profiles.retain(|p| !p.winds.is_empty());
    for profile in &mut profiles {
        profile
            .winds
            .sort_by(|a, b| a.height_above_radar_km.total_cmp(&b.height_above_radar_km));
    }
    profiles.sort_by(|a, b| {
        b.valid_time
            .cmp(&a.valid_time)
            .then_with(|| b.label_hhmm.cmp(&a.label_hhmm))
    });
    profiles
}

/// `HHMM` from a tabular wind page title `VAD Algorithm Output MM/DD/YY HH:MM`.
fn table_title_time(page: &TextPage) -> Option<String> {
    let title = page.lines.first()?;
    let rest = &title[title.find(TABLE_TITLE)? + TABLE_TITLE.len()..];
    let time = rest.split_whitespace().find(|t| t.contains(':'))?;
    let (hh, mm) = time.split_once(':')?;
    let label = format!("{hh}{mm}");
    is_hhmm(&label).then_some(label)
}

/// Rows of a tabular wind page; lines that are not rows are skipped.
fn table_winds(page: &TextPage) -> Vec<VadWind> {
    page.lines
        .iter()
        .skip(TABLE_HEADER_LINES)
        .filter_map(|line| table_row(line))
        .collect()
}

fn table_row(line: &str) -> Option<VadWind> {
    let columns: Vec<&str> = line.split_whitespace().collect();
    let [alt, u, v, w, dir, spd, rms, div, srng, elev] = columns[..] else {
        return None;
    };
    let number = |s: &str| s.parse::<f64>().ok();
    let not_applicable = |s: &str| -> Option<Option<f64>> {
        if s == "NA" {
            Some(None)
        } else {
            number(s).map(Some)
        }
    };
    let slant_range_nm = number(srng)?;
    let elevation_deg = number(elev)?;
    Some(VadWind {
        altitude_ft_msl: alt.parse::<i32>().ok()?.checked_mul(100)?,
        height_above_radar_km: beam_height_km(slant_range_nm, elevation_deg),
        direction_deg: number(dir)?,
        speed_kt: number(spd)?,
        u_m_per_s: Some(number(u)?),
        v_m_per_s: Some(number(v)?),
        w_cm_per_s: not_applicable(w)?,
        rms_kt: Some(number(rms)?),
        divergence_per_s: not_applicable(div)?.map(|d| d * 1e-3),
        slant_range_nm: Some(slant_range_nm),
        elevation_deg: Some(elevation_deg),
        color_level: None,
    })
}

/// Height of the beam above the radar at a slant range and elevation, with the
/// 4/3 effective earth radius.
fn beam_height_km(slant_range_nm: f64, elevation_deg: f64) -> f64 {
    let r = EFFECTIVE_EARTH_RADIUS_KM;
    let s = slant_range_nm * KM_PER_NM;
    (r * r + s * s + 2.0 * r * s * elevation_deg.to_radians().sin()).sqrt() - r
}

/// Adaptable parameters on a page that is not a tabular wind page. The page's
/// lines are joined, so a value on the line after its label is still found.
fn read_parameters(page: &TextPage, parameters: &mut VadParameters) {
    let text = page.lines.join("\n").to_ascii_uppercase();
    let after = |label: &str| {
        text.find(label)
            .map(|at| text[at + label.len()..].split_whitespace())
    };
    let first = |label: &str| after(label).and_then(|mut tokens| tokens.next());
    let number = |label: &str| first(label).and_then(|t| t.parse::<f64>().ok());
    let count = |label: &str| first(label).and_then(|t| t.parse::<u32>().ok());
    fn set(slot: &mut Option<f64>, value: Option<f64>) {
        if value.is_some() {
            *slot = value;
        }
    }
    set(
        &mut parameters.analysis_slant_range_nm,
        number("VAD ANALYSIS SLANT RANGE"),
    );
    set(
        &mut parameters.beginning_azimuth_deg,
        number("BEGINNING AZIMUTH ANGLE"),
    );
    set(
        &mut parameters.ending_azimuth_deg,
        number("ENDING AZIMUTH ANGLE"),
    );
    set(&mut parameters.rms_threshold_kt, number("RMS THRESHOLD"));
    set(
        &mut parameters.symmetry_threshold_kt,
        number("SYMMETRY THRESHOLD"),
    );
    set(
        &mut parameters.optimum_slant_range_nm,
        number("OPTIMUM SLANT RANGE"),
    );
    if let Some(passes) = count("NUMBER OF PASSES") {
        parameters.passes = Some(passes);
    }
    if let Some(points) = count("DATA POINTS THRESHOLD") {
        parameters.data_points_threshold = Some(points);
    }
    if let Some(tokens) = after("ALTITUDES SELECTED") {
        let altitudes: Vec<i32> = tokens.map_while(|t| t.parse().ok()).collect();
        if !altitudes.is_empty() {
            parameters.altitudes_selected_ft = altitudes;
        }
    }
}

/// Wind barbs of the time-height display with their column label.
fn display_winds(product: &Level3Product, radar_height_ft: f64) -> Vec<(String, VadWind)> {
    let Some(symbology) = &product.symbology else {
        return Vec::new();
    };
    let packets = symbology.layers.iter().flatten();
    let texts: Vec<(i16, i16, &str)> = packets
        .clone()
        .filter_map(|packet| match packet {
            Packet::Text(text) => Some((text.i, text.j, text.text.trim())),
            _ => None,
        })
        .collect();
    let Some(&(_, time_row, _)) = texts.iter().find(|(_, _, text)| *text == "TIME") else {
        return Vec::new();
    };
    let columns: Vec<(i16, &str)> = texts
        .iter()
        .filter(|(_, j, text)| *j == time_row && is_hhmm(text))
        .map(|&(i, _, text)| (i, text))
        .collect();
    let altitudes: Vec<(i16, i32)> = texts
        .iter()
        .filter(|(_, j, _)| *j != time_row)
        .filter_map(|&(_, j, text)| {
            let kft = text.parse::<i32>().ok()?;
            (1..=99).contains(&kft).then_some((j, kft))
        })
        .collect();

    let mut winds = Vec::new();
    for packet in packets {
        let Packet::Symbol(SymbolPacket::WindBarbs(barbs)) = packet else {
            continue;
        };
        for barb in barbs {
            let column = nearest(&columns, barb.x, MAX_COLUMN_DISTANCE);
            let altitude = nearest(&altitudes, barb.y, MAX_ALTITUDE_DISTANCE);
            let (Some(label), Some(kft)) = (column, altitude) else {
                continue;
            };
            let altitude_ft_msl = kft * 1000;
            winds.push((
                label.to_string(),
                VadWind {
                    altitude_ft_msl,
                    height_above_radar_km: (f64::from(altitude_ft_msl) - radar_height_ft)
                        * KM_PER_FT,
                    direction_deg: f64::from(barb.direction_deg),
                    speed_kt: f64::from(barb.speed_kt),
                    u_m_per_s: None,
                    v_m_per_s: None,
                    w_cm_per_s: None,
                    rms_kt: None,
                    divergence_per_s: None,
                    slant_range_nm: None,
                    elevation_deg: None,
                    color_level: Some(barb.color_level),
                },
            ));
        }
    }
    winds
}

/// The value of the label nearest `position`, if within `max_distance`; ties
/// go to the label with the smaller coordinate.
fn nearest<T: Copy>(labels: &[(i16, T)], position: i16, max_distance: i32) -> Option<T> {
    labels
        .iter()
        .map(|&(at, value)| ((i32::from(at) - i32::from(position)).abs(), at, value))
        .filter(|(distance, ..)| *distance <= max_distance)
        .min_by_key(|&(distance, at, _)| (distance, at))
        .map(|(.., value)| value)
}

/// The time of a four-digit `HHMM` label.
fn hhmm_time(text: &str) -> Option<NaiveTime> {
    if text.len() != 4 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hour = text[..2].parse().ok()?;
    let minute = text[2..].parse().ok()?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// True for a four-digit `HHMM` time.
fn is_hhmm(text: &str) -> bool {
    hhmm_time(text).is_some()
}

/// The instant with the label's hour and minute nearest `reference` on the
/// previous, same or next day; the earlier one on a tie.
fn anchor_hhmm(label: &str, reference: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let time = hhmm_time(label)?;
    let day = reference.date_naive();
    [-1, 0, 1]
        .into_iter()
        .filter_map(|offset| day.checked_add_signed(Duration::days(offset)))
        .map(|date| date.and_time(time).and_utc())
        .min_by_key(|candidate| {
            let distance = candidate
                .signed_duration_since(reference)
                .num_seconds()
                .unsigned_abs();
            (distance, *candidate > reference)
        })
}
