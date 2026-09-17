//! Generic data packets (28, 29; ICD 2620001AD Figure 3-15c, Appendix E):
//! XDR-encoded (RFC 4506) product description and components.
//!
//! Packet 28 decodes to a [`GenericPacket`]. Layout of the XDR data, as
//! observed in the corpus (products 152 and 176; `docs/level3/reference.md`
//! section 6.1) and consistent with Appendix E:
//!
//! - strings are a `u32` length and the bytes, padded to a multiple of 4;
//! - `INT*2` fields of Figure E-1 are 4-byte XDR integers;
//! - a list (parameters, components, radials) is its element count followed,
//!   when the count is not zero, by an XDR array: the count again and the
//!   elements;
//! - component list elements are XDR optional data: a 4-byte "present" flag
//!   (1) followed by the component type and body.
//!
//! MetPy 1.7.1 (`Level3XDRParser`) reads the same bytes for every corpus file
//! but treats the repeated count as a "pointer" and reads an extra word between
//! list elements; the two readings differ only for parameter lists with two or
//! more entries, which no corpus file has.
//!
//! Radial (type 1, Figures E-3/E-4) and text (type 4, Figure E-8) components
//! are decoded; [`GenericRadialComponent::values`] and
//! [`GenericRadial::values`] map radial bin values to physical values with the
//! product's [`DataLevels`] ([`DataLevels::for_packet`]). Any other component
//! type ends decoding: it and everything after it are kept as
//! [`GenericComponent::Undecoded`], since XDR data cannot be skipped without
//! knowing its layout. Packet 29 (External Data Description,
//! Figure E-1b) has no real sample and is left as [`Packet::Unknown`].

use chrono::{DateTime, Utc};

use super::Packet;
use crate::Level3Error;
use crate::levels::{DataLevels, Level};

/// Generic data packet (28).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericPacket {
    /// Packet code: 28.
    pub code: u16,
    /// Product Description data structure (Figure E-1).
    pub product: GenericProduct,
    /// Components in file order.
    pub components: Vec<GenericComponent>,
}

impl GenericPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }

    /// Radial components, in file order.
    pub fn radial_components(&self) -> impl Iterator<Item = &GenericRadialComponent> {
        self.components.iter().filter_map(|c| match c {
            GenericComponent::Radial(radial) => Some(radial),
            _ => None,
        })
    }
}

/// Product Description data structure of a generic product (Figure E-1).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericProduct {
    /// Product name, e.g. `ASP`.
    pub name: String,
    /// Product description (may contain version information).
    pub description: String,
    /// Product code.
    pub product_code: i32,
    /// Product type: 1 volume, 2 elevation, 3 time, 4 on demand, 5 on request,
    /// 6 radial, 7 external.
    pub product_type: i32,
    /// Generation time, Unix seconds.
    pub generation_time: u32,
    /// Radar name; empty when not applicable.
    pub radar_name: String,
    /// Radar latitude, degrees.
    pub radar_latitude: f32,
    /// Radar longitude, degrees.
    pub radar_longitude: f32,
    /// Radar height, meters above mean sea level.
    pub radar_height: f32,
    /// Volume scan start time, Unix seconds.
    pub volume_time: u32,
    /// Elevation scan start time, Unix seconds (used for elevation products).
    pub elevation_time: u32,
    /// Elevation angle, degrees.
    pub elevation_angle: f32,
    /// Volume scan number.
    pub volume_number: i32,
    /// Operational mode: 1 test, 2 clear air, 3 precipitation.
    pub operational_mode: i32,
    /// Volume coverage pattern.
    pub vcp: i32,
    /// Elevation number within the VCP (elevation products only; other
    /// products may carry any value).
    pub elevation_number: i32,
    /// Spare, reserved for a compression type.
    pub compression: i32,
    /// Spare, reserved for a decompressed size.
    pub uncompressed_size: i32,
    /// Product parameters (Figure E-2).
    pub parameters: Vec<GenericParameter>,
}

