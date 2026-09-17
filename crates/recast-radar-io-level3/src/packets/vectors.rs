//! Vector packets: linked vectors (6, 9; ICD 2620001 Figure 3-7) and unlinked
//! vectors (7, 10; Figure 3-8).
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Linked or unlinked vector packet (6, 7, 9 or 10).
#[derive(Debug, Clone, PartialEq)]
pub struct VectorPacket {
    /// Packet code: 6, 7, 9 or 10.
    pub code: u16,
}

impl VectorPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one vector packet (6, 7, 9, 10). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
