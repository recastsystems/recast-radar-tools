//! Display data packets (ICD 2620001 Figures 3-7 to 3-15c) and their dispatch.
//!
//! `decode_packets` walks a layer or page: it reads each packet code, sizes
//! the packet from its own length fields (`docs/level3/reference.md` section
//! 6), and hands the packet's bytes to its family module:
//!
//! | Module | Codes |
//! |---|---|
//! | [`radial`] | 16, 0xAF1F |
//! | [`raster`] | 0xBA07, 0xBA0F, 17, 18, 33 |
//! | [`generic`] | 28, 29 |
//! | [`text`] | 1, 2, 8 |
//! | [`vectors`] | 6, 7, 9, 10 |
//! | [`contour`] | 0x0802, 0x0E03, 0x3501 |
//! | [`symbols`] | 3, 4, 5, 11-15, 19-26 (including cell trend 21/22 and SCIT 23/24) |
//!
//! A family decoder returning [`Level3Error::UnsupportedPacket`] for the code
//! it was given leaves the packet as [`Packet::Unknown`]. A packet code the
//! walker cannot size ends the walk: the rest of the layer or page becomes one
//! [`Packet::Unknown`].

pub mod contour;
pub mod generic;
pub mod radial;
pub mod raster;
pub mod symbols;
pub mod text;
pub mod vectors;

pub use contour::ContourPacket;
pub use generic::GenericPacket;
pub use radial::RadialPacket;
pub use raster::{DigitalPrecipPacket, RasterPacket};
pub use symbols::SymbolPacket;
pub use text::TextPacket;
pub use vectors::VectorPacket;

use crate::Level3Error;

/// One display data packet.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Packet {
    /// Digital radial data array (16) or run-length radial data (0xAF1F).
    Radial(RadialPacket),
    /// Raster data (0xBA07, 0xBA0F), precipitation rate array (18) or digital raster array (33).
    Raster(RasterPacket),
    /// Digital precipitation data array (17).
    DigitalPrecip(DigitalPrecipPacket),
    /// Generic data (28, 29).
    Generic(GenericPacket),
    /// Text and special symbols (1, 2, 8).
    Text(TextPacket),
    /// Storm, mesocyclone, TVS, hail, SCIT and point feature symbols.
    Symbol(SymbolPacket),
    /// Linked and unlinked vectors (6, 7, 9, 10).
    Vectors(VectorPacket),
    /// Contour packets (0x0802, 0x0E03, 0x3501).
    Contour(ContourPacket),
    /// A packet not decoded (yet). `bytes` is the complete packet starting with
    /// its 2-byte code; for a code the walker cannot size, it runs to the end of
    /// the layer or page.
    Unknown {
        /// Packet code.
        code: u16,
        /// Packet bytes, including the code.
        bytes: Vec<u8>,
    },
}

impl Packet {
    /// The packet code.
    pub fn code(&self) -> u16 {
        match self {
            Self::Radial(p) => p.code(),
            Self::Raster(p) => p.code(),
            Self::DigitalPrecip(p) => p.code(),
            Self::Generic(p) => p.code(),
            Self::Text(p) => p.code(),
            Self::Symbol(p) => p.code(),
            Self::Vectors(p) => p.code(),
            Self::Contour(p) => p.code(),
            Self::Unknown { code, .. } => *code,
        }
    }
}

/// Decodes the packets filling `bytes` (one layer, page, or the contents of a
/// nesting packet). `base` is the message byte offset of `bytes[0]`, used in errors.
pub(crate) fn decode_packets(bytes: &[u8], base: usize) -> Result<Vec<Packet>, Level3Error> {
    let mut packets = Vec::new();
    let mut p = 0;
    while let Some(rest) = bytes.get(p..).filter(|rest| !rest.is_empty()) {
        let &[hi, lo, ..] = rest else {
            return Err(Level3Error::Truncated {
                what: "packet code",
                offset: base + p,
                needed: 2,
                available: rest.len(),
            });
        };
        let code = u16::from_be_bytes([hi, lo]);
        match packet_size(code, rest) {
            Ok(Some(size)) => {
                packets.push(dispatch(code, &rest[..size])?);
                p += size;
            }
            Ok(None) => {
                packets.push(Packet::Unknown {
                    code,
                    bytes: rest.to_vec(),
                });
                break;
            }
            Err(Overrun) => {
                return Err(Level3Error::PacketOverrun {
                    code,
                    offset: base + p,
                    available: rest.len(),
                });
            }
        }
    }
    Ok(packets)
}

