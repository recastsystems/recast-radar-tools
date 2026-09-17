//! Framing and headers: the NOAAPort/WMO/AWIPS text header, the Message Header
//! Block (ICD 2620001 Figure 3-3) and the Product Description Block (Figure 3-6
//! sheets 2, 6 and 7). See `docs/level3/reference.md` sections 3.1-3.3.

use std::borrow::Cow;

use chrono::{DateTime, Utc};

use crate::Level3Error;
use crate::decompress;
use crate::read::{be_i16, be_i32, be_u16, be_u32, slice};

/// Size of the Message Header Block in bytes.
pub(crate) const MESSAGE_HEADER_BYTES: usize = 18;
/// Size of the Message Header Block plus the Product Description Block in bytes;
/// the first byte after them (where a bzip2 stream starts) is at this offset.
pub(crate) const HEADER_BYTES: usize = 120;
/// Number of halfwords in the Message Header Block and Product Description Block.
pub const HEADER_HALFWORDS: usize = 60;

/// NOAAPort start-of-header line.
const SOH_LINE: &[u8] = b"\x01\r\r\n";
/// NOAAPort end-of-text trailer.
const ETX_TRAILER: &[u8] = b"\r\r\n\x03";
/// WMO line ending.
const CRCRLF: &[u8] = b"\r\r\n";

/// Transmission header in front of the binary message: NOAAPort start-of-header
/// and sequence number, WMO abbreviated heading and AWIPS product identifier.
///
/// None of this is defined by ICD 2620001; the layout is the one observed in
/// the corpus (`docs/level3/reference.md` section 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHeader {
    /// Sequence number from the NOAAPort start-of-header (`\x01\r\r\n` then 3-5
    /// digits), when the file carries one.
    pub noaaport_sequence: Option<String>,
    /// WMO abbreviated heading without its line ending, e.g. `SDUS54 KOUN 202016`.
    pub wmo_heading: String,
    /// Data type designator and number `T1T2A1A2ii`, e.g. `SDUS54`.
    pub data_designator: String,
    /// Originating centre `CCCC`, e.g. `KOUN`.
    pub originator: String,
    /// Day of month, hour and minute `YYGGgg`, e.g. `202016`.
    pub day_time: String,
    /// Optional `BBB` indicator (amendment, correction, delay), e.g. `RRA`.
    pub indicator: Option<String>,
    /// AWIPS identifier (product category and site), e.g. `N0RTLX`.
    pub awips_id: Option<String>,
    /// Number of zlib frames removed (0 when the message was not zlib-wrapped).
    /// When nonzero the heading above is the outer one; the copy repeated
    /// inside the zlib data is only used if there is no outer heading.
    pub zlib_frames: u32,
}

/// Message Header Block (ICD 2620001 Figure 3-3), the first 18 bytes of every message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageHeader {
    /// Message code (halfword 1): the product code for products, 2 for a General
    /// Status Message, 100-111 for alphanumeric blocks.
    pub code: i16,
    /// Date of message (halfword 2), modified Julian (1 = 1970-01-01). Archived
    /// 1990s products carry 0.
    pub date: u16,
    /// Time of message (halfwords 3-4), seconds after midnight UTC.
    pub time: i32,
    /// Length of the message in bytes (halfwords 5-6), including this header;
    /// for bzip2 products, the compressed length.
    pub length: u32,
    /// Source ID (halfword 7).
    pub source_id: i16,
    /// Destination ID (halfword 8).
    pub destination_id: i16,
    /// Number of blocks (halfword 9), counting this header and the Product
    /// Description Block.
    pub num_blocks: u16,
}

impl MessageHeader {
    /// Parses the Message Header Block at the start of `message`.
    pub(crate) fn parse(message: &[u8]) -> Result<Self, Level3Error> {
        slice(message, 0, MESSAGE_HEADER_BYTES, "message header block")?;
        let what = "message header block";
        Ok(Self {
            code: be_i16(message, 0, what)?,
            date: be_u16(message, 2, what)?,
            time: be_i32(message, 4, what)?,
            length: be_u32(message, 8, what)?,
            source_id: be_i16(message, 12, what)?,
            destination_id: be_i16(message, 14, what)?,
            num_blocks: be_u16(message, 16, what)?,
        })
    }

