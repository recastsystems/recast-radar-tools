//! Radial image packets: Digital Radial Data Array (16, ICD 2620001 Figure 3-11c)
//! and Radial Data Packet with 16 run-length levels (0xAF1F, Figure 3-10).
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Radial data packet (16 or 0xAF1F).
#[derive(Debug, Clone, PartialEq)]
pub struct RadialPacket {
    /// Packet code: 16 or 0xAF1F.
    pub code: u16,
}

impl RadialPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one radial packet (16, 0xAF1F). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
