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
    /// inside the zlib data is only used if there is no outer heading, and
    /// is kept in [`zlib_wmo_heading`](Self::zlib_wmo_heading) and
    /// [`zlib_awips_id`](Self::zlib_awips_id) either way.
    pub zlib_frames: u32,
    /// The NOAAPort communications control block at the start of the zlib
    /// data (first byte `0x40`, second byte its length in halfwords), as
    /// stored; `None` when the data has none. Observed: 24 bytes.
    pub communications_control_block: Option<Vec<u8>>,
    /// The WMO abbreviated heading repeated inside the zlib data (after the
    /// communications control block), without its line ending.
    pub zlib_wmo_heading: Option<String>,
    /// The AWIPS identifier repeated inside the zlib data.
    pub zlib_awips_id: Option<String>,
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
    /// Some products of 1993-1994 give the end of the message here and carry
    /// no block (observed; the product then has no tabular block).
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

    /// [`operational_mode`](Self::operational_mode) as an [`OperationalMode`].
    pub fn mode(&self) -> OperationalMode {
        OperationalMode::from_code(self.operational_mode)
    }
}

/// Operational (weather) mode: Product Description Block halfword 17 and
/// General Status Message halfword 12 (ICD 2620001 Figures 3-6 and 3-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OperationalMode {
    /// 0: maintenance mode.
    Maintenance,
    /// 1: clear air mode.
    ClearAir,
    /// 2: precipitation/severe weather mode.
    Precipitation,
    /// Any other value, which the ICD does not define.
    Other(u16),
}

impl OperationalMode {
    /// The mode for an ICD code.
    pub fn from_code(code: u16) -> Self {
        match code {
            0 => Self::Maintenance,
            1 => Self::ClearAir,
            2 => Self::Precipitation,
            other => Self::Other(other),
        }
    }

