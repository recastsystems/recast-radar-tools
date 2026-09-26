//! Storm attribute tables as typed rows: the tabular alphanumeric pages of
//! Storm Tracking Information (58), Hail Index (59), Mesocyclone (60),
//! Tornado Vortex Signature (61) and Mesocyclone Detection (141), and the
//! combined storm cell attribute table on the graphic alphanumeric pages of
//! Composite Reflectivity (35-38) and its contour product (39). The
//! stand-alone alphanumeric products that distribute the pages of 58, 59, 60
//! and 61 on their own (101, 102, 103 and 104, section 3.3.2; NCEI archive
//! 1993-2001) give the same tables.
//!
//! Two generations of layouts:
//!
//! - The SCIT, HDA and TDA algorithms (ICD 2620003AE Appendix C tabular
//!   formats and Appendix B Format III): [`Level3Product::storm_tracking`],
//!   [`Level3Product::hail_index`], [`Level3Product::tvs_table`],
//!   [`Level3Product::mesocyclone_detections`] and
//!   [`Level3Product::cell_attributes`]. Observed in the NCEI archive from
//!   1997 (SCIT and HDA, KLZK 1997-03-01) and 1999 (TDA, KTLX 1999-05-03).
//! - The storm series, hail and TVS algorithms they replaced, whose tables
//!   no ICD revision obtained describes (`docs/level3/reference.md` section
//!   4.3; read from the column headings of real products, 1995-1997):
//!   [`Level3Product::legacy_storm_tracking`] (the `STORM TRACKING` page with
//!   X/Y forecasts), [`Level3Product::legacy_hail_index`] (hail status and
//!   weights), [`Level3Product::legacy_tvs_table`] (TVS, mesocyclone and
//!   storm IDs with orientation and rotation) and
//!   [`Level3Product::legacy_cell_attributes`] (the 1995-1996 combined
//!   attribute table). The Mesocyclone product (60) has one table in both
//!   generations, with a TVS ID column until 1997
//!   ([`Level3Product::mesocyclone_table`]), and the combined attribute table
//!   of 1997-2003 has a `MESO` column (`NO`/`YES`, then the Mesocyclone
//!   algorithm's feature type) where later tables have `MDA`
//!   ([`CellAttribute::meso_feature`]).
//!
//! A row is read from the whitespace-separated fields of its line after
//! joining the spaces the RPG puts inside a field (`215/ 91`, `< 0.8`,
//! `NO DATA`, `NO DAT`). Adaptation data pages are not read; lines of a
//! table page that do not parse as a row are kept in the table's `unparsed`
//! list. The pages themselves stay in [`crate::TabularAlphanumeric::pages`]
//! and [`crate::GraphicAlphanumeric::pages`].

use crate::Level3Product;
use crate::packets::Packet;

/// An azimuth (degrees) and range (nautical miles) from the radar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AzRan {
    /// Azimuth, degrees.
    pub azimuth_deg: f32,
    /// Range, nautical miles.
    pub range_nm: f32,
}

/// A value with its display qualifier: `<` (at or below: the feature
/// extends to the lowest elevation) or `>` (at or above: it extends to the
/// highest elevation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Qualified {
    /// The value.
    pub value: f32,
    /// `Some('<')`, `Some('>')` or `None`.
    pub qualifier: Option<char>,
}

/// Storm motion: a new cell (no motion yet) or a direction and speed.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Motion {
    /// `NEW`: first detected this volume scan.
    New,
    /// Direction the storm moves from (degrees) and speed (knots).
    Moving {
        /// Direction, degrees.
        direction_deg: f32,
        /// Speed, knots.
        speed_kt: f32,
    },
}

/// Storm Tracking Information table (product 58).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StormTracking {
    /// Average storm speed, knots (`AVG SPEED`).
    pub average_speed_kt: Option<f32>,
    /// Average storm direction, degrees (`AVG DIRECTION`).
    pub average_direction_deg: Option<f32>,
    /// One row per storm cell, in table order.
    pub cells: Vec<StormTrack>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm cell of the Storm Tracking Information table.
#[derive(Debug, Clone, PartialEq)]
pub struct StormTrack {
    /// Storm cell identifier.
    pub id: String,
    /// Current position.
    pub position: AzRan,
    /// Movement.
    pub motion: Motion,
    /// Forecast positions at 15, 30, 45 and 60 minutes; `None` for `NO DATA`.
    pub forecasts: [Option<AzRan>; 4],
    /// Forecast error, nautical miles.
    pub forecast_error_nm: f32,
    /// Mean forecast error, nautical miles.
    pub mean_error_nm: f32,
}

