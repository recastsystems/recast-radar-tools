//! Block walkers: Product Symbology Block (ICD 2620001 section 3.3.1.2), Graphic
//! Alphanumeric Block (3.3.1.3), Tabular Alphanumeric Block (3.3.1.4) and the
//! stand-alone tabular layouts of section 3.3.2 (`docs/level3/reference.md`
//! section 4).
//!
//! The walkers locate blocks, layers and pages and hand each layer or page to
//! the packet dispatcher. Tabular data is split into text pages and lines
//! ([`TextPage`]).

use crate::Level3Error;
use crate::budget::Budget;
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
#[non_exhaustive]
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
    /// Second Message Header Block ([`TabularLayout::Block`], and
    /// [`TabularLayout::RadarCodedMessage`] in a Tabular Alphanumeric Block).
    /// Its message code is the alphanumeric product code (e.g. 101 for
    /// product 58; 74 for the radar coded message of product 83).
    pub message_header: Option<MessageHeader>,
    /// Second Product Description Block (as for
    /// [`message_header`](Self::message_header), when its divider is
    /// present).
    pub description: Option<ProductDescription>,
    /// Raw data. [`TabularLayout::Block`] and [`TabularLayout::StandAlone`]:
    /// from the page block divider (-1) and page count to the end of the block
    /// (ICD Figure 3-16: per page, lines of `INT*2 count` + characters, ending
    /// with -1). [`TabularLayout::RadarCodedMessage`]: the ASCII text starting
    /// `1234 ROBUU`.
    pub data: Vec<u8>,
    /// The text decoded from [`data`](Self::data).
    ///
    /// - [`TabularLayout::Block`] and [`TabularLayout::StandAlone`]: the pages
    ///   of the page block in file order (at most 17 lines of up to 80
    ///   characters each per the ICD). Bytes after the last end-of-page flag are
    ///   ignored.
    /// - [`TabularLayout::RadarCodedMessage`]: one page whose lines are the
    ///   message's 70-character records. Observed: every corpus message is a
    ///   whole number of space-padded 70-character records and every section
    ///   marker (`/NEXRAA`, `/ENDAA`, ...) starts a record; ICD 2620001P
    ///   Appendix B does not state the record length.
    pub pages: Vec<TextPage>,
}

/// One page of tabular alphanumeric text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextPage {
    /// Lines in file order, one `char` per byte. The ICD defines the characters
    /// as ASCII when the most significant bit is 0 and as special symbols
    /// (numbered by the low 7 bits) when it is 1 (Figure 3-6 sheet 10); bytes
    /// 0x80-0xFF therefore appear as U+0080-U+00FF, and `c as u8` gives back
    /// every byte. Observed: products 78, 79 and 80 pad some lines with NUL
    /// characters, which are kept.
    pub lines: Vec<String>,
}

