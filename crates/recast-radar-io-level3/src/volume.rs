//! Radial, raster and generic products as an FM301 [`Volume`]
//! (`recast_radar_core::model`; spec section 4.5, design note
//! `docs/design/fm301-model.md` section 8.1).
//!
//! [`read_level3_volume`] decodes a file and converts it;
//! [`Level3Product::to_volume`] converts a decoded product. Every data array
//! of the Product Symbology Block ([`Level3Product::data_arrays`]: radial
//! packets 16 and 0xAF1F, raster packets 0xBA07, 0xBA0F, 18 and 33, the
//! digital precipitation array 17, and each radial component of a generic
//! packet 28) becomes one sweep with one field, in symbology order. Graphic,
//! tabular and text products, and products whose symbology holds only
//! symbols, vectors or text, are [`Level3Error::NoDataArray`]; their content
//! is in the decoded [`Level3Product`]. The one data array not converted is
//! a generic grid component (type 2): the ICD gives its grid types (array,
//! equally spaced, latitude/longitude, polar) and leaves their origin and
//! step to component parameters it does not name, and no real product with
//! one has been found (`docs/level3/coverage.md`, Known gaps), so its
//! placement would be a guess. It stays typed in the decoded product
//! ([`crate::packets::generic::GenericGridComponent`]), and the volume keeps
//! its dimensions, parameters and every value as a display packet record
//! (`level3_display_packets`, below).
//!
//! # Mapping
//!
//! **Volume.** `instrument_name` is the generic product's radar name when the
//! packet carries one, else the site of the AWIPS identifier (`N0QTLX` ->
//! `TLX`). `time_reference` is the volume scan start (halfwords 21-23);
//! `volume_number` the volume scan number; `scan` the VCP; `location` the
//! Product Description Block latitude, longitude and height (feet converted
//! to metres). `attrs.other` keeps every value the decoder reads that has no
//! slot: the product code, mnemonic, AWIPS identifier, WMO heading and its
//! parts, NOAAPort sequence number, the number of zlib frames with the
//! communications control block and the WMO heading and AWIPS identifier
//! repeated inside the zlib data (`zlib_*`), Message Header Block fields, sequence
//! number, operational mode, product version, spot blank flag, block
//! offsets, generation time, all 60 raw halfwords (`level3_halfwords`), the
//! Table V product dependent values by name (`level3_<name>`, see
//! [`crate::params`]), the generic product description (`level3_generic_*`)
//! and the text of the product: symbology text packets
//! (`level3_symbology_text`), graphic pages (`level3_graphic_pages`),
//! tabular pages (`level3_tabular_pages`) or radar coded message records
//! (`level3_radar_coded_message`),
//! the second Message Header and Product Description Blocks of a tabular
//! block that has them (`level3_tabular_message_header`: code, date, time,
//! length, source, destination, blocks; `level3_tabular_halfwords`), and
//! every display packet that is not a data array, with its position, colour
//! and values, as text records (`level3_display_packets`,
//! [`crate::records`]). `level3_packet_codes` lists every symbology packet
//! code in order, and the decoded [`Level3Product`] holds the packets
//! typed. `provenance.source_format` is
//! [`SourceFormat::NexradLevel3`] and `provenance.compression` names the
//! bzip2 and zlib wrappers that were removed.
//!
//! **Sweep (radial products).** One ray per radial in file order. `azimuth`
//! is the centre of the radial (start angle plus half the angle delta,
//! ICD 2620001 Figures 3-10 and 3-11c); Py-ART's Level III reader reports the
//! start angle instead. The raw start and delta angles are the per-ray
//! variables `level3_start_angle` and `level3_delta_angle`. `elevation` and
//! `fixed_angle` are halfword 30 / 10 for the products whose Table V defines
//! it (the elevation-based products, see [`elevation_deg`]) and NaN for
//! volume, hybrid-scan and accumulation products, which have no elevation;
//! Py-ART reports 0 for those. `elevation_number` is halfword 29 when it is
//! not 0. `time` is the delay from the volume scan start to the elevation
//! start (halfword 50 bits 5-15, [`elevation_delay_s`]) for the products that
//! carry it and 0 otherwise. `range` is uniform: bin `i` of a product with
//! bin size `s` ([`range_bin_size_m`], ICD Table III) spans `[i*s, (i+1)*s)`
//! from the radar, so the first centre is `(first_bin + 0.5) * s`. Windowed
//! products (43-46, 55) hold the radials and bins of their window, so the
//! same rule places them: in the four Severe Weather Analysis products of one
//! KFTG 1994 window (1 km, 250 m and 500 m bins from bins 0 and 50) the data
//! span the same 25-94 km, around the window range of halfword 28, and the
//! radials hold its azimuth (halfword 27). Those products' radials are in
//! scan order: the window's radials from the start of the sweep, then those
//! from its end, which overlap them. Legacy radial products (1993-1995) have
//! 366-368 radials, the last starting past 360 degrees; `azimuth` is taken
//! modulo 360 and `level3_start_angle` keeps the raw angle. Their bin counts
//! times the Table III bin size give the Table III range (products 16, 17,
//! 18, 21, 24, 26 and 29). Super resolution bin 8 (2125 m) is Level II gate 0.
//! Py-ART instead scales bins by the packet's display scale factor (999 for
//! a 0.5 degree cut) and starts at 0.
//!
//! **Sweep (generic products, packet 28).** One sweep per radial component:
//! azimuth is the leading edge plus half the width (`level3_width` keeps the
//! width), elevation is the radial's own, `range` comes from the
//! component's bin size and range to the first bin. Radials shorter than the
//! longest are padded with the fill level. `level3_bin_count` is the bin
//! count each radial declares; values a radial stores after its declared
//! bins (none in the real products) are kept as the contiguous ragged
//! array `level3_surplus_count` / `level3_surplus_values`.
//!
//! **Sweep (geographic rasters).** FM301 has no Cartesian grid, so a raster
//! is carried as one sweep with `sweep_mode` `Other("raster")`: one ray per
//! image row from the north, NaN azimuth and elevation, `range` holding the
//! east-west offset of each column centre from the radar (negative west) and
//! the per-ray variable `y` the north-south offset of the row (`extra_vars`,
//! metres, positive north). The image is centred on the radar; the cell size
//! is the product's ([`raster_cell_size_m`], ICD Table III; product 87 from
//! its halfword 50). The sweep's `other` attributes keep the packet header.
//!
//! **Sweep (HRAP arrays: packet 17 of product 81, packet 18 of products 81
//! and 82).** Rows from the north, columns from the west, on the polar
//! stereographic HRAP grid ([`crate::hrap`]): `range` and `y` are the
//! projection coordinates (metres, true at 60N) of the box centres relative
//! to the radar, and the 2-D variables `latitude` and `longitude` give every
//! box centre on the 6371.2 km sphere. The sweep attributes name the
//! projection (`grid_mapping_name = polar_stereographic`,
//! `straight_vertical_longitude_from_pole`, `standard_parallel`,
//! `earth_radius`) and the array's HRAP corner.
//!
//! **Sweep (cross sections 50-52, 85, 86).** `sweep_mode`
//! `Other("vertical_cross_section")`: one ray per image row from the top,
//! `range` the distance along the section from its first end point (0.54 nmi
//! = 1 km cells, ICD 2620003AE section 14.2.3), `y` the height of the row
//! centre above the radar (0.27 nmi = 500 m rows). The end points are the
//! Table V values `level3_point1_*` and `level3_point2_*`. Real products 50
//! (KLOT 1994-10-31) and 51 (KMLB 1994-11-16) check this: their own axis
//! annotation gives the same cell sizes within 1 %, their end point labels
//! the Table V end points, and their length is the distance between those;
//! no product 52 (spectrum width, DSI-7000 Table III), 85 or 86 was found.
//!
//! **Sweep (Weak Echo Region, 53; observed).** No ICD revision obtained
//! describes the product beyond its request parameters (2620001H Table X,
//! DSI-7000 Tables IIa and V: window azimuth and range, elevation bit map)
//! and its Table III row (DSI-7000: 0.54 x 0.54 nmi raster). Each of its rasters is one
//! elevation slice of a window of 0.54 nmi cells drawn in oblique
//! projection: 50 rows of 101 columns, raster row `r` (from the top) is
//! window row `r` from the north, shifted right by `49 - r` columns, and the
//! product's axes label both window sides 0-25 nmi. Shifted back, raster
//! cell `(r, c)` is window column `d = c - (49 - r)`. The data of a slice
//! does not fill a 50 x 50 square: in the 29 distinct products 53 of the
//! NCEI days of 1993-1994 its nonzero cells span 51 consecutive columns at
//! most, `d` from -1 to 49 in two products, from 0 or 1 to 50 in five, and
//! inside 0-49 in the rest. Each slice is one `Other("raster")` sweep of the
//! window, north up, whose columns are `d` from -1 to 50 (`rows`), or
//! further when a nonzero cell lies outside them, so that every nonzero
//! cell of the raster is in the sweep (`level3_window_first_column` is the
//! first `d`); the columns `d` from 0 to 49 are centred at the azimuth and
//! range of halfwords 27-28 (0.1 degree, 0.1 nmi). `fixed_angle` is the
//! slice's `DEG` label (packet 1 text beside the raster, kept as
//! `level3_slice_labels`). Three KCAE
//! 1994-06-29 products fix this: their lowest slice matches the 0.5 degree
//! Base Reflectivity of the same volume best (correlation 0.66-0.92) with
//! this placement, of 36 rotations and 1 nmi shifts. Halfwords 27-28 are
//! zero in a KLOT 1994-11-06 product (storm ID halfword 48 `NS`): its window
//! is at the radar, where its lowest slice matches the base reflectivity of
//! its volume best of every centre within 100 nmi (1 nmi steps). Higher
//! slices match best 0.5-1 nmi further along the storms' motion, as if they
//! were moved to the time of the lowest; the sweeps are not moved.
//!
//! **Sweep (quasi-vertical profiles 189-192; no real sample).** `sweep_mode`
//! `Other("quasi_vertical_profile")`: the raster is transposed so that each
//! ray is one column (one volume scan, oldest first) and `range` is the
//! height of each 20 m cell (ICD 2620001AD Table III) above the radar, from
//! the bottom row.
//!
//! **Field.** Named by [`field_name`]: the FM301 moment name for base
//! moments (DBZH, VRADH, WRADH, ZDR, RHOHV, KDP, PHIDP), REC for hydrometeor
//! classifications, RR for precipitation rates (product 176 and the rate
//! arrays of packet 18), else the ICD mnemonic (`CR`, `ET`, `DVL`, `OHA`, ...).
//! `long_name` is the product name; `units` come from [`DataLevels::units`]
//! or the ICD threshold units; `attrs.other` keeps `product_code`,
//! `product_mnemonic`, the packet code and layer, and for 16-level products
//! the display label of every level (`level3_threshold_labels`).
//!
//! Data levels always stay in their stored width: `u8` for radial and
//! raster packets, `u16` for generic packets. The coding follows the
//! product's encoding ([`LevelEncoding`]):
//!
//! - linear encodings ([`LevelEncoding::Linear`],
//!   [`LevelEncoding::ScaleOffset`], [`LevelEncoding::Edr`]) get a CF
//!   packing;
//! - categorical products ([`LevelEncoding::Classes`] and 16-level products
//!   whose levels are all classes) keep their levels as discrete values with
//!   `flag_values` / `flag_meanings`;
//! - 16-level threshold tables, high resolution VIL and enhanced echo tops
//!   get a [`LevelTable`] transform (`LevelTable::Sixteen`,
//!   `LevelTable::LinearLog`, `LevelTable::Masked`), so every level keeps its
//!   code: range folded stays the range-folded flag and a topped echo top
//!   keeps bit 0x80, described by `flag_masks`.
//!
//! In every encoding level 0 "below threshold" (or "no accumulation") is
//! `_Undetect` and `_FillValue`, "no data", "missing", "blank" and "outside
//! coverage" levels are `_FillValue`, "range folded" is the range-folded flag,
//! the other named levels (flagged, bad, reserved, edited, chaff, a second
//! blank level) are `flag_values` with their meanings, and `valid_range`
//! covers the value levels. The FM301 view writes a [`LevelTable`] field
//! decoded, with its codes beside it as `<name>_level`.
//!
//! Physical values equal [`DataLevels::values`] (within one `f32` rounding
//! step for the `REAL*4` scale and offset products, whose packing evaluates
//! `(raw - offset) / scale` in `f32` like Level II) and MetPy 1.7.1
//! `map_data` for every product it maps (`tests/volume.rs`), with the
//! documented exceptions in [`crate::levels`].

