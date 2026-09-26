//! Display packets as text records ([`Level3Product::display_records`]).
//!
//! The FM301 volume ([`crate::volume`]) carries the data arrays as sweeps.
//! Everything else a product's Product Symbology Block and Graphic
//! Alphanumeric Block hold (text, special symbols, vectors, contours, storm
//! and wind symbols, cell trends, SCIT data, the non-radial components of
//! generic packets and packets without a decoder) is rendered here, one
//! record per packet, so that the volume keeps every decoded value
//! (`level3_display_packets`).
//!
//! # Record format
//!
//! `<place> <code> <fields>`, fields separated by one space:
//!
//! - **place**: `s<layer>:<index>` for the packet at `index` (from 0) of
//!   symbology layer `layer` (from 0), `g<page>:<index>` for a graphic page
//!   (the page number the file gives; product 62's cell trend data is page
//!   0), and `<parent place>.<index>` for a packet nested in SCIT packet 23 or
//!   24. Generic components add `.c<index>`, nested event components
//!   `.c<index>` again.
//! - **code**: the packet code in decimal below 256, else `0x` and four
//!   upper-case hexadecimal digits (`0x0E03`).
//! - **strings** are double-quoted with Rust's escapes (`"A\0"`), so a record
//!   splits unambiguously on spaces outside quotes.
//! - **items** of a packet with repeated records are separated by spaces;
//!   the values of one item by commas, in the ICD's field order.
//!
//! | Code | Fields |
//! |---|---|
//! | 1, 2 | `i j "text"` |
//! | 8 | `color i j "text"` |
//! | 6 | points `i,j` (starting point first) |
//! | 9 | `color`, then points `i,j` |
//! | 7, 0x3501 | segments `ib,jb,ie,je` |
//! | 10 | `color`, then segments `ib,jb,ie,je` |
//! | 0x0802 | `level` |
//! | 0x0E03 | points `i,j` (starting point first) |
//! | 3, 11, 25 | `i,j,radius` |
//! | 4 | `color,x,y,direction,speed` |
//! | 5 | `i,j,direction,arrow_length,head_length` |
//! | 12, 13, 14, 26 | `i,j` |
//! | 15 | `i,j,"id"` |
//! | 19 | `i,j,probability_of_hail,probability_of_severe_hail,max_size` |
//! | 20 | `i,j,feature_type,attribute` |
//! | 21 | `"id" i j`, then per trend `code:latest:v,v,...` |
//! | 22 | `latest:v,v,...` |
//! | 23, 24 | `nested=<count>`; the nested packets follow as records |
//! | 30 | the five values |
//! | 31 | the count |
//! | 28, 29 | `name="..." description="..." product_code=.. product_type=.. generation_time=.. radar_name="..." latitude=.. longitude=.. height=.. volume_time=.. elevation_time=.. elevation_angle=.. volume_number=.. operational_mode=.. vcp=.. elevation_number=.. compression=.. uncompressed_size=.. parameters=[..] components=<count>` and, for an external product (type 7), `external_spares=a,b,c,d,e`; each component follows as a record |
//! | data arrays (16, 0xAF1F, 0xBA07, 0xBA0F, 17, 18, 32, 33) | `sweep <n>`: the array is sweep `n` of the volume; one on a graphic page or nested in a SCIT packet (never seen) is not converted: `unconverted` and the packet's Rust debug rendering |
//! | packets without a decoder | `unknown <hex bytes>` |
//!
//! Generic components (place `.c<k>`, after their packet's code):
//! `radial sweep <n>` (a radial component nested in an event is not a sweep:
//! `radial description=".." bin_size=.. range_to_first_bin=.. parameters=[..]
//! radials=<count>`, then per radial `azimuth,elevation,width,num_bins,"attributes":v,v,..`); `text parameters=[..] "text"`; `grid type=..
//! dimensions=a,b,.. parameters=[..] attributes="..." values=v,v,..`; `area
//! type=.. parameters=[..] points=x,y x,y ..`; `table "title" columns=..
//! rows=.. parameters=[..] column_labels=["..",..] row_labels=[..]
//! entries=[..]`; `event parameters=[..] components=<count>` (its components
//! follow); `undecoded kind=.. <hex bytes>`. `parameters=[..]` lists
//! `"id"="attributes"` pairs separated by commas.

use std::fmt::Write as _;