    /// Date and time of the message, or `None` when the date is 0 (archived
    /// 1990s products) or not representable.
    pub fn datetime(&self) -> Option<DateTime<Utc>> {
        if self.date == 0 {
            return None;
        }
        let seconds = (i64::from(self.date) - 1) * 86_400 + i64::from(self.time);
        DateTime::from_timestamp(seconds, 0)
    }
}

/// Product Description Block (ICD 2620001 Figure 3-6 sheets 2, 6 and 7).
///
/// Named fields decode the halfwords whose meaning is the same for every
/// product. Product-dependent halfwords (P1-P10, data level thresholds,
/// compression fields) are available raw in [`halfwords`](Self::halfwords);
/// their per-product meaning is in `docs/level3/reference.md` sections 5 and 8.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductDescription {
    /// Radar latitude in degrees (halfwords 11-12, 0.001 degree).
    pub latitude_deg: f64,
    /// Radar longitude in degrees (halfwords 13-14, 0.001 degree).
    pub longitude_deg: f64,
    /// Radar height in feet above mean sea level (halfword 15).
    pub height_ft: i16,
    /// Product code (halfword 16, Table III).
    pub product_code: i16,
    /// Operational mode (halfword 17): 0 maintenance, 1 clear air, 2 precipitation.
    pub operational_mode: u16,
    /// Volume coverage pattern (halfword 18).
    pub vcp: u16,
    /// Sequence number (halfword 19); -13 for alert-generated products.
    pub sequence_number: i16,
    /// Volume scan number (halfword 20).
    pub volume_scan_number: u16,
    /// Volume scan start (halfwords 21-23). For SAILS products the date is the
    /// elevation start date. Built as `1970-01-01 + (date - 1) days + seconds`.
    pub volume_scan_time: DateTime<Utc>,
    /// Product generation time (halfwords 24-26), built like `volume_scan_time`.
    pub generation_time: DateTime<Utc>,
    /// Elevation number (halfword 29); 0 for volume products.
    pub elevation_number: u16,
    /// Raw halfwords 1-60 of the Message Header Block and Product Description
    /// Block as unsigned big-endian values: `halfwords[n - 1]` is ICD halfword
    /// `n`, so halfword 31 (first data level threshold) is `halfwords[30]`.
    /// Use [`halfword`](Self::halfword) for 1-based access.
    pub halfwords: [u16; HEADER_HALFWORDS],
    /// Product version (halfword 54 high byte).
    pub version: u8,
    /// Spot blank flag (halfword 54 low byte).
    pub spot_blank: u8,
    /// Offset to the symbology block in halfwords from the start of the message
    /// (halfwords 55-56); 0 when absent. Stand-alone tabular products reuse it
    /// for their page block.
    pub symbology_offset: u32,
    /// Offset to the graphic alphanumeric block in halfwords (halfwords 57-58); 0 when absent.
    pub graphic_offset: u32,
    /// Offset to the tabular alphanumeric block in halfwords (halfwords 59-60); 0 when absent.
    pub tabular_offset: u32,
    /// True when the data after the Product Description Block was a bzip2
    /// stream (the product's halfword 51 is then 1); block offsets refer to the
    /// decompressed message.
    pub compressed: bool,
}

