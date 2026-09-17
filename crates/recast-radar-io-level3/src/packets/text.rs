//! Text packets: Write Text (1, 8) and Write Special Symbols (2), ICD 2620001
//! Figure 3-8b.
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Text or special symbol packet (1, 2 or 8).
#[derive(Debug, Clone, PartialEq)]
pub struct TextPacket {
    /// Packet code: 1, 2 or 8.
    pub code: u16,
}

impl TextPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one text packet (1, 2, 8). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
