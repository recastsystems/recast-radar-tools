//! Radar Coded Message (product 74): the coded groups of its three parts
//! (ICD 2620001P Appendix B, 2620001H Appendix B for the remark groups;
//! `docs/level3/reference.md` section 4.4). The unedited Radar Coded
//! Message (product 83, [`crate::packets::irm`]) carries the same message in
//! its Tabular Alphanumeric Block.
//!
//! [`RadarCodedMessage::parse`] reads the ASCII text of the message
//! ([`crate::TabularAlphanumeric::data`] with layout
//! [`crate::TabularLayout::RadarCodedMessage`]; [`Level3Product::radar_coded_message`]).
//! The text is a sequence of 70-character records; a group may continue in
//! the next record and records may be padded with spaces, so whitespace inside
//! the comma-separated lists is ignored.
//!
//! - **Header**: `cccc ROBUU sidd`: communications node (`1234`), product
//!   category (`ROBUU` unedited, `ROBEE` edited) and site identifier.
//! - **Part A** (reflectivity), `/NEXRAA ... /ENDAA`: site, date and time
//!   (`ddmmyytttt`), edit status, `RADNE` (no reportable intensities) and
//!   `RADOM` (radar down), `/MD` operational mode, `/SC` scan strategy, `/NI`
//!   number of intensities and the intensity groups, `/MT` maximum echo top,
//!   `/NCEN` storm centroids with their motion.
//! - **Part B** (VAD winds), `/NEXRBB ... /ENDBB`: site, date and time,
//!   `VADNA` (no VAD winds), and `hhhcdddfff` winds.
//! - **Part C** (remarks), `/NEXRCC ... /ENDCC`: site, date and time, `/NTVS`
//!   tornado vortex signatures, `/NMES` mesocyclones, `/NCEN` storm tops and
//!   hail, and the other remark groups kept verbatim.
//!
//! **Grid.** Locations are boxes of a local 25 x 25 grid of 1/4 LFM boxes
//! (rows and columns `A`-`Y`) of the national radar grid, each box split
//! into 4 x 4 boxes of the 1/16 LFM grid lettered `A`-`P`. The ICD's
//! Figure B-1 gives that lettering only as a picture; the real messages in the
//! corpus fix it: letters run down the columns (`A`-`D` the western column
//! from north to south, `E`-`H` the next), the only order in which their
//! intensity groups run north to south and west to east without overlapping,
//! as Appendix B requires. [`FineBox`] numbers the 100 x 100 fine boxes from
//! the north-west corner. Appendix B puts the radar in box `NM`; real
//! messages put it in `MM` (row 12, column 12), on national boxes with a
//! corner at the HRAP pole: [`crate::hrap::LocalGrid::radar_coded_message`]
//! gives every fine box its latitude and longitude, and
//! [`Level3Product::to_volume`] carries the intensity grid as a sweep on it
//! (the evidence is in [`crate::hrap`]).
//!
//! **Intensities.** Each group is a fine box and a list of levels for it and
//! the boxes east of it: a digit is a level (0-9; 1-6 within 124 nmi, 7 and 8
//! beyond it), a letter repeats the previous level that many more times
//! (`A` = 1). [`IntensityRun::levels`] holds them expanded; runs past the east
//! edge of the grid are cut there and counted in
//! [`PartA::cells_past_grid`].
//!
//! Groups that do not follow Appendix B are kept verbatim in
//! [`RadarCodedMessage::unparsed`]. Parsing fails only when the message
//! would allocate more than [`MAX_RCM_PARSED_BYTES`]
//! ([`Level3Error::ProductTooLarge`]): the text comes from untrusted input
//! and each short group becomes several allocations.

use chrono::{NaiveDate, NaiveDateTime};

use crate::blocks::TabularLayout;
use crate::budget::Budget;
use crate::{Level3Error, Level3Product};

