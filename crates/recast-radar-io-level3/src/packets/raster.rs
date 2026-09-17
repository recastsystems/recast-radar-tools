//! Raster image packets: Raster Data Packet (0xBA07, 0xBA0F, ICD 2620001 Figure 3-11),
//! Digital Precipitation Data Array (17, Figure 3-11a), Precipitation Rate Data
//! Array (18, Figure 3-11b) and Digital Raster Data Array (33, Figure 3-11d).
//!
//! Each packet decodes into its header fields and a [`RasterGrid`] of raw data
//! levels, rows in file order. What a level means depends on the product
//! (Product Description Block halfwords 31-53; `docs/level3/reference.md`
//! section 5): [`RasterGrid::values`] and [`RasterGrid::level_at`] map levels
//! with the mapping [`DataLevels::for_packet`] returns for the packet. Packet
//! 18 levels have no mapping (no halfword describes them).
//!
//! Rows are encoded per packet code:
//!
//! | Code | Row bytes after the `INT*2` byte count | Row width |
//! |---|---|---|
//! | 0xBA07, 0xBA0F | `run << 4 \| level` (4-bit run, 4-bit level) | sum of runs; all rows must be equal |
//! | 18 | `run << 4 \| level` | number of LFM boxes in row (header) |
//! | 17 | 8-bit run, 8-bit level pairs | number of LFM boxes in row (header) |
//! | 33 | one level byte per cell, then at most one pad byte | number of cells (header) |
//!
//! A run of 0 adds no cells, so the zero byte that pads a row to a halfword is
//! skipped. Rows that do not fill the width, a packing descriptor other than 2
//! and grids larger than [`MAX_GRID_DIMENSION`] in either direction are
//! [`Level3Error::InvalidPacket`].
//!
//! 0xBA0F and 33 have no real sample in the corpus (`docs/level3/reference.md`
//! section 7); 0xBA0F shares the 0xBA07 code path.

use super::Packet;
use crate::Level3Error;
use crate::levels::{DataLevels, Level};

/// Largest number of rows, and of cells per row, accepted in a raster packet.
///
/// The ICD maxima are 464 rows (0xBA07, 0xBA0F, 33), 1840 cells per row (33),
/// 131 x 131 boxes (17) and 13 x 13 boxes (18). The limit bounds the memory a
/// corrupt row count or run-length stream can make the decoder allocate.
pub const MAX_GRID_DIMENSION: usize = 4096;

/// A rectangular grid of raw data levels, stored row-major.
///
/// Row 0 is the first row in the packet and column 0 the first cell of each
/// row. Placing cells on the ground uses the packet header (start coordinates
/// and scale) and the product; the grid itself carries no geometry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RasterGrid {
    rows: usize,
    columns: usize,
    levels: Vec<u8>,
}

impl RasterGrid {
    /// Number of rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Number of cells in each row.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// All levels, row-major: the level at row `r`, column `c` is
    /// `levels()[r * columns() + c]`.
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// Consumes the grid and returns its row-major levels.
    pub fn into_levels(self) -> Vec<u8> {
        self.levels
    }

    /// Levels of row `row`, or `None` past the last row.
    pub fn row(&self, row: usize) -> Option<&[u8]> {
        if row >= self.rows {
            return None;
        }
        let start = row * self.columns;
        self.levels.get(start..start + self.columns)
    }

    /// Level at `row`, `column`, or `None` outside the grid.
    pub fn get(&self, row: usize, column: usize) -> Option<u8> {
        if column >= self.columns {
            return None;
        }
        self.row(row)?.get(column).copied()
    }

    /// The rows in order.
    pub fn iter_rows(&self) -> impl ExactSizeIterator<Item = &[u8]> + '_ {
        (0..self.rows).map(move |row| {
            let start = row * self.columns;
            self.levels
                .get(start..start + self.columns)
                .unwrap_or_default()
        })
    }

    /// Physical values of [`levels`](Self::levels) in the same row-major
    /// layout, NaN where a level has no physical value; see
    /// [`DataLevels::values`]. `levels` is the mapping from
    /// [`DataLevels::for_packet`] for the packet holding this grid.
    pub fn values(&self, levels: &DataLevels) -> Vec<f32> {
        levels.values(&self.levels)
    }

    /// What the level at `row`, `column` means, or `None` outside the grid.
    pub fn level_at(&self, row: usize, column: usize, levels: &DataLevels) -> Option<Level> {
        self.get(row, column)
            .map(|level| levels.level(u16::from(level)))
    }
}

/// Raster data packet (0xBA07, 0xBA0F), precipitation rate data array (18) or
/// digital raster data array (33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterPacket {
    /// Packet code: 0xBA07, 0xBA0F, 18 or 33.
    pub code: u16,
    /// Header fields other than the grid dimensions, by packet layout.
    pub header: RasterHeader,
    /// Data levels. Its row count is the header's number of rows; its column
    /// count is the header's boxes (18) or cells (33) per row.
    pub grid: RasterGrid,
}

