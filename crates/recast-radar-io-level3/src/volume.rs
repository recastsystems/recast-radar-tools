//! Radial, raster and generic products as a one-sweep FM301 [`Volume`]
//! (`recast_radar_core::model`; spec section 4.5, design note
//! `docs/design/fm301-model.md` section 8.1).
//!
//! [`read_level3_volume`] decodes a file and converts it;
//! [`Level3Product::to_volume`] converts a decoded product. The first radial,
//! raster or generic data array in the Product Symbology Block becomes the
//! sweep's one field; graphic, tabular and text products, and products whose
//! symbology holds only symbols, vectors or text, are
//! [`Level3Error::NoDataArray`].
//!
//! # Mapping
//!
//! **Volume.** `instrument_name` is the generic product's radar name when the
//! packet carries one, else the site of the AWIPS identifier (`N0QTLX` ->
//! `TLX`). `time_reference` is the volume scan start (halfwords 21-23);
//! `volume_number` the volume scan number; `scan` the VCP; `location` the
//! Product Description Block latitude, longitude and height (feet converted
//! to metres). `attrs.other` keeps the product code, mnemonic, AWIPS
//! identifier, WMO heading, source and sequence numbers, operational mode,
//! product version and generation time. `provenance.source_format` is
//! [`SourceFormat::NexradLevel3`] and `provenance.compression` names the
//! bzip2 and zlib wrappers that were removed.
//!
//! **Sweep (radial products).** One ray per radial in file order. `azimuth`
//! is the centre of the radial (start angle plus half the angle delta,
//! ICD 2620001 Figures 3-10 and 3-11c); Py-ART's Level III reader reports the
//! start angle instead. `elevation` and `fixed_angle` are halfword 30 / 10 for
//! the products whose Table V defines it (the elevation-based products, see
//! [`elevation_deg`]) and NaN for volume, hybrid-scan and accumulation
//! products, which have no elevation; Py-ART reports 0 for those.
//! `elevation_number` is halfword 29 when it is not 0. `time` is the delay
//! from the volume scan start to the elevation start (halfword 50 bits 5-15,
//! [`elevation_delay_s`]) for the products that carry it and 0 otherwise.
//! `range` is uniform: bin `i` of a product with bin size `s`
//! ([`range_bin_size_m`], ICD Table III) spans `[i*s, (i+1)*s)` from the
//! radar, so the first centre is `(first_bin + 0.5) * s`. Super resolution
//! bin 8 (2125 m) is Level II gate 0. Py-ART instead scales bins by the
//! packet's display scale factor (999 for a 0.5 degree cut) and starts at 0.
//!
//! **Sweep (generic products, packet 28).** The first radial component:
//! azimuth is the leading edge plus half the width, elevation is the radial's
//! own, `range` comes from the component's bin size and range to the first
//! bin. Radials shorter than the longest are padded with the fill level.
//!
//! **Sweep (raster products).** FM301 has no Cartesian grid, so a raster is
//! carried as one sweep with `sweep_mode` `Other("raster")`: one ray per
//! image row from the north, NaN azimuth and elevation, `range` holding the
//! east-west offset of each column centre from the radar (negative west) and
//! the per-ray variable `y` the north-south offset of the row (`extra_vars`,
//! metres, positive north). The image is centred on the radar; the cell size
//! is the product's ([`raster_cell_size_m`], ICD Table III). The sweep's
//! `other` attributes keep the packet header (`raster_rows`, `raster_columns`,
//! `raster_cell_m`, start coordinates and scales). Product 81's 1/40 LFM grid
//! is polar stereographic; it gets the nominal mesh length at 60N
//! (4762.5 m) and `raster_projection = "polar_stereographic_lfm_1_40"`.
//!
//! **Field.** Named by [`field_name`]: the FM301 moment name for base
//! moments (DBZH, VRADH, WRADH, ZDR, RHOHV, KDP, PHIDP), REC for hydrometeor
//! classifications, RR for the instantaneous precipitation rate, else the ICD
//! mnemonic (`CR`, `ET`, `DVL`, `OHA`, ...). `long_name` is the product name;
//! `units` come from [`DataLevels::units`] or the ICD threshold units;
//! `attrs.other` keeps `product_code`, `product_mnemonic` and the packet code.
//! Data levels stay in their stored width with a CF packing when the
//! product's encoding is linear ([`LevelEncoding::Linear`],
//! [`LevelEncoding::ScaleOffset`], [`LevelEncoding::Edr`]): `u8` for radial
//! and raster packets, `u16` for generic packets. Level 0 "below threshold"
//! is `_Undetect` and `_FillValue`, "no data" and "missing" levels are
//! `_FillValue`, "range folded" is the range-folded flag, and `valid_range`
//! covers the value levels. Categorical products ([`LevelEncoding::Classes`]
//! and 16-level products whose levels are all classes) keep their levels as
//! discrete `u8` values with `flag_values` / `flag_meanings`. The other
//! encodings (16-level thresholds, VIL, echo tops) are not linear: their
//! levels are expanded to `f32` physical values, NaN where a level has no
//! value, exactly [`DataLevels::values`]. Range folding and "topped" echo
//! tops are then not distinguishable from missing; the packet API keeps them.
//!
//! Physical values equal [`DataLevels::values`] (within one `f32` rounding
//! step for the `REAL*4` scale and offset products, whose packing evaluates
//! `(raw - offset) / scale` in `f32` like Level II) and MetPy 1.7.1
//! `map_data` for every product it maps (`tests/volume.rs`), with the
//! documented exceptions in [`crate::levels`].