impl GenericProduct {
    /// [`generation_time`](Self::generation_time) as a date and time.
    pub fn generation_datetime(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(i64::from(self.generation_time), 0)
    }

    /// [`volume_time`](Self::volume_time) as a date and time.
    pub fn volume_datetime(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(i64::from(self.volume_time), 0)
    }

    /// [`elevation_time`](Self::elevation_time) as a date and time.
    pub fn elevation_datetime(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(i64::from(self.elevation_time), 0)
    }
}

/// A product or component parameter (Figure E-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParameter {
    /// Parameter identifier.
    pub id: String,
    /// Attributes, `name = description;` sections (Figure E-2 Note 1).
    pub attributes: String,
}

/// One component of a generic product.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GenericComponent {
    /// Radial component (type 1).
    Radial(GenericRadialComponent),
    /// Text component (type 4).
    Text {
        /// Component parameters.
        parameters: Vec<GenericParameter>,
        /// The text.
        text: String,
    },
    /// A component type that is not decoded (grid 2, area 3, table 5, event 6,
    /// or unknown): `bytes` holds the XDR data from just after the type to the
    /// end of the packet, including any later components.
    Undecoded {
        /// Component type.
        kind: i32,
        /// Remaining XDR data.
        bytes: Vec<u8>,
    },
}

/// Radial component (Figure E-3).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericRadialComponent {
    /// Component description.
    pub description: String,
    /// Range extent of each bin, meters.
    pub bin_size: f32,
    /// Range to the center of the first bin, meters.
    pub range_to_first_bin: f32,
    /// Component parameters.
    pub parameters: Vec<GenericParameter>,
    /// Radials in file order.
    pub radials: Vec<GenericRadial>,
}

impl GenericRadialComponent {
    /// The most bins any radial holds ([`GenericRadial::bins`]): the column
    /// count of [`values`](Self::values).
    pub fn num_bins(&self) -> usize {
        self.radials
            .iter()
            .map(|radial| radial.bins().len())
            .max()
            .unwrap_or(0)
    }

    /// Physical values of all radials, radials x [`num_bins`](Self::num_bins)
    /// row-major in file order, NaN where a bin value has no physical value
    /// (see [`DataLevels::values`]) and after the last bin of a shorter radial.
    /// `levels` is the product's mapping from [`DataLevels::for_packet`].
    pub fn values(&self, levels: &DataLevels) -> Vec<f32> {
        let columns = self.num_bins();
        // -1 is outside the level range, so padding maps to NaN.
        let mut data = Vec::with_capacity(self.radials.len().saturating_mul(columns));
        for radial in &self.radials {
            let bins = radial.bins();
            data.extend_from_slice(bins);
            data.resize(data.len() + (columns - bins.len()), -1);
        }
        levels.values(&data)
    }
}

/// Radial information (Figure E-4) with its bin values (Figure E-11).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericRadial {
    /// Azimuth of the leading edge, degrees.
    pub azimuth: f32,
    /// Elevation angle, degrees.
    pub elevation: f32,
    /// Radial width, degrees.
    pub width: f32,
    /// Number of bins (a 4-byte integer, not the `REAL*4` Figure E-4 lists).
    pub num_bins: i32,
    /// Bin value attributes, e.g. `type = ushort; Unit = inches/hour`.
    pub attributes: String,
    /// Bin values as stored: an XDR integer array. Data levels map to
    /// physical values through the product's
    /// [`DataLevels`]; see [`values`](Self::values).
    pub values: Vec<i32>,
}

impl GenericRadial {
    /// The stored values that are bins of this radial: the first
    /// [`num_bins`](Self::num_bins) of them (all when fewer are stored, none
    /// when `num_bins` is negative).
    pub fn bins(&self) -> &[i32] {
        let n = usize::try_from(self.num_bins).unwrap_or(0);
        &self.values[..n.min(self.values.len())]
    }