use std::borrow::Cow;

use recast_radar_core::bounded_read::check_sweep_count;
use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldAttrs, FieldData, FieldName, FloatWidth,
    GateMapping, IntCoding, LevelTable, LinearTransform, PackedInt, RangeCoord, Scalar,
    SourceFormat, Sweep, SweepMode, Volume,
};

use crate::blocks::TabularLayout;
use crate::header::{OperationalMode, ProductDescription};
use crate::hrap::{self, LocalGrid};
use crate::levels::{DataLevels, Level, LevelEncoding, LevelFlag};
use crate::packets::generic::{GenericComponent, GenericPacket, GenericRadialComponent};
use crate::packets::irm::IrmPacket;
use crate::packets::raster::{DigitalPrecipPacket, RasterGrid, RasterHeader, RasterPacket};
use crate::packets::symbols::SymbolPacket;
use crate::params::ParameterValue;
use crate::rcm;
use crate::{
    Level3Error, Level3Product, Packet, ProductKind, RadialPacket, decode_product, product_info,
};

/// Decodes one Level III product file into an FM301 volume.
///
/// # Errors
///
/// The errors of [`decode_product`], plus [`Level3Error::NoDataArray`] for a
/// product without a radial, raster or generic data array and
/// [`Level3Error::UnknownGeometry`] for a data array whose bin or cell size
/// the ICD does not define for the product.
pub fn read_level3_volume(bytes: &[u8]) -> Result<Volume, Level3Error> {
    decode_product(bytes)?.to_volume()
}

/// One data array of a product's Product Symbology Block
/// ([`Level3Product::data_arrays`]).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum DataArray<'a> {
    /// A radial packet (16, 0xAF1F).
    Radial {
        /// Symbology layer, from 0.
        layer: usize,
        /// The packet.
        packet: &'a RadialPacket,
    },
    /// One radial component of a generic packet (28).
    GenericRadial {
        /// Symbology layer, from 0.
        layer: usize,
        /// The packet.
        packet: &'a GenericPacket,
        /// Index of the component in [`GenericPacket::components`].
        component_index: usize,
        /// The component.
        component: &'a GenericRadialComponent,
    },
    /// A raster packet (0xBA07, 0xBA0F, 18, 33).
    Raster {
        /// Symbology layer, from 0.
        layer: usize,
        /// The packet.
        packet: &'a RasterPacket,
    },
    /// The digital precipitation array (17) of product 81.
    DigitalPrecip {
        /// Symbology layer, from 0.
        layer: usize,
        /// The packet.
        packet: &'a DigitalPrecipPacket,
    },
}

/// Most bytes the sweeps [`Level3Product::to_volume`] builds may hold, by an
/// upper estimate made before any sweep is built: one byte per radial or
/// raster cell (four per generic bin), 16 more per HRAP box for its latitude
/// and longitude, and [`VOLUME_BYTES_PER_RAY`] per ray. The largest ICD
/// product (720 x 1840 bins) needs 1.4 MB; a product decoded within
/// [`crate::MAX_PRODUCT_DECODED_BYTES`] can need several times that as a
/// volume (converting a 367-byte bzip2 product of 1 800 hourly
/// precipitation arrays allocated 571 MB at peak before this limit), so the
/// conversion is bounded separately. The volume also holds at most
/// [`MAX_SWEEPS_PER_VOLUME`](recast_radar_core::bounded_read::MAX_SWEEPS_PER_VOLUME)
/// sweeps, one per data array.
pub const MAX_VOLUME_BYTES: usize = 64 << 20;

/// Bytes [`MAX_VOLUME_BYTES`] counts per ray: its time (8 bytes), azimuth,
/// elevation, width and row coordinate (4 each), with room for the per-ray
/// variables of generic components.
pub const VOLUME_BYTES_PER_RAY: usize = 64;

/// Largest side of an HRAP array [`Level3Product::to_volume`] places on the
/// grid (the ICD arrays are 131 and 13 boxes).
pub const MAX_HRAP_BOXES: usize = 1024;

impl DataArray<'_> {
    /// Number of cells of the array (for a generic component, radials times
    /// its longest radial).
    pub fn cells(&self) -> usize {
        match self {
            Self::Radial { packet, .. } => packet.levels.len(),
            Self::GenericRadial { component, .. } => {
                component.radials.len().saturating_mul(component.num_bins())
            }
            Self::Raster { packet, .. } => packet.grid.levels().len(),
            Self::DigitalPrecip { packet, .. } => packet.grid.levels().len(),
        }
    }

    /// The packet code of the array.
    pub fn packet_code(&self) -> u16 {
        match self {
            Self::Radial { packet, .. } => packet.code,
            Self::GenericRadial { packet, .. } => packet.code,
            Self::Raster { packet, .. } => packet.code,
            Self::DigitalPrecip { packet, .. } => packet.code,
        }
    }

    /// The symbology layer of the array, from 0.
    pub fn layer(&self) -> usize {
        match *self {
            Self::Radial { layer, .. }
            | Self::GenericRadial { layer, .. }
            | Self::Raster { layer, .. }
            | Self::DigitalPrecip { layer, .. } => layer,
        }
    }
}