/// Hands one sized packet to its family decoder.
fn dispatch(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let decoded = match code {
        16 | 0xAF1F => radial::decode(code, bytes),
        17 | 18 | 33 | 0xBA07 | 0xBA0F => raster::decode(code, bytes),
        28 | 29 => generic::decode(code, bytes),
        1 | 2 | 8 => text::decode(code, bytes),
        6 | 7 | 9 | 10 => vectors::decode(code, bytes),
        0x0802 | 0x0E03 | 0x3501 => contour::decode(code, bytes),
        3..=5 | 11..=15 | 19..=26 => symbols::decode(code, bytes),
        _ => Err(Level3Error::UnsupportedPacket(code)),
    };
    match decoded {
        Err(Level3Error::UnsupportedPacket(unsupported)) if unsupported == code => {
            Ok(Packet::Unknown {
                code,
                bytes: bytes.to_vec(),
            })
        }
        other => other,
    }
}

/// A packet's length fields point past the end of its layer or page.
struct Overrun;

fn u16_at(b: &[u8], offset: usize) -> Result<u16, Overrun> {
    match b.get(offset..offset + 2) {
        Some(&[hi, lo]) => Ok(u16::from_be_bytes([hi, lo])),
        _ => Err(Overrun),
    }
}

fn u32_at(b: &[u8], offset: usize) -> Result<u32, Overrun> {
    match b.get(offset..offset + 4) {
        Some(&[b0, b1, b2, b3]) => Ok(u32::from_be_bytes([b0, b1, b2, b3])),
        _ => Err(Overrun),
    }
}

/// Size in bytes of the packet starting at `b[0]` with `code`, from its own
/// length fields; `None` for codes whose layout is not known.
fn packet_size(code: u16, b: &[u8]) -> Result<Option<usize>, Overrun> {
    let size = match code {
        // code, length of data (bytes), data (Figures 3-7, 3-8, 3-8b, 3-12 to 3-15a).
        1..=15 | 19..=26 => 4 + usize::from(u16_at(b, 2)?),
        // 14-byte header; per radial: bytes (16) or run halfwords (0xAF1F), angles, data.
        16 => radial_size(b, 1)?,
        0xAF1F => radial_size(b, 2)?,
        // Rows count at 18, 22-byte header; per row: byte count, data (Figure 3-11).
        0xBA07 | 0xBA0F => rows_size(b, 18, 22)?,
        // Rows count at 8, 10-byte header (Figures 3-11a, 3-11b).
        17 | 18 => rows_size(b, 8, 10)?,
        // Rows count at 12, 14-byte header (Figure 3-11d).
        33 => rows_size(b, 12, 14)?,
        // code, reserved, INT*4 length, XDR data (Figure 3-15c).
        28 | 29 => usize::try_from(u32_at(b, 4)?)
            .ok()
            .and_then(|len| len.checked_add(8))
            .ok_or(Overrun)?,
        // Set color level: code, 0x0002, level (Figure 3-8a).
        0x0802 => 6,
        // Linked contour: code, 0x8000, I, J, length of vectors, vectors.
        0x0E03 => 10 + usize::from(u16_at(b, 8)?),
        // Unlinked contour: code, length of vectors, vectors.
        0x3501 => 4 + usize::from(u16_at(b, 2)?),
        _ => return Ok(None),
    };
    if size > b.len() {
        return Err(Overrun);
    }
    Ok(Some(size))
}

/// Radial packet size: `bytes_per_unit` is 1 for packet 16 (count in bytes) and
/// 2 for 0xAF1F (count in run-length halfwords).
fn radial_size(b: &[u8], bytes_per_unit: usize) -> Result<usize, Overrun> {
    let radials = u16_at(b, 12)?;
    let mut q = 14;
    for _ in 0..radials {
        q += 6 + bytes_per_unit * usize::from(u16_at(b, q)?);
        if q > b.len() {
            return Err(Overrun);
        }
    }
    Ok(q)
}

/// Row-based packet size: row count at `count_at`, rows from `header`, each a
/// byte count followed by that many bytes.
fn rows_size(b: &[u8], count_at: usize, header: usize) -> Result<usize, Overrun> {
    let rows = u16_at(b, count_at)?;
    let mut q = header;
    for _ in 0..rows {
        q += 2 + usize::from(u16_at(b, q)?);
        if q > b.len() {
            return Err(Overrun);
        }
    }
    Ok(q)
}