impl ProductDescription {
    /// Parses halfwords 1-60 at the start of `message` (Message Header Block
    /// first). Does not check the block divider; `compressed` is left false.
    pub(crate) fn parse(message: &[u8]) -> Result<Self, Level3Error> {
        let what = "product description block";
        let bytes = slice(message, 0, HEADER_BYTES, what)?;
        let mut halfwords = [0u16; HEADER_HALFWORDS];
        for (hw, pair) in halfwords.iter_mut().zip(bytes.chunks_exact(2)) {
            *hw = u16::from_be_bytes([pair[0], pair[1]]);
        }
        let volume_date = be_u16(bytes, 40, what)?;
        let volume_seconds = be_u32(bytes, 42, what)?;
        let generation_date = be_u16(bytes, 46, what)?;
        let generation_seconds = be_u32(bytes, 48, what)?;
        Ok(Self {
            latitude_deg: f64::from(be_i32(bytes, 20, what)?) * 0.001,
            longitude_deg: f64::from(be_i32(bytes, 24, what)?) * 0.001,
            height_ft: be_i16(bytes, 28, what)?,
            product_code: be_i16(bytes, 30, what)?,
            operational_mode: be_u16(bytes, 32, what)?,
            vcp: be_u16(bytes, 34, what)?,
            sequence_number: be_i16(bytes, 36, what)?,
            volume_scan_number: be_u16(bytes, 38, what)?,
            volume_scan_time: julian_time(volume_date, volume_seconds, "volume scan time")?,
            generation_time: julian_time(generation_date, generation_seconds, "generation time")?,
            elevation_number: be_u16(bytes, 56, what)?,
            halfwords,
            version: bytes[106],
            spot_blank: bytes[107],
            symbology_offset: be_u32(bytes, 108, what)?,
            graphic_offset: be_u32(bytes, 112, what)?,
            tabular_offset: be_u32(bytes, 116, what)?,
            compressed: false,
        })
    }

    /// ICD halfword `n` (1-60) as an unsigned value, or `None` outside 1-60.
    pub fn halfword(&self, n: usize) -> Option<u16> {
        n.checked_sub(1)
            .and_then(|i| self.halfwords.get(i))
            .copied()
    }
}

/// `1970-01-01 + (date - 1) days + seconds` (ICD 2620001 Figure 3-3 note).
fn julian_time(date: u16, seconds: u32, field: &'static str) -> Result<DateTime<Utc>, Level3Error> {
    let total = (i64::from(date) - 1) * 86_400 + i64::from(seconds);
    DateTime::from_timestamp(total, 0).ok_or(Level3Error::InvalidTimestamp {
        field,
        date,
        seconds,
    })
}

/// A Level III message with its transmission framing removed.
pub(crate) struct Framed<'a> {
    pub(crate) text_header: Option<TextHeader>,
    /// The message, starting at the Message Header Block.
    pub(crate) message: Cow<'a, [u8]>,
}

/// Removes NOAAPort, WMO/AWIPS and zlib framing (`docs/level3/reference.md` section 3):
///
/// 1. optional NOAAPort start-of-header line and sequence number;
/// 2. optional WMO abbreviated heading and AWIPS identifier line;
/// 3. optional NOAAPort `\r\r\n\x03` trailer;
/// 4. optional zlib frames, whose output starts with a NOAAPort communications
///    control block (first byte `0x40`, second byte its length in halfwords)
///    and repeats the WMO/AWIPS lines.
///
/// A `NOUS` heading marks a plain-text message and yields
/// [`Level3Error::TextOnly`] (the same rule MetPy uses).
pub(crate) fn unwrap_framing(bytes: &[u8]) -> Result<Framed<'_>, Level3Error> {
    let mut pos = 0;
    let mut noaaport_sequence = None;
    if bytes.starts_with(SOH_LINE) {
        pos = SOH_LINE.len();
        if let Some((sequence, end)) = parse_sequence(bytes, pos) {
            noaaport_sequence = Some(sequence);
            pos = end;
        }
    }
    let outer = parse_heading(bytes, pos);
    if let Some(heading) = &outer {
        pos = heading.end;
        if heading.data_designator.starts_with("NOUS") {
            return Err(Level3Error::TextOnly {
                heading: heading.wmo_heading.clone(),
            });
        }
    }
    let mut body = bytes.get(pos..).unwrap_or_default();
    if let Some(stripped) = body.strip_suffix(ETX_TRAILER) {
        body = stripped;
    }

    let mut zlib_frames = 0;
    let mut inner = None;
    let message = if decompress::looks_like_zlib(body) {
        let (mut out, frames) = decompress::inflate_zlib_frames(body)?;
        zlib_frames = frames;
        let mut start = 0;
        if let [0x40, halfwords, ..] = out.as_slice() {
            let ccb = 2 * usize::from(*halfwords);
            if ccb <= out.len() {
                start = ccb;
            }
        }
        if let Some(heading) = parse_heading(&out, start) {
            start = heading.end;
            inner = Some(heading);
        }
        out.drain(..start);
        Cow::Owned(out)
    } else {
        Cow::Borrowed(body)
    };

    let text_header = outer.or(inner).map(|h| TextHeader {
        noaaport_sequence,
        wmo_heading: h.wmo_heading,
        data_designator: h.data_designator,
        originator: h.originator,
        day_time: h.day_time,
        indicator: h.indicator,
        awips_id: h.awips_id,
        zlib_frames,
    });
    Ok(Framed {
        text_header,
        message,
    })
}

