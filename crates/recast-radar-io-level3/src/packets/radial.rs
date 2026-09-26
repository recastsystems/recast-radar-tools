//! Radial image packets: Digital Radial Data Array (16, ICD 2620001AD Figure 3-11c)
//! and Radial Data Packet with 16 run-length levels (0xAF1F, Figure 3-10);
//! `docs/level3/reference.md` section 6.1.
//!
//! Both decode to a [`RadialPacket`]: the 14-byte packet header, the start and
//! width angle of every radial, and a radials x bins grid of data levels.
//! [`RadialPacket::values`] and [`RadialPacket::level_at`] map the levels to
//! physical values with the product's [`DataLevels`]
//! ([`DataLevels::for_packet`]).

use super::Packet;
use crate::Level3Error;
use crate::budget::Budget;
use crate::levels::{DataLevels, Level};

/// Largest radials x bins grid a radial packet may declare. The largest ICD
/// products hold 720 x 1840 bins; packets declaring more than 2^24 cells are
/// rejected with [`Level3Error::InvalidPacket`] instead of allocated.
///
/// A packet must also be able to hold its grid: 0xAF1F encodes at most 15
/// bins per byte (two 4-bit runs of up to 15 in a halfword) and packet 16 one,
/// so a packet declaring more than [`BINS_PER_PACKET_BYTE`] bins per packet
/// byte (plus a 4096-bin allowance) is rejected too. That bounds the grid a
/// packet allocates by its own size, whatever it declares. The grids of all
/// packets of a product share the product decode budget
/// ([`crate::MAX_PRODUCT_DECODED_BYTES`], one byte per bin).
pub const MAX_RADIAL_CELLS: usize = 1 << 24;

/// Declared bins allowed per byte of a radial packet (see
/// [`MAX_RADIAL_CELLS`]).
pub const BINS_PER_PACKET_BYTE: usize = 16;

/// Radial data packet (16 or 0xAF1F).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadialPacket {
    /// Packet code: 16 or 0xAF1F.
    pub code: u16,
    /// Index of the first range bin (halfword 2).
    pub first_bin: u16,
    /// Number of range bins per radial (halfword 3); the width of [`levels`](Self::levels).
    pub num_bins: u16,
    /// I coordinate of the center of the sweep (halfword 4), 1/4 km in the
    /// symbology block.
    pub i_center: i16,
    /// J coordinate of the center of the sweep (halfword 5), 1/4 km in the
    /// symbology block.
    pub j_center: i16,
    /// Range scale factor x1000 (halfword 6): the cosine of the elevation angle
    /// for packet 16 (1000 for volume products; TDWR products carry 1), pixels
    /// per range bin for 0xAF1F.
    pub scale_factor: u16,
    /// Start and width angles, one per radial in file order.
    pub radials: Vec<Radial>,
    /// Data levels, `radials.len()` rows of `num_bins` levels in file order.
    ///
    /// Packet 16 radials carry one byte per bin, padded to a halfword; 0xAF1F
    /// radials carry runs expanded here (4-bit run, 4-bit level). Data beyond
    /// `num_bins` in a radial (such as the pad byte) is dropped; a radial whose
    /// data covers fewer bins is filled with level 0. In the corpus every
    /// radial covers exactly `num_bins` bins but those of the KFTG 1994
    /// product 46, whose 63 radials cover 140 bins of the 188 it declares
    /// from bin 50: they end at bin 190, where the other three products of
    /// the same window end (94 km).
    pub levels: Vec<u8>,
}

/// Angles of one radial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Radial {
    /// Radial start angle in tenths of a degree.
    pub start_angle: i16,
    /// Angle delta (radial width) in tenths of a degree.
    pub delta_angle: i16,
}

impl Radial {
    /// Start angle in degrees: the `f32` nearest to the tenths divided by
    /// 10 (as Py-ART's `float32(angle_start * 0.1)`).
    pub fn start_angle_deg(&self) -> f32 {
        f32::from(self.start_angle) / 10.0
    }

    /// Angle delta in degrees, rounded like
    /// [`start_angle_deg`](Self::start_angle_deg).
    pub fn delta_angle_deg(&self) -> f32 {
        f32::from(self.delta_angle) / 10.0
    }
}