/// Hail Index table (product 59).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HailIndex {
    /// One row per storm cell, in table order.
    pub cells: Vec<HailCell>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm cell of the Hail Index table. `None` means `UNKNOWN` (cell
/// beyond the hail algorithm's range).
#[derive(Debug, Clone, PartialEq)]
pub struct HailCell {
    /// Storm cell identifier.
    pub id: String,
    /// Probability of severe hail, percent.
    pub posh_percent: Option<u8>,
    /// Probability of hail, percent.
    pub poh_percent: Option<u8>,
    /// Maximum expected hail size, inches (`<0.50`: below the smallest
    /// size reported).
    pub max_size_in: Option<Qualified>,
}

/// Tornado Vortex Signature table (product 61).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TvsTable {
    /// One row per feature, TVS before ETVS as the RPG ranks them.
    pub features: Vec<TvsFeature>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One feature of the TVS table.
#[derive(Debug, Clone, PartialEq)]
pub struct TvsFeature {
    /// `TVS` or `ETVS`.
    pub kind: String,
    /// Associated storm cell identifier (`??` when none).
    pub storm_id: String,
    /// Base position.
    pub position: AzRan,
    /// Average delta velocity, knots.
    pub average_dv_kt: f32,
    /// Low-level delta velocity, knots.
    pub low_level_dv_kt: f32,
    /// Maximum delta velocity, knots.
    pub max_dv_kt: f32,
    /// Height of the maximum delta velocity, kft.
    pub max_dv_height_kft: f32,
    /// Depth, kft.
    pub depth_kft: Qualified,
    /// Base, kft.
    pub base_kft: Qualified,
    /// Top, kft.
    pub top_kft: Qualified,
    /// Maximum shear, 10^-3 s^-1.
    pub max_shear: f32,
    /// Height of the maximum shear, kft.
    pub max_shear_height_kft: f32,
}

/// Mesocyclone Detection table (product 141).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MesocycloneDetections {
    /// One row per circulation, strongest first.
    pub circulations: Vec<Circulation>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One circulation of the Mesocyclone Detection table.
#[derive(Debug, Clone, PartialEq)]
pub struct Circulation {
    /// Circulation identifier.
    pub id: u32,
    /// Position.
    pub position: AzRan,
    /// Strength rank.
    pub strength_rank: u32,
    /// Strength rank type letter written after the rank: `L` or `S`
    /// (2620003AE DMD parameters: `' '`, `'L'` or `'S'`); `None` for none.
    pub strength_rank_type: Option<char>,
    /// Associated storm cell identifier.
    pub storm_id: String,
    /// Low-level rotational velocity, knots.
    pub low_level_rv_kt: f32,
    /// Low-level delta velocity, knots.
    pub low_level_dv_kt: f32,
    /// Base, kft.
    pub base_kft: Qualified,
    /// Depth, kft.
    pub depth_kft: Qualified,
    /// Depth relative to the storm depth, percent.
    pub storm_relative_depth_percent: f32,
    /// Height of the maximum rotational velocity, kft.
    pub max_rv_height_kft: f32,
    /// Maximum rotational velocity, knots.
    pub max_rv_kt: f32,
    /// Associated with a TVS (`Y`).
    pub tvs: bool,
    /// Motion, when given.
    pub motion: Option<Motion>,
    /// Mesocyclone strength index.
    pub msi: u32,
}

/// Combined storm cell attribute table (graphic pages of products 35-39).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellAttributes {
    /// One row per storm cell, in table order.
    pub cells: Vec<CellAttribute>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm cell of the combined attribute table.
#[derive(Debug, Clone, PartialEq)]
pub struct CellAttribute {
    /// Storm cell identifier.
    pub id: String,
    /// Position.
    pub position: AzRan,
    /// `TVS` or `ETVS` (`YES` in the 1997 layout); `None` for `NONE` (`NO`).
    pub tvs: Option<String>,
    /// MDA strength rank (the `MDA` column); `None` for `NONE`, and for a
    /// table with a `MESO` column.
    pub mda_rank: Option<u32>,
    /// The `MESO` column of the tables before the MDA algorithm: the
    /// Mesocyclone algorithm's feature type (`MESO`, `3DCO` 3-D correlated
    /// shear, `UNCO` uncorrelated shear; 1999-2003), or `YES` (1997); `None`
    /// for `NONE` (`NO`) and for a table with an `MDA` column.
    pub meso_feature: Option<String>,
    /// Probability of severe hail, percent; `None` for `UNKNOWN`.
    pub posh_percent: Option<u8>,
    /// Probability of hail, percent; `None` for `UNKNOWN`.
    pub poh_percent: Option<u8>,
    /// Maximum expected hail size, inches (`<0.50`: below the smallest
    /// size reported); `None` for `UNKNOWN`.
    pub max_size_in: Option<Qualified>,
    /// Cell-based VIL, kg m-2.
    pub vil: f32,
    /// Maximum reflectivity, dBZ.
    pub max_dbz: f32,
    /// Height of the maximum reflectivity, kft.
    pub max_dbz_height_kft: f32,
    /// Storm top, kft (`>` when on the highest elevation).
    pub top_kft: Qualified,
    /// Forecast movement.
    pub motion: Motion,
}

/// Combined storm cell attribute table of 1995-1996 (graphic pages of
/// products 35-39 headed `STM ID AZ RAN TVS MESO HAIL DBZM HGT VLOW STM
/// TOP FCST MVMT MW VOL`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyCellAttributes {
    /// One row per storm, in table order.
    pub cells: Vec<LegacyCellAttribute>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm of the 1995-1996 combined attribute table. `VLOW` and `STM TOP`
/// equal the `LOW V` and `TOP` columns of the Storm Structure product (62)
/// of the same volume (KFWS 1995-05-17 23:04).
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyCellAttribute {
    /// Storm identifier (one or two characters).
    pub id: String,
    /// Position.
    pub position: AzRan,
    /// A TVS is associated (`YES`).
    pub tvs: bool,
    /// A mesocyclone is associated (`YES`).
    pub mesocyclone: bool,
    /// Hail status: `POS` (positive), `PRO` (probable) or `NEG` (negative)
    /// in the real products.
    pub hail: String,
    /// Maximum reflectivity, dBZ (`DBZM`).
    pub max_dbz: f32,
    /// Height of the maximum reflectivity, kft (`HGT`).
    pub max_dbz_height_kft: f32,
    /// Low-level velocity, knots (`VLOW`, the Storm Structure product's `LOW
    /// V`).
    pub low_level_velocity_kt: f32,
    /// Storm top, kft (`STM TOP`; `>` when on the highest elevation).
    pub top_kft: Qualified,
    /// Forecast movement (`FCST MVMT`, direction and speed).
    pub motion: Motion,
    /// Mass-weighted volume (`MW VOL`; the page gives no unit).
    pub mass_weighted_volume: f32,
}

/// A position east (`x`) and north (`y`) of the radar, nautical miles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xy {
    /// East of the radar, nautical miles.
    pub x_nm: f32,
    /// North of the radar, nautical miles.
    pub y_nm: f32,
}