impl Level3Product {
    /// Every data array of the Product Symbology Block in symbology order
    /// (layer, then packet; the radial components of a generic packet in
    /// component order). Generic grid components are not listed (see the
    /// [module documentation](self)): they stay in their packet.
    pub fn data_arrays(&self) -> Vec<DataArray<'_>> {
        let mut arrays = Vec::new();
        let layers = self.symbology.iter().flat_map(|sym| sym.layers.iter());
        for (layer, packets) in layers.enumerate() {
            for packet in packets {
                match packet {
                    Packet::Radial(packet) => arrays.push(DataArray::Radial { layer, packet }),
                    Packet::Generic(packet) => {
                        for (component_index, component) in packet.components.iter().enumerate() {
                            if let GenericComponent::Radial(component) = component {
                                arrays.push(DataArray::GenericRadial {
                                    layer,
                                    packet,
                                    component_index,
                                    component,
                                });
                            }
                        }
                    }
                    Packet::Raster(packet) => arrays.push(DataArray::Raster { layer, packet }),
                    Packet::DigitalPrecip(packet) => {
                        arrays.push(DataArray::DigitalPrecip { layer, packet });
                    }
                    _ => {}
                }
            }
        }
        arrays
    }

    /// Converts every data array of the product into one sweep of an FM301
    /// volume (see the [module documentation](self)).
    ///
    /// # Errors
    ///
    /// [`Level3Error::NoDataArray`] when the symbology block holds no data
    /// array, [`Level3Error::UnknownGeometry`] when the ICD gives no bin or
    /// cell size for the product, [`Level3Error::InvalidMessage`] when the
    /// arrays would exceed [`MAX_VOLUME_BYTES`] or
    /// [`MAX_SWEEPS_PER_VOLUME`](recast_radar_core::bounded_read::MAX_SWEEPS_PER_VOLUME),
    /// [`Level3Error::ProductTooLarge`] when a radar coded message's text
    /// would need more than [`crate::rcm::MAX_RCM_PARSED_BYTES`] to parse.
    pub fn to_volume(&self) -> Result<Volume, Level3Error> {
        let code = self.description.product_code;
        let arrays = self.data_arrays();
        if arrays.is_empty() {
            return self.radar_coded_message_volume();
        }
        check_sweep_count(arrays.len(), "Level III data arrays")
            .map_err(|reason| Level3Error::InvalidMessage { code, reason })?;
        let bytes = arrays
            .iter()
            .map(|array| self.volume_bytes(array))
            .fold(0, usize::saturating_add);
        if bytes > MAX_VOLUME_BYTES {
            return Err(Level3Error::InvalidMessage {
                code,
                reason: format!(
                    "{} data arrays need up to {bytes} bytes as a volume, more than the \
                     {MAX_VOLUME_BYTES}-byte limit",
                    arrays.len()
                ),
            });
        }
        let mut volume = self.volume_frame()?;
        let mut rays = 0;
        for (index, array) in arrays.iter().enumerate() {
            let mut sweep = self.array_sweep(array)?;
            sweep.sweep_number = u32::try_from(index).unwrap_or(u32::MAX);
            sweep.other.push((
                "level3_layer".into(),
                AttrValue::Scalar(Scalar::U32(
                    u32::try_from(array.layer()).unwrap_or(u32::MAX),
                )),
            ));
            rays += sweep.nrays();
            volume.sweeps.push(sweep);
        }
        volume.provenance.decode.decoded_ray_count = rays;
        volume.seal().map_err(|err| Level3Error::InvalidMessage {
            code,
            reason: format!("data array: {err}"),
        })?;
        volume.time_coverage = volume.ray_time_extent();
        Ok(volume)
    }

    /// The volume of a Radar Coded Message: its intensity grid as one sweep
    /// ([`Level3Error::NoDataArray`] for any other product without a data
    /// array, and for a message without Part A or with the radar down).
    fn radar_coded_message_volume(&self) -> Result<Volume, Level3Error> {
        let code = self.description.product_code;
        let part_a = self
            .radar_coded_message()?
            .and_then(|message| message.part_a)
            .filter(|part_a| !part_a.radar_down)
            .ok_or(Level3Error::NoDataArray { code })?;
        let mut volume = self.volume_frame()?;
        let mut sweep = self.intensity_grid_sweep(0, part_a.intensity_grid())?;
        sweep.sweep_number = 0;
        volume.provenance.decode.decoded_ray_count = sweep.nrays();
        volume.sweeps.push(sweep);
        volume.seal().map_err(|err| Level3Error::InvalidMessage {
            code,
            reason: format!("radar coded message grid: {err}"),
        })?;
        volume.time_coverage = volume.ray_time_extent();
        Ok(volume)
    }

    /// The sweep of a Radar Coded Message's Part A intensity grid (100 x 100
    /// fine boxes of the 1/16 LFM grid, [`LocalGrid::radar_coded_message`]),
    /// row-major from the north-west: from the message's text (product 74,
    /// `packet_code` 0) or from packet 32 of product 83.
    fn intensity_grid_sweep(&self, packet_code: u16, grid: Vec<u8>) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let n = usize::from(rcm::FINE_BOXES);
        if grid.len() != n * n {
            return Err(Level3Error::InvalidMessage {
                code: desc.product_code,
                reason: format!(
                    "intensity grid of {} boxes; the 1/16 LFM grid has {n} x {n}",
                    grid.len()
                ),
            });
        }
        let local = LocalGrid::radar_coded_message(desc.latitude_deg, desc.longitude_deg);
        let mut sweep = raster_frame(SweepMode::Other("raster".into()), n);
        place_on_national_grid(&mut sweep, desc, &local, n, n, "fine (1/16 LFM) box")?;
        let levels = DataLevels::radar_coded_message();
        let mut field = u8_field(
            desc,
            field_name(desc.product_code),
            packet_code,
            Some(&levels),
            u32::from(rcm::FINE_BOXES),
            grid,
        );
        if packet_code == 0 {
            field
                .attrs
                .other
                .retain(|(name, _)| &**name != "level3_packet_code");
        }
        field.attrs.long_name = Some(Cow::Borrowed(
            "Radar Coded Message reflectivity intensity (1/16 LFM grid)",
        ));
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// The sweep of one data array.
    fn array_sweep(&self, array: &DataArray<'_>) -> Result<Sweep, Level3Error> {
        let code = self.description.product_code;
        match *array {
            DataArray::Radial { packet, .. } => self.radial_sweep(packet),
            DataArray::GenericRadial {
                packet,
                component_index,
                component,
                ..
            } => self.generic_sweep(packet, component_index, component),
            DataArray::DigitalPrecip { packet, .. } => self.hrap_sweep(
                packet.code,
                &packet.grid,
                packet.spares,
                LocalGrid::dpa(
                    self.description.latitude_deg,
                    self.description.longitude_deg,
                ),
            ),
            DataArray::Raster { packet, .. } => match (packet.code, code) {
                (18, _) => self.hrap_sweep(
                    packet.code,
                    &packet.grid,
                    match packet.header {
                        RasterHeader::PrecipitationRate { spares } => spares,
                        _ => [0; 2],
                    },
                    LocalGrid::rate_array(
                        self.description.latitude_deg,
                        self.description.longitude_deg,
                    ),
                ),
                (32, _) => self.intensity_grid_sweep(32, packet.grid.levels().to_vec()),
                (_, 50..=52 | 85 | 86) => self.cross_section_sweep(packet),
                (_, 53) => self.weak_echo_region_sweep(packet),
                (_, 189..=192) => self.qvp_sweep(packet),
                _ => self.raster_sweep(packet),
            },
        }
    }

    /// An upper estimate of the bytes the sweep of `array` holds (see
    /// [`MAX_VOLUME_BYTES`]).
    fn volume_bytes(&self, array: &DataArray<'_>) -> usize {
        let per_ray = |rays: usize| rays.saturating_mul(VOLUME_BYTES_PER_RAY);
        let cells = array.cells();
        match *array {
            DataArray::Radial { packet, .. } => cells.saturating_add(per_ray(packet.radials.len())),
            DataArray::GenericRadial { component, .. } => {
                // Bins as 32-bit integers, and per ray its attributes and the
                // values past its bins.
                let rays = component.radials.iter().fold(0usize, |sum, radial| {
                    sum.saturating_add(radial.attributes.len())
                        .saturating_add(radial.values.len().saturating_mul(4))
                });
                cells
                    .saturating_mul(4)
                    .saturating_add(rays)
                    .saturating_add(per_ray(component.radials.len()))
            }
            DataArray::DigitalPrecip { packet, .. } => hrap_bytes(&packet.grid),
            DataArray::Raster { packet, .. } => {
                let grid = &packet.grid;
                match (packet.code, self.description.product_code) {
                    (18, _) => hrap_bytes(grid),
                    // The oblique window is up to `rows` columns wider.
                    (_, 53) => cells
                        .saturating_add(grid.rows().saturating_mul(grid.rows()))
                        .saturating_add(per_ray(grid.rows())),
                    // Rays are rows, or columns (QVP).
                    _ => cells.saturating_add(per_ray(grid.rows().max(grid.columns()))),
                }
            }
        }
    }

    /// The volume with everything but its sweeps.
    fn volume_frame(&self) -> Result<Volume, Level3Error> {
        let desc = &self.description;
        let code = desc.product_code;
        let info = product_info(code);
        let instrument = self
            .generic_radar_name()
            .or_else(|| self.awips_site())
            .unwrap_or_default();
        let mut volume = Volume::new(instrument, desc.volume_scan_time);
        volume.volume_number = Some(i32::from(desc.volume_scan_number));
        volume.location.latitude_deg = Some(desc.latitude_deg);
        volume.location.longitude_deg = Some(desc.longitude_deg);
        volume.location.altitude_m = Some(f64::from(desc.height_ft) * 0.3048);
        volume.scan.name = Some(format!("VCP-{}", desc.vcp));
        volume.scan.id = Some(i64::from(desc.vcp));
        volume.scan.vcp_pattern = Some(desc.vcp);
        let tdwr = (180..=187).contains(&code);
        volume.attrs.source = Some(if tdwr {
            "TDWR Level III".to_owned()
        } else {
            "NEXRAD Level III".to_owned()
        });
        volume.attrs.title = info.map(|info| info.name.to_owned());

        let text = |value: &str| AttrValue::text(value);
        let other = &mut volume.attrs.other;
        other.push(("product_code".into(), AttrValue::Scalar(Scalar::I16(code))));
        if let Some(mnemonic) = info.map(|info| info.mnemonic).filter(|m| !m.is_empty()) {
            other.push(("product_mnemonic".into(), text(mnemonic)));
        }
        if let Some(header) = &self.text_header {
            if let Some(awips) = &header.awips_id {
                other.push(("awips_id".into(), text(awips)));
            }
            other.push(("wmo_heading".into(), text(&header.wmo_heading)));
            other.push(("wmo_data_designator".into(), text(&header.data_designator)));
            other.push(("wmo_originator".into(), text(&header.originator)));
            other.push(("wmo_day_time".into(), text(&header.day_time)));
            if let Some(indicator) = &header.indicator {
                other.push(("wmo_indicator".into(), text(indicator)));
            }
            if let Some(sequence) = &header.noaaport_sequence {
                other.push(("noaaport_sequence".into(), text(sequence)));
            }
            if header.zlib_frames > 0 {
                other.push((
                    "zlib_frames".into(),
                    AttrValue::Scalar(Scalar::U32(header.zlib_frames)),
                ));
            }
            if let Some(ccb) = &header.communications_control_block {
                other.push((
                    "noaaport_communications_control_block".into(),
                    AttrValue::Array(ArrayBuf::U8(ccb.clone())),
                ));
            }
            if let Some(heading) = &header.zlib_wmo_heading {
                other.push(("zlib_wmo_heading".into(), text(heading)));
            }
            if let Some(awips) = &header.zlib_awips_id {
                other.push(("zlib_awips_id".into(), text(awips)));
            }
        }
        let message = &self.message_header;
        other.push((
            "source_id".into(),
            AttrValue::Scalar(Scalar::I16(message.source_id)),
        ));
        other.push((
            "destination_id".into(),
            AttrValue::Scalar(Scalar::I16(message.destination_id)),
        ));
        other.push((
            "message_length".into(),
            AttrValue::Scalar(Scalar::U32(message.length)),
        ));
        other.push((
            "number_of_blocks".into(),
            AttrValue::Scalar(Scalar::U16(message.num_blocks)),
        ));
        other.push((
            "sequence_number".into(),
            AttrValue::Scalar(Scalar::I16(desc.sequence_number)),
        ));
        other.push((
            "operational_mode".into(),
            text(match desc.mode() {
                OperationalMode::Maintenance => "maintenance",
                OperationalMode::ClearAir => "clear_air",
                OperationalMode::Precipitation => "precipitation",
                OperationalMode::Other(_) => "unknown",
            }),
        ));
        other.push((
            "product_version".into(),
            AttrValue::Scalar(Scalar::U8(desc.version)),
        ));
        other.push((
            "spot_blank".into(),
            AttrValue::Scalar(Scalar::U8(desc.spot_blank)),
        ));
        for (name, offset) in [
            ("symbology_offset", desc.symbology_offset),
            ("graphic_offset", desc.graphic_offset),
            ("tabular_offset", desc.tabular_offset),
        ] {
            other.push((name.into(), AttrValue::Scalar(Scalar::U32(offset))));
        }
        other.push((
            "generation_time".into(),
            text(&desc.generation_time.to_rfc3339()),
        ));
        if let Some(time) = message.datetime() {
            other.push(("message_time".into(), text(&time.to_rfc3339())));
        }
        other.push((
            "level3_halfwords".into(),
            AttrValue::Array(ArrayBuf::U16(desc.halfwords.to_vec())),
        ));
        for parameter in desc.parameters() {
            let value = match parameter.value {
                ParameterValue::Int(v) => AttrValue::Scalar(Scalar::I64(v)),
                ParameterValue::Float(v) => AttrValue::Scalar(Scalar::F64(v)),
                ParameterValue::Time(t) => text(&t.to_rfc3339()),
                ParameterValue::Date(d) => text(&d),
                ParameterValue::Text(t) => text(t),
                ParameterValue::Characters(t) => text(&t),
            };
            other.push((format!("level3_{}", parameter.name).into(), value));
        }
        self.push_generic_attrs(other);
        self.push_irm_attrs(other);
        self.push_text_attrs(other);
        let records = self.display_records()?;
        if !records.is_empty() {
            other.push((
                "level3_display_packets".into(),
                AttrValue::Array(ArrayBuf::Text(
                    records.into_iter().map(String::into_boxed_str).collect(),
                )),
            ));
        }

        volume.provenance.source_format = SourceFormat::NexradLevel3;
        let zlib = self.text_header.as_ref().is_some_and(|h| h.zlib_frames > 0);
        volume.provenance.compression = Some(
            match (zlib, desc.compressed) {
                (true, true) => "zlib+bzip2",
                (true, false) => "zlib",
                (false, true) => "bzip2",
                (false, false) => "uncompressed",
            }
            .to_owned(),
        );
        volume.provenance.decode.message_count = 1;
        Ok(volume)
    }

    /// The Product Description data structure of the first generic packet
    /// (Figure E-1) as `level3_generic_*` attributes.
    fn push_generic_attrs(&self, other: &mut Vec<(Box<str>, AttrValue)>) {
        let Some(generic) = self
            .symbology
            .iter()
            .flat_map(|sym| sym.layers.iter().flatten())
            .find_map(|packet| match packet {
                Packet::Generic(generic) => Some(generic),
                _ => None,
            })
        else {
            return;
        };
        let p = &generic.product;
        let text = |value: &str| AttrValue::text(value);
        let int = |value: i32| AttrValue::Scalar(Scalar::I32(value));
        let unsigned = |value: u32| AttrValue::Scalar(Scalar::U32(value));
        let float = |value: f32| AttrValue::Scalar(Scalar::F32(value));
        for (name, value) in [
            ("level3_generic_name", text(&p.name)),
            ("level3_generic_description", text(&p.description)),
            ("level3_generic_product_code", int(p.product_code)),
            ("level3_generic_product_type", int(p.product_type)),
            (
                "level3_generic_generation_time",
                unsigned(p.generation_time),
            ),
            ("level3_generic_radar_name", text(&p.radar_name)),
            ("level3_generic_radar_latitude", float(p.radar_latitude)),
            ("level3_generic_radar_longitude", float(p.radar_longitude)),
            ("level3_generic_radar_height", float(p.radar_height)),
            ("level3_generic_volume_time", unsigned(p.volume_time)),
            ("level3_generic_elevation_time", unsigned(p.elevation_time)),
            ("level3_generic_elevation_angle", float(p.elevation_angle)),
            ("level3_generic_volume_number", int(p.volume_number)),
            ("level3_generic_operational_mode", int(p.operational_mode)),
            ("level3_generic_vcp", int(p.vcp)),
            ("level3_generic_elevation_number", int(p.elevation_number)),
            ("level3_generic_compression", int(p.compression)),
            ("level3_generic_uncompressed_size", int(p.uncompressed_size)),
        ] {
            other.push((name.into(), value));
        }
        push_parameters(other, "level3_generic_parameters", &p.parameters);
        if let Some(spares) = p.external_spares {
            other.push((
                "level3_generic_external_spares".into(),
                AttrValue::Array(ArrayBuf::I32(spares.to_vec())),
            ));
        }
        let texts: Vec<Box<str>> = generic
            .components
            .iter()
            .filter_map(|component| match component {
                GenericComponent::Text { text, .. } => Some(text.as_str().into()),
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            other.push((
                "level3_generic_text_components".into(),
                AttrValue::Array(ArrayBuf::Text(texts)),
            ));
        }
    }

    /// Packets 30 and 31 of an unedited Radar Coded Message (product 83)
    /// as `level3_irm_parameters` and `level3_irm_storm_count`.
    fn push_irm_attrs(&self, other: &mut Vec<(Box<str>, AttrValue)>) {
        for packet in self
            .symbology
            .iter()
            .flat_map(|sym| sym.layers.iter().flatten())
        {
            match packet {
                Packet::Irm(IrmPacket::Parameters { values }) => other.push((
                    "level3_irm_parameters".into(),
                    AttrValue::Array(ArrayBuf::F32(values.to_vec())),
                )),
                Packet::Irm(IrmPacket::StormCount { count }) => other.push((
                    "level3_irm_storm_count".into(),
                    AttrValue::Scalar(Scalar::U16(*count)),
                )),
                _ => {}
            }
        }
    }

    /// The text of the product and the packet codes of its symbology block.
    fn push_text_attrs(&self, other: &mut Vec<(Box<str>, AttrValue)>) {
        let mut codes = Vec::new();
        let mut symbology_text: Vec<Box<str>> = Vec::new();
        for packet in self
            .symbology
            .iter()
            .flat_map(|sym| sym.layers.iter().flatten())
        {
            collect_text(packet, &mut codes, &mut symbology_text);
        }
        other.push((
            "level3_packet_codes".into(),
            AttrValue::Array(ArrayBuf::U16(codes)),
        ));
        if !symbology_text.is_empty() {
            other.push((
                "level3_symbology_text".into(),
                AttrValue::Array(ArrayBuf::Text(symbology_text)),
            ));
        }
        if let Some(graphic) = &self.graphic {
            let pages: Vec<Box<str>> = graphic
                .pages
                .iter()
                .map(|page| {
                    let mut lines = Vec::new();
                    let mut ignored = Vec::new();
                    for packet in &page.packets {
                        collect_text(packet, &mut ignored, &mut lines);
                    }
                    lines.join("\n").into()
                })
                .collect();
            other.push((
                "level3_graphic_pages".into(),
                AttrValue::Array(ArrayBuf::Text(pages)),
            ));
        }
        if let Some(tabular) = &self.tabular {
            let pages: Vec<Box<str>> = tabular
                .pages
                .iter()
                .map(|page| page.lines.join("\n").into())
                .collect();
            let name = match tabular.layout {
                TabularLayout::RadarCodedMessage => "level3_radar_coded_message",
                _ => "level3_tabular_pages",
            };
            other.push((name.into(), AttrValue::Array(ArrayBuf::Text(pages))));
            if let Some(header) = &tabular.message_header {
                other.push((
                    "level3_tabular_message_header".into(),
                    AttrValue::Array(ArrayBuf::I64(vec![
                        i64::from(header.code),
                        i64::from(header.date),
                        i64::from(header.time),
                        i64::from(header.length),
                        i64::from(header.source_id),
                        i64::from(header.destination_id),
                        i64::from(header.num_blocks),
                    ])),
                ));
            }
            if let Some(description) = &tabular.description {
                other.push((
                    "level3_tabular_halfwords".into(),
                    AttrValue::Array(ArrayBuf::U16(description.halfwords.to_vec())),
                ));
            }
        }
    }

    /// The radar name of a generic product, when the packet has one.
    fn generic_radar_name(&self) -> Option<String> {
        self.symbology
            .iter()
            .flat_map(|sym| sym.layers.iter().flatten())
            .find_map(|packet| match packet {
                Packet::Generic(generic) => Some(generic.product.radar_name.trim()),
                _ => None,
            })
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    /// The site of the AWIPS identifier (`N0QTLX` -> `TLX`).
    fn awips_site(&self) -> Option<String> {
        let id = self.text_header.as_ref()?.awips_id.as_deref()?;
        (id.len() == 6).then(|| id[3..].to_owned())
    }

    /// A sweep from a radial packet (16, 0xAF1F).
    fn radial_sweep(&self, radial: &RadialPacket) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let code = desc.product_code;
        let bin_size =
            range_bin_size_m(code, radial.num_bins).ok_or(Level3Error::UnknownGeometry {
                code,
                what: "range bin size",
            })?;
        let elevation = elevation_deg(desc).unwrap_or(f32::NAN);
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, elevation);
        sweep.elevation_number = (desc.elevation_number != 0).then_some(desc.elevation_number);
        let time_s = f64::from(elevation_delay_s(desc).unwrap_or(0));
        sweep.reserve_rays(radial.radials.len());
        for r in &radial.radials {
            let azimuth = (r.start_angle_deg() + 0.5 * r.delta_angle_deg()).rem_euclid(360.0);
            sweep.push_ray(time_s, azimuth, elevation);
        }
        set_angle_resolution(
            &mut sweep,
            radial
                .radials
                .iter()
                .map(|r| (r.start_angle_deg(), r.delta_angle_deg())),
        );
        sweep.range = RangeCoord::Uniform {
            first_center_m: (f64::from(radial.first_bin) + 0.5) * bin_size,
            spacing_m: bin_size,
            ngates: u32::from(radial.num_bins),
        };
        let nrays = u32::try_from(radial.radials.len()).unwrap_or(u32::MAX);
        for (name, long_name, values) in [
            (
                "level3_start_angle",
                "radial start angle",
                radial.radials.iter().map(|r| r.start_angle_deg()).collect(),
            ),
            (
                "level3_delta_angle",
                "radial angle delta",
                radial.radials.iter().map(|r| r.delta_angle_deg()).collect(),
            ),
        ] {
            sweep
                .extra_vars
                .push(per_ray_f32(name, long_name, "degree", nrays, values));
        }
        for (name, value) in [
            ("level3_first_bin", Scalar::U16(radial.first_bin)),
            ("level3_i_center", Scalar::I16(radial.i_center)),
            ("level3_j_center", Scalar::I16(radial.j_center)),
            (
                "level3_range_scale_factor",
                Scalar::U16(radial.scale_factor),
            ),
        ] {
            sweep.other.push((name.into(), AttrValue::Scalar(value)));
        }
        push_supplemental_scan(&mut sweep, desc);

        let levels = DataLevels::for_packet(desc, radial.code);
        let field = u8_field(
            desc,
            field_name(desc.product_code),
            radial.code,
            levels.as_ref(),
            u32::from(radial.num_bins),
            radial.levels.clone(),
        );
        push_field(&mut sweep, code, field)?;
        Ok(sweep)
    }

    /// A sweep from one radial component of a generic packet (28).
    fn generic_sweep(
        &self,
        packet: &GenericPacket,
        component_index: usize,
        component: &GenericRadialComponent,
    ) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let ngates = component.num_bins();
        let ngates_u32 = u32::try_from(ngates).map_err(|_| Level3Error::InvalidPacket {
            code: packet.code,
            reason: format!("{ngates} bins do not fit the range dimension"),
        })?;
        let elevation = elevation_deg(desc).unwrap_or(f32::NAN);
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, elevation);
        sweep.elevation_number = (desc.elevation_number != 0).then_some(desc.elevation_number);
        let time_s = f64::from(elevation_delay_s(desc).unwrap_or(0));
        sweep.reserve_rays(component.radials.len());
        for r in &component.radials {
            let azimuth = (r.azimuth + 0.5 * r.width).rem_euclid(360.0);
            sweep.push_ray(time_s, azimuth, r.elevation);
        }
        set_angle_resolution(
            &mut sweep,
            component.radials.iter().map(|r| (r.azimuth, r.width)),
        );
        sweep.range = RangeCoord::Uniform {
            first_center_m: f64::from(component.range_to_first_bin),
            spacing_m: f64::from(component.bin_size),
            ngates: ngates_u32,
        };
        let nrays = u32::try_from(component.radials.len()).unwrap_or(u32::MAX);
        sweep.extra_vars.push(per_ray_f32(
            "level3_start_angle",
            "radial azimuth as stored (start of the radial)",
            "degree",
            nrays,
            component.radials.iter().map(|r| r.azimuth).collect(),
        ));
        sweep.extra_vars.push(per_ray_f32(
            "level3_width",
            "radial width",
            "degree",
            nrays,
            component.radials.iter().map(|r| r.width).collect(),
        ));
        sweep.extra_vars.push(ExtraVariable {
            name: "level3_bin_count".into(),
            dims: vec!["time".into()],
            shape: vec![nrays],
            values: ArrayBuf::I32(component.radials.iter().map(|r| r.num_bins).collect()),
            attrs: vec![(
                "long_name".into(),
                AttrValue::text("number of bins the radial declares"),
            )],
        });
        push_surplus_values(&mut sweep, component, packet.code, nrays)?;
        let attributes: Vec<&str> = component
            .radials
            .iter()
            .map(|r| r.attributes.as_str())
            .collect();
        if attributes.windows(2).all(|pair| pair[0] == pair[1]) {
            if let Some(first) = attributes.first().filter(|a| !a.is_empty()) {
                sweep
                    .other
                    .push(("level3_bin_attributes".into(), AttrValue::text(*first)));
            }
        } else {
            sweep.extra_vars.push(ExtraVariable {
                name: "level3_bin_attributes".into(),
                dims: vec!["time".into()],
                shape: vec![nrays],
                values: ArrayBuf::Text(attributes.iter().map(|a| (*a).into()).collect()),
                attrs: Vec::new(),
            });
        }
        sweep.other.push((
            "level3_component_index".into(),
            AttrValue::Scalar(Scalar::U32(
                u32::try_from(component_index).unwrap_or(u32::MAX),
            )),
        ));
        if !component.description.trim().is_empty() {
            sweep.other.push((
                "level3_component_description".into(),
                AttrValue::text(component.description.trim()),
            ));
        }
        push_parameters(
            &mut sweep.other,
            "level3_component_parameters",
            &component.parameters,
        );
        push_supplemental_scan(&mut sweep, desc);

        let levels = DataLevels::for_packet(desc, packet.code);
        let coding = Coding::of(levels.as_ref());
        let fill = coding.fill.unwrap_or(0);
        let all_fit = component
            .radials
            .iter()
            .flat_map(|r| r.bins().iter())
            .all(|&v| u16::try_from(v).is_ok());
        let field = if all_fit {
            let mut values = Vec::with_capacity(component.radials.len().saturating_mul(ngates));
            for radial in &component.radials {
                values.extend(radial.bins().iter().map(|&v| v as u16));
                values.resize(values.len() + (ngates - radial.bins().len()), fill);
            }
            let data = FieldData::U16 {
                values,
                coding: coding.int_coding(),
            };
            coded_field(
                desc,
                field_name(desc.product_code),
                packet.code,
                levels.as_ref(),
                &coding,
                ngates_u32,
                data,
            )
        } else {
            // Stored values outside 0-65535 have no level; keep them as i32.
            let mut values = Vec::with_capacity(component.radials.len().saturating_mul(ngates));
            for radial in &component.radials {
                values.extend_from_slice(radial.bins());
                values.resize(
                    values.len() + (ngates - radial.bins().len()),
                    i32::from(fill),
                );
            }
            let data = FieldData::I32 {
                values,
                coding: coding.int_coding(),
            };
            coded_field(
                desc,
                field_name(desc.product_code),
                packet.code,
                levels.as_ref(),
                &coding,
                ngates_u32,
                data,
            )
        };
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// A sweep from a geographic raster packet (0xBA07, 0xBA0F, 33).
    fn raster_sweep(&self, packet: &RasterPacket) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let code = desc.product_code;
        let cell = match code {
            // Product 87: halfword 50 is the resolution in 0.01 nmi.
            87 => Some(f64::from(desc.halfword(50).unwrap_or_default()) * 0.01 * 1852.0)
                .filter(|cell| *cell > 0.0),
            _ => raster_cell_size_m(code),
        }
        .ok_or(Level3Error::UnknownGeometry {
            code,
            what: "raster cell size",
        })?;
        let grid = &packet.grid;
        let (rows, columns) = (grid.rows(), grid.columns());
        let columns_u32 = dimension(columns, packet.code)?;
        let mut sweep = raster_frame(SweepMode::Other("raster".into()), rows);
        if let Some(elevation) = elevation_deg(desc) {
            sweep.fixed_angle_deg = elevation;
        }
        let half_rows = rows as f64 / 2.0;
        sweep.extra_vars.push(y_variable(
            (0..rows)
                .map(|row| ((half_rows - row as f64 - 0.5) * cell) as f32)
                .collect(),
            "north-south offset of the row centre from the radar",
        ));
        sweep.range = RangeCoord::Uniform {
            first_center_m: (0.5 - columns as f64 / 2.0) * cell,
            spacing_m: cell,
            ngates: columns_u32,
        };
        push_raster_attrs(&mut sweep, grid, &packet.header);
        sweep
            .other
            .push(("raster_cell_m".into(), AttrValue::Scalar(Scalar::F64(cell))));
        let levels = DataLevels::for_packet(desc, packet.code);
        let field = u8_field(
            desc,
            field_name(desc.product_code),
            packet.code,
            levels.as_ref(),
            columns_u32,
            grid.levels().to_vec(),
        );
        push_field(&mut sweep, code, field)?;
        Ok(sweep)
    }

    /// A sweep from an HRAP array: the digital precipitation array (17,
    /// [`LocalGrid::dpa`]) or a precipitation rate array (18,
    /// [`LocalGrid::rate_array`]), with the packet's two spare halfwords.
    fn hrap_sweep(
        &self,
        packet_code: u16,
        grid: &RasterGrid,
        spares: [u16; 2],
        local: LocalGrid,
    ) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let (rows, columns) = (grid.rows(), grid.columns());
        if rows > MAX_HRAP_BOXES || columns > MAX_HRAP_BOXES {
            return Err(Level3Error::InvalidPacket {
                code: packet_code,
                reason: format!(
                    "{rows} x {columns} HRAP boxes exceed the {MAX_HRAP_BOXES}-box limit"
                ),
            });
        }
        dimension(rows, packet_code)?;
        let columns_u32 = dimension(columns, packet_code)?;
        let mut sweep = raster_frame(SweepMode::Other("raster".into()), rows);
        let what = if packet_code == 18 {
            "1/4 LFM box"
        } else {
            "HRAP box"
        };
        place_on_national_grid(&mut sweep, desc, &local, rows, columns, what)?;
        sweep.other.push((
            "raster_spares".into(),
            AttrValue::Array(ArrayBuf::U16(spares.to_vec())),
        ));
        let levels = DataLevels::for_packet(desc, packet_code);
        let name = if packet_code == 18 {
            FieldName::Rr
        } else {
            field_name(desc.product_code)
        };
        let mut field = u8_field(
            desc,
            name,
            packet_code,
            levels.as_ref(),
            columns_u32,
            grid.levels().to_vec(),
        );
        if packet_code == 18 {
            field.attrs.long_name =
                Some(Cow::Borrowed("Precipitation rate (1/4 LFM grid, 8 levels)"));
        }
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// A sweep from a cross section raster (products 50-52, 85, 86).
    fn cross_section_sweep(&self, packet: &RasterPacket) -> Result<Sweep, Level3Error> {
        const HORIZONTAL_M: f64 = 1000.0;
        const VERTICAL_M: f64 = 500.0;
        let desc = &self.description;
        let grid = &packet.grid;
        let (rows, columns) = (grid.rows(), grid.columns());
        let columns_u32 = dimension(columns, packet.code)?;
        let mut sweep = raster_frame(SweepMode::Other("vertical_cross_section".into()), rows);
        sweep.extra_vars.push(y_variable(
            (0..rows)
                .map(|row| ((rows as f64 - row as f64 - 0.5) * VERTICAL_M) as f32)
                .collect(),
            "height of the row centre above the radar",
        ));
        sweep.range = RangeCoord::Uniform {
            first_center_m: 0.5 * HORIZONTAL_M,
            spacing_m: HORIZONTAL_M,
            ngates: columns_u32,
        };
        push_raster_attrs(&mut sweep, grid, &packet.header);
        sweep.other.push((
            "level3_geometry".into(),
            AttrValue::text("0.54 nmi x 0.27 nmi cells (ICD 2620003AE 14.2.3)"),
        ));
        let levels = DataLevels::for_packet(desc, packet.code);
        let field = u8_field(
            desc,
            field_name(desc.product_code),
            packet.code,
            levels.as_ref(),
            columns_u32,
            grid.levels().to_vec(),
        );
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// A sweep from one elevation slice of the Weak Echo Region (product 53):
    /// the raster's rows shifted back into a north-up window of 0.54 nmi
    /// cells centred on halfwords 27-28 (see the module documentation).
    ///
    /// Raster cell `(r, c)` is window cell `(r, d)` with `d = c - (rows - 1 -
    /// r)`. The window keeps the columns `d` from -1 to `rows` (the real
    /// products fill 51 consecutive of them) and more when a nonzero level
    /// lies outside, so that every nonzero raster cell is in the sweep.
    fn weak_echo_region_sweep(&self, packet: &RasterPacket) -> Result<Sweep, Level3Error> {
        const CELL_M: f64 = 0.54 * 1852.0;
        const MAX_WINDOW: usize = 512;
        let desc = &self.description;
        let grid = &packet.grid;
        let (rows, columns) = (grid.rows(), grid.columns());
        if rows > MAX_WINDOW {
            return Err(Level3Error::InvalidPacket {
                code: packet.code,
                reason: format!("{rows} weak echo region rows exceed {MAX_WINDOW}"),
            });
        }
        // Row `r` of the raster holds window row `r` from the north, shifted
        // right by `rows - 1 - r` columns (the display's oblique projection).
        // Both sizes are at most `MAX_GRID_DIMENSION`, so these fit an i64.
        let shift = |row: usize| (rows - 1 - row) as i64;
        let (mut first, mut last) = (-1_i64, rows as i64);
        for row in 0..rows {
            for column in 0..columns {
                if grid.get(row, column).is_some_and(|level| level != 0) {
                    let d = column as i64 - shift(row);
                    first = first.min(d);
                    last = last.max(d);
                }
            }
        }
        let width = usize::try_from(last - first + 1).unwrap_or(0);
        let width_u32 = dimension(width, packet.code)?;
        let mut levels = vec![0u8; rows.saturating_mul(width)];
        if width > 0 {
            for (row, out) in levels.chunks_exact_mut(width).enumerate() {
                for (i, level) in out.iter_mut().enumerate() {
                    let column = first + i as i64 + shift(row);
                    *level = usize::try_from(column)
                        .ok()
                        .and_then(|column| grid.get(row, column))
                        .unwrap_or(0);
                }
            }
        }
        let azimuth = (f64::from(desc.halfword(27).unwrap_or_default()) * 0.1).to_radians();
        let range = f64::from(desc.halfword(28).unwrap_or_default()) * 0.1 * 1852.0;
        let (centre_x, centre_y) = (range * azimuth.sin(), range * azimuth.cos());
        // The centre of columns d = 0 .. rows - 1 and of the rows.
        let middle = (rows as f64 - 1.0) / 2.0;
        let labels = self.slice_labels(&packet.header, rows);
        let elevation = labels
            .iter()
            .find_map(|label| label.trim().strip_suffix("DEG")?.trim().parse::<f32>().ok())
            .unwrap_or(f32::NAN);
        let mut sweep = raster_frame(SweepMode::Other("raster".into()), rows);
        sweep.fixed_angle_deg = elevation;
        sweep.extra_vars.push(y_variable(
            (0..rows)
                .map(|row| (centre_y + (middle - row as f64) * CELL_M) as f32)
                .collect(),
            "north-south offset of the row centre from the radar",
        ));
        sweep.range = RangeCoord::Uniform {
            first_center_m: centre_x + (first as f64 - middle) * CELL_M,
            spacing_m: CELL_M,
            ngates: width_u32,
        };
        push_raster_attrs(&mut sweep, grid, &packet.header);
        sweep.other.push((
            "raster_cell_m".into(),
            AttrValue::Scalar(Scalar::F64(CELL_M)),
        ));
        sweep.other.push((
            "level3_window_first_column".into(),
            AttrValue::Scalar(Scalar::I32(i32::try_from(first).unwrap_or(i32::MIN))),
        ));
        sweep.other.push((
            "level3_geometry".into(),
            AttrValue::text(
                "weak echo region window (observed): 0.54 nmi cells, north up; raster \
                 cell (r, c) is window column d = c - (rows - 1 - r), columns d = \
                 level3_window_first_column onwards; columns d = 0 to rows - 1 are \
                 centred at the azimuth and range of halfwords 27-28",
            ),
        ));
        if !labels.is_empty() {
            sweep.other.push((
                "level3_slice_labels".into(),
                AttrValue::text(labels.join("; ")),
            ));
        }
        let levels_map = DataLevels::for_packet(desc, packet.code);
        let field = u8_field(
            desc,
            field_name(desc.product_code),
            packet.code,
            levels_map.as_ref(),
            width_u32,
            levels,
        );
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// The text packets (1) left of a raster and beside its rows: the time,
    /// elevation and height labels of a Weak Echo Region slice, top to bottom.
    fn slice_labels(&self, header: &RasterHeader, rows: usize) -> Vec<String> {
        let RasterHeader::RasterData {
            i_start,
            j_start,
            y_scale,
            ..
        } = *header
        else {
            return Vec::new();
        };
        let top = i64::from(j_start);
        let bottom = top + i64::try_from(rows).unwrap_or(i64::MAX) * i64::from(y_scale.max(1));
        let mut labels: Vec<(i16, String)> = self
            .symbology
            .iter()
            .flat_map(|s| s.layers.iter().flatten())
            .filter_map(|p| match p {
                Packet::Text(t)
                    if t.code == 1 && t.i < i_start && (top..bottom).contains(&i64::from(t.j)) =>
                {
                    Some((t.j, t.text.trim().to_owned()))
                }
                _ => None,
            })
            .collect();
        labels.sort_by_key(|(j, _)| *j);
        labels.into_iter().map(|(_, text)| text).collect()
    }

    /// A sweep from a quasi-vertical profile raster (products 189-192),
    /// transposed so that each ray is one column.
    fn qvp_sweep(&self, packet: &RasterPacket) -> Result<Sweep, Level3Error> {
        const CELL_M: f64 = 20.0;
        let desc = &self.description;
        let grid = &packet.grid;
        let (rows, columns) = (grid.rows(), grid.columns());
        let rows_u32 = dimension(rows, packet.code)?;
        let mut sweep = raster_frame(SweepMode::Other("quasi_vertical_profile".into()), columns);
        sweep.range = RangeCoord::Uniform {
            first_center_m: 0.5 * CELL_M,
            spacing_m: CELL_M,
            ngates: rows_u32,
        };
        push_raster_attrs(&mut sweep, grid, &packet.header);
        sweep.other.push((
            "level3_geometry".into(),
            AttrValue::text(
                "nominal: one ray per column (volume scan), 20 m cells from the bottom row \
                 (ICD 2620001AD Table III)",
            ),
        ));
        // Column c of the raster, bottom row first, is ray c.
        let mut transposed = Vec::with_capacity(rows.saturating_mul(columns));
        for column in 0..columns {
            for row in (0..rows).rev() {
                transposed.push(grid.get(row, column).unwrap_or(0));
            }
        }
        let levels = DataLevels::for_packet(desc, packet.code);
        let field = u8_field(
            desc,
            field_name(desc.product_code),
            packet.code,
            levels.as_ref(),
            rows_u32,
            transposed,
        );
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }
}

/// The text of text packets (1, 2, 8), in order, including those nested in
/// SCIT packets; every packet code visited goes to `codes`.
fn collect_text(packet: &Packet, codes: &mut Vec<u16>, text: &mut Vec<Box<str>>) {
    codes.push(packet.code());
    match packet {
        Packet::Text(t) => text.push(t.text.as_str().into()),
        Packet::Symbol(SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested)) => {
            for inner in nested {
                collect_text(inner, codes, text);
            }
        }
        _ => {}
    }
}

