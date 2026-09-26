//! Level II files that are records rather than volumes: real-time chunks
//! (an intermediate chunk has no volume header; a start chunk holds only the
//! metadata record) and bare LDM records. They are described message by
//! message with `recast_radar_io_nexrad::messages`.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use recast_radar_io_nexrad::messages::{self, MessageBody, MessageWalker};
use serde_json::{Map, Value, json};

use crate::summary::time_text;

/// Most message errors kept in a summary.
const MAX_ERRORS: usize = 20;

/// One elevation cut's radials in a record file.
#[derive(Clone, Debug, PartialEq)]
pub struct ElevationRadials {
    /// ICD elevation number (1-based).
    pub elevation_number: u8,
    /// Elevation angle of the first radial, degrees.
    pub elevation_deg: f32,
    /// Message 31 radials of this cut.
    pub radials: usize,
    /// Azimuth of the first and last radial, degrees.
    pub azimuth_first_deg: f32,
    /// Azimuth of the last radial, degrees.
    pub azimuth_last_deg: f32,
    /// Collection time of the first radial.
    pub first_time: DateTime<Utc>,
    /// Collection time of the last radial.
    pub last_time: DateTime<Utc>,
    /// Moment names (`REF`, `VEL`, ...) in first-seen order.
    pub moments: Vec<String>,
    /// Radial status codes seen (ICD Table III-C, bad-data bit removed).
    pub statuses: Vec<u8>,
}

/// What a Level II record file holds.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordSummary {
    /// Archive II volume header: tape name and extension (`AR2V0006.307`).
    pub tape: Option<String>,
    /// Radar id of the volume header.
    pub site: Option<String>,
    /// Volume header date and time.
    pub header_time: Option<DateTime<Utc>>,
    /// Decompressed record bytes.
    pub record_bytes: usize,
    /// Message count by message type.
    pub messages: BTreeMap<u8, usize>,
    /// Message 31 radials by elevation cut, in first-seen order.
    pub elevations: Vec<ElevationRadials>,
    /// Messages that did not frame or decode (at most 20 kept).
    pub errors: Vec<String>,
    /// Errors beyond those kept.
    pub more_errors: usize,
}

fn header_time(bytes: &[u8]) -> Option<DateTime<Utc>> {
    let date = u32::from_be_bytes(bytes.get(12..16)?.try_into().ok()?);
    let ms = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    if date == 0 {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)?
        .and_hms_opt(0, 0, 0)?
        .and_utc();
    // Checked: a corrupt header can give any date and milliseconds.
    epoch
        .checked_add_signed(Duration::days(i64::from(date) - 1))?
        .checked_add_signed(Duration::milliseconds(i64::from(ms)))
}

fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() {
                char::from(b)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Summarize the messages of Level II record bytes (with or without a
/// volume header, LDM-compressed or not).
pub fn summarize(bytes: &[u8]) -> Result<RecordSummary, String> {
    let header_len = messages::volume_header_len(bytes);
    let (tape, site, time) = if header_len > 0 {
        (
            Some(ascii(&bytes[..12])),
            Some(ascii(&bytes[20..24])),
            header_time(bytes),
        )
    } else {
        (None, None, None)
    };
    let records = messages::record_bytes(bytes).map_err(|err| err.to_string())?;
    let mut summary = RecordSummary {
        tape,
        site,
        header_time: time,
        record_bytes: records.len(),
        messages: BTreeMap::new(),
        elevations: Vec::new(),
        errors: Vec::new(),
        more_errors: 0,
    };
    for item in MessageWalker::new(&records) {
        match item {
            Ok((header, body)) => {
                *summary.messages.entry(header.message_type).or_default() += 1;
                if let MessageBody::DigitalRadarDataGeneric(radial) = body {
                    add_radial(&mut summary.elevations, &radial);
                }
            }
            Err(err) => {
                if summary.errors.len() < MAX_ERRORS {
                    summary.errors.push(err.to_string());
                } else {
                    summary.more_errors += 1;
                }
            }
        }
    }
    Ok(summary)
}

fn add_radial(
    elevations: &mut Vec<ElevationRadials>,
    radial: &messages::msg31_blocks::DigitalRadarDataGeneric<'_>,
) {
    let header = &radial.header;
    let time = header.collection_time();
    let status = header.radial_status_code & 0x7f;
    let index = match elevations
        .iter()
        .position(|cut| cut.elevation_number == header.elevation_number)
    {
        Some(index) => index,
        None => {
            elevations.push(ElevationRadials {
                elevation_number: header.elevation_number,
                elevation_deg: header.elevation_angle_deg,
                radials: 0,
                azimuth_first_deg: header.azimuth_angle_deg,
                azimuth_last_deg: header.azimuth_angle_deg,
                first_time: time,
                last_time: time,
                moments: Vec::new(),
                statuses: Vec::new(),
            });
            elevations.len() - 1
        }
    };
    let cut = &mut elevations[index];
    cut.radials += 1;
    cut.azimuth_last_deg = header.azimuth_angle_deg;
    cut.first_time = cut.first_time.min(time);
    cut.last_time = cut.last_time.max(time);
    for moment in &radial.moments {
        let name = moment.name.to_string();
        if !cut.moments.contains(&name) {
            cut.moments.push(name);
        }
    }
    if !cut.statuses.contains(&status) {
        cut.statuses.push(status);
    }
}

impl RecordSummary {
    /// Total Message 31 radials.
    pub fn radials(&self) -> usize {
        self.elevations.iter().map(|cut| cut.radials).sum()
    }

    /// The summary as JSON.
    pub fn json(&self) -> Value {
        let mut map = Map::new();
        if let Some(tape) = &self.tape {
            map.insert("tape".to_owned(), json!(tape));
        }
        if let Some(site) = &self.site {
            map.insert("site".to_owned(), json!(site));
        }
        if let Some(time) = &self.header_time {
            map.insert("header_time".to_owned(), json!(time_text(time)));
        }
        map.insert("record_bytes".to_owned(), json!(self.record_bytes));
        let messages: Map<String, Value> = self
            .messages
            .iter()
            .map(|(kind, count)| (kind.to_string(), json!(count)))
            .collect();
        map.insert("messages".to_owned(), Value::Object(messages));
        let cuts: Vec<Value> = self
            .elevations
            .iter()
            .map(|cut| {
                json!({
                    "elevation_number": cut.elevation_number,
                    "elevation_deg": cut.elevation_deg,
                    "radials": cut.radials,
                    "azimuth_first_deg": cut.azimuth_first_deg,
                    "azimuth_last_deg": cut.azimuth_last_deg,
                    "first_time": time_text(&cut.first_time),
                    "last_time": time_text(&cut.last_time),
                    "moments": cut.moments,
                    "radial_statuses": cut.statuses,
                })
            })
            .collect();
        map.insert("elevations".to_owned(), Value::Array(cuts));
        if !self.errors.is_empty() {
            map.insert("errors".to_owned(), json!(self.errors));
        }
        if self.more_errors > 0 {
            map.insert("more_errors".to_owned(), json!(self.more_errors));
        }
        Value::Object(map)
    }
}

/// Display name of a message type, `type 99` when Table I has none.
pub(crate) fn message_name(kind: u8) -> String {
    match messages::message_type_name(kind) {
        Some(name) => format!("{kind} {name}"),
        None => format!("type {kind}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// KIWA volume 307, chunks 1 (start) and 2 (intermediate), captured live;
    /// counts from the manifest descriptions.
    #[test]
    fn real_time_chunks_are_described_message_by_message() {
        let start = recast_radar_testdata::bytes("l2chunk-kiwa-307-20260917-003629-001-s").unwrap();
        let summary = summarize(&start).unwrap();
        assert_eq!(summary.tape.as_deref(), Some("AR2V0006.307"));
        assert_eq!(summary.site.as_deref(), Some("KIWA"));
        assert_eq!(summary.radials(), 0);
        // Segmented messages (15 in 5 segments, 18 in 4) count once.
        for kind in [15u8, 18, 3, 5, 2, 32] {
            assert_eq!(summary.messages.get(&kind), Some(&1), "message {kind}");
        }
        assert_eq!(summary.messages.len(), 6);

        let chunk = recast_radar_testdata::bytes("l2chunk-kiwa-307-20260917-003629-002-i").unwrap();
        let summary = summarize(&chunk).unwrap();
        assert_eq!(summary.tape, None);
        assert_eq!(summary.radials(), 120);
        assert_eq!(summary.elevations.len(), 1);
        let cut = &summary.elevations[0];
        assert_eq!(cut.elevation_number, 1);
        assert_eq!(time_text(&cut.first_time), "2026-09-17T00:36:29.397Z");
        assert_eq!(time_text(&cut.last_time), "2026-09-17T00:36:34.532Z");
        let mut statuses = cut.statuses.clone();
        statuses.sort_unstable();
        assert_eq!(statuses, [1, 3]);
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    }
}