/// Rows and columns of the local grid (`A`-`Y`).
pub const GRID_BOXES: u8 = 25;
/// Fine (1/16 LFM) boxes per row and column of the local grid.
pub const FINE_BOXES: u8 = GRID_BOXES * 4;

/// A decoded Radar Coded Message.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RadarCodedMessage {
    /// Communications node (`1234` in current messages).
    pub node: String,
    /// Product category: `ROBUU` (unedited) or `ROBEE` (edited).
    pub category: String,
    /// Site identifier of the header.
    pub site: String,
    /// Part A (reflectivity), when present.
    pub part_a: Option<PartA>,
    /// Part B (VAD winds), when present.
    pub part_b: Option<PartB>,
    /// Part C (remarks), when present.
    pub part_c: Option<PartC>,
    /// Groups that do not follow Appendix B, verbatim, prefixed with their
    /// part (`A: `, `B: `, `C: `, or none for text outside the parts).
    pub unparsed: Vec<String>,
}

/// Site, date and time line of a part (`/NEXRxx sidd ddmmyytttt`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartHeader {
    /// Site identifier.
    pub site: String,
    /// Date and time to the minute (UTC); `None` when not a valid
    /// `ddmmyytttt` (years 70-99 are 19xx, 00-69 20xx).
    pub time: Option<NaiveDateTime>,
    /// The date and time group as written.
    pub time_text: String,
}

/// A box of the local 25 x 25 grid: row and column 0-24 (`A`-`Y`) from the
/// north-west; the radar is in row 12, column 12 (`MM`; Appendix B says `NM`,
/// see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridBox {
    /// Row, 0 = `A` (north).
    pub row: u8,
    /// Column, 0 = `A` (west).
    pub column: u8,
}

/// A box of the fine (1/16 LFM) 100 x 100 grid, from the north-west corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FineBox {
    /// Row 0-99 (north to south).
    pub row: u8,
    /// Column 0-99 (west to east).
    pub column: u8,
}

impl FineBox {
    /// The local grid box holding this fine box.
    pub fn grid_box(&self) -> GridBox {
        GridBox {
            row: self.row / 4,
            column: self.column / 4,
        }
    }
}

/// Part A: reflectivity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartA {
    /// Site, date and time.
    pub header: PartHeader,
    /// Edit status word (`UNEDITED`, `EDITED`).
    pub status: String,
    /// `RADNE`: no reportable intensities.
    pub no_reportable_echoes: bool,
    /// `RADOM`: radar down for maintenance.
    pub radar_down: bool,
    /// Operational mode (`/MD`): `PCPN` or `CLAR`.
    pub mode: Option<String>,
    /// Scan strategy (`/SC`), e.g. `0906`; `Some("")` when the group is blank.
    pub scan_strategy: Option<String>,
    /// Number of intensities the message reports (`/NI`).
    pub intensity_count: Option<u32>,
    /// Intensity groups in message order.
    pub intensities: Vec<IntensityRun>,
    /// Levels of runs reaching past the east edge of the grid, dropped.
    pub cells_past_grid: usize,
    /// Maximum echo top (`/MT`).
    pub max_echo_top: Option<EchoTop>,
    /// Number of centroids (`/NCEN`).
    pub centroid_count: Option<u32>,
    /// Storm centroids with their motion.
    pub centroids: Vec<Centroid>,
}

impl PartA {
    /// The reported levels on the 100 x 100 fine grid, row-major from the
    /// north-west; boxes without a report are 0.
    pub fn intensity_grid(&self) -> Vec<u8> {
        let n = usize::from(FINE_BOXES);
        let mut grid = vec![0u8; n * n];
        for run in &self.intensities {
            let row = usize::from(run.start.row);
            let start = usize::from(run.start.column);
            for (offset, level) in run.levels.iter().enumerate() {
                if let Some(cell) = grid.get_mut(row * n + start + offset) {
                    *cell = *level;
                }
            }
        }
        grid
    }

