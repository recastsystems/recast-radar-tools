//! Generic data packets (28, 29; ICD 2620001AD Figure 3-15c, Appendix E):
//! XDR-encoded (RFC 4506) product description and components.
//!
//! Packets 28 and 29 decode to a [`GenericPacket`]. The layout of the XDR
//! data is the one the RPG writes: `xdr_RPGP_product_t` and the component
//! functions of `orpg_xdr.c` (revision 1.5, 2008) in the public edition of
//! the WSR-88D Common Operations and Development Environment (CODE, ORPG
//! source; `docs/level3/reference.md` section 7). Every corpus product
//! (radial and text components of products 152 and 176; section 6.1)
//! follows it:
//!
//! - strings (`xdr_string`, `xdr_bytes`) are a `u32` length and the bytes,
//!   padded to a multiple of 4;
//! - `short` fields (the `INT*2` of the figures) are 4-byte XDR integers;
//! - a counted list (parameters, components, radials, grid dimensions, area
//!   points, the components of an event) is its count followed, when the
//!   count is positive, by an XDR array: the count again and the elements;
//! - each component is an XDR pointer: a 4-byte flag (1 present, 0 absent)
//!   and, when present, the component, starting with its type;
//! - the product description is the External Data Description (Figure
//!   E-1b: five spares, the compression type and the decompressed size)
//!   when the product type is [`EXTERNAL_PRODUCT_TYPE`], whatever the packet
//!   code, and Figure E-1 otherwise;
//! - binary data (Figure E-11) is the attributes string and, when there are
//!   values, an XDR array whose element type is the required `type`
//!   attribute: `double` 8-byte IEEE, `float` 4-byte IEEE, and `int`,
//!   `uint`, `short`, `ushort`, `byte` and `ubyte` 4-byte integers. ORPG
//!   serializes no other type (not `string`), and another type is
//!   [`Level3Error::InvalidPacket`];
//! - a radial carries its bin data only when its bin count is positive, and a
//!   grid its values only when its dimensions multiply to a positive count;
//! - table column labels, row labels and entries are XDR arrays of strings,
//!   each only when its count (columns, rows, rows x columns) is positive.
//!
//! MetPy 1.7.1 (`Level3XDRParser`) reads the same bytes for every corpus file
//! but treats the repeated count as a "pointer" and reads an extra word
//! between list elements; ORPG writes such a word only before each component
//! (the pointer flag), so the readings differ for parameter lists with two
//! or more entries, which no corpus file has.
//!
//! Radial (type 1, Figures E-3/E-4) and text (type 4, Figure E-8) components
//! are checked against real products; [`GenericRadialComponent::values`] and
//! [`GenericRadial::values`] map radial bin values to physical values with
//! the product's [`DataLevels`] ([`DataLevels::for_packet`]). Radial bins of
//! type `float` or `double` are [`Level3Error::InvalidPacket`]:
//! [`GenericRadial::values`] holds integers. Grid (type 2), area (type 3),
//! table (type 5) and event (type 6) components and External Data
//! Descriptions follow ORPG's serializer, but no public product carrying
//! them has been found (`docs/level3/reference.md` section 7), so they are
//! not checked against data.
//!
//! Events nest components; nesting deeper than [`MAX_EVENT_DEPTH`] is
//! [`Level3Error::InvalidPacket`]. A component type the ICD does not define
//! ends decoding: it and everything after it are kept as
//! [`GenericComponent::Undecoded`], since XDR data cannot be skipped without
//! knowing its layout.
//!
//! A radial component's radials may hold different numbers of bins, and
//! [`GenericRadialComponent::values`] pads them to the longest. A component
//! whose padded grid (radials x the most bins any radial holds) exceeds
//! [`MAX_RADIAL_CELLS`], or the bytes the packet holds, is
//! [`Level3Error::InvalidPacket`], the limit of radial packets: a few short
//! radials and one long one would otherwise make the grid far larger than
//! the packet.

use chrono::{DateTime, Utc};

use super::Packet;
use super::radial::MAX_RADIAL_CELLS;
use crate::Level3Error;
use crate::budget::Budget;
use crate::levels::{DataLevels, Level};

/// Deepest nesting of event components (Figure E-10) accepted.
pub const MAX_EVENT_DEPTH: usize = 8;

/// Product type of an external product (ORPG `RPGP_EXTERNAL`): its description
/// is the External Data Description of Figure E-1b.
pub const EXTERNAL_PRODUCT_TYPE: i32 = 7;

