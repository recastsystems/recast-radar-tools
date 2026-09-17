//! Generic data packets (28, 29; ICD 2620001 Figure 3-15c, Appendix E): XDR-encoded
//! product description and components.
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Generic data packet (28 or 29).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericPacket {
    /// Packet code: 28 or 29.
    pub code: u16,
}

impl GenericPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one generic data packet (28, 29). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