/// Generic parameters (Figure E-2) as a text array of `id: attributes`.
fn push_parameters(
    other: &mut Vec<(Box<str>, AttrValue)>,
    name: &str,
    parameters: &[crate::packets::generic::GenericParameter],
) {
    if parameters.is_empty() {
        return;
    }
    let values = parameters
        .iter()
        .map(|p| format!("{}: {}", p.id, p.attributes).into())
        .collect();
    other.push((name.into(), AttrValue::Array(ArrayBuf::Text(values))));
}

/// The values generic radials store after the bins they declare
/// ([`GenericRadial::bins`](crate::packets::generic::GenericRadial::bins)
/// leaves them out of the field; no real product has any), as a contiguous
/// ragged array: `level3_surplus_count` per ray and every surplus value in
/// ray order in `level3_surplus_values`. Nothing is added when there are
/// none.
fn push_surplus_values(
    sweep: &mut Sweep,
    component: &GenericRadialComponent,
    packet_code: u16,
    nrays: u32,
) -> Result<(), Level3Error> {
    fn surplus(radial: &crate::packets::generic::GenericRadial) -> &[i32] {
        radial.values.get(radial.bins().len()..).unwrap_or_default()
    }
    if component.radials.iter().all(|r| surplus(r).is_empty()) {
        return Ok(());
    }
    let mut counts = Vec::with_capacity(component.radials.len());
    let mut values = Vec::new();
    for radial in &component.radials {
        let extra = surplus(radial);
        counts.push(i32::try_from(extra.len()).unwrap_or(i32::MAX));
        values.extend_from_slice(extra);
    }
    let total = dimension(values.len(), packet_code)?;
    sweep.extra_vars.push(ExtraVariable {
        name: "level3_surplus_count".into(),
        dims: vec!["time".into()],
        shape: vec![nrays],
        values: ArrayBuf::I32(counts),
        attrs: vec![
            (
                "long_name".into(),
                AttrValue::text("number of values the radial stores after its declared bins"),
            ),
            ("sample_dimension".into(), AttrValue::text("level3_surplus")),
        ],
    });
    sweep.extra_vars.push(ExtraVariable {
        name: "level3_surplus_values".into(),
        dims: vec!["level3_surplus".into()],
        shape: vec![total],
        values: ArrayBuf::I32(values),
        attrs: vec![(
            "long_name".into(),
            AttrValue::text("values stored after the declared bins, in ray order"),
        )],
    });
    Ok(())
}

