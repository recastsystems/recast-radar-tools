//! Block walkers: Product Symbology Block (ICD 2620001 section 3.3.1.2), Graphic
//! Alphanumeric Block (3.3.1.3), Tabular Alphanumeric Block (3.3.1.4) and the
//! stand-alone tabular layouts of section 3.3.2 (`docs/level3/reference.md`
//! section 4).
//!
//! The walkers locate blocks, layers and pages and hand each layer or page to
//! the packet dispatcher. Tabular pages are kept as raw bytes here.

use crate::Level3Error;
use crate::header::{HEADER_BYTES, MESSAGE_HEADER_BYTES, MessageHeader, ProductDescription};
use crate::packets::{self, Packet};
use crate::read::{be_i16, be_u16, be_u32, expect_i16, slice};

/// Product Symbology Block: display packets grouped in layers.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbology {
    /// Layers in file order; each holds its top-level display packets in file order.
    pub layers: Vec<Vec<Packet>>,
}

/// Graphic Alphanumeric Block: pages of display packets in screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicAlphanumeric {
    /// Where the packets came from.
    pub layout: GraphicLayout,
    /// Pages in file order.
    pub pages: Vec<GraphicPage>,
}

/// Source layout of a [`GraphicAlphanumeric`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicLayout {
    /// Graphic Alphanumeric Block (block ID 2) with numbered pages.
    Pages,
    /// Product 62 (Storm Structure) cell trend data: packets 22 and 21 from the
    /// "offset to graphic" to the end of the message, with no page structure.
    /// The offset is one halfword too large (observed; MetPy does the same):
    /// the first packet code is at byte `2 * (offset - 1)`. Held as one page
    /// numbered 0.
    CellTrend,
}

/// One graphic alphanumeric page.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicPage {
    /// Page number from the file (0 for [`GraphicLayout::CellTrend`]).
    pub number: u16,
    /// Display packets on the page in file order.
    pub packets: Vec<Packet>,
}

/// Tabular alphanumeric data: a Tabular Alphanumeric Block, a stand-alone page
/// block, or radar coded message text.
#[derive(Debug, Clone, PartialEq)]
pub struct TabularAlphanumeric {
    /// Where the data came from.
    pub layout: TabularLayout,
    /// Second Message Header Block ([`TabularLayout::Block`] only). Its message
    /// code is the alphanumeric product code (e.g. 101 for product 58).
    pub message_header: Option<MessageHeader>,
    /// Second Product Description Block ([`TabularLayout::Block`] only, when its
    /// divider is present).
    pub description: Option<ProductDescription>,
    /// Raw data. [`TabularLayout::Block`] and [`TabularLayout::StandAlone`]:
    /// from the page block divider (-1) and page count to the end of the block
    /// (ICD Figure 3-16: per page, lines of `INT*2 count` + characters, ending
    /// with -1). [`TabularLayout::RadarCodedMessage`]: the ASCII text starting
    /// `1234 ROBUU`.
    pub data: Vec<u8>,
}

/// Source layout of a [`TabularAlphanumeric`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabularLayout {
    /// Tabular Alphanumeric Block (block ID 3) at the tabular offset, with a
    /// second message header and product description block.
    Block,
    /// Stand-alone tabular product (62, 73, 75, 77, 82, or an alphanumeric
    /// message 100-111 on its own): the page block is at the symbology offset.
    StandAlone,
    /// Product 74 radar coded message: ASCII text at the symbology offset.
    RadarCodedMessage,
}

/// Blocks found in a message.
pub(crate) struct Blocks {
    pub(crate) symbology: Option<Symbology>,
    pub(crate) graphic: Option<GraphicAlphanumeric>,
    pub(crate) tabular: Option<TabularAlphanumeric>,
}

/// Products whose symbology offset may point at a stand-alone page block
/// (section 3.3.2), plus alphanumeric message codes distributed on their own.
fn is_standalone_candidate(product_code: i16) -> bool {
    matches!(product_code, 62 | 73 | 75 | 77 | 82 | 100..=111)
}

/// Walks the blocks of `data` (the whole message, decompressed) named by `description`.
pub(crate) fn read_blocks(
    data: &[u8],
    description: &ProductDescription,
) -> Result<Blocks, Level3Error> {
    let sym = byte_offset(description.symbology_offset);
    let gra = byte_offset(description.graphic_offset);
    let tab = byte_offset(description.tabular_offset);
    let product_code = description.product_code;
    let mut blocks = Blocks {
        symbology: None,
        graphic: None,
        tabular: None,
    };

    if let Some(o) = sym
        && product_code == 74
        && let Some(text) = data.get(o..)
        && text.starts_with(b"1234 ROBUU")
    {
        blocks.tabular = Some(TabularAlphanumeric {
            layout: TabularLayout::RadarCodedMessage,
            message_header: None,
            description: None,
            data: text.to_vec(),
        });
    } else if let Some(o) = sym
        && is_standalone_candidate(product_code)
        && !looks_like_symbology(data, o)
    {
        expect_i16(data, o, -1, "stand-alone tabular divider")?;
        // Product 62's graphic offset is one halfword past the first cell trend packet code.
        let trend = gra
            .and_then(|g| g.checked_sub(2))
            .filter(|&start| matches!(be_i16(data, start, "cell trend packet code"), Ok(21 | 22)));
        let end = trend.filter(|&start| start > o).unwrap_or(data.len());
        blocks.tabular = Some(TabularAlphanumeric {
            layout: TabularLayout::StandAlone,
            message_header: None,
            description: None,
            data: data[o..end].to_vec(),
        });
        if let Some(start) = trend {
            blocks.graphic = Some(GraphicAlphanumeric {
                layout: GraphicLayout::CellTrend,
                pages: vec![GraphicPage {
                    number: 0,
                    packets: packets::decode_packets(&data[start..], start)?,
                }],
            });
        }
    } else {
        if let Some(o) = sym {
            blocks.symbology = Some(read_symbology(data, o)?);
        }
        if let Some(o) = gra {
            blocks.graphic = Some(read_graphic(data, o)?);
        }
        if let Some(o) = tab {
            blocks.tabular = Some(read_tabular(data, o)?);
        }
    }
    Ok(blocks)
}