    /// Total number of levels in the intensity groups, including those past
    /// the grid edge.
    pub fn reported_cells(&self) -> usize {
        self.intensities
            .iter()
            .map(|run| run.levels.len())
            .sum::<usize>()
            + self.cells_past_grid
    }

    /// Number of fine boxes with a level above 0. Observed: every corpus
    /// message's [`intensity_count`](Self::intensity_count) (`/NI`) equals
    /// it; Appendix B calls it the number of intensities reported.
    pub fn nonzero_cells(&self) -> usize {
        self.intensities
            .iter()
            .flat_map(|run| run.levels.iter())
            .filter(|&&level| level > 0)
            .count()
    }
}

/// One intensity group: levels of consecutive fine boxes of one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntensityRun {
    /// The group as written, whitespace removed.
    pub group: String,
    /// The first fine box.
    pub start: FineBox,
    /// Levels of the first box and the boxes east of it, repeats expanded.
    pub levels: Vec<u8>,
}

/// The maximum echo top of Part A (`/MThhh:ggg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoTop {
    /// Height in hundreds of feet MSL.
    pub height_hft: u16,
    /// Fine box.
    pub location: FineBox,
}

/// A storm centroid of Part A (`Cnnggg dddfff`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Centroid {
    /// Storm cell identifier (two characters, trimmed).
    pub id: String,
    /// Fine box.
    pub location: FineBox,
    /// Direction the storm moves from, degrees; `None` when not given.
    pub direction_deg: Option<u16>,
    /// Speed, knots; `None` when not given.
    pub speed_kt: Option<u16>,
}

/// Part B: VAD winds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartB {
    /// Site, date and time.
    pub header: PartHeader,
    /// `VADNA`: no VAD winds in the last 15 minutes.
    pub not_available: bool,
    /// Winds, lowest first as written.
    pub winds: Vec<Wind>,
}

/// A wind of Part B (`hhhcdddfff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wind {
    /// Height in hundreds of feet MSL.
    pub height_hft: u16,
    /// Confidence letter: `A` (RMS 2 kt) to `G` (RMS 14 kt or more).
    pub confidence: char,
    /// Direction the wind blows from, degrees.
    pub direction_deg: u16,
    /// Speed, knots.
    pub speed_kt: u16,
}

impl Wind {
    /// RMS of the wind in knots per the confidence letter (`A` = 2 kt, ...,
    /// `F` = 12 kt, `G` = 14 kt or more); `None` for another letter.
    pub fn rms_kt(&self) -> Option<u16> {
        match self.confidence {
            'A'..='G' => Some(2 * (u16::from(self.confidence as u8 - b'A') + 1)),
            _ => None,
        }
    }
}

/// Part C: remarks.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartC {
    /// Site, date and time.
    pub header: PartHeader,
    /// Number of tornado vortex signatures (`/NTVS`).
    pub tvs_count: Option<u32>,
    /// Tornado vortex signatures: identifier number and fine box.
    pub tvs: Vec<(u32, FineBox)>,
    /// Number of mesocyclones (`/NMES`).
    pub mesocyclone_count: Option<u32>,
    /// Mesocyclones: strength rank (identifier number in older messages) and
    /// fine box.
    pub mesocyclones: Vec<(u32, FineBox)>,
    /// Number of centroids (`/NCEN`).
    pub centroid_count: Option<u32>,
    /// Storm tops and hail per centroid.
    pub storm_tops: Vec<StormTop>,
    /// Other remark groups (`/PCTR`, `/LEWP`, `/BASE`, `/MALF`, `/PALF`,
    /// `/MLTLVL`, `/EYE`, `/CNTR`, `/REM:`, `/EDITED:`, `/UNEDITED:`),
    /// verbatim without the slash.
    pub remarks: Vec<String>,
}

