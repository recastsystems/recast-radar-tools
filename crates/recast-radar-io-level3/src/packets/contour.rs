//! Contour packets (ICD 2620001 Figure 3-8a, `docs/level3/reference.md` section
//! 6.2): Set Color Level (0x0802), Linked Contour Vectors (0x0E03) and Unlinked
//! Contour Vectors (0x3501).
//!
//! | Code | Fields after the code |
//! |---|---|
//! | 0x0802 | color value indicator 0x0002, contour level |
//! | 0x0E03 | initial point indicator 0x8000, I, J start, length (4 x vectors), (I, J) end points |
//! | 0x3501 | length (8 x vectors), (I begin, J begin, I end, J end) per vector |
//!
//! A 0x0802 packet sets the level of the contour packets that follow it in the
//! same layer. Coordinates are 1/4 km from the radar.

use super::Packet;
use super::vectors::{self, Point, Vectors};
use crate::Level3Error;
use crate::budget::Budget;

/// Contour packet (0x0802, 0x0E03 or 0x3501).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContourPacket {
    /// Packet code: 0x0802, 0x0E03 or 0x3501.
    pub code: u16,
    /// What the packet carries.
    pub contour: Contour,
}

impl ContourPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Contents of a [`ContourPacket`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Contour {
    /// 0x0802: color level (0-15) of the contour vectors that follow.
    ColorLevel(u16),
    /// 0x0E03 ([`Vectors::Linked`], starting point first) or 0x3501
    /// ([`Vectors::Unlinked`]).
    Vectors(Vectors),
}

/// Color value indicator of packet 0x0802.
const COLOR_VALUE_INDICATOR: u16 = 0x0002;
/// Initial point indicator of packet 0x0E03.
const INITIAL_POINT_INDICATOR: u16 = 0x8000;

/// Decodes one contour packet (0x0802, 0x0E03, 0x3501). `bytes` is the complete
/// packet, starting with its 2-byte code, as sized by the dispatcher (6 bytes
/// for 0x0802, the length halfword for the others).
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    let halfword = |at: usize| {
        bytes
            .get(at..)
            .and_then(|b| b.first_chunk::<2>())
            .map(|b| u16::from_be_bytes(*b))
            .ok_or_else(|| Level3Error::InvalidPacket {
                code,
                reason: format!(
                    "packet of {} bytes ends before byte {}",
                    bytes.len(),
                    at + 2
                ),
            })
    };
    let expect_indicator = |expected: u16, name: &str| {
        let found = halfword(2)?;
        if found == expected {
            Ok(())
        } else {
            Err(Level3Error::InvalidPacket {
                code,
                reason: format!("{name} is 0x{found:04X}, expected 0x{expected:04X}"),
            })
        }
    };
    let contour = match code {
        0x0802 => {
            expect_indicator(COLOR_VALUE_INDICATOR, "color value indicator")?;
            Contour::ColorLevel(halfword(4)?)
        }
        0x0E03 => {
            expect_indicator(INITIAL_POINT_INDICATOR, "initial point indicator")?;
            // The length of vectors at byte 8 ends the 10-byte header; the
            // dispatcher sized the packet as the header plus that length.
            halfword(8)?;
            let mut points = vectors::points(code, &bytes[4..8], budget)?;
            let rest = vectors::points(code, &bytes[10..], budget)?;
            // `points` grows by the rest: charged again.
            budget.charge::<Point>(rest.len(), "contour points")?;
            points.extend(rest);
            Contour::Vectors(Vectors::Linked(points))
        }
        0x3501 => Contour::Vectors(Vectors::Unlinked(vectors::segments(
            code,
            bytes.get(4..).unwrap_or_default(),
            budget,
        )?)),
        _ => return Err(Level3Error::UnsupportedPacket(code)),
    };
    Ok(Packet::Contour(ContourPacket { code, contour }))
}
