//! JMA polar-coordinate radar GRIB2 tar decoder (Japan).
//!
//! The Japan Meteorological Agency distributes its operational radar
//! network's polar-coordinate sweeps as GRIB2 files bundled into ustar
//! archives named `Z__C_RJTD_{yyyymmddHHMMSS}_RDR_JMAGPV_{N5|N6}_grib2.tar`
//! (N5 = reflectivity `Pze`, N6 = radial velocity `Pvr`), publicly mirrored
//! by NICT at `https://pawr.nict.go.jp/jmadata/JMA-PolarCoordsRadar/`. Each
//! tar member is one station's complete multi-elevation scan.
//!
//! Format: WMO FM 92 GRIB Edition 2 (WMO *Manual on Codes*, WMO-No. 306)
//! carrying JMA local templates, per the JMA technical format documentation
//! for GRIB2 distribution materials (JMA "Dissemination Technical
//! Information" / 配信資料に関する技術情報 series, radar GPV):
//!
//! - Grid Definition Template 3.50120 — azimuth-range polar grid (gate
//!   count, radial count, gate spacing, range start, scan direction, start
//!   azimuth).
//! - Product Definition Template 4.51022 — radar site parameters (station
//!   id/number, latitude/longitude/altitude, antenna elevation angle,
//!   per-ray elevation and PRF tables).
//! - Data Representation Template 5.200 — run-length packing against a
//!   table of level values with a decimal scale factor.
//! - Parameters follow WMO Code table 4.2, discipline 0, category 15:
//!   number 1 = reflectivity (dBZ), number 2 = radial velocity (m/s).
//!
//! Decoder lineage: ported from the FahrenheitResearch `jma-radar-bridge`
//! crate (same owner; <https://github.com/FahrenheitResearch/jma-radar-bridge>)
//! and cross-validated sweep-for-sweep and gate-for-gate against its decode
//! of live NICT pulls.
//!
//! Multi-station handling: [`decode_jma_tar_volumes`] returns ONE
//! [`RadarVolume`] per station and never silently drops stations; callers
//! that want a single station pass `site_filter`. The shared byte router
//! (`recast_radar_io::decode_supported_volume_bytes`) uses
//! [`decode_jma_tar_first_station`] instead, which keeps only the first
//! station in the archive.
//!
//! Values are stored as physical `f32` planes (`MomentStorage::F32`, NaN =
//! missing), exactly as the run-length level table dictates — the level
//! table is a lookup, not an affine raw-to-physical mapping, so compact
//! u8/u16 storage does not apply. Nyquist velocity is left `None`: the
//! per-ray PRF tables suggest staggered-PRF operation and a wrong Nyquist
//! would mislead downstream dealiasing.
//!
//! # Limits
//!
//! | Structure | Limit | Real (2019-10-12 09:00Z tars) |
//! |---|---|---|
//! | Tar size | 64 MiB | 37.3 MiB (N5) |
//! | Regular tar members | 128 | 20 |
//! | Member / GRIB2 message size | 32 MiB | 3.8 MiB |
//! | GRIB2 sections per message | 512 | |
//! | Grid gates x radials | 4,096 x 2,048 | 800 x 512 |
//! | Grid points per sweep | 4,194,304 | 409,600 |
//! | Level table values | 4,096 | |
//! | Sweeps per member | 64 | 26 |
//! | Decoded points per member | 33,554,432 | about 7.5 million |
//! | Decoded output of one call (all stations) | `MAX_DECODED_BATCH_BYTES` (2 GiB) | 586 MiB (N5) |
//!
//! Limit violations are [`JmaError::LimitExceeded`] errors. A member that
//! exceeds a per-member limit is skipped like any other corrupt member; the
//! error is returned when no station decodes. The aggregate output limit
//! fails the whole call.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use chrono::{TimeZone, Utc};
use recast_radar_core::bounded_read::{MAX_DECODED_BATCH_BYTES, volume_moment_capacity_bytes};
use recast_radar_core::{
    ElevationCut, GateRange, MomentGrid, MomentStorage, MomentType, RadarSite, RadarVolume, Radial,
    ScanMode,
};
use thiserror::Error;

/// Errors from JMA radar GRIB2 tar decoding.
#[derive(Debug, Error)]
pub enum JmaError {
    /// The tar archive or a GRIB2 member could not be decoded; the message
    /// names the member and the failing structure.
    #[error("{0}")]
    Decode(String),
    /// The archive declares more data than a documented resource limit
    /// allows (see the crate-level `# Limits` section).
    #[error("decode limit exceeded: {0}")]
    LimitExceeded(String),
}

/// Internal decode steps report errors as strings; limit violations carry
/// this prefix (added only by [`limit_error`]) so the public entry points can
/// surface them as [`JmaError::LimitExceeded`].
const LIMIT_ERROR_PREFIX: &str = "decode limit exceeded: ";

fn limit_error(message: String) -> String {
    format!("{LIMIT_ERROR_PREFIX}{message}")
}

fn jma_error(message: String) -> JmaError {
    match message.strip_prefix(LIMIT_ERROR_PREFIX) {
        Some(reason) => JmaError::LimitExceeded(reason.to_owned()),
        None => JmaError::Decode(message),
    }
}

const TAR_BLOCK_LEN: usize = 512;
const TAR_NAME_LEN: usize = 100;
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_SIZE_OFFSET: usize = 124;
const TAR_SIZE_LEN: usize = 12;
const TAR_TYPEFLAG_OFFSET: usize = 156;
/// The real 20-station N5 reflectivity tar of 2019-10-12 09:00Z (Typhoon
/// Hagibis) is 37.3 MiB; N6 velocity is 12.6 MiB. Keep room for network
/// growth while rejecting giant local files before walking
/// attacker-controlled headers.
const MAX_JMA_TAR_BYTES: usize = 64 * 1024 * 1024;
const MAX_JMA_TAR_MEMBERS: usize = 128;
const MAX_JMA_MEMBER_BYTES: usize = 32 * 1024 * 1024;

const GRIB_MAGIC: &[u8; 4] = b"GRIB";
const GRIB_END_MAGIC: &[u8; 4] = b"7777";
/// JMA Grid Definition Template 3.50120 (azimuth-range polar grid).
const GRID_TEMPLATE_AZIMUTH_RANGE: u16 = 50120;
/// JMA Product Definition Template 4.51022 (radar site/elevation params).
const PRODUCT_TEMPLATE_RADAR_ELEVATION: u16 = 51022;
/// JMA Data Representation Template 5.200 (run-length level packing).
const DATA_TEMPLATE_RUN_LENGTH: u16 = 200;
/// Largest observed real sweep is 800 gates x 512 radials = 409,600 points.
/// These per-axis and aggregate ceilings leave roughly an order of magnitude
/// of headroom without letting a tiny RLE stream request hundreds of MiB.
const MAX_GRID_GATES: usize = 4096;
const MAX_GRID_RADIALS: usize = 2048;
const MAX_GRID_POINTS: usize = 4 * 1024 * 1024;
const MAX_SWEEPS_PER_MEMBER: usize = 64;
const MAX_POINTS_PER_MEMBER: usize = 32 * 1024 * 1024;
const MAX_GRIB_SECTIONS: usize = 512;
const MAX_LEVEL_VALUES: usize = 4096;

/// `true` when `bytes` look like a JMA radar GRIB2 tar: ustar magic at
/// byte 257 and a first member named `Z__C_RJTD_*_RDR_JMAGPV*`.
///
/// Needs at least the first 512 bytes (one tar header block); shorter
/// buffers return `false`.
pub fn looks_like_jma_tar_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < TAR_BLOCK_LEN {
        return false;
    }
    if &bytes[TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 5] != b"ustar" {
        return false;
    }
    is_jma_member_name(&tar_field_str(&bytes[..TAR_NAME_LEN]))
}

/// One radar station's identity, parsed from the GRIB2 product section
/// without decoding any gate data — cheap enough to run over a whole tar
/// for catalog building.
#[derive(Clone, Debug, PartialEq)]
pub struct JmaStationHeader {
    /// JMA station id string (e.g. `"ITOK"`), from PDT 4.51022 octets 25-28.
    pub id: String,
    /// JMA station number (e.g. `47937`, the `RS{number}` in member names).
    pub number: u16,
    /// Station latitude in degrees north (10^-6 deg, signed-magnitude).
    pub latitude_deg: f64,
    /// Station longitude in degrees east (10^-6 deg).
    pub longitude_deg: f64,
    /// Antenna altitude in metres (0.1 m units), when encoded.
    pub elevation_m: Option<f32>,
}

/// Parse the station headers of every GRIB2 member in a JMA tar, in archive
/// order, deduplicated by station id. Decodes no gate data. Errors only when
/// no member yields a station (with the first member's parse error when
/// there was one).
pub fn jma_tar_station_headers(bytes: &[u8]) -> Result<Vec<JmaStationHeader>, JmaError> {
    station_headers(bytes).map_err(jma_error)
}