    /// Physical values of [`bins`](Self::bins), NaN where a value has no
    /// physical value; see [`DataLevels::values`].
    pub fn values(&self, levels: &DataLevels) -> Vec<f32> {
        levels.values(self.bins())
    }

    /// What bin `bin` means, or `None` past the last bin.
    pub fn level_at(&self, bin: usize, levels: &DataLevels) -> Option<Level> {
        let value = *self.bins().get(bin)?;
        Some(u16::try_from(value).map_or(Level::Undefined, |n| levels.level(n)))
    }
}

/// Decodes one generic data packet (28, 29). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher.
pub(crate) fn decode(code: u16, bytes: &[u8]) -> Result<Packet, Level3Error> {
    if code != 28 {
        return Err(Level3Error::UnsupportedPacket(code));
    }
    let data = bytes.get(8..).ok_or(Level3Error::InvalidPacket {
        code,
        reason: "shorter than its 8-byte header".into(),
    })?;
    let mut xdr = Xdr { data, pos: 0, code };
    let product = xdr.product()?;
    let components = xdr.components()?;
    Ok(Packet::Generic(GenericPacket {
        code,
        product,
        components,
    }))
}

/// XDR reader over the data of one generic packet. Every count is checked
/// against the bytes left before anything is allocated for it.
struct Xdr<'a> {
    data: &'a [u8],
    pos: usize,
    code: u16,
}

