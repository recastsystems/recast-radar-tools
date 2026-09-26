//! Symbol packets (ICD 2620001AD Figures 3-12 to 3-15a, `docs/level3/reference.md`
//! sections 6.3 and 6.4): mesocyclone (3), wind barb (4), vector arrow (5),
//! 3-D correlated shear (11), TVS (12), hail positive (13) and probable (14),
//! storm ID (15), HDA hail (19), point feature (20), cell trend data (21) and
//! volume scan times (22), SCIT past (23) and forecast (24) data, STI circle (25)
//! and ETVS (26).
//!
//! Every packet is `code, length` followed by `length` bytes. Packets 3-5, 11-15,
//! 19, 20, 25 and 26 hold repeated fixed-size records; a length that is not a
//! whole number of records is [`Level3Error::InvalidPacket`]. Values are kept as
//! the stored integers, in the units the ICD gives:
//!
//! - **Positions** (`i`, `j`): 1/4 km from the radar with I to the east and J to
//!   the north (section 3.3.3) in the Product Symbology Block; screen pixels in
//!   Graphic Alphanumeric Block pages. Packet 21 uses 1/8 km.
//! - **Radii**: 1/4 km for packets 3 and 11 and point feature types 1-4 and
//!   9-11; pixels for STI circles (25) and vector arrow lengths (5).
//!
//! Packets 21 and 22 (product 62 cell trend data) are not in the wave 1 plan's
//! family table; they are decoded here because the dispatcher routes them here.
//!
//! SCIT packets 23 and 24 nest other packets (the ICD allows 2, 6 and 25); they
//! are decoded with the same dispatcher as a layer, so nested codes without a
//! decoder stay [`Packet::Unknown`]. A SCIT packet nested inside another SCIT
//! packet is rejected, which bounds the recursion.

use std::cell::Cell;

use super::{Packet, decode_packets};
use crate::Level3Error;
use crate::budget::Budget;

/// A decoded symbol packet (3, 4, 5, 11-15, 19-26).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SymbolPacket {
    /// Packet 3: mesocyclones (Figure 3-14 sheet 1). A radius of 0 means none
    /// present, with position 0, 0.
    Mesocyclone(Vec<Circle>),
    /// Packet 4: wind barbs (Figure 3-13).
    WindBarbs(Vec<WindBarb>),
    /// Packet 5: vector arrows (Figure 3-12).
    VectorArrows(Vec<VectorArrow>),
    /// Packet 11: 3-D correlated shear (Figure 3-14 sheet 1).
    CorrelatedShear(Vec<Circle>),
    /// Packet 12: tornado vortex signatures (Figure 3-14 sheet 1).
    Tvs(Vec<Position>),
    /// Packet 13: hail positive, drawn filled (Figure 3-14 sheet 1).
    HailPositive(Vec<Position>),
    /// Packet 14: hail probable (Figure 3-14 sheet 1).
    HailProbable(Vec<Position>),
    /// Packet 15: storm IDs (Figure 3-14 sheet 2).
    StormIds(Vec<StormId>),
    /// Packet 19: HDA hail (Figure 3-14 sheets 2-3).
    HdaHail(Vec<HdaHail>),
    /// Packet 20: point features (Figure 3-14 sheet 4).
    PointFeatures(Vec<PointFeature>),
    /// Packet 21: cell trend data for one storm cell (Figure 3-15).
    CellTrend(CellTrend),
    /// Packet 22: cell trend volume scan times, minutes after midnight
    /// (Figure 3-15a).
    CellTrendTimes(VolumeList),
    /// Packet 23: SCIT past position data for one storm cell: its nested packets.
    ScitPast(Vec<Packet>),
    /// Packet 24: SCIT forecast position data for one storm cell: its nested packets.
    ScitForecast(Vec<Packet>),
    /// Packet 25: STI circles, the current storm cell position (Figure 3-14 sheet 2).
    StiCircles(Vec<Circle>),
    /// Packet 26: elevated tornado vortex signatures (Figure 3-14 sheet 1).
    Etvs(Vec<Position>),
}

impl SymbolPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        match self {
            Self::Mesocyclone(_) => 3,
            Self::WindBarbs(_) => 4,
            Self::VectorArrows(_) => 5,
            Self::CorrelatedShear(_) => 11,
            Self::Tvs(_) => 12,
            Self::HailPositive(_) => 13,
            Self::HailProbable(_) => 14,
            Self::StormIds(_) => 15,
            Self::HdaHail(_) => 19,
            Self::PointFeatures(_) => 20,
            Self::CellTrend(_) => 21,
            Self::CellTrendTimes(_) => 22,
            Self::ScitPast(_) => 23,
            Self::ScitForecast(_) => 24,
            Self::StiCircles(_) => 25,
            Self::Etvs(_) => 26,
        }
    }
}

