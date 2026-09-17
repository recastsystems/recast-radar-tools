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
//! [`messages`] holds the General Status Message and text message types.
//!
//! The format reference with ICD section numbers is `docs/level3/reference.md`.

mod blocks;
mod decompress;
mod error;
mod header;
pub mod levels;
pub mod messages;
pub mod packets;
mod products;
mod read;

pub use blocks::{
    GraphicAlphanumeric, GraphicLayout, GraphicPage, Symbology, TabularAlphanumeric, TabularLayout,
    TextPage,
};
pub use error::Level3Error;
pub use header::{
    HEADER_HALFWORDS, MessageHeader, OperationalMode, ProductDescription, TextHeader,
};
pub use messages::{GeneralStatusMessage, TextMessage};
pub use packets::{
    ContourPacket, DigitalPrecipPacket, GenericPacket, Packet, RadialPacket, RasterPacket,
    SymbolPacket, TextPacket, VectorPacket,
};
pub use products::{ProductInfo, ProductKind, product_info, products};

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
    Text(TextMessage),
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
///
/// [`decode_message`] decodes plain-text and General Status Messages instead of
/// returning the first two errors.
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
/// MetPy uses). Its text is the rest of the file after the heading and AWIPS
/// identifier lines; the last four bytes are dropped when their first three are
/// `\r\r\n` (NOAAPort trailer) or `FF FF 0A`, as MetPy does.
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
        Unwrapped::Text { text_header, text } => {
            Ok(Level3Message::Text(TextMessage::new(text_header, text)))
        }
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
            whole.extend(decompress::bunzip2(rest)?);
            description.compressed = true;
            Cow::Owned(whole)
        }
        _ => message,
    };

    let blocks = blocks::read_blocks(&data, &description)?;
    Ok(Level3Product {
        text_header,
        message_header,
        description,
        symbology: blocks.symbology,
        graphic: blocks.graphic,
        tabular: blocks.tabular,
    })
}