/// Storm Tracking table of a product 58 from before the SCIT algorithm (the
/// 1995-1996 `STORM TRACKING` page: two lines per storm, the second holding
/// the `Y` components).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyStormTracking {
    /// One row per storm, in table order.
    pub cells: Vec<LegacyStormTrack>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm of the legacy Storm Tracking table.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyStormTrack {
    /// Storm identifier (one or two characters: `12`, `W`).
    pub id: String,
    /// Current position (`AZRAN`).
    pub position: AzRan,
    /// Movement (`MOVEMENT`, direction the storm moves from and speed).
    pub motion: Motion,
    /// East component of the storm speed, knots (`SPEED X`).
    pub speed_x_kt: f32,
    /// North component of the storm speed, knots (`SPEED Y`).
    pub speed_y_kt: f32,
    /// Forecast positions at 15, 30, 45 and 60 minutes; `None` for `NO DAT`.
    pub forecasts: [Option<Xy>; 4],
    /// Forecast error, nautical miles (`FORCAST ERR`).
    pub forecast_error_nm: f32,
    /// Mean forecast error, nautical miles (`MEAN`).
    pub mean_error_nm: f32,
    /// Track variance in `x`, nautical miles (`TRACKVAR X`).
    pub track_variance_x_nm: f32,
    /// Track variance in `y`, nautical miles (`TRACKVAR Y`).
    pub track_variance_y_nm: f32,
}

/// Hail table of a product 59 from before the HDA algorithm (1995-1996:
/// hail status, weights, confidence and score per storm).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyHailIndex {
    /// Average storm speed, knots (`AVG. SPEED`).
    pub average_speed_kt: Option<f32>,
    /// Average storm direction, degrees (`AVG. DIRECTION`).
    pub average_direction_deg: Option<f32>,
    /// One row per storm, in table order.
    pub cells: Vec<LegacyHailCell>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One storm of the legacy hail table.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyHailCell {
    /// Storm identifier (one or two characters).
    pub id: String,
    /// Hail status: `POSITIVE`, `PROBABLE` or `NONE` in the real products.
    pub status: String,
    /// Positive hail weight (`HAIL-WEIGHT POSITIVE`).
    pub positive_weight: f32,
    /// Probable hail weight (`HAIL-WEIGHT PROBABLE`).
    pub probable_weight: f32,
    /// Confidence factor.
    pub confidence_factor: f32,
    /// Hail score.
    pub score: f32,
}

/// Mesocyclone table of a product 60.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MesocycloneTable {
    /// One row per feature, in table order.
    pub features: Vec<MesocycloneFeature>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One feature of the Mesocyclone table.
#[derive(Debug, Clone, PartialEq)]
pub struct MesocycloneFeature {
    /// Feature identifier.
    pub feature_id: u32,
    /// Storm identifier.
    pub storm_id: String,
    /// Feature type: `MESO` (mesocyclone), `3DC SHR` (3-D correlated shear)
    /// or `UNC SHR` (uncorrelated shear) in the real products.
    pub feature_type: String,
    /// The `TVS ID` column of the 1995-1997 layout, given for `MESO` rows (0:
    /// no TVS); `None` when the column is blank or absent.
    pub tvs_id: Option<u32>,
    /// Base, kft.
    pub base_kft: f32,
    /// Top, kft.
    pub top_kft: f32,
    /// Position.
    pub position: AzRan,
    /// Height (`HGT`), kft.
    pub height_kft: f32,
    /// Radial diameter, nautical miles.
    pub radial_diameter_nm: f32,
    /// Azimuthal diameter, nautical miles.
    pub azimuthal_diameter_nm: f32,
    /// Shear, 10^-3 s^-1.
    pub shear: f32,
}