/// A storm top of Part C (`Cnnggg ShhhHi`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StormTop {
    /// Storm cell identifier (two characters, trimmed).
    pub id: String,
    /// Fine box.
    pub location: FineBox,
    /// Storm top in hundreds of feet.
    pub top_hft: u16,
    /// Hail index: `N` no hail, `P` possible or probable, `H` hail, `U`
    /// unknown.
    pub hail: char,
}

/// Most bytes one parse of a Radar Coded Message may allocate
/// ([`RadarCodedMessage::parse`], and [`Level3Product::radar_coded_message`]
/// with its copy of the product's text): the message's text copied without
/// whitespace, and every group, string and list element of the parsed
/// message at its in-memory size, each charged before it is allocated
/// (temporary copies stay charged after they are freed). A parse that would
/// exceed it stops with [`Level3Error::ProductTooLarge`].
///
/// The text of a product is bounded only by the decompressed message
/// (16 MiB), and each comma-separated group, however short, becomes a
/// string of [`RadarCodedMessage::unparsed`] or a group string, an
/// [`IntensityRun`] and its levels: without this limit the volume of a
/// 760-byte bzip2 product 74 holding 13.8 MB of groups allocated 504 MB
/// while it parsed them (counting allocator, 2026-09-25).
/// Measured the same way, the 2 798 real messages found (products 74 and 83
/// of 16 NCEI day archives of 1994-2008, 28 products 74 of 2022 from AWS
/// and the committed ones) hold at most 4 016 bytes of text and allocate at
/// most 25 KB while they parse. A message that reports each of the 10 000
/// fine boxes as a group of its own, the most groups Appendix B allows
/// Part A without overlapping runs, holds 50 KB of text and allocates
/// 1.0 MB; the limit is four times that.
pub const MAX_RCM_PARSED_BYTES: usize = 4 << 20;

impl Level3Product {
    /// The decoded Radar Coded Message of a product 74 (or the message inside
    /// a product 83), or `None` when the product carries no radar coded
    /// message text.
    ///
    /// # Errors
    ///
    /// [`Level3Error::ProductTooLarge`] when copying and parsing the text
    /// would allocate more than [`MAX_RCM_PARSED_BYTES`].
    pub fn radar_coded_message(&self) -> Result<Option<RadarCodedMessage>, Level3Error> {
        let Some(tabular) = self
            .tabular
            .as_ref()
            .filter(|tabular| tabular.layout == TabularLayout::RadarCodedMessage)
        else {
            return Ok(None);
        };
        let mut budget = Budget::with_limit(MAX_RCM_PARSED_BYTES);
        let text = budget.latin1(&tabular.data, "radar coded message text")?;
        RadarCodedMessage::parse_within(&text, &mut budget).map(Some)
    }
}

/// A grid letter `A`-`Y` as 0-24.
fn grid_letter(c: u8) -> Option<u8> {
    (b'A'..=b'Y').contains(&c).then(|| c - b'A')
}

/// A three-letter fine box `rcs`: row and column `A`-`Y`, sub-box `A`-`P`
/// lettered down the columns.
fn fine_box(text: &str) -> Option<FineBox> {
    let &[r, c, s] = text.as_bytes() else {
        return None;
    };
    let (row, column) = (grid_letter(r)?, grid_letter(c)?);
    if !(b'A'..=b'P').contains(&s) {
        return None;
    }
    let sub = s - b'A';
    Some(FineBox {
        row: row * 4 + sub % 4,
        column: column * 4 + sub / 4,
    })
}

fn digits(text: &str) -> Option<u32> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `ddmmyytttt` to a date and time.
fn part_time(text: &str) -> Option<NaiveDateTime> {
    if text.len() != 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |range: std::ops::Range<usize>| text[range].parse::<u32>().ok();
    let (day, month, yy, hour, minute) = (n(0..2)?, n(2..4)?, n(4..6)?, n(6..8)?, n(8..10)?);
    let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
    NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?.and_hms_opt(hour, minute, 0)
}