    /// The ICD code of the mode.
    pub fn code(self) -> u16 {
        match self {
            Self::Maintenance => 0,
            Self::ClearAir => 1,
            Self::Precipitation => 2,
            Self::Other(code) => code,
        }
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

/// A file with its transmission framing removed.
pub(crate) enum Unwrapped<'a> {
    /// A binary message.
    Binary(Framed<'a>),
    /// A plain-text message (WMO heading `NOUS..`).
    Text {
        text_header: TextHeader,
        /// The bytes after the heading and AWIPS identifier lines, without the
        /// transmission trailer.
        text: &'a [u8],
    },
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
/// A `NOUS` heading marks a plain-text message (the same rule MetPy uses) and
/// yields [`Unwrapped::Text`]. Its last four bytes are dropped when the first
/// three of them are `\r\r\n` (the NOAAPort trailer) or `FF FF 0A`, again as
/// MetPy does; observed: the Free Text Message ends `FF FF 0A 00`.
pub(crate) fn unwrap_framing(bytes: &[u8]) -> Result<Unwrapped<'_>, Level3Error> {
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
    if let Some(heading) = outer
        .as_ref()
        .filter(|h| h.data_designator.starts_with("NOUS"))
    {
        let mut text = bytes.get(heading.end..).unwrap_or_default();
        if let [.., a, b, c, _] = text
            && matches!([*a, *b, *c], [b'\r', b'\r', b'\n'] | [0xFF, 0xFF, b'\n'])
        {
            text = &text[..text.len() - 4];
        }
        return Ok(Unwrapped::Text {
            text_header: heading.to_text_header(noaaport_sequence, 0),
            text,
        });
    }
    if let Some(heading) = &outer {
        pos = heading.end;
    }
    let mut body = bytes.get(pos..).unwrap_or_default();
    if let Some(stripped) = body.strip_suffix(ETX_TRAILER) {
        body = stripped;
    }
    if let Some(heading) = outer.as_ref().filter(|_| is_plain_text(body)) {
        return Ok(Unwrapped::Text {
            text_header: heading.to_text_header(noaaport_sequence, 0),
            text: body,
        });
    }

    let mut zlib_frames = 0;
    let mut inner = None;
    let mut ccb = None;
    let message = if decompress::looks_like_zlib(body) {
        let (mut out, frames) = decompress::inflate_zlib_frames(body)?;
        zlib_frames = frames;
        let mut start = 0;
        if let [0x40, halfwords, ..] = out.as_slice() {
            let len = 2 * usize::from(*halfwords);
            if len <= out.len() {
                start = len;
                ccb = Some(out[..len].to_vec());
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

    let text_header = outer.as_ref().or(inner.as_ref()).map(|h| {
        let mut header = h.to_text_header(noaaport_sequence, zlib_frames);
        header.communications_control_block = ccb;
        header.zlib_wmo_heading = inner.as_ref().map(|i| i.wmo_heading.clone());
        header.zlib_awips_id = inner.as_ref().and_then(|i| i.awips_id.clone());
        header
    });
    Ok(Unwrapped::Binary(Framed {
        text_header,
        message,
    }))
}

/// True when `bytes` start the way a Level III file does (see
/// [`crate::looks_like_level3`]).
pub(crate) fn looks_like_level3(bytes: &[u8]) -> bool {
    let mut pos = 0;
    if bytes.starts_with(SOH_LINE) {
        pos = SOH_LINE.len();
        if let Some((_, end)) = parse_sequence(bytes, pos) {
            pos = end;
        }
    }
    match parse_heading(bytes, pos) {
        Some(heading) if heading.data_designator.starts_with("NOUS") => true,
        Some(heading) => {
            let body = bytes.get(heading.end..).unwrap_or_default();
            decompress::looks_like_zlib(body)
                || looks_like_message(body)
                || (heading.data_designator.starts_with("SDUS") && is_plain_text(body))
        }
        None => pos == 0 && looks_like_message(bytes),
    }
}

/// A body of plain text after a WMO heading that is not `NOUS`: not a
/// Message Header Block and every byte printable ASCII, a line ending, a tab
/// or the record separator 0x1E. Observed: the Radar Observation bulletins
/// (AWIPS `ROBxxx`, heading `SDUS4x`) the NCEI Level III archive holds
/// beside the products, text starting with 0x1E.
fn is_plain_text(body: &[u8]) -> bool {
    !body.is_empty()
        && !looks_like_message(body)
        && body
            .iter()
            .all(|&b| matches!(b, b' '..=b'~' | b'\r' | b'\n' | b'\t' | 0x1E))
}

/// A Message Header Block followed by a block divider (-1) at halfword 10:
/// a General Status Message (code 2) of two blocks sent at a time of day
/// (halfwords 3-4, seconds after midnight), or a product whose Product
/// Description Block repeats the message code as its product code (halfword
/// 16), as every product and stand-alone alphanumeric message in the corpus
/// does.
///
/// The block count and time keep a headerless Level II LDM record out: its
/// 4-byte size is `00 02 xx xx` for a record of 128-192 KiB and its bzip2
/// stream starts `BZh` at byte 4, which as a time is past 10^9 seconds.
fn looks_like_message(message: &[u8]) -> bool {
    let halfword = |offset: usize| {
        message
            .get(offset..offset + 2)
            .map(|b| i16::from_be_bytes([b[0], b[1]]))
    };
    let time_of_day = message
        .get(4..8)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .is_some_and(|seconds| (0..=86_400).contains(&seconds));
    match (halfword(0), halfword(MESSAGE_HEADER_BYTES)) {
        (Some(2), Some(-1)) => time_of_day && halfword(16) == Some(2),
        (Some(code), Some(-1)) if code > 2 => halfword(30) == Some(code),
        _ => false,
    }
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

impl Heading {
    fn to_text_header(&self, noaaport_sequence: Option<String>, zlib_frames: u32) -> TextHeader {
        TextHeader {
            noaaport_sequence,
            wmo_heading: self.wmo_heading.clone(),
            data_designator: self.data_designator.clone(),
            originator: self.originator.clone(),
            day_time: self.day_time.clone(),
            indicator: self.indicator.clone(),
            awips_id: self.awips_id.clone(),
            zlib_frames,
            communications_control_block: None,
            zlib_wmo_heading: None,
            zlib_awips_id: None,
        }
    }
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