/// Tornado Vortex Signature table of a product 61 from before the TDA
/// algorithm (1996-1997: the `TORNADO VORTEX SIG` page).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyTvsTable {
    /// One row per TVS, in table order.
    pub features: Vec<LegacyTvs>,
    /// Table lines that do not parse as a row.
    pub unparsed: Vec<String>,
}

/// One TVS of the legacy TVS table.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyTvs {
    /// TVS identifier.
    pub tvs_id: u32,
    /// Mesocyclone feature identifier (the product 60 feature whose `TVS ID`
    /// is this TVS).
    pub meso_id: u32,
    /// Storm identifier.
    pub storm_id: String,
    /// Base height, kft.
    pub base_height_kft: f32,
    /// Base position.
    pub base_position: AzRan,
    /// Height of the maximum shear, kft.
    pub max_shear_height_kft: f32,
    /// Position of the maximum shear.
    pub max_shear_position: AzRan,
    /// Shear, 10^-3 s^-1.
    pub shear: f32,
    /// Orientation (`ORI`), degrees.
    pub orientation_deg: f32,
    /// Rotation (`ROT`), radians.
    pub rotation_rad: f32,
}

/// The fields of a table line with the RPG's inner spaces removed.
fn fields(line: &str) -> Vec<String> {
    let mut text = line.replace("NO DATA", "NODATA").replace("NO DAT", "NODAT");
    for (from, to) in [("/ ", "/"), ("< ", "<"), ("> ", ">")] {
        while text.contains(from) {
            text = text.replace(from, to);
        }
    }
    text.split_whitespace().map(str::to_owned).collect()
}

fn number(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|v| v.is_finite())
}

fn pair(text: &str) -> Option<(f32, f32)> {
    let (a, b) = text.split_once('/')?;
    Some((number(a)?, number(b)?))
}

fn azran(text: &str) -> Option<AzRan> {
    pair(text).map(|(azimuth_deg, range_nm)| AzRan {
        azimuth_deg,
        range_nm,
    })
}

fn motion(text: &str) -> Option<Motion> {
    if text == "NEW" {
        return Some(Motion::New);
    }
    pair(text).map(|(direction_deg, speed_kt)| Motion::Moving {
        direction_deg,
        speed_kt,
    })
}

fn qualified(text: &str) -> Option<Qualified> {
    let (qualifier, rest) = match text.as_bytes().first() {
        Some(b'<') => (Some('<'), &text[1..]),
        Some(b'>') => (Some('>'), &text[1..]),
        _ => (None, text),
    };
    Some(Qualified {
        value: number(rest)?,
        qualifier,
    })
}

fn percent(text: &str) -> Option<Option<u8>> {
    if text == "UNKNOWN" {
        return Some(None);
    }
    text.parse::<u8>().ok().map(Some)
}

fn storm_id(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    (bytes.len() == 2
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'?'))
    .then(|| text.to_owned())
}

/// Lines of the tabular pages whose text contains `title` (case
/// insensitive), after the column heading lines.
fn table_lines<'a>(product: &'a Level3Product, title: &str) -> Option<Vec<&'a str>> {
    let tabular = product.tabular.as_ref()?;
    let title = title.to_ascii_uppercase();
    let mut lines = Vec::new();
    let mut found = false;
    for page in &tabular.pages {
        if !page
            .lines
            .iter()
            .any(|line| line.to_ascii_uppercase().contains(&title))
        {
            continue;
        }
        found = true;
        lines.extend(page.lines.iter().map(String::as_str));
    }
    found.then_some(lines)
}

/// Whether a line is blank or a heading of the table (no digit-bearing
/// position field).
fn is_heading(fields: &[String]) -> bool {
    fields.is_empty()
        || !fields
            .iter()
            .any(|f| f.contains('/') && f.bytes().any(|b| b.is_ascii_digit()))
}