use crate::packets::contour::Contour;
use crate::packets::generic::{GenericComponent, GenericParameter, GenericValues};
use crate::packets::symbols::VolumeList;
use crate::packets::vectors::{Point, Segment, Vectors};
use crate::packets::{GenericPacket, IrmPacket, SymbolPacket};
use crate::{Level3Error, Level3Product, Packet};

/// Largest total size, in bytes, of the records of one product. A product
/// whose display packets render to more is refused by
/// [`Level3Product::display_records`]; the records of the corpus products
/// are at most a few tens of kilobytes.
pub const MAX_RECORD_BYTES: usize = 64 << 20;

impl Level3Product {
    /// Every display packet of the Product Symbology Block and the Graphic
    /// Alphanumeric Block as a text record, in file order (see the
    /// [module documentation](crate::records) for the format). Data arrays
    /// appear as `sweep <n>`, naming the sweep of
    /// [`Level3Product::to_volume`] that holds them.
    ///
    /// # Errors
    ///
    /// [`Level3Error::InvalidMessage`] when the records would exceed
    /// [`MAX_RECORD_BYTES`].
    pub fn display_records(&self) -> Result<Vec<String>, Level3Error> {
        let mut out = Records {
            lines: Vec::new(),
            bytes: 0,
            sweep: 0,
            code: self.description.product_code,
        };
        let layers = self.symbology.iter().flat_map(|sym| sym.layers.iter());
        for (layer, packets) in layers.enumerate() {
            for (index, packet) in packets.iter().enumerate() {
                out.packet(&format!("s{layer}:{index}"), packet, false)?;
            }
        }
        for page in self.graphic.iter().flat_map(|g| g.pages.iter()) {
            for (index, packet) in page.packets.iter().enumerate() {
                out.packet(&format!("g{}:{index}", page.number), packet, true)?;
            }
        }
        Ok(out.lines)
    }
}

/// The records being built.
struct Records {
    lines: Vec<String>,
    bytes: usize,
    /// The sweep the next data array becomes.
    sweep: usize,
    /// Product code, for errors.
    code: i16,
}

impl Records {
    fn push(&mut self, line: String) -> Result<(), Level3Error> {
        self.bytes = self.bytes.saturating_add(line.len() + 1);
        if self.bytes > MAX_RECORD_BYTES {
            return Err(Level3Error::InvalidMessage {
                code: self.code,
                reason: format!("display packets render to more than {MAX_RECORD_BYTES} bytes"),
            });
        }
        self.lines.push(line);
        Ok(())
    }

