//! Contour packets (ICD 2620001 Figure 3-8a): Set Color Level (0x0802), Linked
//! Contour Vectors (0x0E03) and Unlinked Contour Vectors (0x3501).
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Contour packet (0x0802, 0x0E03 or 0x3501).
#[derive(Debug, Clone, PartialEq)]
pub struct ContourPacket {
    /// Packet code: 0x0802, 0x0E03 or 0x3501.
    pub code: u16,
}

impl ContourPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one contour packet (0x0802, 0x0E03, 0x3501). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