fn station_headers(bytes: &[u8]) -> Result<Vec<JmaStationHeader>, String> {
    let members = ustar_members(bytes)?;
    let mut stations: Vec<JmaStationHeader> = Vec::new();
    let mut first_error: Option<String> = None;
    for member in &members {
        if !is_jma_data_member(&member.name) {
            continue;
        }
        match parse_station_header(member.data, &member.name) {
            Ok(station) => {
                if !stations.iter().any(|existing| existing.id == station.id) {
                    stations
                        .try_reserve(1)
                        .map_err(|err| format!("cannot reserve JMA station table: {err}"))?;
                    stations.push(station);
                }
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    if stations.is_empty() {
        return Err(first_error.unwrap_or_else(|| {
            "tar archive holds no Z__C_RJTD_*_RDR_JMAGPV GRIB2 members".to_owned()
        }));
    }
    Ok(stations)
}

/// Decode a JMA radar GRIB2 tar into one [`RadarVolume`] per station, in
/// archive order.
///
/// `site_filter` selects a single station by JMA id (e.g. `"ITOK"`,
/// case-insensitive) or by station number (`"RS47937"` / `"47937"`); `None`
/// decodes every station. Malformed members are skipped (the tar is network
/// data; one corrupt station must not take down the other nineteen) — the
/// first member error is returned only when nothing decodes. Never panics
/// on malformed input.
pub fn decode_jma_tar_volumes(
    bytes: &[u8],
    site_filter: Option<&str>,
) -> Result<Vec<RadarVolume>, JmaError> {
    decode_tar_volumes(bytes, site_filter).map_err(jma_error)
}

fn decode_tar_volumes(bytes: &[u8], site_filter: Option<&str>) -> Result<Vec<RadarVolume>, String> {
    let members = ustar_members(bytes)?;
    let mut volumes: Vec<RadarVolume> = Vec::new();
    let mut first_error: Option<String> = None;
    let mut data_members = 0usize;
    let mut filter_matches = 0usize;
    let mut decoded_bytes = 0usize;

    for member in &members {
        if !is_jma_data_member(&member.name) {
            continue;
        }
        data_members += 1;
        // Header-only parse first: with a site filter this skips the
        // expensive run-length decode of every other station.
        let header = match parse_station_header(member.data, &member.name) {
            Ok(header) => header,
            Err(err) => {
                first_error.get_or_insert(err);
                continue;
            }
        };
        if let Some(filter) = site_filter
            && !station_matches_filter(&header, filter)
        {
            continue;
        }
        filter_matches += 1;
        match decode_jma_grib2_volume(member.data, &member.name) {
            Ok(volume) => {
                let radials: usize = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
                let member_bytes = volume_moment_capacity_bytes(&volume)
                    .saturating_add(radials.saturating_mul(size_of::<Radial>()));
                decoded_bytes = decoded_bytes
                    .checked_add(member_bytes)
                    .filter(|total| *total <= MAX_DECODED_BATCH_BYTES)
                    .ok_or_else(|| {
                        limit_error(format!(
                            "JMA decode output exceeds the {MAX_DECODED_BATCH_BYTES}-byte aggregate limit"
                        ))
                    })?;
                merge_station_volume(&mut volumes, volume)?;
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }

    if volumes.is_empty() {
        if data_members == 0 {
            return Err("tar archive holds no Z__C_RJTD_*_RDR_JMAGPV GRIB2 members".to_owned());
        }
        if let Some(filter) = site_filter
            && filter_matches == 0
        {
            return Err(format!(
                "no JMA station matched site filter '{filter}' ({data_members} GRIB2 members)"
            ));
        }
        return Err(first_error
            .unwrap_or_else(|| "no JMA GRIB2 member decoded into a radar volume".to_owned()));
    }
    for volume in &mut volumes {
        sort_cuts_lowest_first(volume);
    }
    Ok(volumes)
}

/// JMA packs sweeps in whatever order the GRIB member carries them —
/// observed highest-tilt-first, so tilt 0 in the UI showed the ~25° cone
/// (field report) — and a station spanning several tar members restarts
/// its sweep numbering per member. Sort each station's ladder lowest
/// beam first (stable: repeated elevations — the 10-minute file's two
/// 5-minute repetitions — keep their scan order) and renumber, matching
/// every other provider's cut order.
fn sort_cuts_lowest_first(volume: &mut RadarVolume) {
    volume
        .cuts
        .sort_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg));
    for (index, cut) in volume.cuts.iter_mut().enumerate() {
        cut.elevation_number = u8::try_from(index + 1).ok();
    }
}

/// Decode only the FIRST station of a JMA tar — the shared byte router's
/// entry point, where the contract is one volume per buffer. Providers that
/// need a specific station call [`decode_jma_tar_volumes`] with a
/// `site_filter` instead.
pub fn decode_jma_tar_first_station(bytes: &[u8]) -> Result<RadarVolume, JmaError> {
    decode_first_station(bytes).map_err(jma_error)
}

fn decode_first_station(bytes: &[u8]) -> Result<RadarVolume, String> {
    let members = ustar_members(bytes)?;
    for member in &members {
        if !is_jma_data_member(&member.name) {
            continue;
        }
        if let Ok(header) = parse_station_header(member.data, &member.name) {
            let mut volumes = decode_tar_volumes(bytes, Some(&header.id))?;
            if volumes.is_empty() {
                break; // unreachable: Ok(volumes) is never empty
            }
            return Ok(volumes.remove(0));
        }
    }
    Err("tar archive holds no decodable JMA GRIB2 members".to_owned())
}

/// Fold one decoded member into the per-station volume list: a repeated
/// station id appends its cuts (scan order preserved) instead of producing
/// a duplicate site entry.
fn merge_station_volume(
    volumes: &mut Vec<RadarVolume>,
    mut volume: RadarVolume,
) -> Result<(), String> {
    if let Some(existing) = volumes
        .iter_mut()
        .find(|existing| existing.site.id == volume.site.id)
    {
        if volume.volume_time < existing.volume_time {
            existing.volume_time = volume.volume_time;
        }
        existing.metadata.message_count += volume.metadata.message_count;
        existing.metadata.decoded_radial_count += volume.metadata.decoded_radial_count;
        existing
            .cuts
            .try_reserve(volume.cuts.len())
            .map_err(|err| format!("cannot grow JMA station cut table: {err}"))?;
        existing.cuts.append(&mut volume.cuts);
    } else {
        volumes
            .try_reserve(1)
            .map_err(|err| format!("cannot grow JMA volume table: {err}"))?;
        volumes.push(volume);
    }
    Ok(())
}

fn station_matches_filter(header: &JmaStationHeader, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.eq_ignore_ascii_case(&header.id) {
        return true;
    }
    let number = header.number.to_string();
    filter == number
        || filter
            .strip_prefix("RS")
            .or_else(|| filter.strip_prefix("rs"))
            .is_some_and(|rest| rest == number)
}

// ---------------------------------------------------------------------------
// ustar reading (hand-rolled: JMA members are plain flat files, so a tar
// crate dependency is not warranted).
// ---------------------------------------------------------------------------

struct TarMember<'a> {
    name: String,
    data: &'a [u8],
}

fn ustar_members(bytes: &[u8]) -> Result<Vec<TarMember<'_>>, String> {
    if bytes.len() > MAX_JMA_TAR_BYTES {
        return Err(limit_error(format!(
            "JMA tar is {} bytes (limit {MAX_JMA_TAR_BYTES})",
            bytes.len()
        )));
    }
    let mut members = Vec::new();
    let mut pos = 0usize;
    while pos
        .checked_add(TAR_BLOCK_LEN)
        .is_some_and(|end| end <= bytes.len())
    {
        let header_end = pos
            .checked_add(TAR_BLOCK_LEN)
            .ok_or_else(|| "tar header offset overflow".to_owned())?;
        let header = &bytes[pos..header_end];
        if header.iter().all(|&byte| byte == 0) {
            break; // end-of-archive zero block
        }
        let name = tar_field_str(&header[..TAR_NAME_LEN]);
        if name.is_empty() {
            return Err(format!(
                "tar header at offset {pos} has an empty member name"
            ));
        }
        let size = tar_octal(&header[TAR_SIZE_OFFSET..TAR_SIZE_OFFSET + TAR_SIZE_LEN])
            .ok_or_else(|| format!("tar member '{name}' has an unparsable size field"))?;
        if size > MAX_JMA_MEMBER_BYTES {
            return Err(limit_error(format!(
                "tar member '{name}' declares {size} bytes (limit {MAX_JMA_MEMBER_BYTES})"
            )));
        }
        let data_start = header_end;
        let data_end = data_start
            .checked_add(size)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| format!("tar member '{name}' overruns the archive"))?;
        let padded = size
            .checked_add(TAR_BLOCK_LEN - 1)
            .and_then(|value| value.checked_div(TAR_BLOCK_LEN))
            .and_then(|blocks| blocks.checked_mul(TAR_BLOCK_LEN))
            .ok_or_else(|| format!("tar member '{name}' padded size overflow"))?;
        let next_pos = data_start
            .checked_add(padded)
            .filter(|&next| next <= bytes.len())
            .ok_or_else(|| format!("tar member '{name}' padded body overruns the archive"))?;
        // Regular files only ('0' or the old NUL flag); other entry types
        // (directories, long-name extensions, ...) are skipped over.
        if matches!(header[TAR_TYPEFLAG_OFFSET], b'0' | 0) {
            if members.len() >= MAX_JMA_TAR_MEMBERS {
                return Err(limit_error(format!(
                    "JMA tar contains more than {MAX_JMA_TAR_MEMBERS} regular members (limit)"
                )));
            }
            members
                .try_reserve(1)
                .map_err(|err| format!("cannot reserve JMA tar member table: {err}"))?;
            members.push(TarMember {
                name,
                data: &bytes[data_start..data_end],
            });
        }
        pos = next_pos;
    }
    Ok(members)
}

