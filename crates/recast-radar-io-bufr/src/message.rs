//! BUFR messages: sections 0 to 5 (WMO-No. 306, Manual on Codes, Volume
//! I.2, FM 94 BUFR), editions 2, 3 and 4.

use chrono::{DateTime, NaiveDate, Utc};

use crate::BufrError;

/// The section 0 start of every message.
pub const MAGIC: &[u8; 4] = b"BUFR";
/// The section 5 end of every message.
const END: &[u8; 4] = b"7777";

/// One BUFR message: its section 1 identification, its section 3
/// descriptors and its section 4 data.
#[derive(Clone, Debug)]
pub struct Message<'a> {
    /// BUFR edition (2, 3 or 4).
    pub edition: u8,
    /// Section 1: master table (0 for meteorology).
    pub master_table: u8,
    /// Originating centre (Common Code Table C-1 / C-11: 85 is Toulouse,
    /// Meteo-France).
    pub centre: u16,
    /// Originating sub-centre.
    pub sub_centre: u16,
    /// Update sequence number.
    pub update_sequence: u8,
    /// Data category (Table A: 6 is radar data).
    pub category: u8,
    /// Data sub-category (edition 4: the international one).
    pub sub_category: u8,
    /// Local data sub-category (edition 4 only).
    pub local_sub_category: Option<u8>,
    /// Master table version.
    pub master_version: u8,
    /// Local table version (0: none).
    pub local_version: u8,
    /// Typical time of the data (seconds only in edition 4).
    pub time: Option<DateTime<Utc>>,
    /// Section 2 (optional local use), without its length and reserved
    /// octets.
    pub local_use: Option<&'a [u8]>,
    /// Number of data subsets.
    pub subsets: u16,
    /// Observed data (else other data).
    pub observed: bool,
    /// Compressed data (subsets packed together).
    pub compressed: bool,
    /// The unexpanded descriptors of section 3, as FXXYYY numbers.
    pub descriptors: Vec<u32>,
    /// Section 4 data, from its first data bit.
    pub data: &'a [u8],
}