struct Heading {
    wmo_heading: String,
    data_designator: String,
    originator: String,
    day_time: String,
    indicator: Option<String>,
    awips_id: Option<String>,
    /// Offset just past the heading (and AWIPS line when present).
    end: usize,
}

fn all(bytes: &[u8], class: fn(&u8) -> bool) -> bool {
    bytes.iter().all(class)
}

fn ascii(bytes: &[u8]) -> String {
    bytes.iter().copied().map(char::from).collect()
}

/// WMO abbreviated heading `T1T2A1A2ii CCCC YYGGgg[ BBB]\r\r\n` at `pos`,
/// followed by an optional AWIPS identifier line.
fn parse_heading(buf: &[u8], pos: usize) -> Option<Heading> {
    let b = buf.get(pos..)?;
    let line = b.get(..18)?;
    let well_formed = all(&line[..4], u8::is_ascii_uppercase)
        && all(&line[4..6], u8::is_ascii_digit)
        && line[6] == b' '
        && all(&line[7..11], u8::is_ascii_uppercase)
        && line[11] == b' '
        && all(&line[12..18], u8::is_ascii_digit);
    if !well_formed {
        return None;
    }
    let mut len = 18;
    let mut indicator = None;
    if let Some([b' ', bbb @ ..]) = b.get(18..22)
        && all(bbb, u8::is_ascii_uppercase)
        && b.get(22..25) == Some(CRCRLF)
    {
        indicator = Some(ascii(bbb));
        len = 22;
    }
    if b.get(len..len + 3) != Some(CRCRLF) {
        return None;
    }
    let mut end = len + 3;
    let awips_id = parse_awips(b, end).map(|(id, awips_end)| {
        end = awips_end;
        id
    });
    Some(Heading {
        wmo_heading: ascii(&b[..len]),
        data_designator: ascii(&line[..6]),
        originator: ascii(&line[7..11]),
        day_time: ascii(&line[12..18]),
        indicator,
        awips_id,
        end: pos + end,
    })
}

/// AWIPS identifier line `[A-Z0-9]{3,6} *\r\r\n` at `pos`; returns the identifier
/// and the offset after the line.
fn parse_awips(b: &[u8], pos: usize) -> Option<(String, usize)> {
    let rest = b.get(pos..)?;
    let n = rest
        .iter()
        .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        .count();
    if !(3..=6).contains(&n) {
        return None;
    }
    let spaces = rest[n..].iter().take_while(|&&c| c == b' ').count();
    let after = n + spaces;
    (rest.get(after..after + 3) == Some(CRCRLF)).then(|| (ascii(&rest[..n]), pos + after + 3))
}

/// NOAAPort sequence number line `\d{3,5} ?\r\r\n` at `pos`.
fn parse_sequence(b: &[u8], pos: usize) -> Option<(String, usize)> {
    let rest = b.get(pos..)?;
    let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(3..=5).contains(&n) {
        return None;
    }
    let after = if rest.get(n) == Some(&b' ') { n + 1 } else { n };
    (rest.get(after..after + 3) == Some(CRCRLF)).then(|| (ascii(&rest[..n]), pos + after + 3))
}