fn tar_field_str(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_owned()
}

fn tar_octal(field: &[u8]) -> Option<usize> {
    let text = tar_field_str(field);
    let text = text.trim_matches(' ');
    if text.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(text, 8).ok()
}

fn is_jma_member_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    base.starts_with("Z__C_RJTD_") && base.contains("_RDR_JMAGPV")
}

fn is_jma_data_member(name: &str) -> bool {
    is_jma_member_name(name) && name.to_ascii_lowercase().ends_with(".bin")
}

// ---------------------------------------------------------------------------
// GRIB2 message decode (JMA templates 3.50120 / 4.51022 / 5.200).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct SectionRef {
    number: u8,
    offset: usize,
    length: usize,
}

#[derive(Clone)]
struct PolarGrid {
    gate_count: usize,
    radial_count: usize,
    gate_spacing_m: f32,
    range_start_m: f32,
    scan_mode: u8,
    start_azimuth_deg: f32,
}

impl PolarGrid {
    /// GDT 3.50120 octet 39, bit 2: counter-clockwise when set.
    fn scans_clockwise(&self) -> bool {
        self.scan_mode & 0b0100_0000 == 0
    }

    fn ray_azimuth_deg(&self, ray: usize) -> f32 {
        let step = 360.0 / self.radial_count as f32;
        let signed_step = if self.scans_clockwise() { step } else { -step };
        (self.start_azimuth_deg + signed_step * ray as f32).rem_euclid(360.0)
    }
}

struct SweepProduct {
    moment: MomentType,
    station: JmaStationHeader,
    elevation_deg: Option<f32>,
    ray_elevation_deg: Vec<Option<f32>>,
}

fn decode_jma_grib2_volume(bytes: &[u8], member: &str) -> Result<RadarVolume, String> {
    let msg = grib2_message(bytes, member)?;
    let sections = scan_sections(msg, member)?;

    let identification = sections
        .iter()
        .find(|section| section.number == 1)
        .ok_or_else(|| format!("{member}: GRIB2 message has no identification section"))?;
    let volume_time = parse_reference_time(section_bytes(msg, *identification), member)?;

    let mut volume = RadarVolume::new(RadarSite::new(""), volume_time);
    volume.metadata.archive_version = Some("JMA GRIB2".to_owned());
    volume.metadata.compression = Some("jma-grib2-tar".to_owned());
    volume.metadata.scan_mode = Some(ScanMode::Ppi);

    let mut current_grid: Option<PolarGrid> = None;
    let mut pending_product: Option<SectionRef> = None;
    let mut pending_data_repr: Option<SectionRef> = None;
    let mut pending_bitmap: Option<SectionRef> = None;
    let mut station: Option<JmaStationHeader> = None;
    let mut decoded_points = 0usize;
    let mut decoded_sweeps = 0usize;

    for section in sections {
        match section.number {
            1 | 2 => {}
            3 => current_grid = Some(parse_grid(section_bytes(msg, section), member)?),
            4 => pending_product = Some(section),
            5 => pending_data_repr = Some(section),
            6 => pending_bitmap = Some(section),
            7 => {
                let grid = current_grid
                    .clone()
                    .ok_or_else(|| format!("{member}: data section before any grid section"))?;
                decoded_sweeps = decoded_sweeps
                    .checked_add(1)
                    .filter(|count| *count <= MAX_SWEEPS_PER_MEMBER)
                    .ok_or_else(|| {
                        limit_error(format!(
                            "{member}: more than {MAX_SWEEPS_PER_MEMBER} sweeps in one GRIB member (limit)"
                        ))
                    })?;
                let grid_points = grid
                    .gate_count
                    .checked_mul(grid.radial_count)
                    .ok_or_else(|| format!("{member}: grid point count overflow"))?;
                decoded_points = decoded_points
                    .checked_add(grid_points)
                    .filter(|count| *count <= MAX_POINTS_PER_MEMBER)
                    .ok_or_else(|| {
                        limit_error(format!(
                            "{member}: decoded grids exceed the {MAX_POINTS_PER_MEMBER}-point member limit"
                        ))
                    })?;
                let product_section = pending_product
                    .take()
                    .ok_or_else(|| format!("{member}: data section without a product section"))?;
                let data_repr = pending_data_repr.take().ok_or_else(|| {
                    format!("{member}: data section without a data-representation section")
                })?;
                let bitmap = pending_bitmap
                    .take()
                    .ok_or_else(|| format!("{member}: data section without a bitmap section"))?;

                let product = parse_product(section_bytes(msg, product_section), &grid, member)?;
                let values = decode_data(
                    section_bytes(msg, data_repr),
                    section_bytes(msg, bitmap),
                    section_bytes(msg, section),
                    grid_points,
                    member,
                )?;
                if station.is_none() {
                    station = Some(product.station.clone());
                }
                push_sweep_cut(&mut volume, &grid, &product, values)?;
            }
            8 => break,
            other => return Err(format!("{member}: unexpected GRIB2 section {other}")),
        }
    }

    let station =
        station.ok_or_else(|| format!("{member}: GRIB2 message contains no radar sweeps"))?;
    volume.site = RadarSite {
        id: station.id.clone(),
        name: Some(format!("RS{}", station.number)),
        latitude_deg: Some(station.latitude_deg as f32),
        longitude_deg: Some(station.longitude_deg as f32),
        elevation_m: station.elevation_m,
    };
    volume.metadata.message_count = volume.cuts.len();
    volume.metadata.decoded_radial_count = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
    Ok(volume)
}

fn push_sweep_cut(
    volume: &mut RadarVolume,
    grid: &PolarGrid,
    product: &SweepProduct,
    values: Vec<f32>,
) -> Result<(), String> {
    let expected_points = grid
        .gate_count
        .checked_mul(grid.radial_count)
        .ok_or_else(|| "JMA sweep point count overflow".to_owned())?;
    if values.len() != expected_points {
        return Err(format!(
            "JMA sweep decoded {} values for {expected_points} grid points",
            values.len()
        ));
    }
    let elevation_deg = product.elevation_deg.unwrap_or(0.0);
    let elevation_number = u8::try_from(volume.cuts.len() + 1).ok();
    let gate_range = GateRange {
        first_gate_m: grid.range_start_m.round() as i32,
        gate_spacing_m: (grid.gate_spacing_m.round() as i32).max(1),
        gate_count: grid.gate_count,
    };

    // Repeated elevations with different gate layouts are real here (the
    // 10-minute file carries two 5-minute scan repetitions), so every sweep
    // becomes its own cut in scan order — never elevation-merged.
    let mut cut = ElevationCut::new(elevation_deg, elevation_number);
    cut.radials
        .try_reserve_exact(grid.radial_count)
        .map_err(|err| format!("cannot reserve JMA radial table: {err}"))?;
    for ray in 0..grid.radial_count {
        cut.radials.push(Radial {
            azimuth_deg: grid.ray_azimuth_deg(ray),
            elevation_deg: product
                .ray_elevation_deg
                .get(ray)
                .copied()
                .flatten()
                .unwrap_or(elevation_deg),
            time_offset_ms: 0,
            gate_range: gate_range.clone(),
            nyquist_velocity_mps: None,
            radial_status: None,
        });
    }

    let mut radial_indices = Vec::new();
    radial_indices
        .try_reserve_exact(grid.radial_count)
        .map_err(|err| format!("cannot reserve JMA radial index table: {err}"))?;
    radial_indices.extend(0..grid.radial_count);
    let moment_grid = MomentGrid {
        moment: product.moment.clone(),
        gate_range,
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices,
        // `values` is already radial-major. Move it into the final grid
        // instead of cloning every row into a second full f32 plane.
        storage: MomentStorage::F32(values),
    };
    cut.moments.insert(product.moment.clone(), moment_grid);
    volume
        .cuts
        .try_reserve(1)
        .map_err(|err| format!("cannot grow JMA cut table: {err}"))?;
    volume.cuts.push(cut);
    Ok(())
}