use std::borrow::Cow;

use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldAttrs, FieldData, FieldName, FloatCoding,
    FloatWidth, GateMapping, IntCoding, LinearTransform, PackedInt, RangeCoord, Scalar,
    SourceFormat, Sweep, SweepMode, Volume,
};

use crate::header::{OperationalMode, ProductDescription};
use crate::levels::{DataLevels, Level, LevelEncoding, LevelFlag};
use crate::packets::generic::{GenericPacket, GenericRadialComponent};
use crate::packets::raster::{RasterGrid, RasterHeader};
use crate::{Level3Error, Level3Product, Packet, ProductKind, decode_product, product_info};

/// Decodes one Level III product file into a one-sweep FM301 volume.
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

impl Level3Product {
    /// Converts the product's first radial, raster or generic data array into
    /// a one-sweep FM301 volume (see the [module documentation](self)).
    ///
    /// # Errors
    ///
    /// [`Level3Error::NoDataArray`] when the symbology block holds no such
    /// array, [`Level3Error::UnknownGeometry`] when the ICD gives no bin or
    /// cell size for the product.
    pub fn to_volume(&self) -> Result<Volume, Level3Error> {
        let code = self.description.product_code;
        let array = self
            .symbology
            .iter()
            .flat_map(|sym| sym.layers.iter().flatten())
            .find_map(DataArray::of)
            .ok_or(Level3Error::NoDataArray { code })?;
        let mut volume = self.volume_frame();
        let sweep = match array {
            DataArray::Radial(radial) => self.radial_sweep(radial)?,
            DataArray::Generic(component) => self.generic_sweep(component)?,
            DataArray::Raster {
                packet_code,
                header,
                grid,
            } => self.raster_sweep(packet_code, header, grid)?,
        };
        volume.provenance.decode.decoded_ray_count = sweep.nrays();
        volume.sweeps.push(sweep);
        volume.seal().map_err(|err| Level3Error::InvalidMessage {
            code,
            reason: format!("data array: {err}"),
        })?;
        volume.time_coverage = volume.ray_time_extent();
        Ok(volume)
    }

