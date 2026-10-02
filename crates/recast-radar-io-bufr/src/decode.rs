//! Section 4 data of uncompressed messages: descriptor expansion and bit
//! reading (WMO-No. 306, FM 94 BUFR, regulations 94.5 and the operator
//! table C).
//!
//! The expansion walks section 3's descriptors: an element (F = 0) reads
//! its table B width, a sequence (F = 3) expands to its table D members, a
//! replication (F = 1) repeats the next X descriptors Y times (Y = 0:
//! the count is the next element, a class 31 delayed replication factor),
//! and an operator (F = 2) changes how later elements are read. A
//! replication of a single element (Meteo-France's pixel arrays: a 32-bit
//! count, then hundreds of thousands of 8- or 16-bit codes) is read as one
//! run of codes, without a value per gate.

use crate::BufrError;
use crate::tables::{ElementKind, Tables};

/// Most values (single values plus run codes) one message may decode to.
const MAX_VALUES: usize = 64 * 1024 * 1024;
/// Deepest nesting of sequences and replications.
const MAX_DEPTH: usize = 64;

/// One decoded value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A number (scale and reference applied).
    Number(f64),
    /// All bits set: missing.
    Missing,
    /// CCITT IA5 text, trailing spaces and NULs removed.
    Text(String),
}

/// A decoded item, in data order.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// One element's value.
    Value {
        /// The element (FXXYYY).
        descriptor: u32,
        /// Its value.
        value: Value,
    },
    /// A replicated single element, kept as its raw codes.
    Run {
        /// The element (FXXYYY).
        descriptor: u32,
        /// Bits per code (operators applied).
        width: u32,
        /// Decimal scale (operators applied).
        scale: i32,
        /// Reference value.
        reference: i64,
        /// The codes; all `width` bits set is missing.
        codes: Vec<u32>,
    },
}

impl Item {
    /// The element descriptor.
    pub fn descriptor(&self) -> u32 {
        match self {
            Item::Value { descriptor, .. } | Item::Run { descriptor, .. } => *descriptor,
        }
    }

    /// The number, if this is a numeric value.
    pub fn number(&self) -> Option<f64> {
        match self {
            Item::Value {
                value: Value::Number(number),
                ..
            } => Some(*number),
            _ => None,
        }
    }
}

/// Decode `message`'s data (one subset after another) with `tables`.
pub fn decode_message(
    descriptors: &[u32],
    data: &[u8],
    subsets: u16,
    compressed: bool,
    tables: &Tables,
) -> Result<Vec<Item>, BufrError> {
    if compressed {
        return Err(BufrError::Unsupported(
            "compressed BUFR data (section 3 flag)".into(),
        ));
    }
    let mut decoder = Decoder {
        tables,
        bits: BitReader::new(data),
        out: Vec::new(),
        values: 0,
        width_change: 0,
        scale_change: 0,
        text_width: None,
        local_width: None,
        increase: 0,
    };
    for _ in 0..subsets.max(1) {
        decoder.run(descriptors, 0)?;
    }
    Ok(decoder.out)
}

struct Decoder<'a> {
    tables: &'a Tables,
    bits: BitReader<'a>,
    out: Vec<Item>,
    values: usize,
    /// Operator 2-01-YYY: bits added to non-code elements.
    width_change: i32,
    /// Operator 2-02-YYY: added to the scale of non-code elements.
    scale_change: i32,
    /// Operator 2-08-YYY: width of CCITT IA5 elements, in bits.
    text_width: Option<u32>,
    /// Operator 2-06-YYY: width of the next (local) element.
    local_width: Option<u32>,
    /// Operator 2-07-YYY: scale, reference and width increase.
    increase: i32,
}

/// How one element is read under the operators in force.
#[derive(Clone, Copy)]
struct Reading {
    width: u32,
    scale: i32,
    reference: i64,
    kind: ElementKind,
}