impl RasterPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Header fields of a [`RasterPacket`], by packet layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterHeader {
    /// Raster Data Packet, 0xBA07 or 0xBA0F (Figure 3-11).
    RasterData {
        /// Halfwords 2-3 ("packet code" fields): 0x8000 and 0x00C0.
        op_flags: [u16; 2],
        /// I coordinate of the start of the data, 1/4 km.
        i_start: i16,
        /// J coordinate of the start of the data, 1/4 km.
        j_start: i16,
        /// X scale (integer part): grid cell width factor, 1-67.
        x_scale: i16,
        /// X scale fractional part (reserved for internal PUP use).
        x_scale_fraction: i16,
        /// Y scale (integer part): grid cell height factor, 1-67.
        y_scale: i16,
        /// Y scale fractional part (reserved for internal PUP use).
        y_scale_fraction: i16,
        /// Packing descriptor; always 2 (4-bit run, 4-bit level).
        packing: u16,
    },
    /// Precipitation Rate Data Array, packet 18 (Figure 3-11b).
    PrecipitationRate {
        /// The two spare halfwords after the packet code.
        spares: [u16; 2],
    },
    /// Digital Raster Data Array, packet 33 (Figure 3-11d).
    DigitalRaster {
        /// I coordinate of the upper left corner, pixels.
        i_start: i16,
        /// J coordinate of the upper left corner, pixels.
        j_start: i16,
        /// I (vertical) scale factor, 1-10.
        i_scale: i16,
        /// J (horizontal) scale factor, 1-10.
        j_scale: i16,
    },
}

/// Digital precipitation data array (17): the hourly digital precipitation
/// array of product 81, 131 x 131 boxes of the 1/40 LFM grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigitalPrecipPacket {
    /// Packet code: 17.
    pub code: u16,
    /// The two spare halfwords after the packet code.
    pub spares: [u16; 2],
    /// Data levels (8-bit, product 81 encoding). Its dimensions are the
    /// header's number of rows and LFM boxes per row.
    pub grid: RasterGrid,
}

impl DigitalPrecipPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }
}

/// Decodes one raster packet (0xBA07, 0xBA0F, 17, 18, 33). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    match code {
        0xBA07 | 0xBA0F => {
            let packing = halfword(code, bytes, 20)?;
            if packing != 2 {
                return Err(invalid(
                    code,
                    format!("packing descriptor {packing}; only 2 is defined"),
                ));
            }
            let header = RasterHeader::RasterData {
                op_flags: [halfword(code, bytes, 2)?, halfword(code, bytes, 4)?],
                i_start: signed(code, bytes, 6)?,
                j_start: signed(code, bytes, 8)?,
                x_scale: signed(code, bytes, 10)?,
                x_scale_fraction: signed(code, bytes, 12)?,
                y_scale: signed(code, bytes, 14)?,
                y_scale_fraction: signed(code, bytes, 16)?,
                packing,
            };
            let rows = halfword(code, bytes, 18)?;
            let grid = read_rows(code, bytes, 22, rows, None, RowCoding::Nibbles)?;
            Ok(Packet::Raster(RasterPacket { code, header, grid }))
        }
        17 | 18 => {
            let spares = [halfword(code, bytes, 2)?, halfword(code, bytes, 4)?];
            let boxes = usize::from(halfword(code, bytes, 6)?);
            let rows = halfword(code, bytes, 8)?;
            if code == 17 {
                let grid = read_rows(code, bytes, 10, rows, Some(boxes), RowCoding::BytePairs)?;
                Ok(Packet::DigitalPrecip(DigitalPrecipPacket {
                    code,
                    spares,
                    grid,
                }))
            } else {
                let grid = read_rows(code, bytes, 10, rows, Some(boxes), RowCoding::Nibbles)?;
                let header = RasterHeader::PrecipitationRate { spares };
                Ok(Packet::Raster(RasterPacket { code, header, grid }))
            }
        }
        33 => {
            let header = RasterHeader::DigitalRaster {
                i_start: signed(code, bytes, 2)?,
                j_start: signed(code, bytes, 4)?,
                i_scale: signed(code, bytes, 6)?,
                j_scale: signed(code, bytes, 8)?,
            };
            let cells = usize::from(halfword(code, bytes, 10)?);
            let rows = halfword(code, bytes, 12)?;
            let grid = read_rows(code, bytes, 14, rows, Some(cells), RowCoding::Levels)?;
            Ok(Packet::Raster(RasterPacket { code, header, grid }))
        }
        _ => Err(Level3Error::UnsupportedPacket(code)),
    }
}

/// How the bytes of one row encode its levels.
#[derive(Debug, Clone, Copy)]
enum RowCoding {
    /// Bytes `run << 4 | level` (0xBA07, 0xBA0F, 18).
    Nibbles,
    /// Byte pairs: 8-bit run, 8-bit level (17).
    BytePairs,
    /// One level byte per cell, then at most one pad byte (33).
    Levels,
}

