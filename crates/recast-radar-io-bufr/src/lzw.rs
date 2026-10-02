//! Unix `compress` (`.Z`) streams: LZW with 9- to 16-bit codes.
//!
//! The format, as compress(1) writes it: the magic `1F 9D`, a flags byte
//! (low 5 bits the largest code width, bit 7 "block mode", in which code 256
//! clears the table), then codes packed least significant bit first. The
//! code width grows by one bit when the table fills the current width, and
//! compress reads codes in groups of eight of one width: when the width
//! changes or the table is cleared, the rest of the current group of eight
//! is padding and skipped.

use crate::BufrError;

/// The `.Z` magic.
pub(crate) const MAGIC: [u8; 2] = [0x1F, 0x9D];

/// Code 256 in block mode: clear the table.
const CLEAR: u32 = 256;

/// Decompress a whole `.Z` stream; `limit` caps the output.
pub(crate) fn decompress(input: &[u8], limit: usize) -> Result<Vec<u8>, BufrError> {
    if input.len() < 3 || input[..2] != MAGIC {
        return Err(BufrError::Container("not a compress (.Z) stream".into()));
    }
    let max_bits = u32::from(input[2] & 0x1F);
    let block_mode = input[2] & 0x80 != 0;
    if !(9..=16).contains(&max_bits) {
        return Err(BufrError::Container(format!(
            "compress stream with {max_bits}-bit codes (9 to 16 allowed)"
        )));
    }
    let data = &input[3..];
    let total_bits = data.len() as u64 * 8;

    // The table: each entry is (prefix code, last byte); 0..=255 are bytes.
    let max_entries = 1usize << max_bits;
    let mut prefix: Vec<u32> = vec![0; max_entries];
    let mut suffix: Vec<u8> = (0..max_entries).map(|code| code as u8).collect();
    let first_free = if block_mode { 257 } else { 256 };
    let mut next = first_free;
    let mut width = 9u32;
    let mut bit = 0u64;
    let mut group_start = 0u64;
    let mut previous: Option<u32> = None;
    let mut first_byte = 0u8;
    let mut out = Vec::new();
    let mut stack: Vec<u8> = Vec::new();

    // Skip the rest of the group of eight codes that began at `group_start`.
    let align = |bit: u64, group_start: u64, width: u32| -> u64 {
        let used = (bit - group_start) / u64::from(width);
        let pad = (8 - used % 8) % 8;
        bit + pad * u64::from(width)
    };

    while bit + u64::from(width) <= total_bits {
        let code = read_code(data, bit, width);
        bit += u64::from(width);

        if block_mode && code == CLEAR {
            bit = align(bit, group_start, width);
            next = first_free;
            width = 9;
            group_start = bit;
            previous = None;
            continue;
        }

        let Some(prev) = previous else {
            if code > 255 {
                return Err(BufrError::Container(format!(
                    "compress stream: first code {code} is not a byte"
                )));
            }
            first_byte = code as u8;
            out.push(first_byte);
            previous = Some(code);
            continue;
        };

        // Expand `code` (or, for the code being defined, prev + first byte).
        stack.clear();
        let mut current = if (code as usize) < next {
            code
        } else if code as usize == next {
            stack.push(first_byte);
            prev
        } else {
            return Err(BufrError::Container(format!(
                "compress stream: code {code} beyond the table ({next} entries)"
            )));
        };
        while current > 255 {
            stack.push(suffix[current as usize]);
            current = prefix[current as usize];
        }
        first_byte = current as u8;
        stack.push(first_byte);
        if out.len() + stack.len() > limit {
            return Err(BufrError::Limit(format!(
                "compress stream expands past {limit} bytes"
            )));
        }
        out.extend(stack.iter().rev());

        if next < max_entries {
            prefix[next] = prev;
            suffix[next] = first_byte;
            next += 1;
        }
        previous = Some(code);
        if next >= (1usize << width) && width < max_bits {
            bit = align(bit, group_start, width);
            width += 1;
            group_start = bit;
        }
    }
    Ok(out)
}

/// The `width`-bit code at bit `bit` (least significant bit first).
fn read_code(data: &[u8], bit: u64, width: u32) -> u32 {
    let byte = (bit / 8) as usize;
    let mut word = 0u32;
    for (offset, value) in data.iter().skip(byte).take(3).enumerate() {
        word |= u32::from(*value) << (8 * offset);
    }
    (word >> (bit % 8)) & ((1 << width) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NOAA's 1998-04-16 KBMX Level III day archive: a 32 MB compress(1)
    /// `.tar.Z` (public domain, downloaded on demand).
    fn archive() -> Option<Vec<u8>> {
        recast_radar_testdata::bytes_if_available("l3-kbmx-19980416-archive-tarz")
    }

    /// The archive's member KBMX_SDUS54_NVWBMX_199804160006 comes out as
    /// the committed copy.
    #[test]
    fn decodes_a_real_compress_archive() {
        let Some(archive) = archive() else {
            return;
        };
        let member = recast_radar_testdata::bytes("l3-kbmx-19980416-0006-nvw").unwrap();
        let tar = decompress(&archive, 1 << 30).unwrap();
        let mut at = 0;
        let mut found = None;
        while at + 512 <= tar.len() && tar[at] != 0 {
            let header = &tar[at..at + 512];
            let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
            let name = std::str::from_utf8(&header[..name_end]).unwrap();
            let size_text = std::str::from_utf8(&header[124..136]).unwrap();
            let digits: String = size_text.chars().filter(char::is_ascii_digit).collect();
            let size = usize::from_str_radix(&digits, 8).unwrap();
            if name.ends_with("KBMX_SDUS54_NVWBMX_199804160006") {
                found = Some(tar[at + 512..at + 512 + size].to_vec());
                break;
            }
            at += 512 + size.div_ceil(512) * 512;
        }
        assert_eq!(found.as_deref(), Some(member.as_slice()));
    }

    /// The same archive past an output limit, with its magic damaged, and
    /// with a code beyond the table.
    #[test]
    fn refuses_runaway_output_and_damaged_streams() {
        let Some(archive) = archive() else {
            return;
        };
        assert!(matches!(
            decompress(&archive, 1000),
            Err(BufrError::Limit(_))
        ));
        let mut wrong_magic = archive[..4096].to_vec();
        wrong_magic[1] = 0x8B;
        assert!(decompress(&wrong_magic, 1 << 20).is_err());
        let mut bad_width = archive[..4096].to_vec();
        bad_width[2] = 0x80 | 20;
        assert!(decompress(&bad_width, 1 << 20).is_err());
    }
}