/// A symbol position (packets 12, 13, 14 and 26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    /// I coordinate.
    pub i: i16,
    /// J coordinate.
    pub j: i16,
}

/// A circle symbol: mesocyclone (3), 3-D correlated shear (11) or STI circle (25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Circle {
    /// I coordinate of the center.
    pub i: i16,
    /// J coordinate of the center.
    pub j: i16,
    /// Radius: 1/4 km for packets 3 and 11, pixels for packet 25 (Figure 3-14
    /// sheet 3; units from ICD 2620001P).
    pub radius: i16,
}

/// A wind barb (packet 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindBarb {
    /// Color level, 1-5 (reflects the RMS of the computed velocity).
    pub color_level: i16,
    /// X coordinate where the barb starts.
    pub x: i16,
    /// Y coordinate where the barb starts.
    pub y: i16,
    /// Wind direction in degrees, 0-359, pointing into the wind.
    pub direction_deg: i16,
    /// Wind speed in knots, 0-195.
    pub speed_kt: i16,
}

/// A vector arrow (packet 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VectorArrow {
    /// I coordinate the arrow is centered on.
    pub i: i16,
    /// J coordinate the arrow is centered on.
    pub j: i16,
    /// Arrow direction in degrees, 0-359, pointing with the wind field.
    pub direction_deg: i16,
    /// Arrow length in pixels, 1-512.
    pub arrow_length: i16,
    /// Arrow head length in pixels, 1-512.
    pub head_length: i16,
}

/// A storm ID label (packet 15).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StormId {
    /// I coordinate.
    pub i: i16,
    /// J coordinate.
    pub j: i16,
    /// The two ID characters (`A0` to `Z9` in current products), each byte
    /// mapped to the Unicode code point of the same value. **Observed:** 1995
    /// products use other labels, e.g. ` W` and `48`.
    pub id: String,
}

/// An HDA hail symbol (packet 19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HdaHail {
    /// I coordinate.
    pub i: i16,
    /// J coordinate.
    pub j: i16,
    /// Probability of hail in percent, 0-100, or [`HdaHail::BEYOND_RANGE`].
    pub probability_of_hail: i16,
    /// Probability of severe hail in percent, 0-100, or [`HdaHail::BEYOND_RANGE`].
    pub probability_of_severe_hail: i16,
    /// Maximum expected hail size in inches, 0-4.
    pub max_hail_size_in: i16,
}

impl HdaHail {
    /// Probability value for a cell beyond the maximum range of the hail
    /// algorithm (Figure 3-14 sheet 3 Note 2).
    pub const BEYOND_RANGE: i16 = -999;

    /// True when either probability is [`HdaHail::BEYOND_RANGE`].
    pub fn is_beyond_range(&self) -> bool {
        self.probability_of_hail == Self::BEYOND_RANGE
            || self.probability_of_severe_hail == Self::BEYOND_RANGE
    }
}

/// A point feature (packet 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PointFeature {
    /// I coordinate.
    pub i: i16,
    /// J coordinate.
    pub j: i16,
    /// Point feature type, see [`PointFeatureKind`].
    pub feature_type: i16,
    /// Point feature attribute: the radius for types 1-4 and 9-11, type
    /// dependent otherwise.
    pub attribute: i16,
}

impl PointFeature {
    /// The feature type.
    pub fn kind(&self) -> PointFeatureKind {
        PointFeatureKind::from_code(self.feature_type)
    }

    /// The radius, for feature types 1-4 and 9-11.
    pub fn radius(&self) -> Option<i16> {
        matches!(self.feature_type, 1..=4 | 9..=11).then_some(self.attribute)
    }
}

