//! Pure-Rust decoder for NEXRAD and TDWR Level III products.
//!
//! [`decode_product`] takes the bytes of one Level III product file and returns
//! a [`Level3Product`]. [`decode_message`] takes any Level III file and also
//! decodes the messages that are not products: the General Status Message
//! ([`GeneralStatusMessage`]) and plain-text messages ([`TextMessage`]).
//!
//! Decoding a product goes through these steps:
//!
//! 1. **Framing** ([`TextHeader`]): optional NOAAPort start-of-header and
//!    sequence number, WMO abbreviated heading, AWIPS identifier, NOAAPort
//!    trailer and zlib frames with their communications control block.
//! 2. **Headers**: the Message Header Block ([`MessageHeader`], ICD 2620001
//!    Figure 3-3) and Product Description Block ([`ProductDescription`],
//!    Figure 3-6), including all 60 raw halfwords.
//! 3. **Decompression**: a bzip2 stream after the Product Description Block
//!    replaces the rest of the message (Appendix D).
//! 4. **Blocks**: the Product Symbology Block ([`Symbology`]), Graphic
//!    Alphanumeric Block ([`GraphicAlphanumeric`]) and Tabular Alphanumeric
//!    Block or stand-alone tabular data ([`TabularAlphanumeric`]).
//! 5. **Packets**: each layer or page is split into display packets and
//!    dispatched by packet code to the family modules in [`packets`]. Packets
//!    without a decoder are kept as [`Packet::Unknown`] with their bytes.
//!
//! [`product_info`] looks up a product code's mnemonic, name and kind, and
//! [`levels::DataLevels`] maps a product's data levels to physical values.
//! [`messages`] holds the General Status Message and text message types, and
//! [`vwp::VadWindProfile`] reads the winds of a VAD Wind Profile (product 48).
//!
//! [`read_level3_volume`] and [`Level3Product::to_volume`] carry every radial,
//! raster and generic data array of a product as one sweep of an FM301
//! [`recast_radar_core::model::Volume`] ([`volume`]), with the product
//! dependent halfwords of Table V decoded by name
//! ([`ProductDescription::parameters`], [`params`]) and the HRAP grid of the
//! precipitation arrays in [`hrap`].
//!
//! Every other display packet is kept as a text record
//! ([`Level3Product::display_records`], [`records`]) so that the volume
//! carries every decoded value.
//!
//! The format reference with ICD section numbers is `docs/level3/reference.md`.
//!
//! # Limits
//!
//! A product file is untrusted input. The decoder bounds what one can make
//! it allocate, and a file over a limit is an error:
//!
//! - Decompressed data, a bzip2 product body or the zlib frames of a
//!   NOAAPort file: at most 64 MiB ([`Level3Error::DecompressedTooLarge`]).
//!   The largest product in the test corpus decompresses to under 5 MB.
//! - Radial packets (16, 0xAF1F), and the radial components of generic
//!   packets (28) padded to their longest radial: at most
//!   [`packets::radial::MAX_RADIAL_CELLS`] (2^24) radials x bins
//!   ([`Level3Error::InvalidPacket`]).
//! - Raster packets (0xBA07, 0xBA0F, 17, 18, 33): at most
//!   [`packets::raster::MAX_GRID_DIMENSION`] (4096) rows and cells per row.
//! - A packet's length fields must stay inside its layer or page
//!   ([`Level3Error::PacketOverrun`]), and every
//!   count and string length in a generic packet's XDR data is checked
//!   against the bytes left before anything is allocated for it.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod blocks;
mod budget;
mod decompress;
mod error;
mod header;
pub mod hrap;
pub mod levels;
pub mod messages;
pub mod packets;
pub mod params;
mod products;
pub mod rcm;
mod read;
pub mod records;
pub mod tables;
pub mod volume;
pub mod vwp;