/// Generic data packet (28, 29).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericPacket {
    /// Packet code: 28 (Product Description data structure) or 29 (External
    /// Data Description data structure).
    pub code: u16,
    /// Product Description data structure (Figure E-1). For an external
    /// product ([`EXTERNAL_PRODUCT_TYPE`], whatever the packet code) the
    /// External Data Description (Figure E-1b): name, description, code, type,
    /// generation time, compression type and decompressed size, the radar and
    /// scan fields empty or 0, and the spares in
    /// [`GenericProduct::external_spares`].
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
    /// Compression type (ORPG: not used, 0).
    pub compression: i32,
    /// Decompressed size (ORPG: not used, 0).
    pub uncompressed_size: i32,
    /// Product parameters (Figure E-2).
    pub parameters: Vec<GenericParameter>,
    /// The five spare fields of an External Data Description; `None` for
    /// other products.
    pub external_spares: Option<[i32; 5]>,
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
    /// Grid component (type 2).
    Grid(GenericGridComponent),
    /// Area component (type 3).
    Area(GenericAreaComponent),
    /// Table component (type 5).
    Table(GenericTableComponent),
    /// Event component (type 6): an event with its parameters and components.
    Event(GenericEventComponent),
    /// A component type the ICD does not define: `bytes` holds the XDR data
    /// from just after the type to the end of the packet, including any later
    /// components.
    Undecoded {
        /// Component type.
        kind: i32,
        /// Remaining XDR data.
        bytes: Vec<u8>,
    },
}

/// Binary data (Figure E-11): values described by an attributes string.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericData {
    /// Attributes, `name = description;` sections (Figure E-2 Note 1); the
    /// `type` attribute selects the element type.
    pub attributes: String,
    /// The values.
    pub values: GenericValues,
}

/// The values of [`GenericData`], by element type: the types ORPG
/// serializes (`xdr_RPGP_data_t`; `string` is not one of them).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GenericValues {
    /// `int`, `uint`, `short`, `ushort`, `byte` and `ubyte`: one 4-byte XDR
    /// integer each (`uint` values above `i32::MAX` keep their bits).
    Int(Vec<i32>),
    /// `float`.
    Float(Vec<f32>),
    /// `double`.
    Double(Vec<f64>),
}

/// Grid component (Figure E-5).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericGridComponent {
    /// Grid dimensions, fastest changing first.
    pub dimensions: Vec<i32>,
    /// Grid type: 1 array, 2 equally spaced, 3 lat/lon, 4 polar.
    pub grid_type: i32,
    /// Component parameters (origin, step sizes, ...).
    pub parameters: Vec<GenericParameter>,
    /// Grid data, first dimension varying fastest.
    pub data: GenericData,
}

/// Area component (Figure E-6).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericAreaComponent {
    /// Component parameters.
    pub parameters: Vec<GenericParameter>,
    /// Area type: `0x00001` point, `0x00002` area, `0x00003` polyline, in
    /// latitude/longitude; `0x1000n` in X/Y km; `0x2000n` in azimuth/range.
    pub area_type: i32,
    /// Points: (latitude, longitude), (x, y) or (azimuth, range) by type.
    pub points: Vec<(f32, f32)>,
}

/// Table component (Figure E-9).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericTableComponent {
    /// Component parameters.
    pub parameters: Vec<GenericParameter>,
    /// Title.
    pub title: String,
    /// Number of columns.
    pub columns: i32,
    /// Number of rows.
    pub rows: i32,
    /// Column labels.
    pub column_labels: Vec<String>,
    /// Row labels.
    pub row_labels: Vec<String>,
    /// Entries, rows x columns with the row index varying fastest.
    pub entries: Vec<String>,
}

/// Event component (Figure E-10).
#[derive(Debug, Clone, PartialEq)]
pub struct GenericEventComponent {
    /// Event parameters.
    pub parameters: Vec<GenericParameter>,
    /// The components of the event.
    pub components: Vec<GenericComponent>,
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
    /// when `num_bins` is negative). The volume keeps the values after them
    /// too (`level3_surplus_values`, see [`crate::volume`]).
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
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    if !matches!(code, 28 | 29) {
        return Err(Level3Error::UnsupportedPacket(code));
    }
    let data = bytes.get(8..).ok_or(Level3Error::InvalidPacket {
        code,
        reason: "shorter than its 8-byte header".into(),
    })?;
    let mut xdr = Xdr {
        data,
        pos: 0,
        code,
        budget,
    };
    let product = xdr.product()?;
    let components = xdr.components(0)?;
    Ok(Packet::Generic(GenericPacket {
        code,
        product,
        components,
    }))
}