impl Level3Product {
    /// The Storm Tracking Information table of a product 58 or 101, or
    /// `None` for another product or when the product has no such page.
    pub fn storm_tracking(&self) -> Option<StormTracking> {
        if !self.carries_table(58) {
            return None;
        }
        let lines = table_lines(self, "STORM POSITION/FORECAST")?;
        let mut table = StormTracking::default();
        for line in lines {
            let f = fields(line);
            if let Some(at) = f.iter().position(|w| w == "SPEED") {
                table.average_speed_kt = f.get(at + 1).and_then(|v| number(v));
            }
            if let Some(at) = f.iter().position(|w| w == "DIRECTION") {
                table.average_direction_deg = f.get(at + 1).and_then(|v| number(v));
            }
            if is_heading(&f) || line.contains("DATE/TIME") {
                continue;
            }
            match storm_track(&f) {
                Some(row) => table.cells.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The Hail Index table of a product 59 or 102, or `None`.
    pub fn hail_index(&self) -> Option<HailIndex> {
        if !self.carries_table(59) {
            return None;
        }
        let lines = table_lines(self, "PROBABILITY OF")?;
        let mut table = HailIndex::default();
        for line in lines {
            let f = fields(line);
            if f.len() != 4 || storm_id(&f[0]).is_none() {
                if f.len() == 4 && !line.contains("PROBABILITY") {
                    table.unparsed.push(line.to_owned());
                }
                continue;
            }
            let row = (|| {
                Some(HailCell {
                    id: storm_id(&f[0])?,
                    posh_percent: percent(&f[1])?,
                    poh_percent: percent(&f[2])?,
                    max_size_in: if f[3] == "UNKNOWN" {
                        None
                    } else {
                        Some(qualified(&f[3])?)
                    },
                })
            })();
            match row {
                Some(row) => table.cells.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The Tornado Vortex Signature table of a product 61 or 104, or `None`
    /// (also for a product of the legacy TVS algorithm, see
    /// [`Level3Product::legacy_tvs_table`]).
    pub fn tvs_table(&self) -> Option<TvsTable> {
        if !self.carries_table(61) || self.is_legacy_tvs() {
            return None;
        }
        let lines = table_lines(self, "Tornado Vortex Signature")?;
        let mut table = TvsTable::default();
        for line in lines {
            let f = fields(line);
            if !matches!(f.first().map(String::as_str), Some("TVS" | "ETVS")) {
                continue;
            }
            match tvs_feature(&f) {
                Some(row) => table.features.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The Mesocyclone Detection table of a product 141, or `None`.
    pub fn mesocyclone_detections(&self) -> Option<MesocycloneDetections> {
        if self.description.product_code != 141 {
            return None;
        }
        let lines = table_lines(self, "MESOCYCLONE DETECTION")?;
        let mut table = MesocycloneDetections::default();
        for line in lines {
            let f = fields(line);
            if f.first().and_then(|w| w.parse::<u32>().ok()).is_none() || is_heading(&f) {
                continue;
            }
            match circulation(&f) {
                Some(row) => table.circulations.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The combined storm cell attribute table on the graphic pages of a
    /// composite reflectivity product (35-38) or its contour product (39),
    /// or `None`.
    pub fn cell_attributes(&self) -> Option<CellAttributes> {
        if !matches!(self.description.product_code, 35..=39) {
            return None;
        }
        let graphic = self.graphic.as_ref()?;
        let mut table = CellAttributes::default();
        let mut found = false;
        for page in &graphic.pages {
            for packet in &page.packets {
                let Packet::Text(text) = packet else {
                    continue;
                };
                if text.text.contains("STM ID") {
                    if text.text.contains("MW VOL") {
                        // The 1995-1996 layout: `legacy_cell_attributes`.
                        return None;
                    }
                    found = true;
                    continue;
                }
                let f = fields(&text.text);
                if f.is_empty() {
                    continue;
                }
                match cell_attribute(&f) {
                    Some(row) => table.cells.push(row),
                    None => table.unparsed.push(text.text.clone()),
                }
            }
        }
        found.then_some(table)
    }

    /// The combined storm cell attribute table of 1995-1996 on the graphic
    /// pages of a product 35-39 (heading with `MW VOL`), or `None`.
    pub fn legacy_cell_attributes(&self) -> Option<LegacyCellAttributes> {
        if !matches!(self.description.product_code, 35..=39) {
            return None;
        }
        let graphic = self.graphic.as_ref()?;
        let mut table = LegacyCellAttributes::default();
        let mut found = false;
        for page in &graphic.pages {
            for packet in &page.packets {
                let Packet::Text(text) = packet else {
                    continue;
                };
                if text.text.contains("STM ID") {
                    if !text.text.contains("MW VOL") {
                        return None;
                    }
                    found = true;
                    continue;
                }
                let f = fields(&text.text);
                if f.is_empty() {
                    continue;
                }
                match legacy_cell_attribute(&f) {
                    Some(row) => table.cells.push(row),
                    None => table.unparsed.push(text.text.clone()),
                }
            }
        }
        found.then_some(table)
    }

    /// The Storm Tracking table of a product 58 or 101 from before the SCIT
    /// algorithm (a tabular page with the `TRACKVAR` heading), or `None`.
    pub fn legacy_storm_tracking(&self) -> Option<LegacyStormTracking> {
        if !self.carries_table(58) {
            return None;
        }
        let lines = table_lines(self, "TRACKVAR")?;
        let mut table = LegacyStormTracking::default();
        let mut pending: Option<(Vec<String>, &str)> = None;
        for line in lines {
            let f = fields(line);
            if f.len() == 10 && legacy_storm_id(&f[0]) && azran(&f[1]).is_some() {
                if let Some((_, previous)) = pending.replace((f, line)) {
                    table.unparsed.push(previous.to_owned());
                }
                continue;
            }
            if f.len() == 6 && f.iter().all(|v| v == "NODAT" || number(v).is_some()) {
                match pending.take() {
                    Some((first, first_line)) => match legacy_storm_track(&first, &f) {
                        Some(row) => table.cells.push(row),
                        None => {
                            table.unparsed.push(first_line.to_owned());
                            table.unparsed.push(line.to_owned());
                        }
                    },
                    None => table.unparsed.push(line.to_owned()),
                }
                continue;
            }
            if let Some((_, previous)) = pending.take() {
                table.unparsed.push(previous.to_owned());
            }
            if !(is_heading(&f) || line.contains("DATE/TIME")) {
                table.unparsed.push(line.to_owned());
            }
        }
        if let Some((_, previous)) = pending {
            table.unparsed.push(previous.to_owned());
        }
        Some(table)
    }

    /// The hail table of a product 59 or 102 from before the HDA algorithm
    /// (a tabular page with the `HAIL-WEIGHT` heading), or `None`.
    pub fn legacy_hail_index(&self) -> Option<LegacyHailIndex> {
        if !self.carries_table(59) {
            return None;
        }
        let lines = table_lines(self, "HAIL-WEIGHT")?;
        let mut table = LegacyHailIndex::default();
        for line in lines {
            let f = fields(line);
            if let Some(at) = f.iter().position(|w| w == "SPEED") {
                table.average_speed_kt = f.get(at + 1).and_then(|v| number(v));
            }
            if let Some(at) = f.iter().position(|w| w == "DIRECTION") {
                table.average_direction_deg = f.get(at + 1).and_then(|v| number(v));
            }
            if line.contains("DATE/TIME") || line.contains("AVG.") {
                continue;
            }
            // A row ends in a number; the headings end in words.
            if f.last().and_then(|v| number(v)).is_none() {
                continue;
            }
            match legacy_hail_cell(&f) {
                Some(row) => table.cells.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The Mesocyclone table of a product 60 or 103 (both layouts), or
    /// `None` for another product or when the product has no mesocyclone
    /// page. A
    /// product with no feature has only the adaptation page: its table is
    /// empty.
    pub fn mesocyclone_table(&self) -> Option<MesocycloneTable> {
        if !self.carries_table(60) {
            return None;
        }
        let lines = table_lines(self, "MESOCYCLONE")?;
        let mut table = MesocycloneTable::default();
        let mut in_table = false;
        for line in lines {
            let f = fields(line);
            if line.contains("DIAM(NM)") {
                in_table = true;
                continue;
            }
            if line.contains("ADAPTATION") {
                in_table = false;
            }
            if !in_table || f.get(1).map(String::as_str) != Some("-") {
                continue;
            }
            match mesocyclone_feature(&f) {
                Some(row) => table.features.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// The Tornado Vortex Signature table of a product 61 or 104 from before
    /// the TDA algorithm, or `None` for another product or a TDA product. A
    /// legacy product with no TVS has only the adaptation page (`SEARCH
    /// PERCENTAGE`): its table is empty.
    pub fn legacy_tvs_table(&self) -> Option<LegacyTvsTable> {
        if !self.carries_table(61) || !self.is_legacy_tvs() {
            return None;
        }
        let mut table = LegacyTvsTable::default();
        for line in table_lines(self, "MAX SHEAR HGT").unwrap_or_default() {
            let f = fields(line);
            if f.first().and_then(|w| w.parse::<u32>().ok()).is_none() {
                continue;
            }
            match legacy_tvs(&f) {
                Some(row) => table.features.push(row),
                None => table.unparsed.push(line.to_owned()),
            }
        }
        Some(table)
    }

    /// Whether this product carries the tabular pages of graphic product
    /// `graphic`: it is that product, or the stand-alone alphanumeric product
    /// that distributes the same pages on their own (ICD 2620001 Table III:
    /// 101 Storm Track, 102 Hail Index, 103 Mesocyclone and 104 TVS
    /// Alphanumeric Block, for products 58, 59, 60 and 61).
    fn carries_table(&self, graphic: i16) -> bool {
        let code = self.description.product_code;
        code == graphic
            || match graphic {
                58 => code == 101,
                59 => code == 102,
                60 => code == 103,
                61 => code == 104,
                _ => false,
            }
    }

    /// Whether a product 61 or 104 is from the legacy TVS algorithm: its
    /// table page has the `MAX SHEAR HGT` heading or its adaptation page the
    /// `SEARCH PERCENTAGE` parameter.
    fn is_legacy_tvs(&self) -> bool {
        table_lines(self, "MAX SHEAR HGT").is_some()
            || table_lines(self, "SEARCH PERCENTAGE").is_some()
    }
}

/// A storm identifier of the legacy tables: one or two ASCII letters or
/// digits.
fn legacy_storm_id(text: &str) -> bool {
    (1..=2).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `ID AZ/RAN MOVEMENT SPEEDX F15X F30X F45X F60X ERR/MEAN TRACKVARX` and
/// `SPEEDY F15Y F30Y F45Y F60Y TRACKVARY`.
fn legacy_storm_track(first: &[String], second: &[String]) -> Option<LegacyStormTrack> {
    if first.len() != 10 || second.len() != 6 {
        return None;
    }
    let forecast = |x: &str, y: &str| -> Option<Option<Xy>> {
        match (x, y) {
            ("NODAT", "NODAT") => Some(None),
            _ => Some(Some(Xy {
                x_nm: number(x)?,
                y_nm: number(y)?,
            })),
        }
    };
    let (forecast_error_nm, mean_error_nm) = pair(&first[8])?;
    Some(LegacyStormTrack {
        id: first[0].clone(),
        position: azran(&first[1])?,
        motion: motion(&first[2])?,
        speed_x_kt: number(&first[3])?,
        speed_y_kt: number(&second[0])?,
        forecasts: [
            forecast(&first[4], &second[1])?,
            forecast(&first[5], &second[2])?,
            forecast(&first[6], &second[3])?,
            forecast(&first[7], &second[4])?,
        ],
        forecast_error_nm,
        mean_error_nm,
        track_variance_x_nm: number(&first[9])?,
        track_variance_y_nm: number(&second[5])?,
    })
}

/// `ID STATUS POSITIVE PROBABLE CONFIDENCE SCORE`.
fn legacy_hail_cell(f: &[String]) -> Option<LegacyHailCell> {
    let n = f.len();
    if n < 6 || !legacy_storm_id(&f[0]) {
        return None;
    }
    let status = f[1..n - 4].join(" ");
    if !status.bytes().all(|b| b.is_ascii_uppercase() || b == b' ') {
        return None;
    }
    Some(LegacyHailCell {
        id: f[0].clone(),
        status,
        positive_weight: number(&f[n - 4])?,
        probable_weight: number(&f[n - 3])?,
        confidence_factor: number(&f[n - 2])?,
        score: number(&f[n - 1])?,
    })
}

/// `FEATURE - STORM TYPE [TVS] BASE TOP AZ/RAN HGT RAD AZ SHEAR`, the type
/// one or two words (`MESO`, `3DC SHR`).
fn mesocyclone_feature(f: &[String]) -> Option<MesocycloneFeature> {
    if f.len() < 11 || f[1] != "-" || !legacy_storm_id(&f[2]) {
        return None;
    }
    let words = f[3..]
        .iter()
        .take_while(|w| number(w).is_none() && !w.contains('/'))
        .count();
    let rest = &f[3 + words..];
    let (tvs_id, rest) = match rest.len() {
        8 => (Some(rest[0].parse().ok()?), &rest[1..]),
        7 => (None, rest),
        _ => return None,
    };
    if words == 0 {
        return None;
    }
    Some(MesocycloneFeature {
        feature_id: f[0].parse().ok()?,
        storm_id: f[2].clone(),
        feature_type: f[3..3 + words].join(" "),
        tvs_id,
        base_kft: number(&rest[0])?,
        top_kft: number(&rest[1])?,
        position: azran(&rest[2])?,
        height_kft: number(&rest[3])?,
        radial_diameter_nm: number(&rest[4])?,
        azimuthal_diameter_nm: number(&rest[5])?,
        shear: number(&rest[6])?,
    })
}

/// `TVS MESO STORM BASEHGT AZ/RAN MAXSHEARHGT AZ/RAN SHEAR ORI ROT`.
fn legacy_tvs(f: &[String]) -> Option<LegacyTvs> {
    if f.len() != 10 || !legacy_storm_id(&f[2]) {
        return None;
    }
    Some(LegacyTvs {
        tvs_id: f[0].parse().ok()?,
        meso_id: f[1].parse().ok()?,
        storm_id: f[2].clone(),
        base_height_kft: number(&f[3])?,
        base_position: azran(&f[4])?,
        max_shear_height_kft: number(&f[5])?,
        max_shear_position: azran(&f[6])?,
        shear: number(&f[7])?,
        orientation_deg: number(&f[8])?,
        rotation_rad: number(&f[9])?,
    })
}

/// `ID AZ/RAN MOVEMENT|NEW F15 F30 F45 F60 ERR/MEAN`.
fn storm_track(f: &[String]) -> Option<StormTrack> {
    if f.len() != 8 {
        return None;
    }
    let forecast = |text: &str| -> Option<Option<AzRan>> {
        if text == "NODATA" {
            Some(None)
        } else {
            azran(text).map(Some)
        }
    };
    let (forecast_error_nm, mean_error_nm) = pair(&f[7])?;
    Some(StormTrack {
        id: f[0].clone(),
        position: azran(&f[1])?,
        motion: motion(&f[2])?,
        forecasts: [
            forecast(&f[3])?,
            forecast(&f[4])?,
            forecast(&f[5])?,
            forecast(&f[6])?,
        ],
        forecast_error_nm,
        mean_error_nm,
    })
}

/// `TYPE ID AZ/RAN AVGDV LLDV MXDV/HGT DEPTH BASE/TOP MXSHR/HGT`.
fn tvs_feature(f: &[String]) -> Option<TvsFeature> {
    if f.len() != 9 {
        return None;
    }
    let (max_dv_kt, max_dv_height_kft) = pair(&f[5])?;
    let (base, top) = f[7].split_once('/')?;
    let (max_shear, max_shear_height_kft) = pair(&f[8])?;
    Some(TvsFeature {
        kind: f[0].clone(),
        storm_id: f[1].clone(),
        position: azran(&f[2])?,
        average_dv_kt: number(&f[3])?,
        low_level_dv_kt: number(&f[4])?,
        max_dv_kt,
        max_dv_height_kft,
        depth_kft: qualified(&f[6])?,
        base_kft: qualified(base)?,
        top_kft: qualified(top)?,
        max_shear,
        max_shear_height_kft,
    })
}

/// `CIRC AZ/RAN SR STM RV DV BASE DEPTH STMREL% HGT MXRV TVS [MOTION] MSI`.
fn circulation(f: &[String]) -> Option<Circulation> {
    let motion_given = match f.len() {
        14 => true,
        13 => false,
        _ => return None,
    };
    let (motion_field, msi) = if motion_given {
        (Some(motion(&f[12])?), &f[13])
    } else {
        (None, &f[12])
    };
    let rank = f[2].trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let rank_type = f[2][rank.len()..].chars().next();
    if f[2].len() > rank.len() + 1 {
        return None;
    }
    Some(Circulation {
        id: f[0].parse().ok()?,
        position: azran(&f[1])?,
        strength_rank: rank.parse().ok()?,
        strength_rank_type: rank_type,
        storm_id: f[3].clone(),
        low_level_rv_kt: number(&f[4])?,
        low_level_dv_kt: number(&f[5])?,
        base_kft: qualified(&f[6])?,
        depth_kft: qualified(&f[7])?,
        storm_relative_depth_percent: number(&f[8])?,
        max_rv_height_kft: number(&f[9])?,
        max_rv_kt: number(&f[10])?,
        tvs: match f[11].as_str() {
            "Y" => true,
            "N" => false,
            _ => return None,
        },
        motion: motion_field,
        msi: msi.parse().ok()?,
    })
}

/// `ID AZ RAN TVS MESO HAIL DBZM HGT VLOW TOP DIR SPEED MWVOL`, `TVS` and
/// `MESO` `YES` or `NO`.
fn legacy_cell_attribute(f: &[String]) -> Option<LegacyCellAttribute> {
    if f.len() != 13 || !legacy_storm_id(&f[0]) {
        return None;
    }
    let yes_no = |text: &str| match text {
        "YES" => Some(true),
        "NO" => Some(false),
        _ => None,
    };
    Some(LegacyCellAttribute {
        id: f[0].clone(),
        position: AzRan {
            azimuth_deg: number(&f[1])?,
            range_nm: number(&f[2])?,
        },
        tvs: yes_no(&f[3])?,
        mesocyclone: yes_no(&f[4])?,
        hail: f[5]
            .bytes()
            .all(|b| b.is_ascii_uppercase())
            .then(|| f[5].clone())?,
        max_dbz: number(&f[6])?,
        max_dbz_height_kft: number(&f[7])?,
        low_level_velocity_kt: number(&f[8])?,
        top_kft: qualified(&f[9])?,
        motion: Motion::Moving {
            direction_deg: number(&f[10])?,
            speed_kt: number(&f[11])?,
        },
        mass_weighted_volume: number(&f[12])?,
    })
}

/// `ID AZ/RAN TVS MDA|MESO POSH/POH/SIZE|UNKNOWN VIL DBZM HT TOP MVMT|NEW`.
fn cell_attribute(f: &[String]) -> Option<CellAttribute> {
    if f.len() != 10 {
        return None;
    }
    let (posh_percent, poh_percent, max_size_in) = if f[4] == "UNKNOWN" {
        (None, None, None)
    } else {
        let mut parts = f[4].split('/');
        let posh = parts.next()?.parse().ok()?;
        let poh = parts.next()?.parse().ok()?;
        let size = qualified(parts.next()?)?;
        (Some(posh), Some(poh), Some(size))
    };
    let (mda_rank, meso_feature) = match f[3].as_str() {
        "NONE" | "NO" => (None, None),
        text => match text.parse::<u32>() {
            Ok(rank) => (Some(rank), None),
            Err(_) if text.bytes().all(|b| b.is_ascii_alphanumeric()) => {
                (None, Some(text.to_owned()))
            }
            Err(_) => return None,
        },
    };
    Some(CellAttribute {
        id: f[0].clone(),
        position: azran(&f[1])?,
        tvs: (!matches!(f[2].as_str(), "NONE" | "NO")).then(|| f[2].clone()),
        mda_rank,
        meso_feature,
        posh_percent,
        poh_percent,
        max_size_in,
        vil: number(&f[5])?,
        max_dbz: number(&f[6])?,
        max_dbz_height_kft: number(&f[7])?,
        top_kft: qualified(&f[8])?,
        motion: motion(&f[9])?,
    })
}