impl RadialPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }

    /// Number of radials.
    pub fn num_radials(&self) -> usize {
        self.radials.len()
    }

    /// Data levels of radial `index`, `num_bins` long.
    pub fn row(&self, index: usize) -> Option<&[u8]> {
        let bins = usize::from(self.num_bins);
        let start = index.checked_mul(bins)?;
        self.levels.get(start..start.checked_add(bins)?)
    }

    /// Radials as rows of data levels, in file order.
    pub fn rows(&self) -> impl Iterator<Item = &[u8]> {
        // `max(1)`: `chunks_exact` panics on 0; with 0 bins `levels` is empty.
        self.levels.chunks_exact(usize::from(self.num_bins).max(1))
    }

    /// Physical values of [`levels`](Self::levels) in the same layout
    /// (radials x `num_bins`, file order), NaN where a level has no physical
    /// value; see [`DataLevels::values`]. `levels` is the product's mapping
    /// from [`DataLevels::for_packet`].
    pub fn values(&self, levels: &DataLevels) -> Vec<f32> {
        levels.values(&self.levels)
    }

    /// What the level of bin `bin` in radial `radial` means, or `None` outside
    /// the grid.
    pub fn level_at(&self, radial: usize, bin: usize, levels: &DataLevels) -> Option<Level> {
        let level = self.row(radial)?.get(bin)?;
        Some(levels.level(u16::from(*level)))
    }
}

/// Decodes one radial packet (16, 0xAF1F). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    let u16_at = |offset: usize, what: &str| match bytes.get(offset..offset + 2) {
        Some(&[hi, lo]) => Ok(u16::from_be_bytes([hi, lo])),
        _ => Err(Level3Error::InvalidPacket {
            code,
            reason: format!("{what} at byte {offset} is past the end of the packet"),
        }),
    };
    let first_bin = u16_at(2, "index of first range bin")?;
    let num_bins = u16_at(4, "number of range bins")?;
    let i_center = u16_at(6, "I center of sweep")? as i16;
    let j_center = u16_at(8, "J center of sweep")? as i16;
    let scale_factor = u16_at(10, "range scale factor")?;
    let num_radials = u16_at(12, "number of radials")?;

    let bins = usize::from(num_bins);
    let cells = usize::from(num_radials) * bins;
    let limit = bytes
        .len()
        .saturating_mul(BINS_PER_PACKET_BYTE)
        .saturating_add(4096)
        .min(MAX_RADIAL_CELLS);
    if cells > limit {
        return Err(Level3Error::InvalidPacket {
            code,
            reason: format!(
                "{num_radials} radials of {num_bins} bins exceed the {limit}-cell limit \
                 of a {}-byte packet",
                bytes.len()
            ),
        });
    }

    budget.charge_bytes(cells, "radial packet levels")?;
    let mut radials = budget.vec(usize::from(num_radials), "radial angles")?;
    let mut levels = vec![0u8; cells];
    let mut p = 14;
    for row in 0..usize::from(num_radials) {
        let count = usize::from(u16_at(p, "radial data length")?);
        let start_angle = u16_at(p + 2, "radial start angle")? as i16;
        let delta_angle = u16_at(p + 4, "radial angle delta")? as i16;
        p += 6;
        // Packet 16 counts bytes, 0xAF1F counts run-length halfwords.
        let len = if code == 16 { count } else { 2 * count };
        let data = bytes
            .get(p..p + len)
            .ok_or_else(|| Level3Error::InvalidPacket {
                code,
                reason: format!(
                    "radial {row} data ({len} bytes at byte {p}) is past the end of the packet"
                ),
            })?;
        let out = &mut levels[row * bins..(row + 1) * bins];
        if code == 16 {
            let n = data.len().min(bins);
            out[..n].copy_from_slice(&data[..n]);
        } else {
            let mut q = 0;
            for &run in data {
                if q == bins {
                    break;
                }
                let end = (q + usize::from(run >> 4)).min(bins);
                out[q..end].fill(run & 0x0F);
                q = end;
            }
        }
        radials.push(Radial {
            start_angle,
            delta_angle,
        });
        p += len;
    }

    Ok(Packet::Radial(RadialPacket {
        code,
        first_bin,
        num_bins,
        i_center,
        j_center,
        scale_factor,
        radials,
        levels,
    }))
}