/// A per-ray `f32` variable.
fn per_ray_f32(
    name: &str,
    long_name: &str,
    units: &str,
    nrays: u32,
    values: Vec<f32>,
) -> ExtraVariable {
    ExtraVariable {
        name: name.into(),
        dims: vec!["time".into()],
        shape: vec![nrays],
        values: ArrayBuf::F32(values),
        attrs: vec![
            ("long_name".into(), AttrValue::text(long_name)),
            ("units".into(), AttrValue::text(units)),
        ],
    }
}

/// The per-ray `y` variable of a raster sweep (metres).
/// Places a raster sweep of `rows` x `columns` boxes on a national grid
/// array ([`crate::hrap`]): `range` and `y` are the polar stereographic
/// coordinates of the box centres relative to the radar, the 2-D variables
/// `latitude` and `longitude` the box centres, and the sweep attributes name
/// the projection and the array's corner.
fn place_on_national_grid(
    sweep: &mut Sweep,
    desc: &ProductDescription,
    local: &LocalGrid,
    rows: usize,
    columns: usize,
    what: &str,
) -> Result<(), Level3Error> {
    let columns_u32 = dimension(columns, 0)?;
    let rows_u32 = dimension(rows, 0)?;
    let (hx, hy) = hrap::to_grid(desc.latitude_deg, desc.longitude_deg);
    let spacing = local.box_size * hrap::MESH_M;
    let (x0, _) = local.box_centre(0, 0);
    sweep.range = RangeCoord::Uniform {
        first_center_m: (x0 - hx) * hrap::MESH_M,
        spacing_m: spacing,
        ngates: columns_u32,
    };
    sweep.extra_vars.push(y_variable(
        (0..rows)
            .map(|row| ((local.box_centre(row, 0).1 - hy) * hrap::MESH_M) as f32)
            .collect(),
        "polar stereographic y of the row centre relative to the radar",
    ));
    let mut latitude = Vec::with_capacity(rows.saturating_mul(columns));
    let mut longitude = Vec::with_capacity(rows.saturating_mul(columns));
    for row in 0..rows {
        for column in 0..columns {
            let (lat, lon) = local.box_centre_lat_lon(row, column);
            latitude.push(lat);
            longitude.push(lon);
        }
    }
    for (name, values, units) in [
        ("latitude", latitude, "degrees_north"),
        ("longitude", longitude, "degrees_east"),
    ] {
        sweep.extra_vars.push(ExtraVariable {
            name: name.into(),
            dims: vec!["time".into(), "range".into()],
            shape: vec![rows_u32, columns_u32],
            values: ArrayBuf::F64(values),
            attrs: vec![
                (
                    "long_name".into(),
                    AttrValue::text(format!("{name} of the {what} centre")),
                ),
                ("units".into(), AttrValue::text(units)),
            ],
        });
    }
    let other = &mut sweep.other;
    for (name, value) in [
        ("grid_mapping_name", AttrValue::text("polar_stereographic")),
        (
            "straight_vertical_longitude_from_pole",
            AttrValue::Scalar(Scalar::F64(hrap::VERTICAL_LONGITUDE_DEG)),
        ),
        (
            "standard_parallel",
            AttrValue::Scalar(Scalar::F64(hrap::TRUE_LATITUDE_DEG)),
        ),
        (
            "earth_radius",
            AttrValue::Scalar(Scalar::F64(hrap::EARTH_RADIUS_M)),
        ),
        ("hrap_west", AttrValue::Scalar(Scalar::F64(local.west))),
        ("hrap_north", AttrValue::Scalar(Scalar::F64(local.north))),
        (
            "hrap_box_size",
            AttrValue::Scalar(Scalar::F64(local.box_size)),
        ),
        ("hrap_radar_x", AttrValue::Scalar(Scalar::F64(hx))),
        ("hrap_radar_y", AttrValue::Scalar(Scalar::F64(hy))),
        ("raster_rows", AttrValue::Scalar(Scalar::U32(rows_u32))),
        (
            "raster_columns",
            AttrValue::Scalar(Scalar::U32(columns_u32)),
        ),
        ("raster_cell_m", AttrValue::Scalar(Scalar::F64(spacing))),
    ] {
        other.push((name.into(), value));
    }
    Ok(())
}