/// Point feature types (Figure 3-14 sheet 4; types 2 and 4 from ICD 2620001P).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PointFeatureKind {
    /// 1: mesocyclone, extrapolated.
    MesocycloneExtrapolated,
    /// 2: 3-D correlated shear, extrapolated (legacy).
    CorrelatedShearExtrapolated,
    /// 3: mesocyclone, persistent, new or increasing.
    Mesocyclone,
    /// 4: 3-D correlated shear, persistent, new or increasing (legacy).
    CorrelatedShear,
    /// 5: TVS, extrapolated.
    TvsExtrapolated,
    /// 6: ETVS, extrapolated.
    EtvsExtrapolated,
    /// 7: TVS, persistent, new or increasing.
    Tvs,
    /// 8: ETVS, persistent, new or increasing.
    Etvs,
    /// 9: MDA circulation with strength rank >= 5 and a base at or below 1 km
    /// ARL or on the lowest elevation angle.
    MdaLowBase,
    /// 10: MDA circulation with strength rank >= 5 and a base above 1 km ARL,
    /// not on the lowest elevation angle.
    MdaElevatedBase,
    /// 11: MDA circulation with strength rank < 5.
    MdaWeak,
    /// Any other type code.
    Other(i16),
}

impl PointFeatureKind {
    /// The kind for a point feature type code.
    pub fn from_code(code: i16) -> Self {
        match code {
            1 => Self::MesocycloneExtrapolated,
            2 => Self::CorrelatedShearExtrapolated,
            3 => Self::Mesocyclone,
            4 => Self::CorrelatedShear,
            5 => Self::TvsExtrapolated,
            6 => Self::EtvsExtrapolated,
            7 => Self::Tvs,
            8 => Self::Etvs,
            9 => Self::MdaLowBase,
            10 => Self::MdaElevatedBase,
            11 => Self::MdaWeak,
            other => Self::Other(other),
        }
    }
}

/// Cell trend data for one storm cell (packet 21, product 62).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CellTrend {
    /// Cell ID, two characters (e.g. `K2`), each byte mapped to the Unicode code
    /// point of the same value.
    pub id: String,
    /// I coordinate at the latest volume scan, 1/8 km.
    pub i: i16,
    /// J coordinate at the latest volume scan, 1/8 km.
    pub j: i16,
    /// Trend series in file order.
    pub trends: Vec<Trend>,
}

/// One trend series of a [`CellTrend`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Trend {
    /// Trend code, 1-8, see [`TrendKind`].
    pub code: i16,
    /// Per-volume values.
    pub volumes: VolumeList,
}

impl Trend {
    /// The trend type, or `None` for a code outside 1-8.
    pub fn kind(&self) -> Option<TrendKind> {
        TrendKind::from_code(self.code)
    }
}

/// Cell trend types (Figure 3-15 sheet 2). Heights are stored in hundreds of
/// feet; a height above 700 had 1000 added to flag a cell top (base) found on
/// the highest (lowest) elevation scan. Probabilities of -999 are unknown (cell
/// beyond the maximum hail processing range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrendKind {
    /// 1: cell top, hundreds of feet.
    CellTop,
    /// 2: cell base, hundreds of feet.
    CellBase,
    /// 3: height of maximum reflectivity, hundreds of feet.
    MaxReflectivityHeight,
    /// 4: probability of hail, percent.
    ProbabilityOfHail,
    /// 5: probability of severe hail, percent.
    ProbabilityOfSevereHail,
    /// 6: cell-based VIL, kg/m2.
    CellVil,
    /// 7: maximum reflectivity, dBZ.
    MaxReflectivity,
    /// 8: centroid height, hundreds of feet.
    CentroidHeight,
}

impl TrendKind {
    /// The kind for a trend code, or `None` for a code outside 1-8.
    pub fn from_code(code: i16) -> Option<Self> {
        Some(match code {
            1 => Self::CellTop,
            2 => Self::CellBase,
            3 => Self::MaxReflectivityHeight,
            4 => Self::ProbabilityOfHail,
            5 => Self::ProbabilityOfSevereHail,
            6 => Self::CellVil,
            7 => Self::MaxReflectivity,
            8 => Self::CentroidHeight,
            _ => return None,
        })
    }
}

/// A circular list of per-volume values (packets 21 and 22), in file order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VolumeList {
    /// 1-based index into `values` of the latest volume scan (1-10).
    pub latest: u8,
    /// Values in stored (circular) order.
    pub values: Vec<i16>,
}

impl VolumeList {
    /// The values from oldest to latest: `values[latest..]` then
    /// `values[..latest]`. A `latest` of 0 or past the end leaves file order.
    pub fn chronological(&self) -> Vec<i16> {
        let split = usize::from(self.latest).min(self.values.len());
        let (older, newer) = self.values.split_at(split);
        newer.iter().chain(older).copied().collect()
    }

    /// The value of the latest volume scan, when `latest` is in range.
    pub fn latest_value(&self) -> Option<i16> {
        usize::from(self.latest)
            .checked_sub(1)
            .and_then(|index| self.values.get(index))
            .copied()
    }
}