    /// One packet; `aside` for a packet that is not a top-level packet of
    /// a symbology layer, whose data arrays [`Level3Product::data_arrays`]
    /// does not convert.
    fn packet(&mut self, place: &str, packet: &Packet, aside: bool) -> Result<(), Level3Error> {
        let mut line = format!("{place} {}", code_text(packet.code()));
        match packet {
            Packet::Radial(_) | Packet::Raster(_) | Packet::DigitalPrecip(_) if aside => {
                let _ = write!(line, " unconverted {packet:?}");
            }
            Packet::Radial(_) | Packet::Raster(_) | Packet::DigitalPrecip(_) => {
                let _ = write!(line, " sweep {}", self.sweep);
                self.sweep += 1;
            }
            Packet::Generic(generic) => return self.generic(place, line, generic, aside),
            Packet::Text(t) => {
                if let Some(color) = t.color_level {
                    let _ = write!(line, " {color}");
                }
                let _ = write!(line, " {} {} {:?}", t.i, t.j, t.text);
            }
            Packet::Vectors(v) => {
                if let Some(color) = v.color_level {
                    let _ = write!(line, " {color}");
                }
                vectors(&mut line, &v.vectors);
            }
            Packet::Contour(c) => match &c.contour {
                Contour::ColorLevel(level) => {
                    let _ = write!(line, " {level}");
                }
                Contour::Vectors(v) => vectors(&mut line, v),
            },
            Packet::Symbol(symbol) => {
                if let SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested) = symbol
                {
                    let _ = write!(line, " nested={}", nested.len());
                    self.push(line)?;
                    for (index, inner) in nested.iter().enumerate() {
                        self.packet(&format!("{place}.{index}"), inner, true)?;
                    }
                    return Ok(());
                }
                symbol_fields(&mut line, symbol);
            }
            Packet::Irm(IrmPacket::Parameters { values }) => {
                for value in values {
                    let _ = write!(line, " {value}");
                }
            }
            Packet::Irm(IrmPacket::StormCount { count }) => {
                let _ = write!(line, " {count}");
            }
            Packet::Unknown { bytes, .. } => {
                line.push_str(" unknown ");
                hex(&mut line, bytes);
            }
        }
        self.push(line)
    }

    fn generic(
        &mut self,
        place: &str,
        mut line: String,
        generic: &GenericPacket,
        aside: bool,
    ) -> Result<(), Level3Error> {
        let p = &generic.product;
        let _ = write!(
            line,
            " name={:?} description={:?} product_code={} product_type={} generation_time={} \
             radar_name={:?} latitude={} longitude={} height={} volume_time={} \
             elevation_time={} elevation_angle={} volume_number={} operational_mode={} vcp={} \
             elevation_number={} compression={} uncompressed_size={} parameters=",
            p.name,
            p.description,
            p.product_code,
            p.product_type,
            p.generation_time,
            p.radar_name,
            p.radar_latitude,
            p.radar_longitude,
            p.radar_height,
            p.volume_time,
            p.elevation_time,
            p.elevation_angle,
            p.volume_number,
            p.operational_mode,
            p.vcp,
            p.elevation_number,
            p.compression,
            p.uncompressed_size,
        );
        parameters(&mut line, &p.parameters);
        let _ = write!(line, " components={}", generic.components.len());
        if let Some(spares) = p.external_spares {
            let _ = write!(line, " external_spares={}", join(&spares));
        }
        self.push(line)?;
        let code = code_text(generic.code);
        for (index, component) in generic.components.iter().enumerate() {
            self.component(&format!("{place}.c{index}"), &code, component, aside)?;
        }
        Ok(())
    }

    /// One generic component; `nested` for the components of an event or
    /// of a packet that is not a top-level symbology packet, which
    /// [`Level3Product::data_arrays`] does not convert.
    fn component(
        &mut self,
        place: &str,
        code: &str,
        component: &GenericComponent,
        nested: bool,
    ) -> Result<(), Level3Error> {
        let mut line = format!("{place} {code}");
        match component {
            GenericComponent::Radial(_) if !nested => {
                let _ = write!(line, " radial sweep {}", self.sweep);
                self.sweep += 1;
            }
            GenericComponent::Radial(radial) => {
                let _ = write!(
                    line,
                    " radial description={:?} bin_size={} range_to_first_bin={} parameters=",
                    radial.description, radial.bin_size, radial.range_to_first_bin
                );
                parameters(&mut line, &radial.parameters);
                let _ = write!(line, " radials={}", radial.radials.len());
                for r in &radial.radials {
                    let _ = write!(
                        line,
                        " {},{},{},{},{:?}:{}",
                        r.azimuth,
                        r.elevation,
                        r.width,
                        r.num_bins,
                        r.attributes,
                        join(&r.values)
                    );
                }
            }
            GenericComponent::Text {
                parameters: p,
                text,
            } => {
                line.push_str(" text parameters=");
                parameters(&mut line, p);
                let _ = write!(line, " {text:?}");
            }
            GenericComponent::Grid(grid) => {
                let _ = write!(
                    line,
                    " grid type={} dimensions={} parameters=",
                    grid.grid_type,
                    join(&grid.dimensions)
                );
                parameters(&mut line, &grid.parameters);
                let _ = write!(line, " attributes={:?} values=", grid.data.attributes);
                match &grid.data.values {
                    GenericValues::Int(v) => line.push_str(&join(v)),
                    GenericValues::Float(v) => line.push_str(&join(v)),
                    GenericValues::Double(v) => line.push_str(&join(v)),
                }
            }
            GenericComponent::Area(area) => {
                let _ = write!(line, " area type={} parameters=", area.area_type);
                parameters(&mut line, &area.parameters);
                line.push_str(" points=");
                let points: Vec<String> = area
                    .points
                    .iter()
                    .map(|(x, y)| format!("{x},{y}"))
                    .collect();
                line.push_str(&points.join(" "));
            }
            GenericComponent::Table(table) => {
                let _ = write!(
                    line,
                    " table {:?} columns={} rows={} parameters=",
                    table.title, table.columns, table.rows
                );
                parameters(&mut line, &table.parameters);
                let _ = write!(
                    line,
                    " column_labels={} row_labels={} entries={}",
                    quoted(&table.column_labels),
                    quoted(&table.row_labels),
                    quoted(&table.entries)
                );
            }
            GenericComponent::Event(event) => {
                line.push_str(" event parameters=");
                parameters(&mut line, &event.parameters);
                let _ = write!(line, " components={}", event.components.len());
                self.push(line)?;
                // Event nesting is bounded by the decoder (MAX_EVENT_DEPTH).
                for (index, inner) in event.components.iter().enumerate() {
                    self.component(&format!("{place}.c{index}"), code, inner, true)?;
                }
                return Ok(());
            }
            GenericComponent::Undecoded { kind, bytes } => {
                let _ = write!(line, " undecoded kind={kind} ");
                hex(&mut line, bytes);
            }
        }
        self.push(line)
    }
}