/// Validate the GRIB2 indicator + end marker and return the message slice.
fn grib2_message<'a>(bytes: &'a [u8], member: &str) -> Result<&'a [u8], String> {
    if bytes.len() > MAX_JMA_MEMBER_BYTES {
        return Err(limit_error(format!(
            "{member}: GRIB member is {} bytes (limit {MAX_JMA_MEMBER_BYTES})",
            bytes.len()
        )));
    }
    if bytes.len() < 20 || &bytes[0..4] != GRIB_MAGIC {
        return Err(format!("{member}: missing GRIB indicator"));
    }
    if bytes[7] != 2 {
        return Err(format!(
            "{member}: expected GRIB edition 2, got {}",
            bytes[7]
        ));
    }
    let total_length = usize::try_from(be_u64(bytes, 8, member)?)
        .map_err(|_| format!("{member}: GRIB message length does not fit this platform"))?;
    if total_length < 20 || total_length > bytes.len() {
        return Err(format!(
            "{member}: message declares {total_length} bytes but member has {}",
            bytes.len()
        ));
    }
    let msg = &bytes[..total_length];
    if &msg[msg.len() - 4..] != GRIB_END_MAGIC {
        return Err(format!("{member}: missing GRIB end marker 7777"));
    }
    Ok(msg)
}

fn scan_sections(msg: &[u8], member: &str) -> Result<Vec<SectionRef>, String> {
    let mut sections = Vec::new();
    let mut pos = 16usize;
    while pos < msg.len() {
        if let Some(marker_end) = pos.checked_add(4)
            && marker_end <= msg.len()
            && &msg[pos..marker_end] == GRIB_END_MAGIC
        {
            if sections.len() >= MAX_GRIB_SECTIONS {
                return Err(limit_error(format!(
                    "{member}: more than {MAX_GRIB_SECTIONS} GRIB sections (limit)"
                )));
            }
            sections
                .try_reserve(1)
                .map_err(|err| format!("{member}: cannot reserve GRIB section table: {err}"))?;
            sections.push(SectionRef {
                number: 8,
                offset: pos,
                length: 4,
            });
            return Ok(sections);
        }
        if pos.checked_add(5).is_none_or(|end| end > msg.len()) {
            return Err(format!("{member}: truncated GRIB2 section header"));
        }
        let length = be_u32(msg, pos, member)? as usize;
        let number = msg[pos + 4];
        let Some(section_end) = pos
            .checked_add(length)
            .filter(|end| length >= 5 && *end <= msg.len())
        else {
            return Err(format!(
                "{member}: GRIB2 section {number} has invalid length {length}"
            ));
        };
        if sections.len() >= MAX_GRIB_SECTIONS {
            return Err(limit_error(format!(
                "{member}: more than {MAX_GRIB_SECTIONS} GRIB sections (limit)"
            )));
        }
        sections
            .try_reserve(1)
            .map_err(|err| format!("{member}: cannot reserve GRIB section table: {err}"))?;
        sections.push(SectionRef {
            number,
            offset: pos,
            length,
        });
        pos = section_end;
    }
    Err(format!("{member}: GRIB2 message missing end marker"))
}

fn section_bytes(msg: &[u8], section: SectionRef) -> &[u8] {
    &msg[section.offset..section.offset + section.length]
}

