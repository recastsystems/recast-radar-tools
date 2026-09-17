//! Size-limited expansion helpers shared by the format decoders and the
//! format router.
//!
//! Errors are returned as the complete diagnostic message; each decoder
//! crate wraps it in its own compression error variant.

use std::io::Read;

/// Hard ceiling for one expanded radar payload. Operational Level II,
/// ODIM, CfRadial, and DORADE files are far smaller; this remains generous
/// enough for unusually dense research volumes while bounding compression
/// bombs before they can exhaust the process address space.
pub const MAX_DECODED_RADAR_BYTES: usize = 512 * 1024 * 1024;

/// Read an expanded stream without ever growing the destination beyond
/// `limit`. `try_reserve` turns address-space pressure into a normal decode
/// error instead of invoking the infallible allocation path.
pub fn read_to_end_limited(
    mut reader: impl Read,
    limit: usize,
    context: &'static str,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let remaining = limit.saturating_sub(output.len());
        if remaining == 0 {
            let mut probe = [0u8; 1];
            let count = reader
                .read(&mut probe)
                .map_err(|err| format!("{context}: {err}"))?;
            if count != 0 {
                return Err(format!("{context} expands beyond the {limit}-byte limit"));
            }
            break;
        }
        let read_len = remaining.min(chunk.len());
        let count = reader
            .read(&mut chunk[..read_len])
            .map_err(|err| format!("{context}: {err}"))?;
        if count == 0 {
            break;
        }
        output
            .try_reserve(count)
            .map_err(|err| format!("{context}: cannot reserve decoded buffer: {err}"))?;
        output.extend_from_slice(&chunk[..count]);
    }
    Ok(output)
}

/// Copy `bytes` into a new buffer, rejecting inputs longer than `limit` and
/// reporting allocation failure as an error.
pub fn copy_bytes_limited(
    bytes: &[u8],
    limit: usize,
    context: &'static str,
) -> Result<Vec<u8>, String> {
    if bytes.len() > limit {
        return Err(format!(
            "{context} is {} bytes (limit {limit})",
            bytes.len()
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes.len())
        .map_err(|err| format!("{context}: cannot reserve decoded buffer: {err}"))?;
    output.extend_from_slice(bytes);
    Ok(output)
}