/// `Level3Product::volume_bytes` of an HRAP array: its levels, the
/// latitude and longitude of every box (8 bytes each) and its rows.
fn hrap_bytes(grid: &RasterGrid) -> usize {
    grid.levels()
        .len()
        .saturating_mul(17)
        .saturating_add(grid.rows().saturating_mul(VOLUME_BYTES_PER_RAY))
}

fn y_variable(values: Vec<f32>, long_name: &str) -> ExtraVariable {
    let nrays = u32::try_from(values.len()).unwrap_or(u32::MAX);
    per_ray_f32("y", long_name, "m", nrays, values)
}

/// A dimension that fits the model's `u32` sizes.
fn dimension(n: usize, packet_code: u16) -> Result<u32, Level3Error> {
    u32::try_from(n).map_err(|_| Level3Error::InvalidPacket {
        code: packet_code,
        reason: format!("{n} cells do not fit a dimension"),
    })
}

/// A raster sweep with `rays` rays at time 0 and NaN angles.
fn raster_frame(mode: SweepMode, rays: usize) -> Sweep {
    let mut sweep = Sweep::new(0, mode, f32::NAN);
    sweep.reserve_rays(rays);
    for _ in 0..rays {
        sweep.push_ray(0.0, f32::NAN, f32::NAN);
    }
    sweep
}

/// Grid size and packet header of a raster as sweep attributes.
fn push_raster_attrs(sweep: &mut Sweep, grid: &RasterGrid, header: &RasterHeader) {
    let other = &mut sweep.other;
    for (name, n) in [
        ("raster_rows", grid.rows()),
        ("raster_columns", grid.columns()),
    ] {
        other.push((
            name.into(),
            AttrValue::Scalar(Scalar::U32(u32::try_from(n).unwrap_or(u32::MAX))),
        ));
    }
    match header {
        RasterHeader::RasterData {
            op_flags,
            i_start,
            j_start,
            x_scale,
            x_scale_fraction,
            y_scale,
            y_scale_fraction,
            packing,
        } => {
            other.push((
                "raster_op_flags".into(),
                AttrValue::Array(ArrayBuf::U16(op_flags.to_vec())),
            ));
            for (name, value) in [
                ("raster_i_start", *i_start),
                ("raster_j_start", *j_start),
                ("raster_x_scale", *x_scale),
                ("raster_x_scale_fraction", *x_scale_fraction),
                ("raster_y_scale", *y_scale),
                ("raster_y_scale_fraction", *y_scale_fraction),
            ] {
                other.push((name.into(), AttrValue::Scalar(Scalar::I16(value))));
            }
            other.push((
                "raster_packing".into(),
                AttrValue::Scalar(Scalar::U16(*packing)),
            ));
        }
        RasterHeader::DigitalRaster {
            i_start,
            j_start,
            i_scale,
            j_scale,
        } => {
            for (name, value) in [
                ("raster_i_start", *i_start),
                ("raster_j_start", *j_start),
                ("raster_i_scale", *i_scale),
                ("raster_j_scale", *j_scale),
            ] {
                other.push((name.into(), AttrValue::Scalar(Scalar::I16(value))));
            }
        }
        RasterHeader::PrecipitationRate { spares } => {
            other.push((
                "raster_spares".into(),
                AttrValue::Array(ArrayBuf::U16(spares.to_vec())),
            ));
        }
        RasterHeader::IntensityGrid => {}
    }
}

/// Adds the field, mapping a model error to [`Level3Error::InvalidMessage`].
fn push_field(sweep: &mut Sweep, code: i16, field: Field) -> Result<(), Level3Error> {
    sweep
        .add_field(field)
        .map(|_| ())
        .map_err(|err| Level3Error::InvalidMessage {
            code,
            reason: format!("data array: {err}"),
        })
}