fn parse_reference_time(section: &[u8], member: &str) -> Result<chrono::DateTime<Utc>, String> {
    require_section(section, 1, 21, member)?;
    let year = be_u16(section, 12, member)?;
    let (month, day) = (section[14], section[15]);
    let (hour, minute, second) = (section[16], section[17], section[18]);
    Utc.with_ymd_and_hms(
        i32::from(year),
        u32::from(month),
        u32::from(day),
        u32::from(hour),
        u32::from(minute),
        u32::from(second),
    )
    .single()
    .ok_or_else(|| {
        format!(
            "{member}: invalid GRIB2 reference time \
             {year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        )
    })
}

/// Grid Definition Template 3.50120 (JMA azimuth-range polar grid).
fn parse_grid(section: &[u8], member: &str) -> Result<PolarGrid, String> {
    require_section(section, 3, 41, member)?;
    let template = be_u16(section, 12, member)?;
    if template != GRID_TEMPLATE_AZIMUTH_RANGE {
        return Err(format!(
            "{member}: unsupported GRIB2 grid template 3.{template} (need 3.{GRID_TEMPLATE_AZIMUTH_RANGE})"
        ));
    }
    let gate_count = be_u32(section, 14, member)? as usize;
    let radial_count = be_u32(section, 18, member)? as usize;
    let expected = be_u32(section, 6, member)? as usize;
    if gate_count == 0
        || radial_count == 0
        || gate_count.checked_mul(radial_count) != Some(expected)
    {
        return Err(format!(
            "{member}: grid point count mismatch: {gate_count} gates x {radial_count} radials != {expected}"
        ));
    }
    if gate_count > MAX_GRID_GATES || radial_count > MAX_GRID_RADIALS {
        return Err(limit_error(format!(
            "{member}: grid dimensions {gate_count} gates x {radial_count} radials exceed limits {MAX_GRID_GATES} x {MAX_GRID_RADIALS}"
        )));
    }
    if expected > MAX_GRID_POINTS {
        return Err(limit_error(format!(
            "{member}: grid declares {expected} points (limit {MAX_GRID_POINTS})"
        )));
    }
    Ok(PolarGrid {
        gate_count,
        radial_count,
        gate_spacing_m: be_u32(section, 30, member)? as f32 / 1000.0,
        range_start_m: be_u32(section, 34, member)? as f32 / 1000.0,
        scan_mode: section[38],
        start_azimuth_deg: be_u16(section, 39, member)? as f32 / 100.0,
    })
}

/// Product Definition Template 4.51022 (radar site/elevation parameters).
fn parse_product(section: &[u8], grid: &PolarGrid, member: &str) -> Result<SweepProduct, String> {
    let min_len = grid
        .radial_count
        .checked_mul(4)
        .and_then(|bytes| 60usize.checked_add(bytes))
        .ok_or_else(|| format!("{member}: product radial table length overflow"))?;
    require_section(section, 4, min_len, member)?;
    let template = be_u16(section, 7, member)?;
    if template != PRODUCT_TEMPLATE_RADAR_ELEVATION {
        return Err(format!(
            "{member}: unsupported GRIB2 product template 4.{template} (need 4.{PRODUCT_TEMPLATE_RADAR_ELEVATION})"
        ));
    }

    let station = parse_station_from_product(section, member)?;
    let elevation_deg =
        signed_magnitude_i16(be_u16(section, 41, member)?).map(|value| f32::from(value) / 100.0);
    let mut ray_elevation_deg = Vec::new();
    ray_elevation_deg
        .try_reserve_exact(grid.radial_count)
        .map_err(|err| format!("{member}: cannot reserve per-ray elevation table: {err}"))?;
    for ray in 0..grid.radial_count {
        let offset = 60 + ray * 4;
        ray_elevation_deg.push(
            signed_magnitude_i16(be_u16(section, offset, member)?)
                .map(|value| f32::from(value) / 100.0),
        );
    }

    Ok(SweepProduct {
        moment: moment_for_parameter(section[9], section[10]),
        station,
        elevation_deg,
        ray_elevation_deg,
    })
}

/// WMO Code table 4.2, discipline 0 (meteorological), category 15 (radar):
/// 1 = reflectivity (dBZ), 2 = radial velocity (m/s). Anything else is
/// preserved as an unknown moment instead of being dropped.
fn moment_for_parameter(category: u8, number: u8) -> MomentType {
    match (category, number) {
        (15, 1) => MomentType::Reflectivity,
        (15, 2) => MomentType::Velocity,
        (category, number) => MomentType::Unknown(format!("JMA_{category}_{number}")),
    }
}

fn parse_station_from_product(section: &[u8], member: &str) -> Result<JmaStationHeader, String> {
    let latitude_deg = signed_magnitude_i32(be_u32(section, 14, member)?)
        .map(|value| f64::from(value) / 1_000_000.0)
        .ok_or_else(|| format!("{member}: product section has no site latitude"))?;
    let longitude_deg = f64::from(be_u32(section, 18, member)?) / 1_000_000.0;
    let elevation_m =
        signed_magnitude_i16(be_u16(section, 22, member)?).map(|value| f32::from(value) / 10.0);
    let id = ascii_trim(&section[24..28]);
    let number = be_u16(section, 28, member)?;
    if id.is_empty() {
        return Err(format!("{member}: product section has an empty station id"));
    }
    Ok(JmaStationHeader {
        id,
        number,
        latitude_deg,
        longitude_deg,
        elevation_m,
    })
}

/// Header-only parse: walk sections up to the first product section and
/// return the station identity without touching any data section.
fn parse_station_header(bytes: &[u8], member: &str) -> Result<JmaStationHeader, String> {
    let msg = grib2_message(bytes, member)?;
    let sections = scan_sections(msg, member)?;
    for section in sections {
        if section.number == 4 {
            let body = section_bytes(msg, section);
            require_section(body, 4, 60, member)?;
            let template = be_u16(body, 7, member)?;
            if template != PRODUCT_TEMPLATE_RADAR_ELEVATION {
                return Err(format!(
                    "{member}: unsupported GRIB2 product template 4.{template} (need 4.{PRODUCT_TEMPLATE_RADAR_ELEVATION})"
                ));
            }
            return parse_station_from_product(body, member);
        }
    }
    Err(format!("{member}: GRIB2 message has no product section"))
}

/// Data Representation Template 5.200: expand the run-length packed level
/// stream and map levels through the level-value table (level 0 = missing).
fn decode_data(
    section5: &[u8],
    section6: &[u8],
    section7: &[u8],
    expected_points: usize,
    member: &str,
) -> Result<Vec<f32>, String> {
    require_section(section5, 5, 17, member)?;
    require_section(section6, 6, 6, member)?;
    require_section(section7, 7, 5, member)?;

    let encoded_points = be_u32(section5, 5, member)? as usize;
    let template = be_u16(section5, 9, member)?;
    if template != DATA_TEMPLATE_RUN_LENGTH {
        return Err(format!(
            "{member}: unsupported GRIB2 data template 5.{template} (need 5.{DATA_TEMPLATE_RUN_LENGTH})"
        ));
    }
    if section6[5] != 255 {
        return Err(format!(
            "{member}: bitmap indicator {} is not supported (need 255 = none)",
            section6[5]
        ));
    }
    if encoded_points != expected_points {
        return Err(format!(
            "{member}: encoded point count {encoded_points} != grid points {expected_points}"
        ));
    }
    if expected_points > MAX_GRID_POINTS {
        return Err(limit_error(format!(
            "{member}: data section declares {expected_points} output points (limit {MAX_GRID_POINTS})"
        )));
    }

    let num_bits = section5[11];
    let max_value = be_u16(section5, 12, member)?;
    let max_level = be_u16(section5, 14, member)?;
    let decimal_scale = section5[16];
    let max_level = usize::from(max_level);
    if max_level > MAX_LEVEL_VALUES {
        return Err(limit_error(format!(
            "{member}: level table declares {max_level} values (limit {MAX_LEVEL_VALUES})"
        )));
    }
    let levels_start = 17usize;
    let levels_end = max_level
        .checked_mul(2)
        .and_then(|bytes| levels_start.checked_add(bytes))
        .ok_or_else(|| format!("{member}: level table length overflow"))?;
    if levels_end > section5.len() {
        return Err(format!(
            "{member}: data-representation section ends before all {max_level} level values"
        ));
    }

    let mut level_values = Vec::new();
    level_values
        .try_reserve_exact(max_level.saturating_add(1))
        .map_err(|err| format!("{member}: cannot reserve level table: {err}"))?;
    level_values.push(f32::NAN); // level 0 = missing
    let factor = 10_f32.powi(-i32::from(decimal_scale));
    for index in 0..max_level {
        let raw = be_u16(section5, levels_start + index * 2, member)?;
        level_values.push(
            signed_magnitude_i16(raw)
                .map(|value| f32::from(value) * factor)
                .unwrap_or(f32::NAN),
        );
    }

    let levels = run_length_decode(&section7[5..], num_bits, max_value, expected_points)
        .map_err(|err| format!("{member}: {err}"))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(expected_points)
        .map_err(|err| format!("{member}: cannot reserve decoded value plane: {err}"))?;
    for level in levels {
        values.push(
            level_values
                .get(usize::from(level))
                .copied()
                .ok_or_else(|| format!("{member}: run-length level {level} exceeds level table"))?,
        );
    }
    Ok(values)
}

/// JMA run-length scheme (DRT 5.200): values `<= max_value` are literal
/// levels; values above it are base-`lngu` digits of a repeat count for the
/// previous literal, where `lngu = 2^num_bits - (max_value + 1)`.
fn run_length_decode(
    bytes: &[u8],
    num_bits: u8,
    max_value: u16,
    expected_len: usize,
) -> Result<Vec<u16>, String> {
    if expected_len > MAX_GRID_POINTS {
        return Err(limit_error(format!(
            "run-length output requests {expected_len} values (limit {MAX_GRID_POINTS})"
        )));
    }
    if num_bits == 0 || num_bits > 16 {
        return Err(format!("unsupported run-length packed width {num_bits}"));
    }
    let rlbase = max_value
        .checked_add(1)
        .ok_or_else(|| "run-length base overflow".to_owned())?;
    // checked_sub: a header declaring max_value >= 2^num_bits is malformed
    // network data, not a panic (review finding — debug builds underflowed
    // here, release builds silently wrapped).
    let lngu = (1u32 << num_bits)
        .checked_sub(u32::from(rlbase))
        .ok_or_else(|| format!("run-length base {rlbase} exceeds the {num_bits}-bit value range"))?
        as usize;
    if lngu == 0 {
        return Err("invalid run-length base".to_owned());
    }

    let mut out: Vec<u16> = Vec::new();
    out.try_reserve_exact(expected_len)
        .map_err(|err| format!("cannot reserve run-length output: {err}"))?;
    let mut cached: Option<u16> = None;
    let mut exp = 1usize;
    for value in BitValues::new(bytes, num_bits) {
        if value < rlbase {
            if out.len() >= expected_len {
                break;
            }
            out.push(value);
            cached = Some(value);
            exp = 1;
        } else {
            let prev = cached.ok_or_else(|| "first run-length value is a run marker".to_owned())?;
            let repeat = usize::from(value - rlbase)
                .checked_mul(exp)
                .ok_or_else(|| "run-length repeat count overflow".to_owned())?;
            let next_len = out
                .len()
                .checked_add(repeat)
                .ok_or_else(|| "run-length output length overflow".to_owned())?;
            if next_len > expected_len {
                return Err(format!(
                    "run-length stream expands past expected length {expected_len}"
                ));
            }
            out.resize(next_len, prev);
            exp = exp
                .checked_mul(lngu)
                .ok_or_else(|| "run-length exponent overflow".to_owned())?;
        }
        if out.len() == expected_len {
            break;
        }
    }

    if out.len() != expected_len {
        return Err(format!(
            "run-length stream decoded {} values, expected {expected_len}",
            out.len()
        ));
    }
    Ok(out)
}

/// Big-endian fixed-width bit reader for the run-length stream.
struct BitValues<'a> {
    bytes: &'a [u8],
    width: u8,
    bit_pos: usize,
}

impl<'a> BitValues<'a> {
    fn new(bytes: &'a [u8], width: u8) -> Self {
        Self {
            bytes,
            width,
            bit_pos: 0,
        }
    }
}

impl Iterator for BitValues<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        let total_bits = self.bytes.len() * 8;
        if self.bit_pos + usize::from(self.width) > total_bits {
            return None;
        }
        let mut value = 0u16;
        for _ in 0..self.width {
            let byte = self.bytes[self.bit_pos / 8];
            let shift = 7 - (self.bit_pos % 8);
            value = (value << 1) | u16::from((byte >> shift) & 1);
            self.bit_pos += 1;
        }
        Some(value)
    }
}

fn require_section(section: &[u8], number: u8, min_len: usize, member: &str) -> Result<(), String> {
    if section.len() < min_len {
        return Err(format!(
            "{member}: GRIB2 section {number} too short: {} < {min_len}",
            section.len()
        ));
    }
    if section[4] != number {
        return Err(format!(
            "{member}: expected GRIB2 section {number}, got {}",
            section[4]
        ));
    }
    Ok(())
}

fn ascii_trim(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

/// JMA GRIB2 signed fields use sign-and-magnitude with all-ones = missing.
fn signed_magnitude_i16(raw: u16) -> Option<i16> {
    if raw == u16::MAX {
        return None;
    }
    let magnitude = (raw & 0x7fff) as i16;
    Some(if raw & 0x8000 != 0 {
        -magnitude
    } else {
        magnitude
    })
}

fn signed_magnitude_i32(raw: u32) -> Option<i32> {
    if raw == u32::MAX {
        return None;
    }
    let magnitude = (raw & 0x7fff_ffff) as i32;
    Some(if raw & 0x8000_0000 != 0 {
        -magnitude
    } else {
        magnitude
    })
}

fn be_u16(bytes: &[u8], offset: usize, member: &str) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .ok_or_else(|| format!("{member}: u16 read past section end at offset {offset}"))
}

fn be_u32(bytes: &[u8], offset: usize, member: &str) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .map(|quad| u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]]))
        .ok_or_else(|| format!("{member}: u32 read past section end at offset {offset}"))
}

