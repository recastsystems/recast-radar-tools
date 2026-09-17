//! Pre-FM301 APIs the bench still calls, behind one seam
//! (docs/design/fm301-model.md section 13.3):
//!
//! - decoding through `recast_radar_io`'s byte router, which returns the
//!   legacy volume; [`read_volume_bytes`] moves it into a `Volume`;
//! - the `--dealias` evaluation battery ([`dealias_eval`]), which drives
//!   `recast-radar-correct`'s dealiasing engines on legacy cuts and grids.
//!
//! Both go when those crates migrate: the router then returns a `Volume`,
//! and the battery is rewritten against `correct`'s FM301 API.

#![allow(deprecated)]

pub mod dealias_eval;

use recast_radar_core::Volume;

/// Decode any single-buffer format the app reads to an FM301 volume: the
/// router's legacy decode, then the shim conversion, which moves every gate
/// buffer and copies nothing.
pub fn read_volume_bytes(raw: &[u8]) -> Result<Volume, String> {
    let legacy =
        recast_radar_io::decode_supported_volume_bytes(raw).map_err(|err| err.to_string())?;
    Volume::try_from(legacy).map_err(|err| err.to_string())
}