/// `rays_angle_resolution` and `rays_are_indexed` from `(start, width)`
/// angles: the most common radial width when at least half the radials
/// share it (the RPG rounds radial boundaries to 0.1 degree, so a few
/// radials of a 1 degree product are 0.9 or 1.1 degrees wide), indexed when
/// every start angle is a multiple of that width.
fn set_angle_resolution(sweep: &mut Sweep, radials: impl Iterator<Item = (f32, f32)>) {
    let radials: Vec<(f32, f32)> = radials.collect();
    let mut widths: Vec<(i64, usize)> = Vec::new();
    for &(_, width) in &radials {
        let key = (f64::from(width) * 1000.0).round() as i64;
        match widths.iter_mut().find(|(w, _)| *w == key) {
            Some((_, count)) => *count += 1,
            None => widths.push((key, 1)),
        }
    }
    let Some(&(key, count)) = widths.iter().max_by_key(|(_, count)| *count) else {
        return;
    };
    if key <= 0 || count * 2 < radials.len() {
        return;
    }
    let width = key as f64 / 1000.0;
    let indexed = radials.iter().all(|&(start, _)| {
        let steps = f64::from(start) / width;
        (steps - steps.round()).abs() <= 1e-3
    });
    sweep.rays_angle_resolution_deg = Some(width as f32);
    sweep.rays_are_indexed = Some(indexed);
}

/// Supplemental scan flags of halfword 50 (SAILS, MRLE) as a sweep attribute.
fn push_supplemental_scan(sweep: &mut Sweep, desc: &ProductDescription) {
    if let Some(kind) = supplemental_scan(desc) {
        sweep
            .other
            .push(("level3_supplemental_scan".into(), AttrValue::text(kind)));
    }
}

// ---------------------------------------------------------------------------------
// Product geometry tables (ICD 2620001AD Table III, 2620001P/H for retired
// products, 2620063E for TDWR)
// ---------------------------------------------------------------------------------

/// Range bin size in metres of a radial product's data array (ICD Table III
/// resolution: 0.13 nm = 250 m, 0.27 nm = 500 m, 0.54 nm = 1 km, 1.1 nm =
/// 2 km, 2.2 nm = 4 km; TDWR 0.08 nm = 150 m, 0.16 nm = 300 m), or `None`
/// when the ICD gives none.
///
/// Product 34 (Clutter Filter Control) is 1 km bins out to 124 nm (230 km)
/// per 2620003AE section 34.2.3, but real products carry 230 bins (2013) or
/// 460 bins (2021). The size used is 230 km over `num_bins` (500 m for 460
/// bins), keeping the ICD's range; this is **not verified**: 460 bins of
/// 1 km (the ICD's bin size, out to the 460 km of Level II) fit the ICD as
/// well. Nothing in the products settles it: the radial packet's scale
/// factor is 1000 in both products, but it is a display scale (999 for the
/// 1 km and the 250 m products alike, 3996 for the 500 m product 55), so
/// MetPy's gate scale of 1 km, taken from it, is not evidence; and in
/// review the clutter maps of the Level II volumes of the same site and
/// hour (PAKC 2021-07-30 05:50 and 05:59) overlapped either reading about
/// as well (intersection over union 0.54 for 500 m, 0.48 for 1 km).
pub fn range_bin_size_m(product_code: i16, num_bins: u16) -> Option<f64> {
    Some(match product_code {
        16 | 19 | 24 | 27 | 30 | 32 | 33 | 43 | 56 | 93 | 94 | 132 | 133 | 137 | 158 | 160
        | 162 | 164 | 195 => 1000.0,
        17 | 20 | 31 | 78 | 79 | 80 | 138 | 144..=147 | 150 | 151 | 156 | 157 | 169 | 171 => 2000.0,
        18 | 21 => 4000.0,
        22
        | 25
        | 28
        | 44
        | 45
        | 99
        | 113
        | 153..=155
        | 159
        | 161
        | 163
        | 165
        | 167
        | 168
        | 170
        | 172..=175
        | 177
        | 193
        | 197 => 250.0,
        23 | 26 | 29 | 46 | 55 => 500.0,
        134 | 135 => 1000.0,
        180..=185 => 150.0,
        186 | 187 => 300.0,
        34 if num_bins > 0 => 230_000.0 / f64::from(num_bins),
        _ => return None,
    })
}

/// Cell size in metres of a geographic raster product (ICD Table III: 0.27 nm
/// = 500 m, 0.54 nm = 1 km, 1.1 nm = 2 km, 2.2 nm = 4 km; the 1990s codes 49
/// and 68-72 from the Table III of DSI-7000), the nominal 1/40 LFM mesh length
/// at 60N for product 81, or `None` for rasters that are not geographic
/// (cross sections, quasi-vertical profiles), product 87 (whose resolution is
/// its halfword 50) and unknown products.
pub fn raster_cell_size_m(product_code: i16) -> Option<f64> {
    Some(match product_code {
        49 => 500.0,
        35 | 37 | 95 | 97 => 1000.0,
        78..=80 => 2000.0,
        36 | 38 | 41 | 57 | 63..=72 | 89 | 90 | 96 | 98 => 4000.0,
        81 => hrap::MESH_M,
        _ => return None,
    })
}

/// Products whose halfword 30 is the elevation angle x10 (Table V).
const ELEVATION_PRODUCTS: &[i16] = &[
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 43, 44, 45, 46, 49, 55, 56, 87, 88,
    93, 94, 99, 113, 132, 133, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165,
    167, 168, 180, 181, 182, 183, 184, 185, 186, 187, 193, 195,
];

/// Products whose halfword 50 holds the elevation delay (bits 5-15, seconds
/// after the volume scan start) and supplemental scan type (bits 0-4).
const DELTA_TIME_PRODUCTS: &[i16] = &[
    19, 20, 27, 30, 94, 99, 132, 149, 153, 154, 155, 159, 161, 163, 165, 166, 167, 168,
];

/// The elevation angle of an elevation-based product (halfword 30 / 10,
/// signed), `None` for volume, hybrid-scan and accumulation products.
pub fn elevation_deg(desc: &ProductDescription) -> Option<f32> {
    ELEVATION_PRODUCTS
        .contains(&desc.product_code)
        .then(|| f32::from(desc.halfword(30).unwrap_or_default() as i16) * 0.1)
}

/// Seconds from the volume scan start to the elevation start (halfword 50
/// bits 5-15) for the products that carry it, else `None`.
pub fn elevation_delay_s(desc: &ProductDescription) -> Option<u16> {
    DELTA_TIME_PRODUCTS
        .contains(&desc.product_code)
        .then(|| desc.halfword(50).unwrap_or_default() >> 5)
}

/// The supplemental scan type of halfword 50 bits 0-4: `"mrle"` (1),
/// `"sails"` (2), another nonzero code as text; `None` when 0 or when the
/// product does not carry the field. Table V Note 24 gives 1 = SAILS and 2 =
/// MRLE; real products carry the opposite (see [`crate::params`]).
pub fn supplemental_scan(desc: &ProductDescription) -> Option<Cow<'static, str>> {
    if !DELTA_TIME_PRODUCTS.contains(&desc.product_code) {
        return None;
    }
    match desc.halfword(50).unwrap_or_default() & 0x1F {
        0 => None,
        1 => Some(Cow::Borrowed("mrle")),
        2 => Some(Cow::Borrowed("sails")),
        other => Some(Cow::Owned(other.to_string())),
    }
}

/// The FM301 field name of a product's data array: the moment name for base
/// moments, REC for hydrometeor classifications, RR for the precipitation
/// rate, else the ICD mnemonic, else `P<code>`.
pub fn field_name(product_code: i16) -> FieldName {
    match product_code {
        16..=21 | 32 | 33 | 94 | 153 | 180 | 181 | 186 | 187 | 193 | 195 => FieldName::Dbzh,
        22..=27 | 93 | 99 | 154 | 182 | 183 => FieldName::Vradh,
        28..=30 | 155 | 184 | 185 => FieldName::Wradh,
        158 | 159 => FieldName::Zdr,
        160 | 161 | 167 => FieldName::Rhohv,
        162 | 163 => FieldName::Kdp,
        168 => FieldName::Phidp,
        164 | 165 | 177 => FieldName::Rec,
        176 => FieldName::Rr,
        code => {
            let mnemonic = product_info(code)
                .map(|info| info.mnemonic)
                .filter(|m| !m.is_empty());
            match mnemonic {
                Some(mnemonic) => FieldName::Other(mnemonic.into()),
                None => FieldName::Other(format!("P{code}").into()),
            }
        }
    }
}

