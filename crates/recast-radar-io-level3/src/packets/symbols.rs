//! Symbol packets (ICD 2620001 Figures 3-12 to 3-15a): mesocyclone (3), wind barb (4),
//! vector arrow (5), correlated shear (11), TVS (12), hail (13, 14), storm ID (15),
//! HDA hail (19), point feature (20), cell trend (21, 22), SCIT past/forecast
//! (23, 24, which nest packets 2, 6 and 25), STI circle (25) and ETVS (26).
//!
//! Cell trend packets 21 and 22 (product 62) are routed here although the
//! wave 1 plan's family table does not list them. Packets nested in 23 and 24
//! can be walked with `super::decode_packets` on the bytes after the 4-byte
//! header.
//!
//! Placeholder until its Task L3.3 packet family: the structs carry only the
//! packet code and `decode` reports every code as unsupported, so dispatch
//! keeps these packets as [`Packet::Unknown`].

use super::Packet;
use crate::Level3Error;

/// Symbol packet (3, 4, 5, 11-15, 19-26).
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolPacket {
    /// Packet code.
    pub code: u16,
}

impl SymbolPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one symbol packet (3, 4, 5, 11-15, 19-26). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let _ = bytes;
    Err(Level3Error::UnsupportedPacket(code))
}
