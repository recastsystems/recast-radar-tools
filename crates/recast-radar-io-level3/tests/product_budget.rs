//! The product decode budget ([`MAX_PRODUCT_DECODED_BYTES`]), the volume
//! limits ([`MAX_VOLUME_BYTES`], [`MAX_SWEEPS_PER_VOLUME`]) and the radar
//! coded message parse limit ([`MAX_RCM_PARSED_BYTES`]): what the packets of
//! one product allocate together, what its volume holds, and what parsing
//! its radar coded message allocates are bounded, not only what each packet
//! allocates.
//!
//! Each input is a real uncompressed product whose symbology layer (or, for
//! the radar coded message, whose list of intensity groups) is repeated in
//! place (the layer, block and message lengths and the later block offsets
//! patched to match), so that the product holds many copies of the same
//! real packets or groups. No value of a packet or group is changed. This is
//! a resource bound on hostile input, not a check of decoded values: before
//! these limits, the per-packet limits let a 1,462-byte bzip2 product of
//! packet 17 arrays allocate 1.9 GB, a 764-byte product of empty packets
//! 923 MB, the volume of a 367-byte product of 1 800 hourly precipitation
//! arrays 571 MB, and the volume of a 760-byte product 74 holding 13.8 MB
//! of groups 504 MB (counting allocator, 2026-09-25).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::mem::size_of;

use recast_radar_core::bounded_read::MAX_SWEEPS_PER_VOLUME;
use recast_radar_io_level3::rcm::MAX_RCM_PARSED_BYTES;
use recast_radar_io_level3::volume::MAX_VOLUME_BYTES;
use recast_radar_io_level3::{Level3Error, MAX_PRODUCT_DECODED_BYTES, Packet, decode_product};

/// A real uncompressed product with the location of one symbology layer.
struct RealLayer {
    id: &'static str,
    bytes: Vec<u8>,
    /// File offset of the Message Header Block.
    message_start: usize,
    /// File offset of the Product Symbology Block.
    block: usize,
    /// File offset of the layer's divider.
    layer: usize,
    /// Index of the layer.
    index: usize,
    /// Layer length in bytes (after its 6-byte header).
    len: usize,
}

impl RealLayer {
    fn locate(id: &'static str, index: usize) -> Self {
        let entry = common::entry(id);
        let golden = entry.golden();
        let bytes = entry.bytes();
        let message_start = common::uncompressed_message_start(&golden, bytes.len());
        let halfwords = golden.get("halfwords").items();
        let offset_hw = (halfwords[54].int("hw55") << 16) | halfwords[55].int("hw56");
        let block = message_start + 2 * usize::try_from(offset_hw).unwrap();
        let layers = golden.get("blocks").get("symbology").get("layers").items();
        // Block header (divider, ID, length, layer count): 10 bytes; layer
        // header (divider, length): 6 bytes.
        let mut layer = block + 10;
        for skipped in &layers[..index] {
            layer += 6 + usize::try_from(skipped.get("length").int("layer length")).unwrap();
        }
        let len = usize::try_from(layers[index].get("length").int("layer length")).unwrap();
        assert_eq!(bytes[layer..layer + 2], [0xFF, 0xFF], "{id}: layer divider");
        assert_eq!(
            u32::from_be_bytes(bytes[layer + 2..layer + 6].try_into().unwrap()) as usize,
            len
        );
        Self {
            id,
            bytes,
            message_start,
            block,
            layer,
            index,
            len,
        }
    }

    /// The layer's packets as the unmodified file decodes them.
    fn packets(&self) -> Vec<Packet> {
        let product = decode_product(&self.bytes).unwrap_or_else(|e| panic!("{}: {e}", self.id));
        product.symbology.unwrap().layers.swap_remove(self.index)
    }