/// Decodes one symbol packet (3, 4, 5, 11-15, 19-26). `bytes` is the complete
/// packet, starting with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    let body = bytes.get(4..).ok_or(Level3Error::Truncated {
        what: "symbol packet header",
        offset: 0,
        needed: 4,
        available: bytes.len(),
    })?;
    let symbol = match code {
        3 => SymbolPacket::Mesocyclone(records(code, body, circle, budget)?),
        4 => SymbolPacket::WindBarbs(records(code, body, wind_barb, budget)?),
        5 => SymbolPacket::VectorArrows(records(code, body, vector_arrow, budget)?),
        11 => SymbolPacket::CorrelatedShear(records(code, body, circle, budget)?),
        12 => SymbolPacket::Tvs(records(code, body, position, budget)?),
        13 => SymbolPacket::HailPositive(records(code, body, position, budget)?),
        14 => SymbolPacket::HailProbable(records(code, body, position, budget)?),
        15 => {
            // Each ID is a string of two characters, at most 4 bytes.
            budget.charge_bytes(4 * (body.len() / 6), "storm IDs")?;
            SymbolPacket::StormIds(records(code, body, storm_id, budget)?)
        }
        19 => SymbolPacket::HdaHail(records(code, body, hda_hail, budget)?),
        20 => SymbolPacket::PointFeatures(records(code, body, point_feature, budget)?),
        21 => SymbolPacket::CellTrend(cell_trend(body, budget)?),
        22 => SymbolPacket::CellTrendTimes(trend_times(body, budget)?),
        23 => SymbolPacket::ScitPast(scit(code, body, budget)?),
        24 => SymbolPacket::ScitForecast(scit(code, body, budget)?),
        25 => SymbolPacket::StiCircles(records(code, body, circle, budget)?),
        26 => SymbolPacket::Etvs(records(code, body, position, budget)?),
        _ => return Err(Level3Error::UnsupportedPacket(code)),
    };
    Ok(Packet::Symbol(symbol))
}

fn invalid(code: u16, reason: String) -> Level3Error {
    Level3Error::InvalidPacket { code, reason }
}

/// Splits `body` into `N`-byte records and converts each with `record`,
/// charging `budget` for the list.
fn records<const N: usize, T>(
    code: u16,
    body: &[u8],
    record: fn(&[u8; N]) -> T,
    budget: &mut Budget,
) -> Result<Vec<T>, Level3Error> {
    let (whole, rest) = body.as_chunks::<N>();
    if !rest.is_empty() {
        return Err(invalid(
            code,
            format!(
                "length {} is not a whole number of {N}-byte records",
                body.len()
            ),
        ));
    }
    budget.charge::<T>(whole.len(), "symbol records")?;
    Ok(whole.iter().map(record).collect())
}

fn int(hi: u8, lo: u8) -> i16 {
    i16::from_be_bytes([hi, lo])
}

/// Two 8-bit characters as a string (bytes map to the code points 0-255).
fn chars(c1: u8, c2: u8) -> String {
    [char::from(c1), char::from(c2)].into_iter().collect()
}

fn position(&[i0, i1, j0, j1]: &[u8; 4]) -> Position {
    Position {
        i: int(i0, i1),
        j: int(j0, j1),
    }
}

fn circle(&[i0, i1, j0, j1, r0, r1]: &[u8; 6]) -> Circle {
    Circle {
        i: int(i0, i1),
        j: int(j0, j1),
        radius: int(r0, r1),
    }
}

fn storm_id(&[i0, i1, j0, j1, c1, c2]: &[u8; 6]) -> StormId {
    StormId {
        i: int(i0, i1),
        j: int(j0, j1),
        id: chars(c1, c2),
    }
}

fn wind_barb(&[v0, v1, x0, x1, y0, y1, d0, d1, s0, s1]: &[u8; 10]) -> WindBarb {
    WindBarb {
        color_level: int(v0, v1),
        x: int(x0, x1),
        y: int(y0, y1),
        direction_deg: int(d0, d1),
        speed_kt: int(s0, s1),
    }
}

fn vector_arrow(&[i0, i1, j0, j1, d0, d1, a0, a1, h0, h1]: &[u8; 10]) -> VectorArrow {
    VectorArrow {
        i: int(i0, i1),
        j: int(j0, j1),
        direction_deg: int(d0, d1),
        arrow_length: int(a0, a1),
        head_length: int(h0, h1),
    }
}