impl Decoder<'_> {
    fn count(&mut self, n: usize) -> Result<(), BufrError> {
        self.values = self.values.saturating_add(n);
        if self.values > MAX_VALUES {
            return Err(BufrError::Limit(format!(
                "message decodes to more than {MAX_VALUES} values"
            )));
        }
        Ok(())
    }

    fn reading(&self, descriptor: u32) -> Result<Reading, BufrError> {
        let element = self.tables.element(descriptor).ok_or_else(|| {
            BufrError::Format(format!("element {descriptor:06} is in no table B"))
        })?;
        let class = descriptor / 1000 % 100;
        Ok(match element.kind {
            ElementKind::Text => Reading {
                width: self.text_width.unwrap_or(element.width),
                scale: 0,
                reference: 0,
                kind: ElementKind::Text,
            },
            // Code and flag tables, and class 31 (replication factors and
            // the like), are read as the table gives them.
            ElementKind::Code => Reading {
                width: element.width,
                scale: 0,
                reference: element.reference,
                kind: ElementKind::Code,
            },
            ElementKind::Numeric if class == 31 => Reading {
                width: element.width,
                scale: element.scale,
                reference: element.reference,
                kind: ElementKind::Numeric,
            },
            ElementKind::Numeric => {
                let increase = self.increase;
                let width = i64::from(element.width)
                    + i64::from(self.width_change)
                    + i64::from((10 * increase + 2) / 3);
                let width = u32::try_from(width)
                    .ok()
                    .filter(|width| (1..=64).contains(width))
                    .ok_or_else(|| {
                        BufrError::Format(format!("element {descriptor:06}: width {width} bits"))
                    })?;
                let reference = if increase > 0 {
                    element
                        .reference
                        .checked_mul(10i64.pow(increase.unsigned_abs()))
                        .ok_or_else(|| BufrError::Format("reference value overflow".into()))?
                } else {
                    element.reference
                };
                Reading {
                    width,
                    scale: element.scale + self.scale_change + increase,
                    reference,
                    kind: ElementKind::Numeric,
                }
            }
        })
    }

    /// Read element `descriptor` and push its value.
    fn element(&mut self, descriptor: u32) -> Result<Value, BufrError> {
        if let Some(width) = self.local_width.take()
            && self.tables.element(descriptor).is_none()
        {
            // 2-06-YYY: an element no table describes, skipped by width.
            self.bits.skip(width)?;
            return Ok(Value::Missing);
        }
        let reading = self.reading(descriptor)?;
        self.count(1)?;
        let value = self.read_value(descriptor, reading)?;
        self.out.push(Item::Value {
            descriptor,
            value: value.clone(),
        });
        Ok(value)
    }

    fn read_value(&mut self, descriptor: u32, reading: Reading) -> Result<Value, BufrError> {
        if reading.kind == ElementKind::Text {
            if !reading.width.is_multiple_of(8) {
                return Err(BufrError::Format(format!(
                    "text element {descriptor:06} of {} bits",
                    reading.width
                )));
            }
            let mut text = Vec::with_capacity(reading.width as usize / 8);
            for _ in 0..reading.width / 8 {
                text.push(self.bits.read(8)? as u8);
            }
            if text.iter().all(|&byte| byte == 0xFF) {
                return Ok(Value::Missing);
            }
            let text: String = text.iter().map(|&byte| char::from(byte)).collect();
            return Ok(Value::Text(
                text.trim_end_matches([' ', '\0']).trim_start().to_owned(),
            ));
        }
        let raw = self.bits.read(reading.width)?;
        // All ones is missing, except a delayed replication factor (class
        // 31) and 1-bit values.
        let class = descriptor / 1000 % 100;
        if reading.width > 1 && class != 31 && raw == ones(reading.width) {
            return Ok(Value::Missing);
        }
        Ok(Value::Number(physical(
            raw,
            reading.reference,
            reading.scale,
        )))
    }

    fn run(&mut self, descriptors: &[u32], depth: usize) -> Result<(), BufrError> {
        if depth > MAX_DEPTH {
            return Err(BufrError::Limit(format!(
                "descriptors nest deeper than {MAX_DEPTH}"
            )));
        }
        let mut i = 0;
        while i < descriptors.len() {
            let descriptor = descriptors[i];
            let (f, x, y) = fxy(descriptor);
            match f {
                0 => {
                    self.element(descriptor)?;
                    i += 1;
                }
                3 => {
                    let members = self
                        .tables
                        .sequence(descriptor)
                        .ok_or_else(|| {
                            BufrError::Format(format!("sequence {descriptor:06} is in no table D"))
                        })?
                        .to_vec();
                    self.run(&members, depth + 1)?;
                    i += 1;
                }
                2 => {
                    self.operator(x, y)?;
                    i += 1;
                }
                1 => {
                    let x = x as usize;
                    let (count, body_start) = if y == 0 {
                        let factor = *descriptors.get(i + 1).ok_or_else(|| {
                            BufrError::Format("delayed replication without its factor".into())
                        })?;
                        if fxy(factor).0 != 0 || fxy(factor).1 != 31 {
                            return Err(BufrError::Format(format!(
                                "delayed replication factor {factor:06} is not a class 31 element"
                            )));
                        }
                        let count = match self.element(factor)? {
                            Value::Number(count) if count >= 0.0 => count as usize,
                            _ => 0,
                        };
                        (count, i + 2)
                    } else {
                        (y as usize, i + 1)
                    };
                    let body = descriptors.get(body_start..body_start + x).ok_or_else(|| {
                        BufrError::Format(format!(
                            "replication of {x} descriptors runs past the list"
                        ))
                    })?;
                    if !self.replicate_single_element(body, count)? {
                        for _ in 0..count {
                            self.run(body, depth + 1)?;
                        }
                    }
                    i = body_start + x;
                }
                _ => unreachable!("F is two bits"),
            }
        }
        Ok(())
    }

    /// A replication whose body is one element between data width or
    /// scale operators: read every repetition into one run of codes.
    /// Returns false (nothing read) for any other body.
    fn replicate_single_element(&mut self, body: &[u32], count: usize) -> Result<bool, BufrError> {
        let elements: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, d)| fxy(**d).0 == 0)
            .map(|(index, _)| index)
            .collect();
        let only_width_ops = body.iter().all(|d| {
            let (f, x, _) = fxy(*d);
            f == 0 || (f == 2 && (x == 1 || x == 2))
        });
        if elements.len() != 1 || !only_width_ops || count < 2 {
            return Ok(false);
        }
        let at = elements[0];
        let descriptor = body[at];
        for d in &body[..at] {
            let (_, x, y) = fxy(*d);
            self.operator(x, y)?;
        }
        let reading = self.reading(descriptor)?;
        if reading.kind == ElementKind::Text || reading.width > 32 {
            // Undo nothing: the operators before the element are in force
            // for the generic path, which repeats them harmlessly.
            return Ok(false);
        }
        self.count(count)?;
        let mut codes = Vec::new();
        codes
            .try_reserve_exact(count)
            .map_err(|err| BufrError::Limit(format!("cannot hold {count} codes: {err}")))?;
        self.bits.read_many(reading.width, count, &mut codes)?;
        self.out.push(Item::Run {
            descriptor,
            width: reading.width,
            scale: reading.scale,
            reference: reading.reference,
            codes,
        });
        for d in &body[at + 1..] {
            let (_, x, y) = fxy(*d);
            self.operator(x, y)?;
        }
        Ok(true)
    }

    fn operator(&mut self, x: u32, y: u32) -> Result<(), BufrError> {
        let signed = |y: u32| if y == 0 { 0 } else { y as i32 - 128 };
        match x {
            1 => self.width_change = signed(y),
            2 => self.scale_change = signed(y),
            6 => self.local_width = Some(y),
            7 => self.increase = y as i32,
            8 => self.text_width = (y != 0).then_some(y * 8),
            5 => {
                // Y characters of text, inserted in the data.
                let mut text = String::new();
                for _ in 0..y {
                    text.push(char::from(self.bits.read(8)? as u8));
                }
                self.count(1)?;
                self.out.push(Item::Value {
                    descriptor: 205_000 + y,
                    value: Value::Text(text.trim_end().to_owned()),
                });
            }
            _ => {
                return Err(BufrError::Unsupported(format!("operator 2-{x:02}-{y:03}")));
            }
        }
        Ok(())
    }
}