impl Xdr<'_> {
    fn invalid(&self, reason: String) -> Level3Error {
        Level3Error::InvalidPacket {
            code: self.code,
            reason: format!("XDR data byte {}: {reason}", self.pos),
        }
    }

    fn word(&mut self, what: &str) -> Result<[u8; 4], Level3Error> {
        match self.data.get(self.pos..self.pos + 4) {
            Some(&[a, b, c, d]) => {
                self.pos += 4;
                Ok([a, b, c, d])
            }
            _ => Err(self.invalid(format!("{what} runs past the end of the packet"))),
        }
    }

    fn u32(&mut self, what: &str) -> Result<u32, Level3Error> {
        self.word(what).map(u32::from_be_bytes)
    }

    fn i32(&mut self, what: &str) -> Result<i32, Level3Error> {
        self.word(what).map(i32::from_be_bytes)
    }

    fn f32(&mut self, what: &str) -> Result<f32, Level3Error> {
        self.word(what).map(f32::from_be_bytes)
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// XDR string: length, bytes, padding to a multiple of 4. Bytes are kept as
    /// Latin-1 characters; trailing NUL terminators (Appendix E strings are C
    /// strings, and text components count the terminator in the length) are dropped.
    fn string(&mut self, what: &str) -> Result<String, Level3Error> {
        let len = self.u32(what)?;
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let padded = len.div_ceil(4).saturating_mul(4);
        if padded > self.remaining() {
            return Err(self.invalid(format!(
                "{what} of {len} bytes runs past the end of the packet"
            )));
        }
        let bytes = &self.data[self.pos..self.pos + len];
        let text = bytes[..bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1)]
            .iter()
            .copied()
            .map(char::from)
            .collect();
        self.pos += padded;
        Ok(text)
    }

    /// A list count followed, when not zero, by the XDR array length, which
    /// must repeat it. `min_bytes` is the smallest encoded size of one element.
    fn list(&mut self, what: &str, min_bytes: usize) -> Result<usize, Level3Error> {
        let count = self.i32(what)?;
        let Ok(n) = usize::try_from(count) else {
            return Err(self.invalid(format!("{what} is negative ({count})")));
        };
        if n == 0 {
            return Ok(0);
        }
        let listed = self.u32(what)?;
        if u32::try_from(n).ok() != Some(listed) {
            return Err(self.invalid(format!("{what} is {n} but its array holds {listed}")));
        }
        if n.saturating_mul(min_bytes) > self.remaining() {
            return Err(self.invalid(format!(
                "{what} ({n}) cannot fit in the {} bytes left",
                self.remaining()
            )));
        }
        Ok(n)
    }

    fn parameters(&mut self) -> Result<Vec<GenericParameter>, Level3Error> {
        let n = self.list("number of parameters", 8)?;
        let mut parameters = Vec::with_capacity(n);
        for _ in 0..n {
            parameters.push(GenericParameter {
                id: self.string("parameter id")?,
                attributes: self.string("parameter attributes")?,
            });
        }
        Ok(parameters)
    }

    fn product(&mut self) -> Result<GenericProduct, Level3Error> {
        Ok(GenericProduct {
            name: self.string("product name")?,
            description: self.string("product description")?,
            product_code: self.i32("product code")?,
            product_type: self.i32("product type")?,
            generation_time: self.u32("generation time")?,
            radar_name: self.string("radar name")?,
            radar_latitude: self.f32("radar latitude")?,
            radar_longitude: self.f32("radar longitude")?,
            radar_height: self.f32("radar height")?,
            volume_time: self.u32("volume scan start time")?,
            elevation_time: self.u32("elevation scan start time")?,
            elevation_angle: self.f32("elevation angle")?,
            volume_number: self.i32("volume scan number")?,
            operational_mode: self.i32("operational mode")?,
            vcp: self.i32("volume coverage pattern")?,
            elevation_number: self.i32("elevation number")?,
            compression: self.i32("compression spare")?,
            uncompressed_size: self.i32("decompressed size spare")?,
            parameters: self.parameters()?,
        })
    }

    fn components(&mut self) -> Result<Vec<GenericComponent>, Level3Error> {
        let n = self.list("number of components", 4)?;
        let mut components = Vec::new();
        for _ in 0..n {
            match self.u32("component present flag")? {
                0 => continue,
                1 => {}
                other => {
                    return Err(self.invalid(format!("component present flag is {other}")));
                }
            }
            let kind = self.i32("component type")?;
            match kind {
                1 => components.push(GenericComponent::Radial(self.radial_component()?)),
                4 => components.push(GenericComponent::Text {
                    parameters: self.parameters()?,
                    text: self.string("text")?,
                }),
                _ => {
                    components.push(GenericComponent::Undecoded {
                        kind,
                        bytes: self.data[self.pos..].to_vec(),
                    });
                    self.pos = self.data.len();
                    break;
                }
            }
        }
        Ok(components)
    }

    fn radial_component(&mut self) -> Result<GenericRadialComponent, Level3Error> {
        let description = self.string("radial component description")?;
        let bin_size = self.f32("bin size")?;
        let range_to_first_bin = self.f32("range to first bin")?;
        let parameters = self.parameters()?;
        // Azimuth, elevation, width, bins, attributes length, values length.
        let n = self.list("number of radials", 24)?;
        let mut radials = Vec::with_capacity(n);
        for _ in 0..n {
            let azimuth = self.f32("radial azimuth")?;
            let elevation = self.f32("radial elevation")?;
            let width = self.f32("radial width")?;
            let num_bins = self.i32("number of bins")?;
            let attributes = self.string("bin value attributes")?;
            let count = self.u32("bin value count")?;
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            if count.saturating_mul(4) > self.remaining() {
                return Err(self.invalid(format!(
                    "{count} bin values cannot fit in the {} bytes left",
                    self.remaining()
                )));
            }
            let values = self.data[self.pos..self.pos + 4 * count]
                .chunks_exact(4)
                .map(|w| i32::from_be_bytes([w[0], w[1], w[2], w[3]]))
                .collect();
            self.pos += 4 * count;
            radials.push(GenericRadial {
                azimuth,
                elevation,
                width,
                num_bins,
                attributes,
                values,
            });
        }
        Ok(GenericRadialComponent {
            description,
            bin_size,
            range_to_first_bin,
            parameters,
            radials,
        })
    }
}