pub use blocks::{
    GraphicAlphanumeric, GraphicLayout, GraphicPage, Symbology, TabularAlphanumeric, TabularLayout,
    TextPage,
};
pub use budget::MAX_PRODUCT_DECODED_BYTES;
pub use error::Level3Error;
pub use header::{
    HEADER_HALFWORDS, MessageHeader, OperationalMode, ProductDescription, TextHeader,
};
pub use messages::{GeneralStatusMessage, TextMessage};
pub use packets::{
    ContourPacket, DigitalPrecipPacket, GenericPacket, Packet, RadialPacket, RasterPacket,
    SymbolPacket, TextPacket, VectorPacket,
};
pub use params::{ParameterValue, ProductParameter};
pub use products::{ProductInfo, ProductKind, product_info, products};
pub use rcm::RadarCodedMessage;
pub use volume::{DataArray, read_level3_volume};

use std::borrow::Cow;

use header::{HEADER_BYTES, MESSAGE_HEADER_BYTES, Unwrapped};

/// A decoded Level III product.
#[derive(Debug, Clone, PartialEq)]
pub struct Level3Product {
    /// NOAAPort/WMO/AWIPS transmission header, when the file has one.
    pub text_header: Option<TextHeader>,
    /// Message Header Block.
    pub message_header: MessageHeader,
    /// Product Description Block.
    pub description: ProductDescription,
    /// Product Symbology Block, when present.
    pub symbology: Option<Symbology>,
    /// Graphic Alphanumeric Block (or product 62 cell trend data), when present.
    pub graphic: Option<GraphicAlphanumeric>,
    /// Tabular Alphanumeric Block, stand-alone tabular pages, or radar coded
    /// message text, when present.
    pub tabular: Option<TabularAlphanumeric>,
}

/// Any decoded Level III file.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Level3Message {
    /// A product: a message with a Product Description Block.
    Product(Box<Level3Product>),
    /// A General Status Message (message code 2).
    GeneralStatus(Box<GeneralStatusMessage>),
    /// A plain-text message (WMO heading `NOUS..`).
    Text(Box<TextMessage>),
}

/// Decodes one Level III product file.
///
/// # Errors
///
/// - [`Level3Error::TextOnly`] for plain-text messages (WMO heading `NOUS..`).
/// - [`Level3Error::NotAProduct`] for messages without a Product Description
///   Block, such as the General Status Message (code 2).
/// - [`Level3Error::Truncated`], [`Level3Error::BadBlockHeader`],
///   [`Level3Error::PacketOverrun`] and decompression errors for malformed input.
/// - [`Level3Error::ProductTooLarge`] when the decoded packets, data levels
///   and text would take more than [`MAX_PRODUCT_DECODED_BYTES`].
///
/// [`decode_message`] decodes plain-text and General Status Messages instead of
/// returning the first two errors.
///
/// # Limits
///
/// Decompressed data (bzip2, and the zlib frames of NOAAPort files) is
/// capped at 16 MiB. What the decoder allocates for the product decoded from
/// it is charged, before each allocation, to one budget of
/// [`MAX_PRODUCT_DECODED_BYTES`] for the whole product; each packet also has
/// its own limits ([`packets::radial::MAX_RADIAL_CELLS`],
/// [`packets::raster::MAX_GRID_DIMENSION`],
/// [`packets::generic::MAX_EVENT_DEPTH`]). What the product's readers
/// allocate afterwards has limits of its own: the volume
/// ([`volume::MAX_VOLUME_BYTES`]) and the parse of a radar coded message
/// ([`rcm::MAX_RCM_PARSED_BYTES`]).
pub fn decode_product(bytes: &[u8]) -> Result<Level3Product, Level3Error> {
    match header::unwrap_framing(bytes)? {
        Unwrapped::Text { text_header, .. } => Err(Level3Error::TextOnly {
            heading: text_header.wmo_heading,
        }),
        Unwrapped::Binary(framed) => {
            let message_header = MessageHeader::parse(&framed.message)?;
            decode_framed_product(framed, message_header)
        }
    }
}