/// The comma-separated groups of a list whose whitespace was removed.
fn groups(compact: &str) -> impl Iterator<Item = &str> {
    compact.split(',').filter(|group| !group.is_empty())
}

/// Slash chunks of a part body: the text after each `/` up to the next.
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    text.split('/').skip(1)
}

/// A count followed by `:` (`0596:`), and the text after the colon.
fn counted<'t>(chunk: &'t str, key: &str) -> Option<(Option<u32>, &'t str)> {
    let rest = chunk.strip_prefix(key)?;
    let (count, list) = match rest.find(':') {
        Some(colon) => (&rest[..colon], &rest[colon + 1..]),
        None => (rest, ""),
    };
    Some((digits(count.trim()), list))
}

/// One parse: the message so far and the budget its allocations are
/// charged to.
struct Parser<'b> {
    budget: &'b mut Budget,
    message: RadarCodedMessage,
}

impl Parser<'_> {
    /// A copy of `text`, charged.
    fn string(&mut self, text: &str, what: &'static str) -> Result<String, Level3Error> {
        self.budget.charge_bytes(text.len(), what)?;
        Ok(text.to_owned())
    }

    /// `text` with all whitespace removed, charged.
    fn compact(&mut self, text: &str, what: &'static str) -> Result<String, Level3Error> {
        let len = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(char::len_utf8)
            .sum();
        self.budget.charge_bytes(len, what)?;
        let mut compact = String::with_capacity(len);
        compact.extend(text.chars().filter(|c| !c.is_whitespace()));
        Ok(compact)
    }

    /// The words of `text` joined by single spaces, charged.
    fn words(&mut self, text: &str, what: &'static str) -> Result<String, Level3Error> {
        let (count, bytes) = text
            .split_whitespace()
            .fold((0usize, 0usize), |(count, bytes), word| {
                (count + 1, bytes + word.len())
            });
        let len = bytes + count.saturating_sub(1);
        self.budget.charge_bytes(len, what)?;
        let mut joined = String::with_capacity(len);
        for (index, word) in text.split_whitespace().enumerate() {
            if index > 0 {
                joined.push(' ');
            }
            joined.push_str(word);
        }
        Ok(joined)
    }

    /// Keeps `group`, prefixed with `part`, in [`RadarCodedMessage::unparsed`].
    fn unparsed(&mut self, part: &str, group: &str) -> Result<(), Level3Error> {
        let len = part.len() + group.len();
        self.budget
            .charge_bytes(len, "radar coded message unparsed group")?;
        let mut text = String::with_capacity(len);
        text.push_str(part);
        text.push_str(group);
        self.budget.push(
            &mut self.message.unparsed,
            text,
            "radar coded message unparsed groups",
        )
    }

    /// The part header: site then `ddmmyytttt`; returns it and the rest.
    fn part_header<'t>(&mut self, text: &'t str) -> Result<(PartHeader, &'t str), Level3Error> {
        let mut rest = text.trim_start();
        let mut header = PartHeader::default();
        for slot in 0..2 {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let word = &rest[..end];
            if word.starts_with('/') || word.is_empty() {
                break;
            }
            if slot == 0 {
                header.site = self.string(word, "radar coded message part site")?;
            } else {
                header.time = part_time(word);
                header.time_text = self.string(word, "radar coded message part time")?;
            }
            rest = rest[end..].trim_start();
        }
        Ok((header, rest))
    }

    fn part_a(&mut self, part: &str) -> Result<PartA, Level3Error> {
        let (header, rest) = self.part_header(part)?;
        let mut a = PartA {
            header,
            ..PartA::default()
        };
        let first_slash = rest.find('/').unwrap_or(rest.len());
        for word in rest[..first_slash].split_whitespace() {
            match word {
                "RADNE" => a.no_reportable_echoes = true,
                "RADOM" => a.radar_down = true,
                word if a.status.is_empty() => {
                    a.status = self.string(word, "radar coded message status")?;
                }
                other => self.unparsed("A: ", other)?,
            }
        }
        for chunk in chunks(&rest[first_slash..]) {
            if let Some(mode) = chunk.strip_prefix("MD") {
                a.mode = Some(self.string(mode.trim(), "radar coded message mode")?);
            } else if let Some(strategy) = chunk.strip_prefix("SC") {
                a.scan_strategy =
                    Some(self.string(strategy.trim(), "radar coded message scan strategy")?);
            } else if let Some((count, list)) = counted(chunk, "NI") {
                a.intensity_count = count;
                let list = self.compact(list, "radar coded message intensity groups")?;
                for group in groups(&list) {
                    self.intensity_group(&mut a, group)?;
                }
            } else if let Some(top) = chunk.strip_prefix("MT") {
                let compact = self.compact(top, "radar coded message echo top")?;
                let parsed = compact.split_once(':').and_then(|(height, location)| {
                    Some(EchoTop {
                        height_hft: u16::try_from(digits(height)?).ok()?,
                        location: fine_box(location)?,
                    })
                });
                match parsed {
                    Some(top) => a.max_echo_top = Some(top),
                    None => self.unparsed("A: /", chunk.trim())?,
                }
            } else if let Some((count, list)) = counted(chunk, "NCEN") {
                a.centroid_count = count;
                for group in list.split(',') {
                    let group = group.trim();
                    if group.is_empty() {
                        continue;
                    }
                    match centroid(group) {
                        Some((id, centroid)) => {
                            let id = self.string(id, "radar coded message centroid")?;
                            self.budget.push(
                                &mut a.centroids,
                                Centroid { id, ..centroid },
                                "radar coded message centroids",
                            )?;
                        }
                        None => self.unparsed("A: ", group)?,
                    }
                }
            } else if !chunk.trim().is_empty() {
                self.unparsed("A: /", chunk.trim())?;
            }
        }
        Ok(a)
    }

    fn intensity_group(&mut self, a: &mut PartA, group: &str) -> Result<(), Level3Error> {
        let Some(start) = group.get(..3).and_then(fine_box) else {
            return self.unparsed("A: ", group);
        };
        let room = usize::from(FINE_BOXES - start.column);
        // A run keeps at most the boxes up to the east edge of its row.
        let mut kept = [0u8; FINE_BOXES as usize];
        let mut len = 0;
        let mut last = None;
        let mut total = 0usize;
        for c in group[3..].bytes() {
            let (level, count) = match c {
                b'0'..=b'9' => {
                    last = Some(c - b'0');
                    (c - b'0', 1)
                }
                b'A'..=b'Z' => match last {
                    Some(level) => (level, usize::from(c - b'A') + 1),
                    None => return self.unparsed("A: ", group),
                },
                _ => return self.unparsed("A: ", group),
            };
            total = total.saturating_add(count);
            let keep = count.min(room.saturating_sub(len));
            if let Some(cells) = kept.get_mut(len..len + keep) {
                cells.fill(level);
                len += keep;
            }
        }
        let run = IntensityRun {
            group: self.string(group, "radar coded message intensity group")?,
            start,
            levels: self.budget.bytes(
                kept.get(..len).unwrap_or_default(),
                "radar coded message intensity levels",
            )?,
        };
        a.cells_past_grid = a.cells_past_grid.saturating_add(total - len);
        self.budget.push(
            &mut a.intensities,
            run,
            "radar coded message intensity runs",
        )
    }

    fn part_b(&mut self, part: &str) -> Result<PartB, Level3Error> {
        let (header, rest) = self.part_header(part)?;
        let mut b = PartB {
            header,
            ..PartB::default()
        };
        let list = self.compact(rest, "radar coded message winds")?;
        for group in groups(&list) {
            if group == "VADNA" {
                b.not_available = true;
                continue;
            }
            match wind(group) {
                Some(w) => self
                    .budget
                    .push(&mut b.winds, w, "radar coded message winds")?,
                None => self.unparsed("B: ", group)?,
            }
        }
        Ok(b)
    }

    fn part_c(&mut self, part: &str) -> Result<PartC, Level3Error> {
        let (header, rest) = self.part_header(part)?;
        let mut c = PartC {
            header,
            ..PartC::default()
        };
        let first_slash = rest.find('/').unwrap_or(rest.len());
        for word in rest[..first_slash].split_whitespace() {
            self.unparsed("C: ", word)?;
        }
        for chunk in chunks(&rest[first_slash..]) {
            if let Some((count, list)) = counted(chunk, "NTVS") {
                c.tvs_count = count;
                let list = self.compact(list, "radar coded message TVS groups")?;
                for group in groups(&list) {
                    match numbered(group, "TVS") {
                        Some(tvs) => {
                            self.budget
                                .push(&mut c.tvs, tvs, "radar coded message TVS")?
                        }
                        None => self.unparsed("C: ", group)?,
                    }
                }
            } else if let Some((count, list)) = counted(chunk, "NMES") {
                c.mesocyclone_count = count;
                let list = self.compact(list, "radar coded message mesocyclone groups")?;
                for group in groups(&list) {
                    match numbered(group, "M") {
                        Some(m) => self.budget.push(
                            &mut c.mesocyclones,
                            m,
                            "radar coded message mesocyclones",
                        )?,
                        None => self.unparsed("C: ", group)?,
                    }
                }
            } else if let Some((count, list)) = counted(chunk, "NCEN") {
                c.centroid_count = count;
                for group in list.split(',') {
                    let group = group.trim();
                    if group.is_empty() {
                        continue;
                    }
                    match self.storm_top(group)? {
                        Some(top) => self.budget.push(
                            &mut c.storm_tops,
                            top,
                            "radar coded message storm tops",
                        )?,
                        None => self.unparsed("C: ", group)?,
                    }
                }
            } else if !chunk.trim().is_empty() {
                let remark = self.words(chunk, "radar coded message remark")?;
                self.budget
                    .push(&mut c.remarks, remark, "radar coded message remarks")?;
            }
        }
        Ok(c)
    }

    /// `Cnnggg ShhhHi`, or `None` when the group does not follow it.
    fn storm_top(&mut self, group: &str) -> Result<Option<StormTop>, Level3Error> {
        let Some(rest) = group.strip_prefix('C') else {
            return Ok(None);
        };
        let (Some(id), Some(location)) = (rest.get(..2), rest.get(2..5).and_then(fine_box)) else {
            return Ok(None);
        };
        let tail = self.compact(&rest[5..], "radar coded message storm top")?;
        let parsed = tail.strip_prefix('S').and_then(|tail| {
            let hail_at = tail.find('H')?;
            let top = u16::try_from(digits(&tail[..hail_at])?).ok()?;
            let hail = tail[hail_at + 1..].chars().next()?;
            Some((top, hail))
        });
        let Some((top_hft, hail)) = parsed else {
            return Ok(None);
        };
        Ok(Some(StormTop {
            id: self.string(id.trim(), "radar coded message storm top")?,
            location,
            top_hft,
            hail,
        }))
    }
}