/// F, X and Y of an FXXYYY descriptor.
pub fn fxy(descriptor: u32) -> (u32, u32, u32) {
    (
        descriptor / 100_000,
        descriptor / 1000 % 100,
        descriptor % 1000,
    )
}

fn ones(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// `(raw + reference) / 10^scale`.
pub fn physical(raw: u64, reference: i64, scale: i32) -> f64 {
    let value = raw as f64 + reference as f64;
    if scale == 0 {
        value
    } else {
        value / 10f64.powi(scale)
    }
}

/// Big-endian bit reader over section 4.
struct BitReader<'a> {
    data: &'a [u8],
    bit: u64,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn check(&self, bits: u64) -> Result<(), BufrError> {
        if self.bit + bits > self.data.len() as u64 * 8 {
            return Err(BufrError::Format(format!(
                "section 4 ends at bit {} before {} more bits",
                self.data.len() * 8,
                bits
            )));
        }
        Ok(())
    }

    fn skip(&mut self, bits: u32) -> Result<(), BufrError> {
        self.check(u64::from(bits))?;
        self.bit += u64::from(bits);
        Ok(())
    }

    /// The next `width` bits (at most 64).
    fn read(&mut self, width: u32) -> Result<u64, BufrError> {
        if width == 0 {
            return Ok(0);
        }
        self.check(u64::from(width))?;
        let mut value = 0u64;
        let mut remaining = width;
        while remaining > 0 {
            let byte = self.data[(self.bit / 8) as usize];
            let offset = (self.bit % 8) as u32;
            let take = remaining.min(8 - offset);
            let bits = (u32::from(byte) >> (8 - offset - take)) & ((1 << take) - 1);
            // At most 8 bits a step: the shift never reaches 64.
            value = (value << take) | u64::from(bits);
            remaining -= take;
            self.bit += u64::from(take);
        }
        Ok(value)
    }

    /// `count` codes of `width` bits (1 to 32) into `out`.
    fn read_many(&mut self, width: u32, count: usize, out: &mut Vec<u32>) -> Result<(), BufrError> {
        let total = u64::from(width) * count as u64;
        self.check(total)?;
        let mask = (1u64 << width) - 1;
        let data = self.data;
        let mut bit = self.bit;
        if width == 8 && bit.is_multiple_of(8) {
            let start = (bit / 8) as usize;
            out.extend(data[start..start + count].iter().map(|&b| u32::from(b)));
        } else if width == 16 && bit.is_multiple_of(8) {
            let start = (bit / 8) as usize;
            out.extend(
                data[start..start + 2 * count]
                    .chunks_exact(2)
                    .map(|pair| u32::from(u16::from_be_bytes([pair[0], pair[1]]))),
            );
        } else {
            for _ in 0..count {
                let byte = (bit / 8) as usize;
                // Up to 32 bits plus a 7-bit offset fit in 5 bytes.
                let mut window = 0u64;
                for k in 0..5 {
                    window = (window << 8) | u64::from(*data.get(byte + k).unwrap_or(&0));
                }
                let shift = 40 - (bit % 8) - u64::from(width);
                out.push(((window >> shift) & mask) as u32);
                bit += u64::from(width);
            }
        }
        self.bit += total;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_across_bytes() {
        let data = [0b1010_1100, 0b0101_0011, 0xFF, 0x00, 0x12, 0x34];
        let mut bits = BitReader::new(&data);
        assert_eq!(bits.read(3).unwrap(), 0b101);
        assert_eq!(bits.read(7).unwrap(), 0b011_0001);
        assert_eq!(bits.read(6).unwrap(), 0b01_0011);
        let mut out = Vec::new();
        bits.read_many(8, 2, &mut out).unwrap();
        assert_eq!(out, [0xFF, 0x00]);
        out.clear();
        bits.read_many(4, 4, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
        assert!(bits.read(1).is_err());
    }

    /// A hand-assembled message body: 004001 (12 bits, ref 0) = 2013, a
    /// delayed replication 101000 031001 (8 bits) of 012001-like element
    /// 030001 (4 bits) three times, then a missing 004002 (4 bits all ones).
    #[test]
    fn expands_elements_replications_and_runs() {
        let tables = Tables::for_message(0, 0);
        // 2013 = 0b0111_1101_1101 (12 bits); count 3 (8 bits); codes 1, 2,
        // 15 (4 bits each); 004002 month all ones (4 bits): 36 bits.
        let bits = "011111011101".to_owned() + "00000011" + "0001" + "0010" + "1111" + "1111";
        let mut data = Vec::new();
        let padded = format!("{bits:0<40}");
        for chunk in padded.as_bytes().chunks(8) {
            data.push(u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 2).unwrap());
        }
        let items = decode_message(
            &[4001, 101_000, 31001, 30001, 4002],
            &data,
            1,
            false,
            tables,
        )
        .unwrap();
        assert_eq!(items[0].number(), Some(2013.0));
        assert_eq!(items[1].number(), Some(3.0));
        match &items[2] {
            Item::Run {
                descriptor,
                codes,
                width,
                ..
            } => {
                assert_eq!((*descriptor, *width), (30001, 4));
                assert_eq!(codes, &[1, 2, 15]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            items[3],
            Item::Value {
                descriptor: 4002,
                value: Value::Missing
            }
        );
    }

    #[test]
    fn compressed_data_and_unknown_operators_are_refused() {
        let tables = Tables::for_message(0, 0);
        assert!(decode_message(&[4001], &[0, 0], 1, true, tables).is_err());
        assert!(decode_message(&[203_010, 4001], &[0, 0, 0], 1, false, tables).is_err());
    }
}