    /// The volume with everything but its sweep.
    fn volume_frame(&self) -> Volume {
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
        }
        other.push((
            "source_id".into(),
            AttrValue::Scalar(Scalar::I16(self.message_header.source_id)),
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
            "generation_time".into(),
            text(&desc.generation_time.to_rfc3339()),
        ));
        if let Some(time) = self.message_header.datetime() {
            other.push(("message_time".into(), text(&time.to_rfc3339())));
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
        volume
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
    fn radial_sweep(&self, radial: &crate::RadialPacket) -> Result<Sweep, Level3Error> {
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
        sweep.other.push((
            "level3_first_bin".into(),
            AttrValue::Scalar(Scalar::U16(radial.first_bin)),
        ));
        sweep.other.push((
            "level3_range_scale_factor".into(),
            AttrValue::Scalar(Scalar::U16(radial.scale_factor)),
        ));
        push_supplemental_scan(&mut sweep, desc);

        let levels = DataLevels::for_packet(desc, radial.code);
        let field = level_field(
            desc,
            radial.code,
            levels.as_ref(),
            u32::from(radial.num_bins),
            &radial.levels,
        );
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// A sweep from the first radial component of a generic packet (28).
    fn generic_sweep(&self, component: &GenericRadialComponent) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let ngates = component.num_bins();
        let ngates_u32 = u32::try_from(ngates).map_err(|_| Level3Error::InvalidPacket {
            code: 28,
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
        if !component.description.trim().is_empty() {
            sweep.other.push((
                "level3_component_description".into(),
                AttrValue::text(component.description.trim()),
            ));
        }
        push_supplemental_scan(&mut sweep, desc);

        let levels = DataLevels::for_packet(desc, 28);
        let packing = levels.as_ref().map_or(Packing::Table, Packing::of);
        let fill = packing.fill_level().unwrap_or(0);
        let all_fit = component
            .radials
            .iter()
            .flat_map(|r| r.bins().iter())
            .all(|&v| u16::try_from(v).is_ok());
        let field = if all_fit && !matches!(packing, Packing::Table) {
            let mut values = Vec::with_capacity(component.radials.len() * ngates);
            for radial in &component.radials {
                values.extend(radial.bins().iter().map(|&v| v as u16));
                values.resize(values.len() + (ngates - radial.bins().len()), fill);
            }
            packed_field(
                desc,
                28,
                &packing,
                ngates_u32,
                FieldData::U16 {
                    values,
                    coding: packing.int_coding(),
                },
            )
        } else {
            let values = match &levels {
                Some(levels) => component.values(levels),
                None => vec![f32::NAN; component.radials.len() * ngates],
            };
            float_field(desc, 28, levels.as_ref(), ngates_u32, values)
        };
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }

    /// A sweep from a raster packet (0xBA07, 0xBA0F, 17, 33).
    fn raster_sweep(
        &self,
        packet_code: u16,
        header: Option<&RasterHeader>,
        grid: &RasterGrid,
    ) -> Result<Sweep, Level3Error> {
        let desc = &self.description;
        let code = desc.product_code;
        let cell = raster_cell_size_m(code).ok_or(Level3Error::UnknownGeometry {
            code,
            what: "raster cell size",
        })?;
        let (rows, columns) = (grid.rows(), grid.columns());
        let columns_u32 = u32::try_from(columns).map_err(|_| Level3Error::InvalidPacket {
            code: packet_code,
            reason: format!("{columns} columns do not fit the range dimension"),
        })?;
        let rows_u32 = u32::try_from(rows).unwrap_or(u32::MAX);
        let mut sweep = Sweep::new(0, SweepMode::Other("raster".into()), f32::NAN);
        sweep.reserve_rays(rows);
        let half_rows = rows as f64 / 2.0;
        let y: Vec<f32> = (0..rows)
            .map(|row| ((half_rows - row as f64 - 0.5) * cell) as f32)
            .collect();
        for _ in 0..rows {
            sweep.push_ray(0.0, f32::NAN, f32::NAN);
        }
        sweep.extra_vars.push(ExtraVariable {
            name: "y".into(),
            dims: vec!["time".into()],
            shape: vec![rows_u32],
            values: ArrayBuf::F32(y),
            attrs: vec![
                (
                    "long_name".into(),
                    AttrValue::text("north-south offset of the row centre from the radar"),
                ),
                ("units".into(), AttrValue::text("m")),
            ],
        });
        sweep.range = RangeCoord::Uniform {
            first_center_m: (0.5 - columns as f64 / 2.0) * cell,
            spacing_m: cell,
            ngates: columns_u32,
        };
        let other = &mut sweep.other;
        other.push((
            "raster_rows".into(),
            AttrValue::Scalar(Scalar::U32(rows_u32)),
        ));
        other.push((
            "raster_columns".into(),
            AttrValue::Scalar(Scalar::U32(columns_u32)),
        ));
        other.push(("raster_cell_m".into(), AttrValue::Scalar(Scalar::F64(cell))));
        if code == 81 {
            other.push((
                "raster_projection".into(),
                AttrValue::text("polar_stereographic_lfm_1_40"),
            ));
        }
        match header {
            Some(RasterHeader::RasterData {
                i_start,
                j_start,
                x_scale,
                y_scale,
                packing,
                ..
            }) => {
                for (name, value) in [
                    ("raster_i_start", *i_start),
                    ("raster_j_start", *j_start),
                    ("raster_x_scale", *x_scale),
                    ("raster_y_scale", *y_scale),
                ] {
                    other.push((name.into(), AttrValue::Scalar(Scalar::I16(value))));
                }
                other.push((
                    "raster_packing".into(),
                    AttrValue::Scalar(Scalar::U16(*packing)),
                ));
            }
            Some(RasterHeader::DigitalRaster {
                i_start,
                j_start,
                i_scale,
                j_scale,
            }) => {
                for (name, value) in [
                    ("raster_i_start", *i_start),
                    ("raster_j_start", *j_start),
                    ("raster_i_scale", *i_scale),
                    ("raster_j_scale", *j_scale),
                ] {
                    other.push((name.into(), AttrValue::Scalar(Scalar::I16(value))));
                }
            }
            Some(RasterHeader::PrecipitationRate { .. }) | None => {}
        }

        let levels = DataLevels::for_packet(desc, packet_code);
        let field = level_field(
            desc,
            packet_code,
            levels.as_ref(),
            columns_u32,
            grid.levels(),
        );
        push_field(&mut sweep, desc.product_code, field)?;
        Ok(sweep)
    }
}

/// The first data array of a product.
enum DataArray<'a> {
    Radial(&'a crate::RadialPacket),
    Generic(&'a GenericRadialComponent),
    Raster {
        packet_code: u16,
        header: Option<&'a RasterHeader>,
        grid: &'a RasterGrid,
    },
}

impl<'a> DataArray<'a> {
    /// The data array a packet holds, if any. Packet 18 (precipitation rate
    /// array) has no level mapping and is skipped.
    fn of(packet: &'a Packet) -> Option<Self> {
        match packet {
            Packet::Radial(radial) => Some(Self::Radial(radial)),
            Packet::Generic(GenericPacket { components, .. }) => components
                .iter()
                .find_map(|c| match c {
                    crate::packets::generic::GenericComponent::Radial(radial) => Some(radial),
                    _ => None,
                })
                .map(Self::Generic),
            Packet::Raster(raster) if raster.code != 18 => Some(Self::Raster {
                packet_code: raster.code,
                header: Some(&raster.header),
                grid: &raster.grid,
            }),
            Packet::DigitalPrecip(precip) => Some(Self::Raster {
                packet_code: precip.code,
                header: None,
                grid: &precip.grid,
            }),
            _ => None,
        }
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
// Product geometry tables (ICD 2620001AD Table III, 2620063E for TDWR)
// ---------------------------------------------------------------------------------

/// Range bin size in metres of a radial product's data array (ICD Table III
/// resolution: 0.13 nm = 250 m, 0.27 nm = 500 m, 0.54 nm = 1 km, 1.1 nm =
/// 2 km, 2.2 nm = 4 km; TDWR 0.08 nm = 150 m, 0.16 nm = 300 m), or `None`
/// when the ICD gives none. Product 34 (Clutter Filter Control) always spans
/// 124 nm but its bin count changed between builds (230 and 460 in the
/// corpus), so its size is 230 km over `num_bins`.
pub fn range_bin_size_m(product_code: i16, num_bins: u16) -> Option<f64> {
    Some(match product_code {
        16 | 19 | 24 | 27 | 30 | 32 | 33 | 43 | 56 | 94 | 132 | 133 | 137 | 158 | 160 | 162
        | 164 | 195 => 1000.0,
        17 | 20 | 31 | 78 | 79 | 80 | 138 | 144..=147 | 150 | 151 | 169 | 171 => 2000.0,
        18 | 21 => 4000.0,
        22
        | 25
        | 28
        | 44
        | 45
        | 55
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
        23 | 26 | 29 | 46 => 500.0,
        134 | 135 => 1000.0,
        180..=185 => 150.0,
        186 | 187 => 300.0,
        34 if num_bins > 0 => 230_000.0 / f64::from(num_bins),
        _ => return None,
    })
}

/// Cell size in metres of a geographic raster product (ICD Table III: 0.54 nm
/// = 1 km, 1.1 nm = 2 km, 2.2 nm = 4 km), the nominal 1/40 LFM mesh length
/// at 60N for product 81, or `None` for rasters that are not geographic
/// (cross sections, quasi-vertical profiles) or unknown.
pub fn raster_cell_size_m(product_code: i16) -> Option<f64> {
    Some(match product_code {
        35 | 37 | 95 | 97 => 1000.0,
        78..=80 => 2000.0,
        36 | 38 | 41 | 57 | 63..=67 | 89 | 90 | 96 | 98 => 4000.0,
        81 => 4762.5,
        _ => return None,
    })
}

/// Products whose halfword 30 is the elevation angle x10 (Table V).
const ELEVATION_PRODUCTS: &[i16] = &[
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 43, 44, 45, 46, 55, 56, 93, 94, 99,
    113, 132, 133, 153, 154, 155, 158, 159, 160, 161, 162, 163, 164, 165, 167, 168, 180, 181, 182,
    183, 184, 185, 186, 187, 193, 195,
];

/// Products whose halfword 50 holds the elevation delay (bits 5-15, seconds
/// after the volume scan start) and supplemental scan type (bits 0-4).
const DELTA_TIME_PRODUCTS: &[i16] = &[
    19, 20, 27, 30, 94, 99, 132, 153, 154, 155, 159, 161, 163, 165, 167, 168,
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

/// The supplemental scan type of halfword 50 bits 0-4: `"sails"` (1),
/// `"mrle"` (2), another nonzero code as text; `None` when 0 or when the
/// product does not carry the field.
pub fn supplemental_scan(desc: &ProductDescription) -> Option<Cow<'static, str>> {
    if !DELTA_TIME_PRODUCTS.contains(&desc.product_code) {
        return None;
    }
    match desc.halfword(50).unwrap_or_default() & 0x1F {
        0 => None,
        1 => Some(Cow::Borrowed("sails")),
        2 => Some(Cow::Borrowed("mrle")),
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
        16..=21 | 33 | 35..=38 | 43 | 50 | 63..=67 | 85 | 89 | 90 | 95..=98 | 137 | 181 | 187 => {
            "dBZ"
        }
        22..=30 | 44 | 45 | 51 | 55 | 56 | 86 | 183 | 185 => "kt",
        41 => "kft",
        57 => "kg m-2",
        31 | 78 | 79 | 80 | 144..=147 | 150 | 151 => "in",
        158 => "dB",
        160 => "1",
        162 => "deg km-1",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------
// Data level encodings as field codings
// ---------------------------------------------------------------------------------

/// How a product's data levels are carried in the field.
enum Packing {
    /// Levels as stored, with a linear transform and sentinel levels.
    Linear(IntSpec),
    /// Levels as stored, categorical: classes are `flag_values`.
    Discrete(IntSpec, Vec<(u16, String)>),
    /// Levels expanded to `f32` physical values through the level table.
    Table,
}

/// Integer coding parameters in level units.
struct IntSpec {
    transform: LinearTransform,
    fill: Option<u16>,
    undetect: Option<u16>,
    range_folded: Option<u16>,
    valid_range: Option<[u16; 2]>,
}

impl IntSpec {
    fn identity() -> Self {
        Self {
            transform: LinearTransform::CfScaleOffset {
                scale_factor: 1.0,
                add_offset: 0.0,
                attr_width: FloatWidth::F32,
            },
            fill: None,
            undetect: None,
            range_folded: None,
            valid_range: None,
        }
    }

    /// Record a flag level: below threshold is `_Undetect` (and `_FillValue`
    /// when nothing else is), no data / missing / blank / outside coverage
    /// are `_FillValue`, range folded is the range-folded flag.
    fn flag(&mut self, level: u16, flag: LevelFlag) {
        match flag {
            LevelFlag::BelowThreshold | LevelFlag::NoAccumulation => {
                self.undetect.get_or_insert(level);
                self.fill.get_or_insert(level);
            }
            LevelFlag::RangeFolded => {
                self.range_folded.get_or_insert(level);
            }
            LevelFlag::NoData
            | LevelFlag::Missing
            | LevelFlag::Blank
            | LevelFlag::OutsideCoverage
            | LevelFlag::Bad => {
                if self.fill.is_none() || self.fill == self.undetect {
                    self.fill = Some(level);
                }
            }
            LevelFlag::Flagged | LevelFlag::Reserved | LevelFlag::EditRemove | LevelFlag::Chaff => {
            }
        }
    }

    fn coding<T: PackedInt>(&self) -> IntCoding<T> {
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
}

impl Packing {
    /// The packing of a product's level encoding.
    fn of(levels: &DataLevels) -> Self {
        match levels.encoding() {
            LevelEncoding::Linear(linear) => {
                let mut spec = IntSpec {
                    transform: LinearTransform::CfScaleOffset {
                        scale_factor: linear.increment,
                        add_offset: linear.first_value
                            - f64::from(linear.first_level) * linear.increment,
                        attr_width: FloatWidth::F32,
                    },
                    fill: None,
                    undetect: None,
                    range_folded: None,
                    valid_range: None,
                };
                for &(level, flag) in linear.flags {
                    spec.flag(level, flag);
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
                spec.valid_range = (linear.count > 0).then_some([lo, hi]);
                Self::Linear(spec)
            }
            LevelEncoding::ScaleOffset {
                scale,
                offset,
                max_level,
                leading_flags,
                trailing_flags,
                flags,
            } => {
                let mut spec = IntSpec {
                    transform: LinearTransform::IcdScaleOffset {
                        scale: *scale,
                        offset: *offset,
                    },
                    fill: None,
                    undetect: None,
                    range_folded: None,
                    valid_range: Some([*leading_flags, max_level.saturating_sub(*trailing_flags)]),
                };
                for &(level, flag) in *flags {
                    spec.flag(level, flag);
                }
                if *leading_flags > 0 && spec.fill.is_none() {
                    spec.fill = Some(0);
                }
                Self::Linear(spec)
            }
            LevelEncoding::Edr {
                scale,
                offset,
                levels: count,
                leading_flags,
            } => Self::Linear(IntSpec {
                transform: LinearTransform::CfScaleOffset {
                    scale_factor: *scale,
                    add_offset: *offset,
                    attr_width: FloatWidth::F32,
                },
                fill: (*leading_flags > 0).then_some(0),
                undetect: None,
                range_folded: None,
                valid_range: Some([*leading_flags, count.saturating_sub(1)]),
            }),
            LevelEncoding::Classes(_) => Self::discrete(levels, 256),
            LevelEncoding::Thresholds(_) => {
                let all_classes = (0..16u16).map(|n| levels.level(n)).all(|level| {
                    matches!(level, Level::Class(_) | Level::Flag(_) | Level::Undefined)
                }) && (0..16u16)
                    .map(|n| levels.level(n))
                    .any(|level| matches!(level, Level::Class(_)));
                if all_classes {
                    Self::discrete(levels, 16)
                } else {
                    Self::Table
                }
            }
            LevelEncoding::Vil { .. } | LevelEncoding::EchoTops { .. } => Self::Table,
        }
    }

    /// A discrete packing over levels `0..count`.
    fn discrete(levels: &DataLevels, count: u16) -> Self {
        let mut spec = IntSpec::identity();
        let mut classes = Vec::new();
        let mut meanings: Vec<String> = Vec::new();
        for n in 0..count {
            match levels.level(n) {
                Level::Class(class) => {
                    classes.push((n, class));
                }
                Level::Flag(flag) => spec.flag(n, flag),
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
        for (_, class) in &classes {
            meanings.push(if unique {
                snake_case(class.description)
            } else {
                snake_case(&format!("{} {}", class.label, class.description))
            });
        }
        let table = classes
            .iter()
            .zip(meanings)
            .map(|((n, _), meaning)| (*n, meaning))
            .collect();
        Self::Discrete(spec, table)
    }

    fn int_coding<T: PackedInt>(&self) -> IntCoding<T> {
        match self {
            Self::Linear(spec) | Self::Discrete(spec, _) => spec.coding(),
            Self::Table => IntSpec::identity().coding(),
        }
    }

    /// The level shorter generic radials are padded with.
    fn fill_level(&self) -> Option<u16> {
        match self {
            Self::Linear(spec) | Self::Discrete(spec, _) => spec.fill,
            Self::Table => None,
        }
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

/// A field from `u8` data levels (radial and raster packets).
fn level_field(
    desc: &ProductDescription,
    packet_code: u16,
    levels: Option<&DataLevels>,
    ngates: u32,
    data: &[u8],
) -> Field {
    let packing = levels.map_or(Packing::Table, Packing::of);
    match (&packing, levels) {
        (Packing::Table, Some(levels)) => {
            float_field(desc, packet_code, Some(levels), ngates, levels.values(data))
        }
        // No level mapping for the product: the raw levels, untransformed.
        _ => packed_field(
            desc,
            packet_code,
            &packing,
            ngates,
            FieldData::U8 {
                values: data.to_vec(),
                coding: packing.int_coding(),
            },
        ),
    }
}

/// A field over packed integer data with its attributes.
fn packed_field(
    desc: &ProductDescription,
    packet_code: u16,
    packing: &Packing,
    ngates: u32,
    data: FieldData,
) -> Field {
    let mut field = Field::new(
        field_name(desc.product_code),
        GateMapping::IDENTITY,
        ngates,
        data,
    );
    field.attrs = field_attrs(
        desc,
        packet_code,
        DataLevels::from_description(desc).as_ref(),
    );
    if let Packing::Discrete(_, classes) = packing {
        field.attrs.is_discrete = Some(true);
        field.attrs.units = None;
        for (level, meaning) in classes {
            field.attrs.flag_values.push(i64::from(*level));
            field.attrs.flag_meanings.push(meaning.as_str().into());
        }
    }
    field
}

/// A field of expanded `f32` physical values.
fn float_field(
    desc: &ProductDescription,
    packet_code: u16,
    levels: Option<&DataLevels>,
    ngates: u32,
    values: Vec<f32>,
) -> Field {
    let data = FieldData::F32 {
        values,
        coding: FloatCoding::default(),
    };
    let mut field = Field::new(
        field_name(desc.product_code),
        GateMapping::IDENTITY,
        ngates,
        data,
    );
    field.attrs = field_attrs(desc, packet_code, levels);
    field
}

/// Attributes shared by every field: product name, units, product code,
/// mnemonic and packet code.
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