impl RowCoding {
    /// Most cells one row byte can produce, for sizing the level buffer.
    fn max_cells_per_byte(self) -> usize {
        match self {
            Self::Nibbles => 15,
            Self::BytePairs => 128,
            Self::Levels => 1,
        }
    }
}

/// Reads `rows` rows starting at packet byte `start`, each an `INT*2` byte count
/// and its bytes. `width` is the row width from the header; `None` takes the
/// first row's width and requires every other row to match it.
fn read_rows(
    code: u16,
    bytes: &[u8],
    start: usize,
    rows: u16,
    width: Option<usize>,
    coding: RowCoding,
) -> Result<RasterGrid, Level3Error> {
    let rows = usize::from(rows);
    check_dimension(code, "number of rows", rows)?;
    if let Some(width) = width {
        check_dimension(code, "row width", width)?;
    }
    // Room for `rows_left` rows of `columns` cells, but never more than the
    // bytes from `offset` on can encode.
    let reserve = |levels: &mut Vec<u8>, columns: usize, rows_left: usize, offset: usize| {
        let encodable = bytes
            .len()
            .saturating_sub(offset)
            .saturating_mul(coding.max_cells_per_byte());
        levels.reserve(columns.saturating_mul(rows_left).min(encodable));
    };
    let mut levels: Vec<u8> = Vec::new();
    let mut columns = width;
    let mut offset = start;
    if let Some(columns) = columns {
        reserve(&mut levels, columns, rows, offset);
    }
    for row in 0..rows {
        let count = usize::from(halfword(code, bytes, offset)?);
        let data = bytes.get(offset + 2..offset + 2 + count).ok_or_else(|| {
            invalid(
                code,
                format!("row {row} ({count} bytes) runs past the end of the packet"),
            )
        })?;
        offset += 2 + count;

        let row_start = levels.len();
        let limit = columns.unwrap_or(MAX_GRID_DIMENSION);
        let too_wide = || match columns {
            Some(columns) => invalid(code, format!("row {row} holds more than {columns} cells")),
            None => invalid(
                code,
                format!("row {row} is wider than the {MAX_GRID_DIMENSION}-cell limit"),
            ),
        };
        match coding {
            RowCoding::Nibbles => {
                for &byte in data {
                    let run = usize::from(byte >> 4);
                    if levels.len() - row_start + run > limit {
                        return Err(too_wide());
                    }
                    levels.resize(levels.len() + run, byte & 0x0F);
                }
            }
            RowCoding::BytePairs => {
                if count % 2 != 0 {
                    return Err(invalid(
                        code,
                        format!("row {row} has an odd byte count ({count}) for run/level pairs"),
                    ));
                }
                for pair in data.chunks_exact(2) {
                    let &[run, level] = pair else { continue };
                    let run = usize::from(run);
                    if levels.len() - row_start + run > limit {
                        return Err(too_wide());
                    }
                    levels.resize(levels.len() + run, level);
                }
            }
            RowCoding::Levels => {
                if count != limit && count != limit + 1 {
                    return Err(invalid(
                        code,
                        format!("row {row} has {count} bytes for {limit} cells"),
                    ));
                }
                levels.extend_from_slice(data.get(..limit).unwrap_or_default());
            }
        }

        let filled = levels.len() - row_start;
        match columns {
            Some(columns) if filled != columns => {
                return Err(invalid(
                    code,
                    format!("row {row} holds {filled} cells, expected {columns}"),
                ));
            }
            Some(_) => {}
            None => {
                columns = Some(filled);
                reserve(&mut levels, filled, rows - row - 1, offset);
            }
        }
    }
    if offset != bytes.len() {
        return Err(invalid(
            code,
            format!(
                "{} bytes follow the last row",
                bytes.len().saturating_sub(offset)
            ),
        ));
    }
    Ok(RasterGrid {
        rows,
        columns: columns.unwrap_or(0),
        levels,
    })
}

fn check_dimension(code: u16, what: &str, value: usize) -> Result<(), Level3Error> {
    if value > MAX_GRID_DIMENSION {
        return Err(invalid(
            code,
            format!("{what} {value} exceeds the limit of {MAX_GRID_DIMENSION}"),
        ));
    }
    Ok(())
}

fn invalid(code: u16, reason: String) -> Level3Error {
    Level3Error::InvalidPacket { code, reason }
}

/// Unsigned halfword at packet byte `offset`.
fn halfword(code: u16, bytes: &[u8], offset: usize) -> Result<u16, Level3Error> {
    match bytes.get(offset..offset + 2) {
        Some(&[hi, lo]) => Ok(u16::from_be_bytes([hi, lo])),
        _ => Err(invalid(
            code,
            format!("packet ends before the halfword at packet byte {offset}"),
        )),
    }
}

/// Signed halfword at packet byte `offset`.
fn signed(code: u16, bytes: &[u8], offset: usize) -> Result<i16, Level3Error> {
    halfword(code, bytes, offset).map(|value| i16::from_be_bytes(value.to_be_bytes()))
}