/// A packet code as the ICD writes it: decimal below 256, else hexadecimal.
fn code_text(code: u16) -> String {
    if code < 256 {
        code.to_string()
    } else {
        format!("0x{code:04X}")
    }
}

fn vectors(line: &mut String, vectors: &Vectors) {
    match vectors {
        Vectors::Linked(points) => {
            for Point { i, j } in points {
                let _ = write!(line, " {i},{j}");
            }
        }
        Vectors::Unlinked(segments) => {
            for Segment { begin, end } in segments {
                let _ = write!(line, " {},{},{},{}", begin.i, begin.j, end.i, end.j);
            }
        }
    }
}

fn symbol_fields(line: &mut String, symbol: &SymbolPacket) {
    match symbol {
        SymbolPacket::Mesocyclone(circles)
        | SymbolPacket::CorrelatedShear(circles)
        | SymbolPacket::StiCircles(circles) => {
            for c in circles {
                let _ = write!(line, " {},{},{}", c.i, c.j, c.radius);
            }
        }
        SymbolPacket::WindBarbs(barbs) => {
            for b in barbs {
                let _ = write!(
                    line,
                    " {},{},{},{},{}",
                    b.color_level, b.x, b.y, b.direction_deg, b.speed_kt
                );
            }
        }
        SymbolPacket::VectorArrows(arrows) => {
            for a in arrows {
                let _ = write!(
                    line,
                    " {},{},{},{},{}",
                    a.i, a.j, a.direction_deg, a.arrow_length, a.head_length
                );
            }
        }
        SymbolPacket::Tvs(positions)
        | SymbolPacket::HailPositive(positions)
        | SymbolPacket::HailProbable(positions)
        | SymbolPacket::Etvs(positions) => {
            for p in positions {
                let _ = write!(line, " {},{}", p.i, p.j);
            }
        }
        SymbolPacket::StormIds(ids) => {
            for s in ids {
                let _ = write!(line, " {},{},{:?}", s.i, s.j, s.id);
            }
        }
        SymbolPacket::HdaHail(hail) => {
            for h in hail {
                let _ = write!(
                    line,
                    " {},{},{},{},{}",
                    h.i,
                    h.j,
                    h.probability_of_hail,
                    h.probability_of_severe_hail,
                    h.max_hail_size_in
                );
            }
        }
        SymbolPacket::PointFeatures(features) => {
            for f in features {
                let _ = write!(line, " {},{},{},{}", f.i, f.j, f.feature_type, f.attribute);
            }
        }
        SymbolPacket::CellTrend(trend) => {
            let _ = write!(line, " {:?} {} {}", trend.id, trend.i, trend.j);
            for t in &trend.trends {
                let _ = write!(line, " {}:", t.code);
                volume_list(line, &t.volumes);
            }
        }
        SymbolPacket::CellTrendTimes(times) => {
            line.push(' ');
            volume_list(line, times);
        }
        // Rendered with their nested packets by the caller.
        SymbolPacket::ScitPast(_) | SymbolPacket::ScitForecast(_) => {}
    }
}

fn volume_list(line: &mut String, list: &VolumeList) {
    let _ = write!(line, "{}:{}", list.latest, join(&list.values));
}

fn parameters(line: &mut String, parameters: &[GenericParameter]) {
    let pairs: Vec<String> = parameters
        .iter()
        .map(|p| format!("{:?}={:?}", p.id, p.attributes))
        .collect();
    let _ = write!(line, "[{}]", pairs.join(","));
}

fn join<T: std::fmt::Display>(values: &[T]) -> String {
    let parts: Vec<String> = values.iter().map(ToString::to_string).collect();
    parts.join(",")
}

fn quoted(values: &[String]) -> String {
    let parts: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
    format!("[{}]", parts.join(","))
}

fn hex(line: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(line, "{byte:02x}");
    }
}