/// Decodes one Level III file: a product, a General Status Message or a
/// plain-text message.
///
/// A plain-text message is recognized by its WMO heading `NOUS..` (the rule
/// MetPy uses), or by a body after any other heading that is plain text and
/// not a binary message (the Radar Observation bulletins, AWIPS `ROBxxx`,
/// that the NCEI archive holds beside the products). Its text is the rest of
/// the file after the heading and AWIPS identifier lines; for `NOUS`
/// messages the last four bytes are dropped when their first three are
/// `\r\r\n` (NOAAPort trailer) or `FF FF 0A`, as MetPy does, and other text
/// loses a NOAAPort `\r\r\n\x03` trailer.
///
/// # Errors
///
/// - [`Level3Error::NotAProduct`] for messages other than the General Status
///   Message that have no Product Description Block.
/// - [`Level3Error::InvalidMessage`] for a General Status Message block shorter
///   than the ICD layout.
/// - The errors of [`decode_product`] for malformed input.
pub fn decode_message(bytes: &[u8]) -> Result<Level3Message, Level3Error> {
    match header::unwrap_framing(bytes)? {
        Unwrapped::Text { text_header, text } => Ok(Level3Message::Text(Box::new(
            TextMessage::new(text_header, text),
        ))),
        Unwrapped::Binary(framed) => {
            let message_header = MessageHeader::parse(&framed.message)?;
            if message_header.code == GENERAL_STATUS_MESSAGE_CODE {
                GeneralStatusMessage::parse(framed.text_header, message_header, &framed.message)
                    .map(|gsm| Level3Message::GeneralStatus(Box::new(gsm)))
            } else {
                decode_framed_product(framed, message_header)
                    .map(|product| Level3Message::Product(Box::new(product)))
            }
        }
    }
}

/// True when `bytes` look like a Level III file: a NOAAPort start-of-header
/// line and sequence number (optional), then a WMO abbreviated heading and
/// AWIPS identifier followed by a zlib frame, a binary message or plain
/// text (after a `NOUS` heading, or an `SDUS` heading as the Radar
/// Observation bulletins have; other WMO text bulletins are not Level
/// III); or a bare binary message. A binary message is recognised by its
/// Message Header Block and the block divider (-1) at halfword 10, with the
/// message code repeated as the product code at halfword 16, or message
/// code 2 (the General Status Message) with two blocks and a time of day
/// in halfwords 3-4.
///
/// A cheap check on the first bytes, for format routers: it does not
/// validate the rest of the file.
pub fn looks_like_level3(bytes: &[u8]) -> bool {
    header::looks_like_level3(bytes)
}

/// Message code of the General Status Message (ICD 2620001 Table II).
const GENERAL_STATUS_MESSAGE_CODE: i16 = 2;

/// Decodes a product from a message with its framing removed.
fn decode_framed_product(
    framed: header::Framed<'_>,
    message_header: MessageHeader,
) -> Result<Level3Product, Level3Error> {
    let header::Framed {
        text_header,
        message,
    } = framed;
    let has_description = message_header.code != GENERAL_STATUS_MESSAGE_CODE
        && read::be_i16(
            &message,
            MESSAGE_HEADER_BYTES,
            "product description block divider",
        )? == -1;
    if !has_description {
        return Err(Level3Error::NotAProduct {
            code: message_header.code,
        });
    }
    let mut description = ProductDescription::parse(&message)?;

    let data = match message.get(HEADER_BYTES..) {
        Some(rest) if decompress::is_bzip2(rest) => {
            let mut whole = Vec::with_capacity(HEADER_BYTES);
            whole.extend_from_slice(&message[..HEADER_BYTES]);
            decompress::bunzip2_into(rest, &mut whole)?;
            description.compressed = true;
            Cow::Owned(whole)
        }
        _ => message,
    };

    let blocks = blocks::read_blocks(&data, &description, &mut budget::Budget::product())?;
    Ok(Level3Product {
        text_header,
        message_header,
        description,
        symbology: blocks.symbology,
        graphic: blocks.graphic,
        tabular: blocks.tabular,
    })
}