impl RadarCodedMessage {
    /// Decodes the text of a Radar Coded Message (see the
    /// [module documentation](self)).
    ///
    /// # Errors
    ///
    /// [`Level3Error::ProductTooLarge`] when the parse would allocate more
    /// than [`MAX_RCM_PARSED_BYTES`] (the text itself is the caller's and is
    /// not counted). Groups that do not follow Appendix B are not an error:
    /// they are kept in [`unparsed`](Self::unparsed).
    pub fn parse(text: &str) -> Result<Self, Level3Error> {
        Self::parse_within(text, &mut Budget::with_limit(MAX_RCM_PARSED_BYTES))
    }

    fn parse_within(text: &str, budget: &mut Budget) -> Result<Self, Level3Error> {
        let mut parser = Parser {
            budget,
            message: Self::default(),
        };
        let text = text.trim_start();
        let first_part = text.find("/NEXR").unwrap_or(text.len());
        let mut words = text[..first_part].split_whitespace();
        let (node, category, site) = (words.next(), words.next(), words.next());
        parser.message.node =
            parser.string(node.unwrap_or_default(), "radar coded message header")?;
        parser.message.category =
            parser.string(category.unwrap_or_default(), "radar coded message header")?;
        parser.message.site =
            parser.string(site.unwrap_or_default(), "radar coded message header")?;
        for word in words {
            parser.unparsed("", word)?;
        }

        let body = &text[first_part..];
        for (marker, end) in [
            ("/NEXRAA", "/ENDAA"),
            ("/NEXRBB", "/ENDBB"),
            ("/NEXRCC", "/ENDCC"),
        ] {
            let Some(start) = body.find(marker) else {
                continue;
            };
            let after = &body[start + marker.len()..];
            // A part ends at its end marker, the next part or the end of the
            // message.
            let stop = [end, "/NEXR", "/ENDALL"]
                .iter()
                .filter_map(|m| after.find(m))
                .min()
                .unwrap_or(after.len());
            let part = &after[..stop];
            match marker {
                "/NEXRAA" => parser.message.part_a = Some(parser.part_a(part)?),
                "/NEXRBB" => parser.message.part_b = Some(parser.part_b(part)?),
                _ => parser.message.part_c = Some(parser.part_c(part)?),
            }
        }
        Ok(parser.message)
    }
}

