//! Vector packets: linked vectors (6, 9; ICD 2620001 Figure 3-7) and unlinked
//! vectors (7, 10; Figure 3-8). See `docs/level3/reference.md` section 6.2.
//!
//! | Code | Fields after the code and length halfword |
//! |---|---|
//! | 6 | I, J start, then (I, J) end points |
//! | 9 | color level, I, J start, then (I, J) end points |
//! | 7 | (I begin, J begin, I end, J end) per vector |
//! | 10 | color level, then (I begin, J begin, I end, J end) per vector |
//!
//! Coordinates are 1/4 km from the radar in the Product Symbology Block and
//! screen pixels in the Graphic Alphanumeric Block (section 3.3.3). The point
//! and segment types here are shared with the contour packets.

use super::Packet;
use crate::Level3Error;
use crate::budget::Budget;

/// A point in packet coordinates (1/4 km or screen pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    /// I coordinate (east, or screen right).
    pub i: i16,
    /// J coordinate (north, or screen down for pixels).
    pub j: i16,
}

/// One unlinked vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Segment {
    /// Beginning point.
    pub begin: Point,
    /// End point.
    pub end: Point,
}

/// The vectors of a vector or contour packet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Vectors {
    /// Linked vectors: a line through the points in order. The first point is
    /// the starting point; each further point ends one vector.
    Linked(Vec<Point>),
    /// Unlinked vectors: independent segments.
    Unlinked(Vec<Segment>),
}

impl Vectors {
    /// Every vector as a segment, in file order (linked vectors join consecutive points).
    pub fn segments(&self) -> Vec<Segment> {
        match self {
            Self::Linked(points) => points
                .windows(2)
                .map(|pair| Segment {
                    begin: pair[0],
                    end: pair[1],
                })
                .collect(),
            Self::Unlinked(segments) => segments.clone(),
        }
    }
}

/// Linked or unlinked vector packet (6, 7, 9 or 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorPacket {
    /// Packet code: 6 or 9 (linked), 7 or 10 (unlinked).
    pub code: u16,
    /// Color level of the vectors, 0-15 (packets 9 and 10 only).
    pub color_level: Option<u16>,
    /// The vectors: [`Vectors::Linked`] for 6 and 9, [`Vectors::Unlinked`] for 7 and 10.
    pub vectors: Vectors,
}

impl VectorPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one vector packet (6, 7, 9, 10). `bytes` is the complete packet,
/// starting with its 2-byte code, as sized by the dispatcher from its length
/// halfword.
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    let (linked, with_color) = match code {
        6 => (true, false),
        9 => (true, true),
        7 => (false, false),
        10 => (false, true),
        _ => return Err(Level3Error::UnsupportedPacket(code)),
    };
    let mut data = bytes.get(4..).unwrap_or_default();
    let mut color_level = None;
    if with_color {
        let Some((level, rest)) = data.split_first_chunk::<2>() else {
            return Err(Level3Error::InvalidPacket {
                code,
                reason: "no room for the color level".into(),
            });
        };
        color_level = Some(u16::from_be_bytes(*level));
        data = rest;
    }
    let vectors = if linked {
        if data.is_empty() {
            return Err(Level3Error::InvalidPacket {
                code,
                reason: "linked vectors without a starting point".into(),
            });
        }
        Vectors::Linked(points(code, data, budget)?)
    } else {
        Vectors::Unlinked(segments(code, data, budget)?)
    };
    Ok(Packet::Vectors(VectorPacket {
        code,
        color_level,
        vectors,
    }))
}

/// `(I, J)` halfword pairs filling `data` (4 bytes each).
pub(crate) fn points(
    code: u16,
    data: &[u8],
    budget: &mut Budget,
) -> Result<Vec<Point>, Level3Error> {
    let chunks = data.chunks_exact(4);
    if !chunks.remainder().is_empty() {
        return Err(Level3Error::InvalidPacket {
            code,
            reason: format!("{} bytes of points is not a multiple of 4", data.len()),
        });
    }
    budget.charge::<Point>(chunks.len(), "vector points")?;
    Ok(chunks.map(|c| point([c[0], c[1], c[2], c[3]])).collect())
}

/// `(I begin, J begin, I end, J end)` halfword groups filling `data` (8 bytes each).
pub(crate) fn segments(
    code: u16,
    data: &[u8],
    budget: &mut Budget,
) -> Result<Vec<Segment>, Level3Error> {
    let chunks = data.chunks_exact(8);
    if !chunks.remainder().is_empty() {
        return Err(Level3Error::InvalidPacket {
            code,
            reason: format!("{} bytes of vectors is not a multiple of 8", data.len()),
        });
    }
    budget.charge::<Segment>(chunks.len(), "vector segments")?;
    Ok(chunks
        .map(|c| Segment {
            begin: point([c[0], c[1], c[2], c[3]]),
            end: point([c[4], c[5], c[6], c[7]]),
        })
        .collect())
}

fn point([i0, i1, j0, j1]: [u8; 4]) -> Point {
    Point {
        i: i16::from_be_bytes([i0, i1]),
        j: i16::from_be_bytes([j0, j1]),
    }
}
