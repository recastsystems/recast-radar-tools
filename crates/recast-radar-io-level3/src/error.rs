//! Errors returned by the Level III decoder.
//!
//! Byte offsets in errors count from the start of the Level III message (the
//! Message Header Block) after any NOAAPort/WMO framing has been removed and,
//! for bzip2-compressed products, after decompression.

use thiserror::Error;

/// Error decoding a Level III product.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Level3Error {
    /// The input ended before a structure the format requires.
    #[error("truncated {what}: needs {needed} bytes at byte {offset}, {available} available")]
    Truncated {
        /// Structure being read.
        what: &'static str,
        /// Byte offset of the structure.
        offset: usize,
        /// Bytes the structure needs.
        needed: usize,
        /// Bytes available from `offset` to the end of the enclosing data.
        available: usize,
    },

    /// The file is a plain-text message (WMO heading `NOUS..`, e.g. a free text
    /// message) with no binary Level III message. Returned by
    /// [`crate::decode_product`]; [`crate::decode_message`] decodes the text
    /// into [`crate::Level3Message::Text`].
    #[error("plain-text message {heading:?} carries no binary product")]
    TextOnly {
        /// The WMO abbreviated heading, e.g. `NOUS63 KABR 281331`.
        heading: String,
    },

    /// The message has no Product Description Block, e.g. a General Status
    /// Message (message code 2, ICD 2620001 Figure 3-17). Returned by
    /// [`crate::decode_product`]; [`crate::decode_message`] decodes a General
    /// Status Message into [`crate::Level3Message::GeneralStatus`] and returns
    /// this error only for other message codes.
    #[error("message code {code} carries no Product Description Block")]
    NotAProduct {
        /// Message code (Message Header Block halfword 1).
        code: i16,
    },

    /// A message block whose contents contradict its ICD layout, e.g. a General
    /// Status Message block shorter than the 82 bytes of Figure 3-17.
    #[error("message code {code}: {reason}")]
    InvalidMessage {
        /// Message code (Message Header Block halfword 1).
        code: i16,
        /// What is wrong.
        reason: String,
    },

    /// A product-specific reader was given another product, e.g.
    /// [`crate::vwp::VadWindProfile::from_product`] a product other than 48.
    #[error("product code {found}, expected {expected}")]
    UnexpectedProduct {
        /// Product code the reader handles.
        expected: i16,
        /// Product code of the product given.
        found: i16,
    },

    /// A block divider or block ID does not have the value the ICD requires.
    #[error("{what} at byte {offset}: expected {expected}, found {found}")]
    BadBlockHeader {
        /// Field being checked.
        what: &'static str,
        /// Byte offset of the field.
        offset: usize,
        /// Value the ICD requires.
        expected: i16,
        /// Value found.
        found: i16,
    },

    /// A display packet's own length fields run past the end of its layer or page.
    #[error(
        "packet {code} at byte {offset} runs past the end of its layer or page ({available} bytes left)"
    )]
    PacketOverrun {
        /// Packet code.
        code: u16,
        /// Byte offset of the packet code.
        offset: usize,
        /// Bytes from the packet code to the end of the layer or page.
        available: usize,
    },

    /// A known packet whose contents do not follow its ICD layout.
    #[error("packet {code}: {reason}")]
    InvalidPacket {
        /// Packet code.
        code: u16,
        /// What is wrong.
        reason: String,
    },

    /// No decoder for this packet code yet. [`crate::decode_product`] never
    /// returns this: it keeps such packets as [`crate::Packet::Unknown`].
    #[error("packet code {0} (0x{0:04X}) is not supported")]
    UnsupportedPacket(u16),

    /// A zlib frame of a NOAAPort-wrapped product could not be decompressed.
    #[error("zlib frame {frame}: {reason}")]
    Zlib {
        /// Zero-based index of the failing frame.
        frame: u32,
        /// What went wrong.
        reason: String,
    },

    /// The bzip2 stream after the Product Description Block could not be decompressed.
    #[error("bzip2: {reason}")]
    Bzip2 {
        /// What went wrong.
        reason: String,
    },

    /// Decompressed data would exceed the decoder's size limit.
    #[error("{format} output exceeds the {limit}-byte limit")]
    DecompressedTooLarge {
        /// `"zlib"` or `"bzip2"`.
        format: &'static str,
        /// The limit in bytes.
        limit: usize,
    },

    /// The product has no radial, raster or generic data array to convert
    /// into a volume ([`crate::Level3Product::to_volume`]).
    #[error("product {code} has no radial, raster or generic data array")]
    NoDataArray {
        /// Product code.
        code: i16,
    },

    /// The ICD gives no range bin or raster cell size for the product, so its
    /// data array cannot be placed on a range coordinate.
    #[error("product {code}: {what} is not known")]
    UnknownGeometry {
        /// Product code.
        code: i16,
        /// What is missing: "range bin size" or "raster cell size".
        what: &'static str,
    },

    /// A date/time pair is outside the range `chrono` represents.
    #[error("{field}: date {date} and {seconds} s are not a representable time")]
    InvalidTimestamp {
        /// Field being converted.
        field: &'static str,
        /// Modified Julian date (1 = 1970-01-01).
        date: u16,
        /// Seconds after midnight.
        seconds: u32,
    },
}
