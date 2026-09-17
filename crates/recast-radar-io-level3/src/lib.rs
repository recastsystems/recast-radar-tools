//! Pure-Rust decoder for NEXRAD and TDWR Level III products.
//!
//! [`decode_product`] takes the bytes of one Level III file and returns a
//! [`Level3Product`]:
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
//! [`product_info`] looks up a product code's mnemonic, name and kind.
//!
//! The format reference with ICD section numbers is `docs/level3/reference.md`.

mod blocks;
mod decompress;
mod error;
mod header;
pub mod packets;
mod products;
mod read;

pub use blocks::TextPage;
pub use blocks::{
    GraphicAlphanumeric, GraphicLayout, GraphicPage, Symbology, TabularAlphanumeric, TabularLayout,
};
pub use error::Level3Error;
pub use header::{HEADER_HALFWORDS, MessageHeader, ProductDescription, TextHeader};
pub use packets::{
    ContourPacket, DigitalPrecipPacket, GenericPacket, Packet, RadialPacket, RasterPacket,
    SymbolPacket, TextPacket, VectorPacket,
};
pub use products::{ProductInfo, ProductKind, product_info, products};

use std::borrow::Cow;

use header::{HEADER_BYTES, MESSAGE_HEADER_BYTES};

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

/// Decodes one Level III product file.
///
/// # Errors
///
/// - [`Level3Error::TextOnly`] for plain-text messages (WMO heading `NOUS..`).
/// - [`Level3Error::NotAProduct`] for messages without a Product Description
///   Block, such as the General Status Message (code 2).
/// - [`Level3Error::Truncated`], [`Level3Error::BadBlockHeader`],
///   [`Level3Error::PacketOverrun`] and decompression errors for malformed input.
pub fn decode_product(bytes: &[u8]) -> Result<Level3Product, Level3Error> {
    let header::Framed {
        text_header,
        message,
    } = header::unwrap_framing(bytes)?;
    let message_header = MessageHeader::parse(&message)?;
    let has_description = message_header.code != 2
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