/// Units of a 16-level threshold product's values (ICD Table III), where
/// [`DataLevels::units`] has none. Legacy 16-level velocity and spectrum
/// width products are in knots.
fn threshold_units(product_code: i16) -> Option<&'static str> {
    Some(match product_code {
        16..=21
        | 33
        | 35..=39
        | 43
        | 49
        | 50
        | 53
        | 63..=67
        | 85
        | 89
        | 90
        | 95..=98
        | 137
        | 181
        | 187 => "dBZ",
        22..=30 | 44 | 45 | 51 | 52 | 55 | 56 | 86 | 183..=185 => "kt",
        41 | 42 => "kft",
        57 => "kg m-2",
        31 | 78 | 79 | 80 | 144..=147 | 150 | 151 | 169 | 171 => "in",
        158 => "dB",
        160 => "1",
        162 => "deg km-1",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------
// Data level encodings as field codings
// ---------------------------------------------------------------------------------

/// How a product's data levels are carried in its field (see the module
/// documentation).
struct Coding {
    transform: LinearTransform,
    fill: Option<u16>,
    undetect: Option<u16>,
    range_folded: Option<u16>,
    valid_range: Option<[u16; 2]>,
    /// Named levels besides the sentinels, with their meanings.
    flag_values: Vec<i64>,
    /// Bit masks paired with `flag_values` (enhanced echo tops only).
    flag_masks: Vec<i64>,
    flag_meanings: Vec<String>,
    discrete: bool,
    /// Short name of the encoding.
    encoding: &'static str,
}

impl Coding {
    fn new(transform: LinearTransform, encoding: &'static str) -> Self {
        Self {
            transform,
            fill: None,
            undetect: None,
            range_folded: None,
            valid_range: None,
            flag_values: Vec::new(),
            flag_masks: Vec::new(),
            flag_meanings: Vec::new(),
            discrete: false,
            encoding,
        }
    }

    fn identity(encoding: &'static str) -> Self {
        Self::new(
            LinearTransform::CfScaleOffset {
                scale_factor: 1.0,
                add_offset: 0.0,
                attr_width: FloatWidth::F32,
            },
            encoding,
        )
    }

    /// Record a flag level: below threshold is `_Undetect` (and `_FillValue`
    /// when nothing else is), no data / missing / blank / outside coverage
    /// are `_FillValue`, range folded is the range-folded flag, and every
    /// other named level (or a second fill level) is a flag value.
    fn flag(&mut self, level: u16, flag: LevelFlag) {
        match flag {
            LevelFlag::BelowThreshold | LevelFlag::NoAccumulation if self.undetect.is_none() => {
                self.undetect = Some(level);
                self.fill.get_or_insert(level);
                return;
            }
            LevelFlag::RangeFolded if self.range_folded.is_none() => {
                self.range_folded = Some(level);
                return;
            }
            LevelFlag::NoData
            | LevelFlag::Missing
            | LevelFlag::Blank
            | LevelFlag::OutsideCoverage
                if self.fill.is_none() || self.fill == self.undetect =>
            {
                self.fill = Some(level);
                return;
            }
            // A blank level is not displayed: without a value it reads as
            // missing, and repeated blank levels are not listed.
            LevelFlag::Blank => return,
            _ => {}
        }
        self.flag_values.push(i64::from(level));
        self.flag_meanings.push(flag_meaning(flag).to_owned());
    }

    fn int_coding<T: PackedInt>(&self) -> IntCoding<T> {
        let narrow = |level: Option<u16>| level.and_then(|l| T::from_i64(i64::from(l)));
        IntCoding {
            transform: self.transform,
            fill_value: narrow(self.fill),
            undetect: narrow(self.undetect),
            range_folded: narrow(self.range_folded),
            valid_range: self.valid_range.and_then(|[lo, hi]| {
                Some([
                    narrow(Some(lo))?,
                    T::from_i64(i64::from(hi)).unwrap_or(T::MAX),
                ])
            }),
        }
    }

    /// The coding of a product's level encoding; `None` (no mapping) keeps
    /// the levels as their own values.
    fn of(levels: Option<&DataLevels>) -> Self {
        let Some(levels) = levels else {
            return Self::identity("undescribed");
        };
        match levels.encoding() {
            LevelEncoding::Linear(linear) => {
                let mut coding = Self::new(
                    LinearTransform::CfScaleOffset {
                        scale_factor: linear.increment,
                        add_offset: linear.first_value
                            - f64::from(linear.first_level) * linear.increment,
                        attr_width: FloatWidth::F32,
                    },
                    "linear",
                );
                for &(level, flag) in linear.flags {
                    coding.flag(level, flag);
                }
                // The value levels, excluding named flag levels at either end.
                let last = linear
                    .first_level
                    .saturating_add(linear.count.saturating_sub(1));
                let is_flag = |n: u16| linear.flags.iter().any(|(level, _)| *level == n);
                let mut lo = linear.first_level;
                let mut hi = last;
                while lo < hi && is_flag(lo) {
                    lo += 1;
                }
                while hi > lo && is_flag(hi) {
                    hi -= 1;
                }
                coding.valid_range = (linear.count > 0).then_some([lo, hi]);
                coding
            }
            LevelEncoding::ScaleOffset {
                scale,
                offset,
                max_level,
                leading_flags,
                trailing_flags,
                flags,
            } => {
                let mut coding = Self::new(
                    LinearTransform::IcdScaleOffset {
                        scale: *scale,
                        offset: *offset,
                    },
                    "scale_offset",
                );
                coding.valid_range =
                    Some([*leading_flags, max_level.saturating_sub(*trailing_flags)]);
                for &(level, flag) in *flags {
                    coding.flag(level, flag);
                }
                // Unnamed leading and trailing flag levels (at most 16 each
                // listed; the others are outside `valid_range` all the same).
                let named = |n: u16| flags.iter().any(|(level, _)| *level == n);
                let leading = (*leading_flags).min(max_level.saturating_add(1)).min(16);
                let trailing = max_level.saturating_sub(*trailing_flags).saturating_add(1);
                let trailing = trailing.max(leading).max(max_level.saturating_sub(15));
                for n in (0..leading).chain(trailing..=*max_level) {
                    if !named(n) {
                        coding.flag(n, LevelFlag::Flagged);
                    }
                }
                if *leading_flags > 0 && coding.fill.is_none() {
                    coding.fill = Some(0);
                }
                coding
            }
            LevelEncoding::Edr {
                scale,
                offset,
                levels: count,
                leading_flags,
            } => {
                let mut coding = Self::new(
                    LinearTransform::CfScaleOffset {
                        scale_factor: *scale,
                        add_offset: *offset,
                        attr_width: FloatWidth::F32,
                    },
                    "edr",
                );
                coding.fill = (*leading_flags > 0).then_some(0);
                for n in 1..*leading_flags {
                    coding.flag(n, LevelFlag::Flagged);
                }
                coding.valid_range = Some([*leading_flags, count.saturating_sub(1)]);
                coding
            }
            LevelEncoding::Classes(_) => Self::discrete(levels, 256),
            LevelEncoding::Thresholds(_) => {
                let all_classes = (0..16u16).map(|n| levels.level(n)).all(|level| {
                    matches!(level, Level::Class(_) | Level::Flag(_) | Level::Undefined)
                }) && (0..16u16)
                    .map(|n| levels.level(n))
                    .any(|level| matches!(level, Level::Class(_)));
                if all_classes {
                    return Self::discrete(levels, 16);
                }
                let mut values = [f32::NAN; 16];
                let mut coding = Self::identity("thresholds");
                let mut value_levels = Vec::new();
                for n in 0..16u16 {
                    match levels.level(n) {
                        Level::Value(v) | Level::Topped(v) => {
                            values[usize::from(n)] = v as f32;
                            value_levels.push(n);
                        }
                        Level::Flag(flag) => coding.flag(n, flag),
                        Level::Class(class) => {
                            coding.flag_values.push(i64::from(n));
                            coding.flag_meanings.push(snake_case(class.description));
                        }
                        Level::Undefined => {}
                    }
                }
                coding.transform = LinearTransform::Levels(LevelTable::Sixteen(values));
                coding.valid_range = value_levels
                    .first()
                    .zip(value_levels.last())
                    .map(|(lo, hi)| [*lo, *hi]);
                coding
            }
            LevelEncoding::Vil {
                linear_scale,
                linear_offset,
                log_start,
                log_scale,
                log_offset,
            } => {
                let mut coding = Self::new(
                    LinearTransform::Levels(LevelTable::LinearLog {
                        linear_scale: *linear_scale as f32,
                        linear_offset: *linear_offset as f32,
                        log_start: *log_start,
                        log_scale: *log_scale as f32,
                        log_offset: *log_offset as f32,
                    }),
                    "vil",
                );
                coding.flag(0, LevelFlag::BelowThreshold);
                coding.flag(1, LevelFlag::Flagged);
                coding.flag(255, LevelFlag::Reserved);
                coding.valid_range = Some([2, 254]);
                coding
            }
            LevelEncoding::EchoTops {
                data_mask,
                scale,
                offset,
                topped_mask,
            } => {
                let mut coding = Self::new(
                    LinearTransform::Levels(LevelTable::Masked {
                        mask: *data_mask,
                        scale: f32::from(*scale),
                        offset: f32::from(*offset),
                    }),
                    "echo_tops",
                );
                coding.flag(0, LevelFlag::BelowThreshold);
                // "bad" is level 1 exactly; "topped" is a bit.
                coding.flag_values = vec![1, i64::from(*topped_mask)];
                coding.flag_masks = vec![0xFF, i64::from(*topped_mask)];
                coding.flag_meanings =
                    vec![flag_meaning(LevelFlag::Bad).to_owned(), "topped".to_owned()];
                coding.valid_range = Some([2, 255]);
                coding
            }
        }
    }

    /// A discrete coding over levels `0..count`: classes are flag values.
    fn discrete(levels: &DataLevels, count: u16) -> Self {
        let mut coding = Self::identity("classes");
        coding.discrete = true;
        let mut classes = Vec::new();
        for n in 0..count {
            match levels.level(n) {
                Level::Class(class) => classes.push((n, class)),
                Level::Flag(flag) => coding.flag(n, flag),
                _ => {}
            }
        }
        // Meanings: the description, or label and description when
        // descriptions repeat (product 34's "Bypass map in control").
        let unique = classes
            .iter()
            .map(|(_, c)| c.description)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == classes.len();
        // Flag levels recorded above come first in `flag_values`; classes
        // follow in level order.
        let flags: Vec<(i64, String)> = coding
            .flag_values
            .drain(..)
            .zip(coding.flag_meanings.drain(..))
            .collect();
        for (n, class) in &classes {
            coding.flag_values.push(i64::from(*n));
            coding.flag_meanings.push(if unique {
                snake_case(class.description)
            } else {
                snake_case(&format!("{} {}", class.label, class.description))
            });
        }
        for (value, meaning) in flags {
            coding.flag_values.push(value);
            coding.flag_meanings.push(meaning);
        }
        coding
    }
}

/// CF `flag_meanings` word of a level flag.
fn flag_meaning(flag: LevelFlag) -> &'static str {
    match flag {
        LevelFlag::BelowThreshold => "below_threshold",
        LevelFlag::Missing => "missing",
        LevelFlag::RangeFolded => "range_folded",
        LevelFlag::NoData => "no_data",
        LevelFlag::Blank => "blank",
        LevelFlag::Flagged => "flagged",
        LevelFlag::Bad => "bad_data",
        LevelFlag::Reserved => "reserved",
        LevelFlag::NoAccumulation => "no_accumulation",
        LevelFlag::OutsideCoverage => "outside_coverage",
        LevelFlag::EditRemove => "edited_removed",
        LevelFlag::Chaff => "chaff",
    }
}

/// CF `flag_meanings` word: lowercase, runs of other characters as `_`.
fn snake_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// A field over `u8` data levels (radial and raster packets).
fn u8_field(
    desc: &ProductDescription,
    name: FieldName,
    packet_code: u16,
    levels: Option<&DataLevels>,
    ngates: u32,
    data: Vec<u8>,
) -> Field {
    let coding = Coding::of(levels);
    let data = FieldData::U8 {
        values: data,
        coding: coding.int_coding(),
    };
    coded_field(desc, name, packet_code, levels, &coding, ngates, data)
}

/// A field over packed integer data with its attributes.
fn coded_field(
    desc: &ProductDescription,
    name: FieldName,
    packet_code: u16,
    levels: Option<&DataLevels>,
    coding: &Coding,
    ngates: u32,
    data: FieldData,
) -> Field {
    let mut field = Field::new(name, GateMapping::IDENTITY, ngates, data);
    field.attrs = field_attrs(desc, packet_code, levels);
    field.attrs.flag_values.clone_from(&coding.flag_values);
    field.attrs.flag_masks.clone_from(&coding.flag_masks);
    field.attrs.flag_meanings = coding
        .flag_meanings
        .iter()
        .map(|m| m.as_str().into())
        .collect();
    if coding.discrete {
        field.attrs.is_discrete = Some(true);
        field.attrs.units = None;
    }
    field
        .attrs
        .other
        .push(("level3_encoding".into(), AttrValue::text(coding.encoding)));
    if let Some(LevelEncoding::Thresholds(thresholds)) = levels.map(DataLevels::encoding) {
        let labels = thresholds.iter().map(|t| t.label().into()).collect();
        field.attrs.other.push((
            "level3_threshold_labels".into(),
            AttrValue::Array(ArrayBuf::Text(labels)),
        ));
    }
    field
}

/// Attributes shared by every field: product name, units, product code,
/// mnemonic, kind and packet code.
fn field_attrs(
    desc: &ProductDescription,
    packet_code: u16,
    levels: Option<&DataLevels>,
) -> FieldAttrs {
    let code = desc.product_code;
    let info = product_info(code);
    let mut attrs = FieldAttrs {
        long_name: info.map(|info| Cow::Borrowed(info.name)),
        ..FieldAttrs::default()
    };
    let units = levels
        .and_then(DataLevels::units)
        .or_else(|| threshold_units(code));
    if let Some(units) = units {
        attrs.units = Some(Cow::Borrowed(units));
    }
    attrs
        .other
        .push(("product_code".into(), AttrValue::Scalar(Scalar::I16(code))));
    if let Some(mnemonic) = info.map(|info| info.mnemonic).filter(|m| !m.is_empty()) {
        attrs
            .other
            .push(("product_mnemonic".into(), AttrValue::text(mnemonic)));
    }
    if let Some(kind) = info.map(|info| info.kind) {
        let kind = match kind {
            ProductKind::Radial => "radial",
            ProductKind::Raster => "raster",
            ProductKind::Generic => "generic",
            ProductKind::Graphic => "graphic",
            ProductKind::Tabular => "tabular",
            ProductKind::Text => "text",
        };
        attrs
            .other
            .push(("product_kind".into(), AttrValue::text(kind)));
    }
    attrs.other.push((
        "level3_packet_code".into(),
        AttrValue::Scalar(Scalar::U16(packet_code)),
    ));
    attrs
}