/// XDR reader over the data of one generic packet. Every count is checked
/// against the bytes left, and what it allocates is charged to the product
/// decode budget, before anything is allocated for it.
struct Xdr<'a, 'b> {
    data: &'a [u8],
    pos: usize,
    code: u16,
    budget: &'b mut Budget,
}

impl Xdr<'_, '_> {
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
        let text = self.budget.latin1(
            &bytes[..bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1)],
            "generic data string",
        )?;
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
        let mut parameters = self.budget.vec(n, "generic parameters")?;
        for _ in 0..n {
            parameters.push(GenericParameter {
                id: self.string("parameter id")?,
                attributes: self.string("parameter attributes")?,
            });
        }
        Ok(parameters)
    }

    /// The product description (Figure E-1, ORPG `RPGP_product_t`), or the
    /// External Data Description (Figure E-1b, `RPGP_ext_data_t`) when the
    /// product type is [`EXTERNAL_PRODUCT_TYPE`]: ORPG's
    /// `xdr_RPGP_product_t` chooses the layout by the type, not by the
    /// packet code.
    fn product(&mut self) -> Result<GenericProduct, Level3Error> {
        let name = self.string("product name")?;
        let description = self.string("product description")?;
        let product_code = self.i32("product code")?;
        let product_type = self.i32("product type")?;
        let generation_time = self.u32("generation time")?;
        if product_type == EXTERNAL_PRODUCT_TYPE {
            return self.external(name, description, product_code, generation_time);
        }
        Ok(GenericProduct {
            name,
            description,
            product_code,
            product_type,
            generation_time,
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
            external_spares: None,
        })
    }

    /// The rest of an External Data Description, as a [`GenericProduct`]:
    /// five spares, the compression type and the decompressed size (each a
    /// 4-byte XDR integer), then the parameters.
    fn external(
        &mut self,
        name: String,
        description: String,
        product_code: i32,
        generation_time: u32,
    ) -> Result<GenericProduct, Level3Error> {
        let mut spares = [0i32; 5];
        for spare in &mut spares {
            *spare = self.i32("external data spare")?;
        }
        let compression = self.i32("compression type")?;
        let uncompressed_size = self.i32("decompressed size")?;
        Ok(GenericProduct {
            name,
            description,
            product_code,
            product_type: EXTERNAL_PRODUCT_TYPE,
            generation_time,
            radar_name: String::new(),
            radar_latitude: 0.0,
            radar_longitude: 0.0,
            radar_height: 0.0,
            volume_time: 0,
            elevation_time: 0,
            elevation_angle: 0.0,
            volume_number: 0,
            operational_mode: 0,
            vcp: 0,
            elevation_number: 0,
            compression,
            uncompressed_size,
            parameters: self.parameters()?,
            external_spares: Some(spares),
        })
    }

    /// A component list; `depth` counts the events it is nested in.
    fn components(&mut self, depth: usize) -> Result<Vec<GenericComponent>, Level3Error> {
        if depth > MAX_EVENT_DEPTH {
            return Err(self.invalid(format!("events nested more than {MAX_EVENT_DEPTH} deep")));
        }
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
            let component = match kind {
                1 => GenericComponent::Radial(self.radial_component()?),
                2 => GenericComponent::Grid(self.grid_component()?),
                3 => GenericComponent::Area(self.area_component()?),
                4 => GenericComponent::Text {
                    parameters: self.parameters()?,
                    text: self.string("text")?,
                },
                5 => GenericComponent::Table(self.table_component()?),
                6 => GenericComponent::Event(GenericEventComponent {
                    parameters: self.parameters()?,
                    components: self.components(depth + 1)?,
                }),
                _ => {
                    let bytes = self
                        .budget
                        .bytes(&self.data[self.pos..], "undecoded component")?;
                    self.pos = self.data.len();
                    let component = GenericComponent::Undecoded { kind, bytes };
                    self.budget
                        .push(&mut components, component, "generic components")?;
                    break;
                }
            };
            self.budget
                .push(&mut components, component, "generic components")?;
        }
        Ok(components)
    }

    fn radial_component(&mut self) -> Result<GenericRadialComponent, Level3Error> {
        let description = self.string("radial component description")?;
        let bin_size = self.f32("bin size")?;
        let range_to_first_bin = self.f32("range to first bin")?;
        let parameters = self.parameters()?;
        // Azimuth, elevation, width and bin count; a radial with bins adds its
        // attributes and values.
        let n = self.list("number of radials", 16)?;
        let mut radials = self.budget.vec(n, "generic radials")?;
        for _ in 0..n {
            let azimuth = self.f32("radial azimuth")?;
            let elevation = self.f32("radial elevation")?;
            let width = self.f32("radial width")?;
            let num_bins = self.i32("number of bins")?;
            // ORPG writes a radial's bin data only when it has bins.
            let (attributes, values) = if num_bins > 0 {
                self.radial_bins()?
            } else {
                (String::new(), Vec::new())
            };
            radials.push(GenericRadial {
                azimuth,
                elevation,
                width,
                num_bins,
                attributes,
                values,
            });
        }
        // The grid of the component (radials x longest radial, shorter
        // radials padded) may not exceed the radial packet limit nor the bytes
        // the packet holds (4 per stored bin when every radial is full length).
        let longest = radials.iter().map(|r| r.bins().len()).max().unwrap_or(0);
        let cells = radials.len().saturating_mul(longest);
        if cells > MAX_RADIAL_CELLS || cells > self.data.len() {
            return Err(self.invalid(format!(
                "{} radials padded to {longest} bins exceed the {MAX_RADIAL_CELLS}-cell limit \
                 or the packet's {} bytes",
                radials.len(),
                self.data.len()
            )));
        }
        Ok(GenericRadialComponent {
            description,
            bin_size,
            range_to_first_bin,
            parameters,
            radials,
        })
    }

    /// The bin data of a radial (ORPG `xdr_RPGP_data_t`): the attributes and
    /// an XDR array of integers. Bins of type `float` or `double` are an
    /// error: [`GenericRadial::values`] holds integers.
    fn radial_bins(&mut self) -> Result<(String, Vec<i32>), Level3Error> {
        let attributes = self.string("bin value attributes")?;
        if self.element_type(&attributes)? != ElementType::Int {
            return Err(self.invalid(format!(
                "radial bins of type {:?} are not decoded; only integer types are",
                attribute(&attributes, "type").unwrap_or_default()
            )));
        }
        let count = self.array_len("bin value count", 4)?;
        self.budget.charge::<i32>(count, "generic bin values")?;
        let values = self.data[self.pos..self.pos + 4 * count]
            .chunks_exact(4)
            .map(|w| i32::from_be_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        self.pos += 4 * count;
        Ok((attributes, values))
    }

    /// The element type binary data `attributes` name (ORPG
    /// `Get_data_type`, which requires one).
    fn element_type(&self, attributes: &str) -> Result<ElementType, Level3Error> {
        let Some(kind) = attribute(attributes, "type") else {
            return Err(self.invalid("binary data attributes name no type".into()));
        };
        let is = |name: &str| kind.eq_ignore_ascii_case(name);
        if ["int", "uint", "short", "ushort", "byte", "ubyte"]
            .into_iter()
            .any(is)
        {
            Ok(ElementType::Int)
        } else if is("float") {
            Ok(ElementType::Float)
        } else if is("double") {
            Ok(ElementType::Double)
        } else {
            Err(self.invalid(format!(
                "binary data type {kind:?} is not one ORPG serializes"
            )))
        }
    }

    /// An XDR array of `min_bytes`-byte elements: its length, checked
    /// against the bytes left.
    fn array_len(&mut self, what: &str, min_bytes: usize) -> Result<usize, Level3Error> {
        let n = usize::try_from(self.u32(what)?).unwrap_or(usize::MAX);
        if n.saturating_mul(min_bytes) > self.remaining() {
            return Err(self.invalid(format!(
                "{what} ({n}) cannot fit in the {} bytes left",
                self.remaining()
            )));
        }
        Ok(n)
    }

    /// An XDR array of strings.
    fn strings(&mut self, what: &str) -> Result<Vec<String>, Level3Error> {
        let n = self.array_len(what, 4)?;
        let mut out = self.budget.vec(n, "generic strings")?;
        for _ in 0..n {
            out.push(self.string(what)?);
        }
        Ok(out)
    }

    /// Binary data (Figure E-11; ORPG `xdr_RPGP_data_t`): attributes, then,
    /// when `has_values`, an array typed by them.
    fn data(&mut self, has_values: bool) -> Result<GenericData, Level3Error> {
        let attributes = self.string("binary data attributes")?;
        let kind = self.element_type(&attributes)?;
        let values = match kind {
            ElementType::Int if !has_values => GenericValues::Int(Vec::new()),
            ElementType::Float if !has_values => GenericValues::Float(Vec::new()),
            ElementType::Double if !has_values => GenericValues::Double(Vec::new()),
            ElementType::Float => {
                let n = self.array_len("float value count", 4)?;
                let mut v = self.budget.vec(n, "generic float values")?;
                for _ in 0..n {
                    v.push(self.f32("float value")?);
                }
                GenericValues::Float(v)
            }
            ElementType::Double => {
                let n = self.array_len("double value count", 8)?;
                let mut v = self.budget.vec(n, "generic double values")?;
                for _ in 0..n {
                    let hi = self.u32("double value")?;
                    let lo = self.u32("double value")?;
                    v.push(f64::from_bits((u64::from(hi) << 32) | u64::from(lo)));
                }
                GenericValues::Double(v)
            }
            ElementType::Int => {
                let n = self.array_len("integer value count", 4)?;
                let mut v = self.budget.vec(n, "generic integer values")?;
                for _ in 0..n {
                    v.push(self.i32("integer value")?);
                }
                GenericValues::Int(v)
            }
        };
        Ok(GenericData { attributes, values })
    }

    fn grid_component(&mut self) -> Result<GenericGridComponent, Level3Error> {
        let n = self.list("number of dimensions", 4)?;
        let mut dimensions = self.budget.vec(n, "generic grid dimensions")?;
        for _ in 0..n {
            dimensions.push(self.i32("grid dimension")?);
        }
        // ORPG writes the values only when the dimensions multiply to a
        // positive count (none without dimensions).
        let has_values = !dimensions.is_empty()
            && dimensions
                .iter()
                .fold(1i64, |product, &d| product.saturating_mul(i64::from(d)))
                > 0;
        Ok(GenericGridComponent {
            dimensions,
            grid_type: self.i32("grid type")?,
            parameters: self.parameters()?,
            data: self.data(has_values)?,
        })
    }

    fn area_component(&mut self) -> Result<GenericAreaComponent, Level3Error> {
        let parameters = self.parameters()?;
        let area_type = self.i32("area type")?;
        let n = self.list("number of points", 8)?;
        let mut points = self.budget.vec(n, "generic area points")?;
        for _ in 0..n {
            points.push((self.f32("point coordinate")?, self.f32("point coordinate")?));
        }
        Ok(GenericAreaComponent {
            parameters,
            area_type,
            points,
        })
    }

    /// Table component (ORPG `xdr_RPGP_table_t`): each array of strings is
    /// present only when its count (columns, rows, rows x columns) is
    /// positive.
    fn table_component(&mut self) -> Result<GenericTableComponent, Level3Error> {
        let parameters = self.parameters()?;
        let title = self.string("table title")?;
        let columns = self.i32("number of columns")?;
        let rows = self.i32("number of rows")?;
        let column_labels = self.strings_if(columns > 0, "column labels")?;
        let row_labels = self.strings_if(rows > 0, "row labels")?;
        let cells = i64::from(rows) * i64::from(columns);
        let entries = self.strings_if(cells > 0, "table entries")?;
        Ok(GenericTableComponent {
            parameters,
            title,
            columns,
            rows,
            column_labels,
            row_labels,
            entries,
        })
    }

    /// An XDR array of strings when `present`, else none.
    fn strings_if(&mut self, present: bool, what: &str) -> Result<Vec<String>, Level3Error> {
        if present {
            self.strings(what)
        } else {
            Ok(Vec::new())
        }
    }
}

/// Element type of binary data (see `Xdr::element_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementType {
    /// One of the integer types: a 4-byte XDR integer each.
    Int,
    /// `float`: 4-byte IEEE.
    Float,
    /// `double`: 8-byte IEEE.
    Double,
}

/// The value of attribute `name` (case insensitive) in an attributes string
/// (`name = value; ...`, Figure E-2 Note 1).
pub fn attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    attributes.split(';').find_map(|section| {
        let (key, value) = section.split_once('=')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}