/// Every message in `bytes`, in order. Bytes between messages (a WMO
/// bulletin heading, padding) are skipped.
pub fn messages(bytes: &[u8]) -> Result<Vec<Message<'_>>, BufrError> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(offset) = find(&bytes[at..], MAGIC) {
        let start = at + offset;
        let message = parse(&bytes[start..])?;
        let length = message_length(&bytes[start..])?;
        out.push(message);
        at = start + length;
    }
    if out.is_empty() {
        return Err(BufrError::Format(
            "no BUFR message (no `BUFR` start)".into(),
        ));
    }
    Ok(out)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn u24(bytes: &[u8], at: usize) -> Result<usize, BufrError> {
    let b = bytes
        .get(at..at + 3)
        .ok_or_else(|| BufrError::Format(format!("message ends before octet {}", at + 3)))?;
    Ok((usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, BufrError> {
    let b = bytes
        .get(at..at + 2)
        .ok_or_else(|| BufrError::Format(format!("section ends before octet {}", at + 2)))?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

fn octet(bytes: &[u8], at: usize) -> Result<u8, BufrError> {
    bytes
        .get(at)
        .copied()
        .ok_or_else(|| BufrError::Format(format!("section ends before octet {}", at + 1)))
}

/// Total length of the message at the start of `bytes` (section 0).
fn message_length(bytes: &[u8]) -> Result<usize, BufrError> {
    let edition = octet(bytes, 7)?;
    if !(2..=4).contains(&edition) {
        return Err(BufrError::Unsupported(format!("BUFR edition {edition}")));
    }
    let length = u24(bytes, 4)?;
    if length < 8 + 4 || length > bytes.len() {
        return Err(BufrError::Format(format!(
            "BUFR message of {length} bytes in {} bytes",
            bytes.len()
        )));
    }
    Ok(length)
}

/// Parse the message at the start of `bytes`.
pub fn parse(bytes: &[u8]) -> Result<Message<'_>, BufrError> {
    let length = message_length(bytes)?;
    let message = &bytes[..length];
    if &message[length - 4..] != END {
        return Err(BufrError::Format(
            "BUFR message does not end with 7777".into(),
        ));
    }
    let edition = message[7];

    // Section 1.
    let s1 = 8;
    let s1_len = u24(message, s1)?;
    let section1 = message
        .get(s1..s1 + s1_len)
        .ok_or_else(|| BufrError::Format("section 1 runs past the message".into()))?;
    let (ident, has_section2) = if edition == 4 {
        identification_v4(section1)?
    } else {
        identification_v2(section1)?
    };

    // Section 2.
    let mut at = s1 + s1_len;
    let mut local_use = None;
    if has_section2 {
        let len = u24(message, at)?;
        local_use = Some(
            message
                .get(at + 4..at + len)
                .ok_or_else(|| BufrError::Format("section 2 runs past the message".into()))?,
        );
        at += len;
    }

    // Section 3.
    let s3_len = u24(message, at)?;
    let section3 = message
        .get(at..at + s3_len)
        .ok_or_else(|| BufrError::Format("section 3 runs past the message".into()))?;
    let subsets = u16_at(section3, 4)?;
    let flags = octet(section3, 6)?;
    let descriptors = section3
        .get(7..)
        .unwrap_or_default()
        .chunks_exact(2)
        .map(|pair| {
            let d = u16::from_be_bytes([pair[0], pair[1]]);
            u32::from(d >> 14) * 100_000 + u32::from((d >> 8) & 0x3F) * 1000 + u32::from(d & 0xFF)
        })
        .collect();
    at += s3_len;

    // Section 4.
    let s4_len = u24(message, at)?;
    let data = message
        .get(at + 4..at + s4_len)
        .ok_or_else(|| BufrError::Format("section 4 runs past the message".into()))?;

    Ok(Message {
        edition,
        local_use,
        subsets,
        observed: flags & 0x80 != 0,
        compressed: flags & 0x40 != 0,
        descriptors,
        data,
        ..ident
    })
}

/// Section 1 of editions 2 and 3.
fn identification_v2(s: &[u8]) -> Result<(Message<'static>, bool), BufrError> {
    let year_of_century = i32::from(octet(s, 12)?);
    // Year of the century; the century the data most likely belongs to.
    let year = if year_of_century > 70 {
        1900 + year_of_century
    } else {
        2000 + year_of_century
    };
    let time = timestamp(
        year,
        octet(s, 13)?,
        octet(s, 14)?,
        octet(s, 15)?,
        octet(s, 16)?,
        0,
    );
    Ok((
        Message {
            edition: 0,
            master_table: octet(s, 3)?,
            sub_centre: u16::from(octet(s, 4)?),
            centre: u16::from(octet(s, 5)?),
            update_sequence: octet(s, 6)?,
            category: octet(s, 8)?,
            sub_category: octet(s, 9)?,
            local_sub_category: None,
            master_version: octet(s, 10)?,
            local_version: octet(s, 11)?,
            time,
            local_use: None,
            subsets: 0,
            observed: false,
            compressed: false,
            descriptors: Vec::new(),
            data: &[],
        },
        octet(s, 7)? & 0x80 != 0,
    ))
}

/// Section 1 of edition 4.
fn identification_v4(s: &[u8]) -> Result<(Message<'static>, bool), BufrError> {
    let time = timestamp(
        i32::from(u16_at(s, 15)?),
        octet(s, 17)?,
        octet(s, 18)?,
        octet(s, 19)?,
        octet(s, 20)?,
        octet(s, 21)?,
    );
    Ok((
        Message {
            edition: 0,
            master_table: octet(s, 3)?,
            centre: u16_at(s, 4)?,
            sub_centre: u16_at(s, 6)?,
            update_sequence: octet(s, 8)?,
            category: octet(s, 10)?,
            sub_category: octet(s, 11)?,
            local_sub_category: Some(octet(s, 12)?),
            master_version: octet(s, 13)?,
            local_version: octet(s, 14)?,
            time,
            local_use: None,
            subsets: 0,
            observed: false,
            compressed: false,
            descriptors: Vec::new(),
            data: &[],
        },
        octet(s, 9)? & 0x80 != 0,
    ))
}

fn timestamp(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(year, u32::from(month), u32::from(day))?
        .and_hms_opt(u32::from(hour), u32::from(minute), u32::from(second))
        .map(|time| time.and_utc())
}
