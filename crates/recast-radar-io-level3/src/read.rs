//! Bounds-checked big-endian readers shared by the header, block and packet code.

use crate::Level3Error;

/// `len` bytes at `offset`, or [`Level3Error::Truncated`].
pub(crate) fn slice<'a>(
    data: &'a [u8],
    offset: usize,
    len: usize,
    what: &'static str,
) -> Result<&'a [u8], Level3Error> {
    offset
        .checked_add(len)
        .and_then(|end| data.get(offset..end))
        .ok_or(Level3Error::Truncated {
            what,
            offset,
            needed: len,
            available: data.len().saturating_sub(offset),
        })
}

fn array<const N: usize>(
    data: &[u8],
    offset: usize,
    what: &'static str,
) -> Result<[u8; N], Level3Error> {
    let mut out = [0u8; N];
    out.copy_from_slice(slice(data, offset, N, what)?);
    Ok(out)
}

/// Unsigned 16-bit big-endian integer at `offset`.
pub(crate) fn be_u16(data: &[u8], offset: usize, what: &'static str) -> Result<u16, Level3Error> {
    array(data, offset, what).map(u16::from_be_bytes)
}

/// Signed 16-bit big-endian integer at `offset`.
pub(crate) fn be_i16(data: &[u8], offset: usize, what: &'static str) -> Result<i16, Level3Error> {
    array(data, offset, what).map(i16::from_be_bytes)
}

/// Unsigned 32-bit big-endian integer at `offset`.
pub(crate) fn be_u32(data: &[u8], offset: usize, what: &'static str) -> Result<u32, Level3Error> {
    array(data, offset, what).map(u32::from_be_bytes)
}

/// Signed 32-bit big-endian integer at `offset`.
pub(crate) fn be_i32(data: &[u8], offset: usize, what: &'static str) -> Result<i32, Level3Error> {
    array(data, offset, what).map(i32::from_be_bytes)
}

/// Checks that the signed halfword at `offset` equals `expected` (block dividers and IDs).
pub(crate) fn expect_i16(
    data: &[u8],
    offset: usize,
    expected: i16,
    what: &'static str,
) -> Result<(), Level3Error> {
    let found = be_i16(data, offset, what)?;
    if found == expected {
        Ok(())
    } else {
        Err(Level3Error::BadBlockHeader {
            what,
            offset,
            expected,
            found,
        })
    }
}
