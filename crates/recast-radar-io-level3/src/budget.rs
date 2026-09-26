//! The allocation budget of one product decode.
//!
//! Every packet limit ([`crate::packets::radial::MAX_RADIAL_CELLS`],
//! [`crate::packets::raster::MAX_GRID_DIMENSION`], ...) bounds one packet.
//! A product may hold any number of packets, and the decompressed data they
//! come from ([`crate::decode_product`] caps it at 16 MiB) can be produced by
//! a few hundred bytes of bzip2. Run-length rows expand up to 128 cells per
//! byte (packet 17) and a 4-byte packet becomes a
//! [`Packet`](crate::Packet) of over 200 bytes, so the sum over all packets
//! needs its own bound: the product decode budget,
//! [`MAX_PRODUCT_DECODED_BYTES`].
//!
//! The decoders charge the budget before they allocate anything whose size
//! the input drives: the data levels of radial and raster packets (one byte
//! per cell), every decoded packet, the elements of every list (records,
//! points, radials, parameters, components, text lines and pages) at their
//! in-memory size, and the bytes of every string and copied byte range. A
//! growing list is charged its capacity growth before it grows. A decode
//! that would exceed the budget stops with
//! [`Level3Error::ProductTooLarge`].
//!
//! Parsing a radar coded message's text
//! ([`crate::Level3Product::radar_coded_message`]) happens after the decode,
//! and is charged the same way to a budget of its own,
//! [`crate::rcm::MAX_RCM_PARSED_BYTES`].

use std::mem::size_of;

use crate::Level3Error;

/// Most bytes one call of [`crate::decode_product`] or
/// [`crate::decode_message`] may allocate for the product it decodes, not
/// counting the decompressed message (at most 16 MiB) the product is decoded
/// from.
///
/// Radial and raster levels cost one byte per cell: the largest ICD array
/// (720 x 1840 bins) is 1.3 MB of levels. Measured with a counting allocator
/// (2026-09-25), no committed real product allocates more than 2.7 MB at
/// peak while it decodes, decompression included, besides the bzip2
/// decoder's working memory (about 7 MB, allocated once per thread and
/// reused). Converting a decoded product into a volume has its own limit,
/// [`crate::volume::MAX_VOLUME_BYTES`].
pub const MAX_PRODUCT_DECODED_BYTES: usize = 32 << 20;

/// Running total of the bytes a product decode has allocated, checked
/// against [`MAX_PRODUCT_DECODED_BYTES`] before each allocation.
#[derive(Debug)]
pub(crate) struct Budget {
    used: usize,
    limit: usize,
}

impl Budget {
    /// An empty budget for one product.
    pub(crate) fn product() -> Self {
        Self::with_limit(MAX_PRODUCT_DECODED_BYTES)
    }

    /// An empty budget with another limit (the radar coded message parse,
    /// [`crate::rcm::MAX_RCM_PARSED_BYTES`]; tests).
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    /// Commits `bytes` before they are allocated.
    pub(crate) fn charge_bytes(
        &mut self,
        bytes: usize,
        what: &'static str,
    ) -> Result<(), Level3Error> {
        if bytes > self.limit.saturating_sub(self.used) {
            return Err(Level3Error::ProductTooLarge {
                what,
                needed: bytes,
                used: self.used,
                limit: self.limit,
            });
        }
        self.used += bytes;
        Ok(())
    }

    /// Commits `count` elements of `T` before they are allocated.
    pub(crate) fn charge<T>(
        &mut self,
        count: usize,
        what: &'static str,
    ) -> Result<(), Level3Error> {
        self.charge_bytes(count.saturating_mul(size_of::<T>()), what)
    }

    /// An empty vector with room for `count` elements, charged.
    pub(crate) fn vec<T>(
        &mut self,
        count: usize,
        what: &'static str,
    ) -> Result<Vec<T>, Level3Error> {
        self.charge::<T>(count, what)?;
        Ok(Vec::with_capacity(count))
    }

    /// Appends `item` to `list`, charging the capacity growth (doubling, at
    /// least 4 elements) before the list grows.
    pub(crate) fn push<T>(
        &mut self,
        list: &mut Vec<T>,
        item: T,
        what: &'static str,
    ) -> Result<(), Level3Error> {
        if list.len() == list.capacity() {
            let grow = list.capacity().max(4);
            self.charge::<T>(grow, what)?;
            list.reserve_exact(grow);
        }
        list.push(item);
        Ok(())
    }

    /// A copy of `bytes`, charged.
    pub(crate) fn bytes(
        &mut self,
        bytes: &[u8],
        what: &'static str,
    ) -> Result<Vec<u8>, Level3Error> {
        self.charge_bytes(bytes.len(), what)?;
        Ok(bytes.to_vec())
    }

    /// `bytes` as a string of one `char` per byte (ISO 8859-1), charged
    /// its UTF-8 length.
    pub(crate) fn latin1(
        &mut self,
        bytes: &[u8],
        what: &'static str,
    ) -> Result<String, Level3Error> {
        let len = bytes.len() + bytes.iter().filter(|&&b| b >= 0x80).count();
        self.charge_bytes(len, what)?;
        let mut text = String::with_capacity(len);
        text.extend(bytes.iter().copied().map(char::from));
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_stop_at_the_limit() {
        let mut budget = Budget::with_limit(10);
        budget.charge_bytes(6, "a").unwrap();
        budget.charge::<u16>(2, "b").unwrap();
        match budget.charge_bytes(1, "c") {
            Err(Level3Error::ProductTooLarge {
                what: "c",
                needed: 1,
                used: 10,
                limit: 10,
            }) => {}
            other => panic!("{other:?}"),
        }
        // A count whose size overflows is refused, not wrapped.
        assert!(budget.charge::<u64>(usize::MAX, "d").is_err());
    }

    #[test]
    fn push_charges_capacity_growth_before_growing() {
        // Room for 12 elements: capacity 4, then 8; growing to 16 does not fit.
        let mut budget = Budget::with_limit(12 * size_of::<u32>());
        let mut list = Vec::new();
        for n in 0..8u32 {
            budget.push(&mut list, n, "list").unwrap();
        }
        assert_eq!(budget.used, list.capacity() * size_of::<u32>());
        assert!(budget.push(&mut list, 8, "list").is_err());
        assert_eq!(list.len(), 8);
        assert_eq!(list.capacity(), 8);
    }

    #[test]
    fn latin1_counts_utf8_bytes() {
        let mut budget = Budget::with_limit(3);
        assert_eq!(budget.latin1(b"a\xE9", "text").unwrap(), "a\u{e9}");
        assert!(budget.latin1(b"b", "text").is_err());
    }
}