/// Source layout of a [`TabularAlphanumeric`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TabularLayout {
    /// Tabular Alphanumeric Block (block ID 3) at the tabular offset, with a
    /// second message header and product description block.
    Block,
    /// Stand-alone tabular product (62, 73, 75, 77, 82, or an alphanumeric
    /// message 100-111 on its own): the page block is at the symbology offset.
    StandAlone,
    /// Radar coded message text: at the symbology offset of product 74, or
    /// (observed) after the second headers of the Tabular Alphanumeric Block
    /// of the unedited Radar Coded Message (product 83,
    /// [`crate::packets::irm`]), where it is the product 74 of the same
    /// volume.
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
///
/// **Observed:** products of 1993-1994 in the NCEI archive (KLOT, KIND,
/// KCYS: products 48, 58, 60, 78, 79, 80 and 83) name a Tabular
/// Alphanumeric Block at the offset where the message ends, and their
/// message length (halfwords 5-6) is that end: the block was not sent. Such
/// an offset reads as no block; [`ProductDescription::tabular_offset`] keeps
/// the value.
pub(crate) fn read_blocks(
    data: &[u8],
    description: &ProductDescription,
    budget: &mut Budget,
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
            data: budget.bytes(text, "radar coded message")?,
            pages: vec![radar_coded_message_page(text, budget)?],
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
            data: budget.bytes(&data[o..end], "tabular data")?,
            pages: read_pages(&data[..end], o, budget)?,
        });
        if let Some(start) = trend {
            blocks.graphic = Some(GraphicAlphanumeric {
                layout: GraphicLayout::CellTrend,
                pages: vec![GraphicPage {
                    number: 0,
                    packets: packets::decode_packets(&data[start..], start, budget)?,
                }],
            });
        }
    } else {
        if let Some(o) = sym {
            blocks.symbology = Some(read_symbology(data, o, budget)?);
        }
        if let Some(o) = gra {
            blocks.graphic = Some(read_graphic(data, o, budget)?);
        }
        if let Some(o) = tab
            && o != data.len()
        {
            blocks.tabular = Some(read_tabular(data, o, budget)?);
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
fn read_symbology(data: &[u8], o: usize, budget: &mut Budget) -> Result<Symbology, Level3Error> {
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
        let packets = packets::decode_packets(layer, start, budget)?;
        budget.push(&mut layers, packets, "symbology layers")?;
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
fn read_graphic(
    data: &[u8],
    o: usize,
    budget: &mut Budget,
) -> Result<GraphicAlphanumeric, Level3Error> {
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
        let packets = packets::decode_packets(page, start, budget)?;
        budget.push(&mut pages, GraphicPage { number, packets }, "graphic pages")?;
        q = start + length;
    }
    Ok(GraphicAlphanumeric {
        layout: GraphicLayout::Pages,
        pages,
    })
}

/// Tabular Alphanumeric Block (Figure 3-6 sheets 5 and 10).
fn read_tabular(
    data: &[u8],
    o: usize,
    budget: &mut Budget,
) -> Result<TabularAlphanumeric, Level3Error> {
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
    if message_header.code == 74
        && let Some(text) = data.get(start..end)
        && text.starts_with(b"1234 ROBUU")
    {
        return Ok(TabularAlphanumeric {
            layout: TabularLayout::RadarCodedMessage,
            message_header: Some(message_header),
            description,
            data: budget.bytes(text, "radar coded message")?,
            pages: vec![radar_coded_message_page(text, budget)?],
        });
    }
    Ok(TabularAlphanumeric {
        layout: TabularLayout::Block,
        message_header: Some(message_header),
        description,
        data: budget.bytes(&data[start..end], "tabular data")?,
        pages: read_pages(&data[..end], start, budget)?,
    })
}

/// Record length of a radar coded message (observed; see [`TabularAlphanumeric::pages`]).
const RADAR_CODED_MESSAGE_RECORD: usize = 70;

/// Pages of the page block starting at byte `start` of `data`, which ends where
/// the block ends (ICD Figure 3-16 and Figure 3-6 sheet 10): divider -1, number
/// of pages, then for each page lines of `INT*2` character count and
/// characters, closed by the end of page flag -1.
fn read_pages(
    data: &[u8],
    start: usize,
    budget: &mut Budget,
) -> Result<Vec<TextPage>, Level3Error> {
    expect_i16(data, start, -1, "tabular page block divider")?;
    // INT*2 1-48; read unsigned so a corrupt negative count runs out of data
    // instead of being mistaken for zero pages.
    let num_pages = be_u16(data, start + 2, "tabular page count")?;
    let mut pages = Vec::new();
    let mut q = start + 4;
    for _ in 0..num_pages {
        let mut lines = Vec::new();
        loop {
            let count = be_i16(data, q, "tabular line character count")?;
            let Ok(len) = usize::try_from(count) else {
                // Negative: must be the end of page flag.
                expect_i16(data, q, -1, "tabular end of page flag")?;
                q += 2;
                break;
            };
            let line = budget.latin1(slice(data, q + 2, len, "tabular line")?, "tabular line")?;
            budget.push(&mut lines, line, "tabular lines")?;
            q += 2 + len;
        }
        budget.push(&mut pages, TextPage { lines }, "tabular pages")?;
    }
    Ok(pages)
}

/// Radar coded message text as one page of 70-character records.
fn radar_coded_message_page(text: &[u8], budget: &mut Budget) -> Result<TextPage, Level3Error> {
    let records = text.chunks(RADAR_CODED_MESSAGE_RECORD);
    let mut lines = budget.vec(records.len(), "radar coded message records")?;
    for record in records {
        lines.push(budget.latin1(record, "radar coded message record")?);
    }
    Ok(TextPage { lines })
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
