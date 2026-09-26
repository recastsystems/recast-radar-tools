//! Packets 30 and 31 of the unedited Radar Coded Message (message and
//! product code 83, AWIPS `IRMxxx`), the pre-edit message the RPG sends to
//! its operator for editing. No ICD revision on the ROC site defines it
//! (their Table III lists code 83 as spare); the 1990s Table III and Figure
//! 3-22 sheets 3-7 that NCDC reproduces in its Level III documentation
//! (DSI-7000, 2005) do: "Radar Coded Message (Unedited)", which NCDC calls
//! the Interim Radar Message. The NCEI archive holds one for every volume
//! scan of the 1990s (`docs/level3/reference.md` section 4.4).
//!
//! Layout (DSI-7000 Figure 3-22, and observed at KILX 1996-04-19, KLZK
//! 1997-03-01, KTLX 1999-05-03, KFWS 2000-03-28 and ten sites of
//! 1994-1995): three symbology layers and a Tabular Alphanumeric Block.
//! Some 1994 products (KLOT, KIND, KCYS) name the tabular block at the end
//! of their message and carry none.
//!
//! - Layer 0: one packet 30, the code and five IEEE single-precision values,
//!   the "LFM grid adaptation parameters" ([`IrmPacket::Parameters`]).
//! - Layer 1: one packet 31, the code and the number of storm centroids
//!   ([`IrmPacket::StormCount`]), followed by that many pairs of a storm ID
//!   packet (15) and a special symbol packet (2, symbol `0x2220`) at the same
//!   position (km/4).
//! - Layer 2: one packet 32, the code, a row count (100) and per row an
//!   `INT*2` byte count and `run << 4 | level` bytes
//!   ([`crate::packets::raster::RasterHeader::IntensityGrid`]): the 100 x 100
//!   composite reflectivity of the radar coded message on the 1/16 LFM
//!   grid, equal level for level to the grid of the product 74 of the same
//!   volume.
//! - The Tabular Alphanumeric Block holds that radar coded message: a second
//!   Message Header Block and Product Description Block of product 74, then
//!   its text ([`crate::TabularLayout::RadarCodedMessage`]).

use super::Packet;
use crate::Level3Error;

/// Byte length of packet 30: the code and five 4-byte values.
pub(crate) const PARAMETERS_BYTES: usize = 22;
/// Byte length of packet 31: the code and the count.
pub(crate) const STORM_COUNT_BYTES: usize = 4;

/// A packet of the unedited Radar Coded Message (30 or 31).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum IrmPacket {
    /// Packet 30: the LFM grid adaptation parameters, five IEEE
    /// single-precision values, constant for a site (KILX 1996-04-19:
    /// -36.025, 114.0, 117.5, 21.0, 0.0). DSI-7000 Figure 3-22 sheets 4 and
    /// 7 name them ([`IrmPacket::PARAMETER_NAMES`]): the angle of rotation
    /// from north to the LFM grid column axis (degrees, -180 to 180), the X
    /// and Y distances from the radar to the upper right corner of the
    /// unrotated `MM` box (the grid box holding the antenna; km, 0 to 45),
    /// the 1/16 LFM grid box size (km, 8.75 to 11.25) and a spare.
    ///
    /// The stored values do not follow those units. At the thirteen sites
    /// of the NCEI archive examined (1994-2000, `docs/level3/reference.md`
    /// section 4.4; the corpus IRMs in `tests/rcm.rs`): the box size is
    /// twice the 1/16 LFM box at the site's latitude in km, `2 * 11.90625 *
    /// (1 + sin(lat)) / (1 + sin 60 deg)`, within 0.05 (KTLX 1994 stores
    /// 20.1412, equal to four decimals); the X distance is four times the
    /// distance in km from the radar east to the edge of its 1/4 LFM box on
    /// the national grid ([`crate::hrap`]) within 0.1 km at nine sites (0.35
    /// to 3.8 km off at KLOT, KMLB, KLZK and KCAE); the Y distance is four
    /// times the distance north to the box edge within 0.35 km at four sites
    /// and within 4.1 km at all; the spare is 0. The rotation is not the grid
    /// convergence (site longitude + 105 degrees): -36.025 at KILX (15.7
    /// degrees east of 105W), -4.0 at KCYS (0.2), 14.4 at KIWA (-6.7). The
    /// values are kept as stored.
    Parameters {
        /// The five values in file order.
        values: [f32; 5],
    },
    /// Packet 31: the number of storm ID (15) and special symbol (2) packet
    /// pairs that follow it in the layer.
    StormCount {
        /// The count.
        count: u16,
    },
}

impl IrmPacket {
    /// Names of the five values of packet 30 ([`IrmPacket::Parameters`]),
    /// after DSI-7000 Figure 3-22 sheet 7.
    pub const PARAMETER_NAMES: [&'static str; 5] = [
        "angle_rotation",
        "x_offset_distance",
        "y_offset_distance",
        "sixteenth_lfm_grid_box_size",
        "spare",
    ];

    /// The packet code: 30 or 31.
    pub fn code(&self) -> u16 {
        match self {
            Self::Parameters { .. } => 30,
            Self::StormCount { .. } => 31,
        }
    }
}

/// Decodes packet 30 or 31. `bytes` is the complete packet, starting with
/// its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    let short = || Level3Error::InvalidPacket {
        code,
        reason: format!("{} bytes", bytes.len()),
    };
    match code {
        30 => {
            let mut values = [0.0_f32; 5];
            for (k, value) in values.iter_mut().enumerate() {
                let at = 2 + 4 * k;
                let &[b0, b1, b2, b3] = bytes.get(at..at + 4).ok_or_else(short)? else {
                    return Err(short());
                };
                *value = f32::from_be_bytes([b0, b1, b2, b3]);
            }
            Ok(Packet::Irm(IrmPacket::Parameters { values }))
        }
        31 => {
            let &[hi, lo] = bytes.get(2..4).ok_or_else(short)? else {
                return Err(short());
            };
            Ok(Packet::Irm(IrmPacket::StormCount {
                count: u16::from_be_bytes([hi, lo]),
            }))
        }
        _ => Err(Level3Error::UnsupportedPacket(code)),
    }
}