fn be_u64(bytes: &[u8], offset: usize, member: &str) -> Result<u64, String> {
    bytes
        .get(offset..offset + 8)
        .map(|oct| {
            u64::from_be_bytes([
                oct[0], oct[1], oct[2], oct[3], oct[4], oct[5], oct[6], oct[7],
            ])
        })
        .ok_or_else(|| format!("{member}: u64 read past section end at offset {offset}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real inputs: corpus entries `jma-n5-20191012-090000-rs47773` and
    // `jma-n6-20191012-090000-rs47773` (committed single-member tars of the
    // Osaka/Takayasu TAKA station, Typhoon Hagibis) and the full NICT tars
    // `jma-n5-20191012-090000` / `jma-n6-20191012-090000` (downloads, 20
    // stations each).
    //
    // Expected values: tools/golden_io_formats.py, section `jma` (ustar
    // headers read with the POSIX layout; GRIB2 sections, templates 3.50120 /
    // 4.51022 / 5.200 and the DRT 5.200 run-length stream decoded by an
    // independent Python walker).

    const N5_TAKA: &str = "jma-n5-20191012-090000-rs47773";
    const N6_TAKA: &str = "jma-n6-20191012-090000-rs47773";

    fn corpus(id: &str) -> Vec<u8> {
        recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
    }

    /// The member's header block plus its padded data blocks (no end-of-archive
    /// zero blocks), cut from a real single-member tar.
    fn member_blocks(tar: &[u8]) -> &[u8] {
        let size = tar_octal(&tar[TAR_SIZE_OFFSET..TAR_SIZE_OFFSET + TAR_SIZE_LEN]).unwrap();
        &tar[..TAR_BLOCK_LEN + size.div_ceil(TAR_BLOCK_LEN) * TAR_BLOCK_LEN]
    }

    /// A tar holding the real N5 (reflectivity) member followed by the real N6
    /// (velocity) member of TAKA, exactly as both appear in their NICT tars.
    fn taka_n5_then_n6() -> Vec<u8> {
        let n5 = corpus(N5_TAKA);
        let n6 = corpus(N6_TAKA);
        let mut tar = member_blocks(&n5).to_vec();
        tar.extend_from_slice(&n6);
        tar
    }

    /// Stations of both full tars in `jma-n5-20191012-090000` member order
    /// (station id, number, latitude, longitude, altitude m): ustar member
    /// names and PDT 4.51022 octets.
    const N5_STATIONS: [(&str, u16, f64, f64, f32); 20] = [
        ("MURO", 47899, 33.252222, 134.177222, 198.9),
        ("AKIT", 47582, 39.717778, 140.099444, 55.3),
        ("KASH", 47695, 35.859722, 139.959722, 74.0),
        ("HAIG", 47792, 34.270278, 132.593333, 746.9),
        ("SAPP", 47415, 43.138889, 141.009722, 749.0),
        ("ITOK", 47937, 26.153333, 127.764444, 208.2),
        ("TANE", 47869, 30.639444, 130.978611, 290.5),
        ("MISA", 47791, 35.541667, 133.103333, 553.0),
        ("SEND", 47590, 38.262222, 140.896667, 98.2),
        ("ISHI", 47920, 24.426667, 124.182222, 533.5),
        ("KURU", 47611, 36.103056, 138.195833, 1937.1),
        ("TAKA", 47773, 34.616389, 135.656389, 497.6),
        ("FUNC", 47909, 28.394167, 129.551944, 318.8),
        ("SEFU", 47806, 33.434722, 130.356944, 982.7),
        ("KUSH", 47419, 42.960833, 144.5175, 121.5),
        ("MAKI", 47659, 34.742778, 138.133611, 186.0),
        ("TOJI", 47705, 36.2375, 136.142222, 107.0),
        ("HAKO", 47432, 41.933611, 140.781389, 1141.7),
        ("NAGO", 47636, 35.168333, 136.964722, 73.1),
        ("YAHI", 47572, 37.718611, 138.816111, 645.0),
    ];

    /// Member order of `jma-n6-20191012-090000`.
    const N6_ORDER: [&str; 20] = [
        "AKIT", "MURO", "KASH", "ITOK", "SEND", "SAPP", "HAIG", "MISA", "KURU", "FUNC", "TANE",
        "KUSH", "TAKA", "ISHI", "SEFU", "MAKI", "NAGO", "TOJI", "HAKO", "YAHI",
    ];

    /// TAKA N5 sweep elevations in scan order (26 sweeps, four descending
    /// ladders) and their gate counts.
    const N5_TAKA_SCAN_ORDER: [(f32, usize); 26] = [
        (5.0, 300),
        (2.5, 300),
        (1.2, 300),
        (0.3, 500),
        (1.8, 800),
        (1.2, 800),
        (0.7, 800),
        (0.3, 800),
        (0.0, 800),
        (25.0, 300),
        (18.0, 300),
        (13.0, 300),
        (9.5, 300),
        (6.9, 300),
        (5.0, 300),
        (2.5, 300),
        (1.2, 300),
        (0.3, 500),
        (5.0, 800),
        (3.6, 800),
        (2.5, 800),
        (1.8, 800),
        (1.2, 800),
        (0.7, 800),
        (0.3, 800),
        (0.0, 800),
    ];
    /// TAKA N6 sweep elevations in scan order (13 sweeps).
    const N6_TAKA_SCAN_ORDER: [f32; 13] = [
        5.0, 2.5, 1.2, 0.3, 25.0, 18.0, 13.0, 9.5, 6.9, 5.0, 2.5, 1.2, 0.3,
    ];

    fn assert_station(volume: &RadarVolume, id: &str) {
        let (_, number, latitude, longitude, altitude) = *N5_STATIONS
            .iter()
            .find(|station| station.0 == id)
            .expect("known station");
        assert_eq!(volume.site.id, id);
        let expected_name = format!("RS{number}");
        assert_eq!(volume.site.name.as_deref(), Some(expected_name.as_str()));
        assert!((f64::from(volume.site.latitude_deg.unwrap()) - latitude).abs() < 1e-5);
        assert!((f64::from(volume.site.longitude_deg.unwrap()) - longitude).abs() < 1e-4);
        assert_eq!(volume.site.elevation_m, Some(altitude), "{id} altitude");
        assert_eq!(volume.volume_time.to_rfc3339(), "2019-10-12T09:00:00+00:00");
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
    }

    /// JMA level values are signed-magnitude integers scaled by 10^-2 in f32.
    fn assert_level(actual: Option<f32>, expected: f32) {
        let actual = actual.expect("gate value");
        assert!((actual - expected).abs() < 1e-4, "{actual} != {expected}");
    }

    /// Cut-by-cut equality with F32 planes compared bitwise (NaN = missing).
    fn assert_same_cuts(left: &RadarVolume, right: &RadarVolume) {
        assert_eq!(left.cuts.len(), right.cuts.len());
        for (a, b) in left.cuts.iter().zip(&right.cuts) {
            assert_eq!(a.elevation_deg, b.elevation_deg);
            assert_eq!(a.radials, b.radials);
            assert_eq!(a.moments.len(), b.moments.len());
            for (moment, grid) in &a.moments {
                let (MomentStorage::F32(x), MomentStorage::F32(y)) =
                    (&grid.storage, &b.moments[moment].storage)
                else {
                    panic!("JMA planes are F32");
                };
                assert!(
                    x.iter()
                        .map(|v| v.to_bits())
                        .eq(y.iter().map(|v| v.to_bits()))
                );
            }
        }
    }

    // -- pure run-length / signed-magnitude checks ------------------------

    #[test]
    fn decodes_signed_magnitude_fields() {
        assert_eq!(signed_magnitude_i16(0x0032), Some(50));
        assert_eq!(signed_magnitude_i16(0x8032), Some(-50));
        assert_eq!(signed_magnitude_i16(0xffff), None);
        assert_eq!(signed_magnitude_i32(0x8000_0032), Some(-50));
        assert_eq!(signed_magnitude_i32(0xffff_ffff), None);
    }

    /// The worked run-length example from the JMA GRIB2 format notes for
    /// DRT 5.200 (same vector the jma-radar-bridge decoder validates).
    #[test]
    fn decodes_jma_run_length_reference_example() {
        let input: Vec<u8> = [3u8, 9, 12, 6, 4, 15, 2, 1, 0, 13, 12, 2, 3]
            .iter()
            .map(|n| n + 240)
            .collect();
        let expected: Vec<u16> = [
            3u16, 9, 9, 6, 4, 4, 4, 4, 4, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3,
        ]
        .iter()
        .map(|n| n + 240)
        .collect();
        assert_eq!(
            run_length_decode(&input, 8, 250, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn run_length_rejects_leading_run_marker_and_short_streams() {
        let err = run_length_decode(&[251], 8, 250, 4).unwrap_err();
        assert!(err.contains("run marker"), "unexpected error: {err}");
        let err = run_length_decode(&[1, 2], 8, 250, 4).unwrap_err();
        assert!(err.contains("decoded 2"), "unexpected error: {err}");
    }

    #[test]
    fn run_length_rejects_a_base_exceeding_the_bit_width() {
        // Review repro: max_value >= 2^num_bits underflowed `2^n - rlbase`
        // and panicked in debug builds (wrapped silently in release).
        let err = run_length_decode(&[1, 2], 8, 300, 4).unwrap_err();
        assert!(
            err.contains("exceeds the 8-bit value range"),
            "unexpected error: {err}"
        );
        let err = run_length_decode(&[1, 2], 8, u16::MAX - 1, 4).unwrap_err();
        assert!(err.contains("exceeds"), "unexpected error: {err}");
    }

    #[test]
    fn tiny_run_length_payload_cannot_request_an_oversized_plane() {
        let err = run_length_decode(&[1], 8, 250, MAX_GRID_POINTS + 1).unwrap_err();
        assert!(
            err.contains("run-length output requests") && err.contains("limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn grid_axis_limits_reject_pathological_radial_tables() {
        // First GRIB2 section 3 of the TAKA N5 member: member data at tar
        // offset 512, section at message offset 37 (golden
        // n5_rs47773.sweeps[0].grid.section3_offset).
        let tar = corpus(N5_TAKA);
        let start = TAR_BLOCK_LEN + 37;
        let length = u32::from_be_bytes(tar[start..start + 4].try_into().unwrap()) as usize;
        let mut section = tar[start..start + length].to_vec();
        assert_eq!(section[4], 3);
        let grid = parse_grid(&section, "RS47773").expect("real grid section");
        // golden: 300 gates x 512 radials (153600 points), 500 m gates from
        // 0 m, clockwise, start azimuth 11.25 deg.
        assert_eq!((grid.gate_count, grid.radial_count), (300, 512));
        assert_eq!((grid.gate_spacing_m, grid.range_start_m), (500.0, 0.0));
        assert!(grid.scans_clockwise());
        assert_eq!(grid.start_azimuth_deg, 11.25);

        // Claim MAX_GRID_RADIALS + 1 radials (and a consistent point count).
        let radials = (MAX_GRID_RADIALS + 1) as u32;
        section[18..22].copy_from_slice(&radials.to_be_bytes());
        section[6..10].copy_from_slice(&(300 * radials).to_be_bytes());
        let err = parse_grid(&section, "RS47773")
            .err()
            .expect("pathological grid must be rejected");
        assert!(
            err.contains("grid dimensions") && err.contains("exceed limits"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn oversized_tar_member_is_rejected_from_its_header() {
        let mut tar = corpus(N5_TAKA);
        // golden n5_rs47773.tar.members[0].size = 1752093.
        let members = ustar_members(&tar).expect("real tar");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].data.len(), 1_752_093);
        let size = format!("{:011o}\0", MAX_JMA_MEMBER_BYTES + 1);
        tar[TAR_SIZE_OFFSET..TAR_SIZE_OFFSET + TAR_SIZE_LEN].copy_from_slice(size.as_bytes());
        let err = ustar_members(&tar).err().expect("oversized member error");
        assert!(
            err.contains("declares") && err.contains("limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sniffs_jma_tar_bytes() {
        // Manifest format jma-grib2-tar; ustar magic at byte 257 and a
        // Z__C_RJTD_*_RDR_JMAGPV member name.
        let n5 = corpus(N5_TAKA);
        assert_eq!(&n5[TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 5], b"ustar");
        assert!(looks_like_jma_tar_bytes(&n5));
        assert!(looks_like_jma_tar_bytes(&corpus(N6_TAKA)));
        // Too short for one header block.
        assert!(!looks_like_jma_tar_bytes(&n5[..TAR_BLOCK_LEN - 1]));
        // The same real ustar header naming a non-JMA member.
        let mut renamed = n5.clone();
        renamed[..10].copy_from_slice(b"notjma.bin");
        assert!(!looks_like_jma_tar_bytes(&renamed));
        // Non-tar real files.
        assert!(!looks_like_jma_tar_bytes(&corpus(
            "odim-bejab-20190606-0000-pvol"
        )));
        assert!(!looks_like_jma_tar_bytes(&corpus(
            "l2-ktlx-20240315-000217-trim"
        )));
    }

    /// Field report: tilt #00 on JMA radars showed the ~25 deg cone because
    /// members carry sweeps high-tilt-first, and a station split across tar
    /// members restarted its numbering. The ladder must come back lowest beam
    /// first with sequential numbering.
    #[test]
    fn cuts_sort_lowest_elevation_first_across_members() {
        let volumes =
            decode_jma_tar_volumes(&taka_n5_then_n6(), None).expect("decode TAKA N5 + N6");
        assert_eq!(volumes.len(), 1, "same station must merge");
        let cuts = &volumes[0].cuts;
        assert_eq!(cuts.len(), 26 + 13);

        // Stable sort of the N5 scan-order elevations followed by the N6 ones.
        let mut expected: Vec<(f32, MomentType)> = N5_TAKA_SCAN_ORDER
            .iter()
            .map(|(elevation, _)| (*elevation, MomentType::Reflectivity))
            .chain(
                N6_TAKA_SCAN_ORDER
                    .iter()
                    .map(|elevation| (*elevation, MomentType::Velocity)),
            )
            .collect();
        expected.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (index, (cut, (elevation, moment))) in cuts.iter().zip(&expected).enumerate() {
            assert_eq!(cut.elevation_deg, *elevation, "cut {index}");
            assert!(cut.moments.contains_key(moment), "cut {index} {moment}");
            assert_eq!(cut.elevation_number, Some(index as u8 + 1));
        }
        assert_eq!(cuts[0].elevation_deg, 0.0);
        assert_eq!(cuts[38].elevation_deg, 25.0);
    }

    #[test]
    fn decodes_single_station_member_with_real_gate_values() {
        let volumes = decode_jma_tar_volumes(&corpus(N5_TAKA), None).expect("decode TAKA N5");
        assert_eq!(volumes.len(), 1);
        let volume = &volumes[0];
        assert_station(volume, "TAKA");
        assert_eq!(volume.metadata.decoded_radial_count, 26 * 512);

        // Stable sort: the two 0.0 deg sweeps are scan indices 8 then 25.
        let lowest = &volume.cuts[0];
        assert_eq!(lowest.elevation_deg, 0.0);
        assert_eq!(lowest.radials.len(), 512);
        let gates = &lowest.radials[0].gate_range;
        assert_eq!(
            (gates.first_gate_m, gates.gate_spacing_m, gates.gate_count),
            (0, 500, 800)
        );
        // Scan index 8: start azimuth 261.56 deg, clockwise 360/512 steps;
        // per-ray elevation table 0.09 deg wins over the 0.0 product angle.
        assert_eq!(lowest.radials[0].azimuth_deg, 261.56);
        assert!((lowest.radials[1].azimuth_deg - (261.56 + 0.703_125)).abs() < 1e-4);
        assert_eq!(lowest.radials[0].elevation_deg, 0.09);
        let reflectivity = &lowest.moments[&MomentType::Reflectivity];
        assert_level(reflectivity.scaled_value(100, 20), 12.96);
        assert_level(reflectivity.scaled_value(0, 0), 0.0);
        assert_eq!(volume.cuts[1].radials[0].azimuth_deg, 142.73);
        assert_level(
            volume.cuts[1].moments[&MomentType::Reflectivity].scaled_value(256, 100),
            13.6,
        );
        // Scan index 3 (0.3 deg, 500 gates, per-ray 0.26 deg).
        let third = &volume.cuts[2];
        assert_eq!(third.radials[0].gate_range.gate_count, 500);
        assert_eq!(third.radials[0].elevation_deg, 0.26);
        assert_level(
            third.moments[&MomentType::Reflectivity].scaled_value(100, 20),
            10.08,
        );
        assert_level(
            third.moments[&MomentType::Reflectivity].scaled_value(256, 100),
            10.72,
        );

        // N6 velocity: level 0 = missing -> NaN. Sorted cut 0 is scan index 3
        // (0.3 deg, 500 gates, per-ray elevation table missing -> product
        // angle, first valid gate [0,1] = -4.0 m/s, 66708 valid gates).
        let velocity_volume = &decode_jma_tar_volumes(&corpus(N6_TAKA), None).unwrap()[0];
        let low = &velocity_volume.cuts[0];
        assert_eq!(low.radials[0].elevation_deg, 0.3);
        assert_eq!(low.radials[0].azimuth_deg, 243.98);
        let velocity = &low.moments[&MomentType::Velocity];
        assert!(velocity.scaled_value(0, 0).is_some_and(f32::is_nan));
        assert_level(velocity.scaled_value(0, 1), -4.0);
        assert_level(velocity.scaled_value(100, 20), -18.5);
        let valid = |grid: &MomentGrid| {
            let MomentStorage::F32(values) = &grid.storage else {
                panic!("JMA planes are F32");
            };
            values.iter().filter(|value| value.is_finite()).count()
        };
        assert_eq!(valid(velocity), 66_708);
        // Every N6 sweep: 547108 non-missing gates in total (manifest).
        let total: usize = velocity_volume
            .cuts
            .iter()
            .map(|cut| valid(&cut.moments[&MomentType::Velocity]))
            .sum();
        assert_eq!(total, 547_108);
    }

    #[test]
    fn decodes_every_station_in_archive_order() {
        // The full N6 (velocity) tar: 20 members of 13 sweeps each, 44032000
        // grid points (golden n6_full.total_grid_points). The full N5 tar holds
        // 150528000 points (n5_full.total_grid_points), over the 67108864-point
        // MAX_POINTS_PER_DECODE limit, so an unfiltered N5 decode is refused;
        // N5 stations are decoded with a site filter below.
        let tar = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n6-20191012-090000"
        ))
        .expect("read N6 tar");
        let volumes = decode_jma_tar_volumes(&tar, None).expect("20-station decode");
        assert_eq!(volumes.len(), 20, "every station must come back");
        for (volume, id) in volumes.iter().zip(N6_ORDER) {
            assert_station(volume, id);
            assert_eq!(volume.cuts.len(), 13, "{id}");
            assert!(volume.cuts.iter().all(|cut| cut.radials.len() == 512));
            assert!(
                volume
                    .cuts
                    .iter()
                    .all(|cut| cut.moments.contains_key(&MomentType::Velocity))
            );
        }
        // The TAKA member decodes exactly as the committed single-member tar.
        let taka = decode_jma_tar_volumes(&corpus(N6_TAKA), None)
            .unwrap()
            .remove(0);
        assert_same_cuts(&volumes[12], &taka);
    }

    #[test]
    fn site_filter_selects_one_station_by_id_or_number() {
        let tar = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n5-20191012-090000"
        ))
        .expect("read N5 tar");
        for filter in ["TAKA", "taka", "RS47773", "47773"] {
            let volumes = decode_jma_tar_volumes(&tar, Some(filter)).expect("filtered decode");
            assert_eq!(volumes.len(), 1, "filter '{filter}'");
            assert_station(&volumes[0], "TAKA");
        }
        let err = decode_jma_tar_volumes(&tar, Some("NOPE")).unwrap_err();
        assert!(err.to_string().contains("NOPE"), "unexpected error: {err}");
    }

    #[test]
    fn first_station_decode_takes_the_first_member_only() {
        let n5 = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n5-20191012-090000"
        ))
        .expect("read N5 tar");
        let volume = decode_jma_tar_first_station(&n5).expect("first-station decode");
        assert_station(&volume, "MURO"); // first member RS47899
        assert_eq!(volume.cuts.len(), 26);
        // The N6 tar lists AKIT (RS47582) first.
        let n6 = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n6-20191012-090000"
        ))
        .expect("read N6 tar");
        let volume = decode_jma_tar_first_station(&n6).expect("first-station decode");
        assert_eq!(volume.site.id, "AKIT");
        assert_eq!(volume.cuts.len(), 13);
    }

    #[test]
    fn station_headers_skip_gate_data_and_dedupe() {
        // Committed members: the TAKA N5 and N6 members name one station.
        let stations = jma_tar_station_headers(&taka_n5_then_n6()).expect("station headers");
        assert_eq!(stations.len(), 1);
        assert_eq!(
            (stations[0].id.as_str(), stations[0].number),
            ("TAKA", 47773)
        );
        assert!((stations[0].latitude_deg - 34.616389).abs() < 1e-9);
        assert!((stations[0].longitude_deg - 135.656389).abs() < 1e-9);
        assert_eq!(stations[0].elevation_m, Some(497.6));

        // Full N5 members followed by the full N6 tar: 40 members, 20 unique
        // stations in first-seen (N5) order.
        let n5 = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n5-20191012-090000"
        ))
        .expect("read N5 tar");
        let n6 = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n6-20191012-090000"
        ))
        .expect("read N6 tar");
        // golden n5_full: last member YAHI data ends at 35083264 + 4012647.
        let n5_members_end = (35_083_264 + 4_012_647usize).div_ceil(TAR_BLOCK_LEN) * TAR_BLOCK_LEN;
        let mut both = n5[..n5_members_end].to_vec();
        both.extend_from_slice(&n6);
        let stations = jma_tar_station_headers(&both).expect("station headers");
        assert_eq!(stations.len(), 20);
        for (station, (id, number, latitude, longitude, altitude)) in
            stations.iter().zip(N5_STATIONS)
        {
            assert_eq!((station.id.as_str(), station.number), (id, number));
            assert!((station.latitude_deg - latitude).abs() < 1e-9, "{id}");
            assert!((station.longitude_deg - longitude).abs() < 1e-9, "{id}");
            assert_eq!(station.elevation_m, Some(altitude), "{id}");
        }
    }

    #[test]
    fn repeated_station_members_merge_into_one_volume() {
        let volumes = decode_jma_tar_volumes(&taka_n5_then_n6(), None).expect("merged decode");
        assert_eq!(volumes.len(), 1);
        let volume = &volumes[0];
        assert_station(volume, "TAKA");
        assert_eq!(volume.metadata.message_count, 26 + 13);
        assert_eq!(volume.metadata.decoded_radial_count, 39 * 512);
        let reflectivity_cuts = volume
            .cuts
            .iter()
            .filter(|cut| cut.moments.contains_key(&MomentType::Reflectivity))
            .count();
        let velocity_cuts = volume
            .cuts
            .iter()
            .filter(|cut| cut.moments.contains_key(&MomentType::Velocity))
            .count();
        assert_eq!((reflectivity_cuts, velocity_cuts), (26, 13));
    }

    #[test]
    fn corrupt_member_is_skipped_but_alone_is_an_error() {
        // A lone TAKA member whose GRIB indicator is overwritten.
        let mut lone = corpus(N5_TAKA);
        assert_eq!(&lone[TAR_BLOCK_LEN..TAR_BLOCK_LEN + 4], GRIB_MAGIC);
        lone[TAR_BLOCK_LEN..TAR_BLOCK_LEN + 4].fill(0);
        let err = decode_jma_tar_volumes(&lone, None).unwrap_err();
        assert!(err.to_string().contains("GRIB"), "unexpected error: {err}");

        // The same tar whose only member is no longer a JMA data member.
        let mut renamed = corpus(N5_TAKA);
        renamed[..10].copy_from_slice(b"notjma.bin");
        let err = decode_jma_tar_volumes(&renamed, None).unwrap_err();
        assert!(
            err.to_string().contains("no Z__C_RJTD"),
            "unexpected error: {err}"
        );

        // TAKA N5 + N6 with the N5 member corrupted: the N6 member survives.
        let mut mixed = taka_n5_then_n6();
        mixed[TAR_BLOCK_LEN..TAR_BLOCK_LEN + 4].fill(0);
        let volumes = decode_jma_tar_volumes(&mixed, None).expect("good member survives");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].site.id, "TAKA");
        assert_eq!(volumes[0].cuts.len(), 13);
        assert!(
            volumes[0]
                .cuts
                .iter()
                .all(|cut| cut.moments.contains_key(&MomentType::Velocity))
        );
    }

    #[test]
    fn corrupt_station_in_full_archive_is_skipped() {
        // Full N6 tar with MURO's (second member, data at 1178624) GRIB
        // indicator overwritten: the other 19 stations decode in order.
        let mut tar = std::fs::read(recast_radar_testdata::require_file!(
            "jma-n6-20191012-090000"
        ))
        .expect("read N6 tar");
        const MURO_DATA: usize = 1_178_624;
        assert_eq!(&tar[MURO_DATA..MURO_DATA + 4], GRIB_MAGIC);
        tar[MURO_DATA..MURO_DATA + 4].fill(0);
        let expected: Vec<&str> = N6_ORDER.into_iter().filter(|id| *id != "MURO").collect();
        let volumes = decode_jma_tar_volumes(&tar, None).expect("19 stations survive");
        let ids: Vec<&str> = volumes
            .iter()
            .map(|volume| volume.site.id.as_str())
            .collect();
        assert_eq!(ids, expected);
        let stations = jma_tar_station_headers(&tar).expect("headers survive");
        assert_eq!(stations.len(), 19);
    }

    #[test]
    fn truncated_tar_member_is_an_error_not_a_panic() {
        let tar = corpus(N5_TAKA);
        let err = decode_jma_tar_volumes(&tar[..TAR_BLOCK_LEN + 17], None).unwrap_err();
        assert!(
            err.to_string().contains("overruns"),
            "unexpected error: {err}"
        );
    }
}
