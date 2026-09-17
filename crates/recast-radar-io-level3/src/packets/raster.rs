//! Raster image packets: Raster Data Packet (0xBA07, 0xBA0F, ICD 2620001 Figure 3-11),
//! Digital Precipitation Data Array (17, Figure 3-11a), Precipitation Rate Data
//! Array (18, Figure 3-11b) and Digital Raster Data Array (33, Figure 3-11d).
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Raster data packet (0xBA07, 0xBA0F, 18, 33).
#[derive(Debug, Clone, PartialEq)]
pub struct RasterPacket {
    /// Packet code: 0xBA07, 0xBA0F, 18 or 33.
    pub code: u16,
}

impl RasterPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Digital precipitation data array (17).
#[derive(Debug, Clone, PartialEq)]
pub struct DigitalPrecipPacket {
    /// Packet code: 17.
    pub code: u16,
}

impl DigitalPrecipPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one raster packet (0xBA07, 0xBA0F, 17, 18, 33). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