fn hda_hail(&[i0, i1, j0, j1, p0, p1, s0, s1, m0, m1]: &[u8; 10]) -> HdaHail {
    HdaHail {
        i: int(i0, i1),
        j: int(j0, j1),
        probability_of_hail: int(p0, p1),
        probability_of_severe_hail: int(s0, s1),
        max_hail_size_in: int(m0, m1),
    }
}

fn point_feature(&[i0, i1, j0, j1, t0, t1, a0, a1]: &[u8; 8]) -> PointFeature {
    PointFeature {
        i: int(i0, i1),
        j: int(j0, j1),
        feature_type: int(t0, t1),
        attribute: int(a0, a1),
    }
}

/// A circular list: number of volumes (byte), latest pointer (byte), then one
/// INT*2 per volume. Returns the list and the bytes after it.
///
/// Figure 3-15a's field table types the two counts as INT*2, but its layout and
/// its length range (4 to 22 bytes for 1 to 10 times) put both in one halfword,
/// as Figure 3-15 does for packet 21.
fn volume_list<'a>(
    code: u16,
    bytes: &'a [u8],
    budget: &mut Budget,
) -> Result<(VolumeList, &'a [u8]), Level3Error> {
    let &[count, latest, ref rest @ ..] = bytes else {
        return Err(invalid(
            code,
            format!("{} bytes left, too few for a volume count", bytes.len()),
        ));
    };
    let Some((values, after)) = rest.split_at_checked(2 * usize::from(count)) else {
        return Err(invalid(
            code,
            format!(
                "{count} volumes need {} bytes, {} left",
                2 * usize::from(count),
                rest.len()
            ),
        ));
    };
    let (pairs, _) = values.as_chunks::<2>();
    budget.charge::<i16>(pairs.len(), "volume list")?;
    let values = pairs.iter().map(|&[hi, lo]| int(hi, lo)).collect();
    Ok((VolumeList { latest, values }, after))
}

/// Packet 21 body: cell ID, I, J, then trend code + volume list until the end.
fn cell_trend(body: &[u8], budget: &mut Budget) -> Result<CellTrend, Level3Error> {
    let &[c1, c2, i0, i1, j0, j1, ref series @ ..] = body else {
        return Err(invalid(
            21,
            format!(
                "length {} is shorter than the 6-byte cell header",
                body.len()
            ),
        ));
    };
    let mut trends = Vec::new();
    let mut rest = series;
    while !rest.is_empty() {
        let &[t0, t1, ref list @ ..] = rest else {
            return Err(invalid(21, "1 byte left, too few for a trend code".into()));
        };
        let (volumes, after) = volume_list(21, list, budget)?;
        let trend = Trend {
            code: int(t0, t1),
            volumes,
        };
        budget.push(&mut trends, trend, "cell trends")?;
        rest = after;
    }
    Ok(CellTrend {
        id: chars(c1, c2),
        i: int(i0, i1),
        j: int(j0, j1),
        trends,
    })
}

/// Packet 22 body: one volume list filling the packet.
fn trend_times(body: &[u8], budget: &mut Budget) -> Result<VolumeList, Level3Error> {
    let (times, after) = volume_list(22, body, budget)?;
    if !after.is_empty() {
        return Err(invalid(
            22,
            format!(
                "{} bytes after {} volume times",
                after.len(),
                times.values.len()
            ),
        ));
    }
    Ok(times)
}

thread_local! {
    /// Set while a SCIT packet's nested packets are being decoded on this thread.
    static IN_SCIT: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as decoding SCIT contents until dropped.
struct ScitScope;

impl ScitScope {
    /// `None` when this thread is already inside a SCIT packet.
    fn enter() -> Option<Self> {
        (!IN_SCIT.with(|flag| flag.replace(true))).then_some(ScitScope)
    }
}

impl Drop for ScitScope {
    fn drop(&mut self) {
        IN_SCIT.with(|flag| flag.set(false));
    }
}

/// Packets 23 and 24: the nested display packets filling the body.
fn scit(code: u16, body: &[u8], budget: &mut Budget) -> Result<Vec<Packet>, Level3Error> {
    let Some(_scope) = ScitScope::enter() else {
        return Err(invalid(
            code,
            "SCIT packet nested inside another SCIT packet".into(),
        ));
    };
    // Nested byte offsets are relative to the SCIT packet (its body starts at byte 4).
    // The product limit is not a fault of this packet: passed on as is.
    decode_packets(body, 4, budget).map_err(|error| match error {
        Level3Error::ProductTooLarge { .. } => error,
        error => invalid(
            code,
            format!("nested packet (byte offsets relative to this packet): {error}"),
        ),
    })
}