/// `Cnnggg dddfff`: the identifier (2 characters, trimmed), and the centroid
/// with its fine box, direction and speed (both optional) and an empty
/// identifier.
fn centroid(group: &str) -> Option<(&str, Centroid)> {
    let rest = group.strip_prefix('C')?;
    let id = rest.get(..2)?;
    let location = fine_box(rest.get(2..5)?)?;
    // The motion without whitespace: none, or six ASCII digits.
    let mut motion = [0u8; 6];
    let mut len = 0;
    for c in rest[5..].chars().filter(|c| !c.is_whitespace()) {
        *motion.get_mut(len)? = u8::try_from(c).ok().filter(u8::is_ascii)?;
        len += 1;
    }
    let (direction_deg, speed_kt) = match len {
        0 => (None, None),
        6 => {
            let motion = std::str::from_utf8(&motion).ok()?;
            (
                Some(u16::try_from(digits(&motion[..3])?).ok()?),
                Some(u16::try_from(digits(&motion[3..])?).ok()?),
            )
        }
        _ => return None,
    };
    Some((
        id.trim(),
        Centroid {
            id: String::new(),
            location,
            direction_deg,
            speed_kt,
        },
    ))
}

/// `hhhcdddfff`.
fn wind(group: &str) -> Option<Wind> {
    if group.len() != 10 || !group.is_ascii() {
        return None;
    }
    let confidence = char::from(group.as_bytes()[3]);
    if !confidence.is_ascii_uppercase() {
        return None;
    }
    Some(Wind {
        height_hft: u16::try_from(digits(&group[..3])?).ok()?,
        confidence,
        direction_deg: u16::try_from(digits(&group[4..7])?).ok()?,
        speed_kt: u16::try_from(digits(&group[7..])?).ok()?,
    })
}

/// `<prefix>nnggg`: a two-digit number and a fine box.
fn numbered(group: &str, prefix: &str) -> Option<(u32, FineBox)> {
    let rest = group.strip_prefix(prefix)?;
    Some((digits(rest.get(..2)?)?, fine_box(rest.get(2..)?)?))
}