    /// The file with the layer's contents repeated `copies` times.
    fn repeated(&self, copies: usize) -> Vec<u8> {
        // The committed file, read again: every byte written below is one of
        // its bytes or one of its length fields, patched.
        let path = recast_radar_testdata::path(self.id).unwrap_or_else(|e| panic!("{e}"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(bytes, self.bytes);
        let start = self.layer + 6;
        let contents = &bytes[start..start + self.len];
        let mut out = bytes[..start].to_vec();
        for _ in 0..copies {
            out.extend_from_slice(contents);
        }
        out.extend_from_slice(&bytes[start + self.len..]);
        let grown = (copies - 1) * self.len;
        let add = |out: &mut Vec<u8>, at: usize, amount: usize| {
            let old = u32::from_be_bytes(out[at..at + 4].try_into().unwrap());
            let new = u32::try_from(old as usize + amount).unwrap();
            out[at..at + 4].copy_from_slice(&new.to_be_bytes());
        };
        add(&mut out, self.layer + 2, grown); // layer length
        add(&mut out, self.block + 4, grown); // block length
        add(&mut out, self.message_start + 8, grown); // message length (halfwords 5-6)
        // Offsets of the graphic and tabular blocks after the symbology
        // block (halfwords 57-58 and 59-60), in halfwords.
        assert_eq!(grown % 2, 0);
        for at in [self.message_start + 112, self.message_start + 116] {
            let offset = u32::from_be_bytes(out[at..at + 4].try_into().unwrap()) as usize;
            if self.message_start + 2 * offset > self.block {
                add(&mut out, at, grown / 2);
            }
        }
        out
    }
}

/// Decoding `file` stops with [`Level3Error::ProductTooLarge`] at the budget.
fn assert_too_large(file: &[u8], what: &str) {
    match decode_product(file) {
        Err(Level3Error::ProductTooLarge {
            needed,
            used,
            limit,
            ..
        }) => {
            assert_eq!(limit, MAX_PRODUCT_DECODED_BYTES, "{what}");
            assert!(used <= limit && needed > limit - used, "{what}");
        }
        other => panic!(
            "{what}: expected ProductTooLarge, got {:?}",
            other.map(|_| ())
        ),
    }
}

/// Room left for `copies` copies of a layer after a 1 MiB allowance for the
/// rest of the product: each copy charged `per_copy` bytes, and the list of
/// packets up to twice its length.
fn fits(copies: usize, packets_per_copy: usize, per_copy: usize) -> bool {
    let packets = (copies * packets_per_copy).next_power_of_two();
    packets * size_of::<Packet>() + copies * per_copy + (1 << 20) <= MAX_PRODUCT_DECODED_BYTES
}

/// Largest copy count that [`fits`].
fn most_copies(packets_per_copy: usize, per_copy: usize) -> usize {
    let mut copies = 1;
    while fits(copies + 1, packets_per_copy, per_copy) {
        copies += 1;
    }
    copies
}

/// Radial and raster packets repeated: the copies decode while their levels
/// fit the budget and are refused once the levels alone exceed it.
#[test]
fn data_levels_of_all_packets_share_the_budget() {
    for (id, layer, code) in [
        // KTLX 2013 DPA: packet 17, 131 x 131 boxes in 2 840 bytes (byte pairs,
        // width from the header).
        ("l3-tlx-dpa-20130520-2016", 0, 17),
        // KTLX 2013 NCR: 0xBA07, 464 x 464 cells in 28 900 bytes (4-bit runs,
        // width from the first row).
        ("l3-tlx-ncr-20130520-2016", 0, 0xBA07),
        // KFWS 1995 N0R: 0xAF1F, 367 radials of 230 bins in 18 924 bytes.
        ("l3-fws-n0r-19950517-2304", 0, 0xAF1F),
    ] {
        let real = RealLayer::locate(id, layer);
        let original = real.packets();
        assert_eq!(original.len(), 1, "{id}");
        assert_eq!(original[0].code(), code, "{id}");
        let cells = match &original[0] {
            Packet::Radial(p) => p.levels.len(),
            Packet::Raster(p) => p.grid.levels().len(),
            Packet::DigitalPrecip(p) => p.grid.levels().len(),
            other => panic!("{id}: {other:?}"),
        };

        // Levels, angles and the other layers: at most the cells plus the
        // layer's bytes per copy.
        let copies = most_copies(1, cells + real.len);
        let product = decode_product(&real.repeated(copies)).unwrap();
        let packets = &product.symbology.as_ref().unwrap().layers[layer];
        assert_eq!(packets.len(), copies, "{id}");
        assert!(packets.iter().all(|p| *p == original[0]), "{id}");

        // The levels alone exceed the budget.
        let copies = MAX_PRODUCT_DECODED_BYTES / cells + 1;
        assert_too_large(&real.repeated(copies), id);
    }
}

/// Packets without data levels repeated: every decoded packet is charged, so
/// the number of packets is bounded too.
#[test]
fn every_packet_is_charged() {
    // KTLX 2013 VWP: layer 0 holds 364 packets (298 wind barbs, 4; 63 text,
    // 8; 3 unlinked vectors, 10) in 5 344 bytes, 15 bytes a packet.
    let real = RealLayer::locate("l3-tlx-nvw-20130520-2016", 0);
    let original = real.packets();
    assert_eq!(original.len(), 364);
    assert!(original.iter().all(|p| matches!(p.code(), 4 | 8 | 10)));

    // Wind barbs, text and vectors: at most four times the layer's bytes per copy.
    let copies = most_copies(original.len(), 4 * real.len);
    let product = decode_product(&real.repeated(copies)).unwrap();
    let packets = &product.symbology.as_ref().unwrap().layers[0];
    assert_eq!(packets.len(), copies * original.len());
    assert!(packets.chunks(original.len()).all(|copy| copy == original));

    // The packets alone exceed the budget.
    let copies = MAX_PRODUCT_DECODED_BYTES / (original.len() * size_of::<Packet>()) + 1;
    assert_too_large(&real.repeated(copies), "VWP packets");
}

/// The shape found in review: a small bzip2 product whose decompressed
/// packets expand past the budget. The data after the Product Description
/// Block of the repeated KTLX 2013 DPA is compressed with libbzip2, as real
/// compressed products are; the decode is refused at the budget.
#[test]
fn a_small_compressed_product_is_refused_at_the_budget() {
    let real = RealLayer::locate("l3-tlx-dpa-20130520-2016", 0);
    let copies = MAX_PRODUCT_DECODED_BYTES / (131 * 131) + 1;
    let repeated = real.repeated(copies);
    // Message Header Block and Product Description Block: 120 bytes.
    let body = real.message_start + 120;
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
    encoder.write_all(&repeated[body..]).unwrap();
    let mut compressed = repeated[..body].to_vec();
    compressed.extend(encoder.finish().unwrap());
    let length = u32::try_from(compressed.len() - real.message_start).unwrap();
    compressed[real.message_start + 8..real.message_start + 12]
        .copy_from_slice(&length.to_be_bytes());
    assert!(
        compressed.len() < 64 << 10,
        "{} compressed bytes",
        compressed.len()
    );
    assert!(repeated.len() - body > 5 << 20);
    assert_too_large(&compressed, "compressed DPA");
}

/// Converting a product into a volume has its own bound: the sweeps of
/// every data array, estimated before any is built, stay within
/// [`MAX_VOLUME_BYTES`], and the volume has at most
/// [`MAX_SWEEPS_PER_VOLUME`] sweeps. The KTLX 2013 DPA decodes to 17 arrays;
/// repeating its hourly array (131 x 131 boxes, each placed with a latitude
/// and a longitude) or its first rate array (13 x 13 boxes) reaches each
/// limit while the product still decodes well within its own budget.
#[test]
fn volume_conversion_is_bounded() {
    let hourly = RealLayer::locate("l3-tlx-dpa-20130520-2016", 0);
    // The hourly array and 16 rate arrays.
    let arrays = decode_product(&hourly.bytes).unwrap().data_arrays().len();
    assert_eq!(arrays, 17);
    let others = arrays - 1;
    let within = decode_product(&hourly.repeated(100)).unwrap();
    let volume = within.to_volume().unwrap();
    assert_eq!(volume.sweeps.len(), 100 + others);

    // 131 x 131 boxes of 17 bytes (level, latitude, longitude) per copy.
    let copies = MAX_VOLUME_BYTES / (131 * 131 * 17) + 1;
    let beyond = decode_product(&hourly.repeated(copies)).unwrap();
    assert!(copies + others <= MAX_SWEEPS_PER_VOLUME);
    match beyond.to_volume() {
        Err(Level3Error::InvalidMessage { code: 81, reason }) => {
            assert!(reason.contains("bytes as a volume"), "{reason}");
        }
        other => panic!("expected InvalidMessage, got {:?}", other.map(|_| ())),
    }

    let rate = RealLayer::locate("l3-tlx-dpa-20130520-2016", 1);
    let copies = MAX_SWEEPS_PER_VOLUME - others + 1;
    let many = decode_product(&rate.repeated(copies)).unwrap();
    assert_eq!(many.data_arrays().len(), MAX_SWEEPS_PER_VOLUME + 1);
    match many.to_volume() {
        Err(Level3Error::InvalidMessage { code: 81, reason }) => {
            assert!(reason.contains("sweeps"), "{reason}");
        }
        other => panic!("expected InvalidMessage, got {:?}", other.map(|_| ())),
    }
    let fewer = decode_product(&rate.repeated(copies - 1)).unwrap();
    assert_eq!(
        fewer.to_volume().unwrap().sweeps.len(),
        MAX_SWEEPS_PER_VOLUME
    );
}

/// The KTLX 2013-05-20 20:16 radar coded message (product 74,
/// uncompressed): its text runs from the symbology offset to the end of the
/// message, and its Part A holds 83 intensity groups after `/NI0356:`.
struct RealRcm {
    bytes: Vec<u8>,
    /// File offset of the Message Header Block.
    message_start: usize,
    /// File offsets of the repeated groups: the first group through the
    /// comma before the last one.
    groups: std::ops::Range<usize>,
}

impl RealRcm {
    const ID: &str = "l3-tlx-rcm-20130520-2016";

    fn locate() -> Self {
        let entry = common::entry(Self::ID);
        let golden = entry.golden();
        let bytes = entry.bytes();
        let message_start = common::uncompressed_message_start(&golden, bytes.len());
        let find = |needle: &[u8], from: usize| {
            from + bytes[from..]
                .windows(needle.len())
                .position(|w| w == needle)
                .unwrap_or_else(|| panic!("{}: {needle:?}", Self::ID))
        };
        let start = find(b"/NI0356:", message_start) + 8;
        let top = find(b"/MT", start);
        let end = start + bytes[start..top].iter().rposition(|&b| b == b',').unwrap() + 1;
        Self {
            bytes,
            message_start,
            groups: start..end,
        }
    }

    /// The file with the groups repeated `copies` times.
    fn repeated(&self, copies: usize) -> Vec<u8> {
        // The committed file, read again: every byte written below is one of
        // its bytes or its message length, patched.
        let path = recast_radar_testdata::path(Self::ID).unwrap_or_else(|e| panic!("{e}"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(bytes, self.bytes);
        let mut out = bytes[..self.groups.start].to_vec();
        for _ in 0..copies {
            out.extend_from_slice(&bytes[self.groups.clone()]);
        }
        out.extend_from_slice(&bytes[self.groups.end..]);
        let at = self.message_start + 8; // message length (halfwords 5-6)
        let length = u32::try_from(out.len() - self.message_start).unwrap();
        out[at..at + 4].copy_from_slice(&length.to_be_bytes());
        out
    }
}

/// `result` is [`Level3Error::ProductTooLarge`] at the radar coded message
/// limit, for `what` (`None`: any part of the parse after the text copy).
fn assert_rcm_too_large<T>(result: Result<T, Level3Error>, what: Option<&str>, context: &str) {
    match result {
        Err(Level3Error::ProductTooLarge {
            what: found,
            needed,
            used,
            limit,
        }) => {
            assert_eq!(limit, MAX_RCM_PARSED_BYTES, "{context}");
            assert!(used <= limit && needed > limit - used, "{context}");
            assert!(
                found.starts_with("radar coded message"),
                "{context}: {found}"
            );
            match what {
                Some(what) => assert_eq!(found, what, "{context}"),
                None => assert_ne!(found, "radar coded message text", "{context}"),
            }
        }
        Err(other) => panic!("{context}: expected ProductTooLarge, got {other}"),
        Ok(_) => panic!("{context}: expected ProductTooLarge, got a result"),
    }
}

/// Parsing a radar coded message has its own bound, because each short
/// group of the text becomes a group string, an intensity run and its
/// levels. The KTLX 2013 message with its intensity groups repeated parses
/// while the copies fit (each copy's runs equal the message's, and the grid
/// is the message's), and once they do not, it is refused at
/// [`MAX_RCM_PARSED_BYTES`] by `radar_coded_message` and by `to_volume`; a
/// text longer than the limit is refused before it is parsed. The product
/// itself still decodes: its text is well within the product budget.
#[test]
fn radar_coded_message_parse_is_bounded() {
    let real = RealRcm::locate();
    let product = decode_product(&real.bytes).unwrap();
    let message = product.radar_coded_message().unwrap().unwrap();
    let runs = &message.part_a.as_ref().unwrap().intensities;
    assert_eq!(runs.len(), 83);
    let repeated_runs = runs.len() - 1;
    let grid = product.to_volume().unwrap().sweeps.remove(0);
    let text_len = product.tabular.as_ref().unwrap().data.len();
    let per_copy = real.groups.len();
    assert_eq!(per_copy, 693);

    // 200 copies (140 KB of text) parse; the repeated runs cover the
    // same boxes with the same levels.
    let copies = 200;
    let within = decode_product(&real.repeated(copies)).unwrap();
    let parsed = within.radar_coded_message().unwrap().unwrap();
    let a = parsed.part_a.as_ref().unwrap();
    assert_eq!(
        a.intensities.len(),
        runs.len() + (copies - 1) * repeated_runs
    );
    let (copied, last) = a.intensities.split_at(copies * repeated_runs);
    for copy in copied.chunks(repeated_runs) {
        assert_eq!(copy, &runs[..repeated_runs]);
    }
    assert_eq!(last, &runs[repeated_runs..]);
    assert_eq!(
        a.intensity_grid(),
        message.part_a.as_ref().unwrap().intensity_grid()
    );
    let sweep = within.to_volume().unwrap().sweeps.remove(0);
    assert_eq!(sweep.fields[0].data, grid.fields[0].data);

    // 2 000 copies: 1.4 MB of text, within the limit; its groups are not.
    let copies = 2000;
    let text = text_len + (copies - 1) * per_copy;
    assert!(text < MAX_RCM_PARSED_BYTES / 2, "{text}");
    let beyond = decode_product(&real.repeated(copies)).unwrap();
    assert_eq!(beyond.tabular.as_ref().unwrap().data.len(), text);
    assert_rcm_too_large(beyond.radar_coded_message(), None, "2 000 copies");
    assert_rcm_too_large(beyond.to_volume(), None, "2 000 copies, volume");

    // A text longer than the limit is refused at its copy.
    let copies = MAX_RCM_PARSED_BYTES / per_copy + 1;
    let long = decode_product(&real.repeated(copies)).unwrap();
    assert!(long.tabular.as_ref().unwrap().data.len() > MAX_RCM_PARSED_BYTES);
    let what = Some("radar coded message text");
    assert_rcm_too_large(long.radar_coded_message(), what, "long text");
    assert_rcm_too_large(long.to_volume(), what, "long text, volume");
}