/// Converts a halfword offset from the Product Description Block to a byte
/// offset; `None` for offset 0 (block absent). Readers bounds-check it.
fn byte_offset(halfwords: u32) -> Option<usize> {
    (halfwords != 0).then(|| usize::try_from(u64::from(halfwords) * 2).unwrap_or(usize::MAX))
}

/// True when a symbology block header starts at `o`: divider -1, block ID 1, a
/// length that fits, 1-18 layers and a layer divider. Distinguishes the 1995
/// version 0 product 82 (symbology block) from stand-alone page blocks.
fn looks_like_symbology(data: &[u8], o: usize) -> bool {
    symbology_header_plausible(data, o).unwrap_or(false)
}

fn symbology_header_plausible(data: &[u8], o: usize) -> Result<bool, Level3Error> {
    let what = "symbology block header";
    let room = data.len().saturating_sub(o);
    Ok(room >= 16
        && be_i16(data, o, what)? == -1
        && be_i16(data, o + 2, what)? == 1
        && (10..=room).contains(&to_usize(be_u32(data, o + 4, what)?))
        && (1..=18).contains(&be_u16(data, o + 8, what)?)
        && be_i16(data, o + 10, what)? == -1)
}

/// Product Symbology Block (Figure 3-6 sheets 3 and 8).
fn read_symbology(data: &[u8], o: usize) -> Result<Symbology, Level3Error> {
    expect_i16(data, o, -1, "symbology block divider")?;
    expect_i16(data, o + 2, 1, "symbology block ID")?;
    let num_layers = be_u16(data, o + 8, "symbology block header")?;
    let mut layers = Vec::new();
    let mut q = o + 10;
    for _ in 0..num_layers {
        expect_i16(data, q, -1, "symbology layer divider")?;
        let length = to_usize(be_u32(data, q + 2, "symbology layer header")?);
        let start = q + 6;
        let layer = slice(data, start, length, "symbology layer")?;
        layers.push(packets::decode_packets(layer, start)?);
        q = start + length;
    }
    Ok(Symbology { layers })
}

/// Graphic Alphanumeric Block (section 3.3.1.3, Figure 3-6 sheets 4 and 9).
///
/// Each page is walked from its own page number and length; the block length is
/// not used to bound the pages. **Observed:** it can be shorter than the pages
/// it holds (KTLX product 61 of 2022-05-03 00:52Z declares 1162 bytes for a
/// 10-byte header and two 578-byte pages, 1166 bytes). In every corpus file the
/// pages hold only the storm attribute table of Table VII, drawn with text
/// packets 8 and unlinked vector packets 10 in screen pixels; symbol packets
/// occur in the symbology block and in product 62 cell trend data.
fn read_graphic(data: &[u8], o: usize) -> Result<GraphicAlphanumeric, Level3Error> {
    expect_i16(data, o, -1, "graphic block divider")?;
    expect_i16(data, o + 2, 2, "graphic block ID")?;
    let num_pages = be_u16(data, o + 8, "graphic block header")?;
    let mut pages = Vec::new();
    let mut q = o + 10;
    for _ in 0..num_pages {
        let number = be_u16(data, q, "graphic page header")?;
        let length = usize::from(be_u16(data, q + 2, "graphic page header")?);
        let start = q + 4;
        let page = slice(data, start, length, "graphic page")?;
        pages.push(GraphicPage {
            number,
            packets: packets::decode_packets(page, start)?,
        });
        q = start + length;
    }
    Ok(GraphicAlphanumeric {
        layout: GraphicLayout::Pages,
        pages,
    })
}

/// Tabular Alphanumeric Block (Figure 3-6 sheets 5 and 10).
fn read_tabular(data: &[u8], o: usize) -> Result<TabularAlphanumeric, Level3Error> {
    expect_i16(data, o, -1, "tabular block divider")?;
    expect_i16(data, o + 2, 3, "tabular block ID")?;
    let length = to_usize(be_u32(data, o + 4, "tabular block header")?);
    let headers = slice(data, o + 8, HEADER_BYTES, "tabular block headers")?;
    let message_header = MessageHeader::parse(headers)?;
    let description = if be_i16(headers, MESSAGE_HEADER_BYTES, "tabular block headers")? == -1 {
        Some(ProductDescription::parse(headers)?)
    } else {
        None
    };
    let start = o + 8 + HEADER_BYTES;
    let end = o.saturating_add(length).clamp(start, data.len());
    Ok(TabularAlphanumeric {
        layout: TabularLayout::Block,
        message_header: Some(message_header),
        description,
        data: data[start..end].to_vec(),
    })
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
