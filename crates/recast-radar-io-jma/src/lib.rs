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
//! Multi-station handling: [`read_jma_tar_volumes`] returns ONE FM301
//! [`Volume`] per station and never silently drops stations; callers that
//! want a single station pass `site_filter`. The shared byte router
//! (`recast_radar_io::read_supported_volume_bytes`) uses
//! [`read_jma_tar_first_station`] instead, which keeps only the first
//! station in the archive.
//!
//! Each GRIB data section becomes one [`Sweep`], sorted lowest elevation
//! first (members carry sweeps high-tilt-first) and numbered in that order,
//! with FM301 field names (`DBZH` for Pze, `VRADH` for Pvr; other
//! parameters keep a `JMA_<category>_<number>` name). Values are stored as
//! physical `f32` planes (NaN = missing), exactly as the run-length level
//! table dictates — the level table is a lookup, not an affine
//! raw-to-physical mapping, so compact u8/u16 storage does not apply. The
//! `range` coordinate treats the template 3.50120 range start as the centre
//! of the first gate (the pre-FM301 convention; design note 17.9). Ray
//! azimuths follow the grid's start azimuth and scan direction; the format
//! has no per-ray times, so every ray of a sweep carries its GRIB reference
//! time. Nyquist velocity is left unset: the per-ray PRF tables suggest
//! staggered-PRF operation and a wrong Nyquist would mislead downstream
//! dealiasing.
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

use chrono::{DateTime, TimeZone, Utc};
use recast_radar_core::bounded_read::MAX_DECODED_BATCH_BYTES;
use recast_radar_core::model::{
    Field, FieldData, FieldName, FloatCoding, FollowMode, GateMapping, RangeCoord, SourceFormat,
    Sweep, SweepMode, Volume,
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

/// Decode a JMA radar GRIB2 tar into one [`Volume`] per station, in
/// archive order.
///
/// `site_filter` selects a single station by JMA id (e.g. `"ITOK"`,
/// case-insensitive) or by station number (`"RS47937"` / `"47937"`); `None`
/// decodes every station. Malformed members are skipped (the tar is network
/// data; one corrupt station must not take down the other nineteen) — the
/// first member error is returned only when nothing decodes. Never panics
/// on malformed input.
pub fn read_jma_tar_volumes(
    bytes: &[u8],
    site_filter: Option<&str>,
) -> Result<Vec<Volume>, JmaError> {
    decode_tar_volumes(bytes, site_filter).map_err(jma_error)
}

/// Bytes a ray occupies in the model (three coordinates).
const RAY_BYTES: usize = 3 * size_of::<f64>();

/// Allocated bytes of a volume's fields and ray tables.
pub fn volume_retained_bytes(volume: &Volume) -> usize {
    let rays: usize = volume.sweeps.iter().map(Sweep::nrays).sum();
    volume
        .sweeps
        .iter()
        .flat_map(|sweep| sweep.fields.iter())
        .fold(0usize, |total, field| {
            let bytes = match &field.data {
                FieldData::U8 { values, .. } => values.capacity(),
                FieldData::I8 { values, .. } => values.capacity(),
                FieldData::U16 { values, .. } => values.capacity().saturating_mul(2),
                FieldData::I16 { values, .. } => values.capacity().saturating_mul(2),
                FieldData::F32 { values, .. } => values.capacity().saturating_mul(4),
                FieldData::F64 { values, .. } => values.capacity().saturating_mul(8),
            };
            total.saturating_add(bytes)
        })
        .saturating_add(rays.saturating_mul(RAY_BYTES))
}

fn decode_tar_volumes(bytes: &[u8], site_filter: Option<&str>) -> Result<Vec<Volume>, String> {
    let members = ustar_members(bytes)?;
    let mut volumes: Vec<Volume> = Vec::new();
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
                let member_bytes = volume_retained_bytes(&volume);
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
        sort_sweeps_lowest_first(volume)?;
    }
    Ok(volumes)
}

/// JMA packs sweeps in whatever order the GRIB member carries them —
/// observed highest-tilt-first, so tilt 0 in the UI showed the ~25° cone
/// (field report) — and a station spanning several tar members restarts
/// its sweep numbering per member. Sort each station's ladder lowest
/// beam first (stable: repeated elevations — the 10-minute file's two
/// 5-minute repetitions — keep their scan order) and renumber, matching
/// every other provider's sweep order.
fn sort_sweeps_lowest_first(volume: &mut Volume) -> Result<(), String> {
    volume
        .sweeps
        .sort_by(|a, b| a.fixed_angle_deg.total_cmp(&b.fixed_angle_deg));
    for (index, sweep) in volume.sweeps.iter_mut().enumerate() {
        sweep.sweep_number = index as u32;
        sweep.elevation_number = u16::try_from(index + 1).ok();
    }
    volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
    volume.provenance.decode.message_count = volume.sweeps.len();
    volume
        .seal()
        .map_err(|err| format!("JMA volume violates the model invariants: {err}"))?;
    volume.time_coverage = volume.ray_time_extent();
    Ok(())
}

/// Decode only the FIRST station of a JMA tar — the shared byte router's
/// entry point, where the contract is one volume per buffer. Providers that
/// need a specific station call [`read_jma_tar_volumes`] with a
/// `site_filter` instead.
pub fn read_jma_tar_first_station(bytes: &[u8]) -> Result<Volume, JmaError> {
    decode_first_station(bytes).map_err(jma_error)
}

fn decode_first_station(bytes: &[u8]) -> Result<Volume, String> {
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
/// station id appends its sweeps (scan order preserved, ray times rebased
/// onto the earlier reference) instead of producing a duplicate site entry.
fn merge_station_volume(volumes: &mut Vec<Volume>, mut volume: Volume) -> Result<(), String> {
    if let Some(existing) = volumes
        .iter_mut()
        .find(|existing| existing.attrs.instrument_name == volume.attrs.instrument_name)
    {
        if volume.time_reference < existing.time_reference {
            let shift = (existing.time_reference - volume.time_reference).num_seconds() as f64;
            for sweep in &mut existing.sweeps {
                sweep.rays.time_s.iter_mut().for_each(|time| *time += shift);
            }
            existing.time_reference = volume.time_reference;
        }
        let shift = (volume.time_reference - existing.time_reference).num_seconds() as f64;
        for sweep in &mut volume.sweeps {
            sweep.rays.time_s.iter_mut().for_each(|time| *time += shift);
        }
        existing
            .sweeps
            .try_reserve(volume.sweeps.len())
            .map_err(|err| format!("cannot grow JMA station sweep table: {err}"))?;
        existing.sweeps.append(&mut volume.sweeps);
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
    name: FieldName,
    station: JmaStationHeader,
    elevation_deg: Option<f32>,
    ray_elevation_deg: Vec<Option<f32>>,
}

fn decode_jma_grib2_volume(bytes: &[u8], member: &str) -> Result<Volume, String> {
    let msg = grib2_message(bytes, member)?;
    let sections = scan_sections(msg, member)?;

    let identification = sections
        .iter()
        .find(|section| section.number == 1)
        .ok_or_else(|| format!("{member}: GRIB2 message has no identification section"))?;
    let volume_time = parse_reference_time(section_bytes(msg, *identification), member)?;

    let mut volume = Volume::new("", volume_time);
    volume.provenance.source_format = SourceFormat::JmaGrib2;
    volume.provenance.source_version = Some("JMA GRIB2".to_owned());
    volume.provenance.compression = Some("jma-grib2-tar".to_owned());

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
                push_sweep(&mut volume, &grid, &product, values, volume_time)?;
            }
            8 => break,
            other => return Err(format!("{member}: unexpected GRIB2 section {other}")),
        }
    }

    let station =
        station.ok_or_else(|| format!("{member}: GRIB2 message contains no radar sweeps"))?;
    volume.attrs.instrument_name = station.id.clone();
    volume.attrs.site_name = Some(format!("RS{}", station.number));
    volume.attrs.wmo.id = Some(station.number.to_string());
    volume.location.latitude_deg = Some(station.latitude_deg);
    volume.location.longitude_deg = Some(station.longitude_deg);
    volume.location.altitude_m = station.elevation_m.map(f64::from);
    volume.provenance.decode.message_count = volume.sweeps.len();
    volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
    Ok(volume)
}

fn push_sweep(
    volume: &mut Volume,
    grid: &PolarGrid,
    product: &SweepProduct,
    values: Vec<f32>,
    reference_time: DateTime<Utc>,
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
    let ngates =
        u32::try_from(grid.gate_count).map_err(|_| "JMA gate count overflow".to_owned())?;

    // Repeated elevations with different gate layouts are real here (the
    // 10-minute file carries two 5-minute scan repetitions), so every sweep
    // becomes its own sweep in scan order — never elevation-merged.
    let mut sweep = Sweep::new(
        volume.sweeps.len() as u32,
        SweepMode::AzimuthSurveillance,
        elevation_deg,
    );
    sweep.follow_mode = Some(FollowMode::None);
    sweep.range = RangeCoord::Uniform {
        first_center_m: f64::from(grid.range_start_m),
        spacing_m: if grid.gate_spacing_m > 0.0 && grid.gate_spacing_m.is_finite() {
            f64::from(grid.gate_spacing_m)
        } else {
            1.0
        },
        ngates,
    };
    sweep
        .rays
        .azimuth_deg
        .try_reserve_exact(grid.radial_count)
        .map_err(|err| format!("cannot reserve JMA ray table: {err}"))?;
    sweep.reserve_rays(grid.radial_count);
    let time_s = (reference_time - volume.time_reference).num_seconds() as f64;
    for ray in 0..grid.radial_count {
        sweep.push_ray(
            time_s,
            grid.ray_azimuth_deg(ray),
            product
                .ray_elevation_deg
                .get(ray)
                .copied()
                .flatten()
                .unwrap_or(elevation_deg),
        );
    }

    // `values` is already ray-major. Move it into the field instead of
    // cloning every row into a second full f32 plane.
    let field = Field::new(
        product.name.clone(),
        GateMapping::IDENTITY,
        ngates,
        FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
    );
    sweep
        .add_field(field)
        .map_err(|err| format!("JMA sweep field: {err}"))?;
    volume
        .sweeps
        .try_reserve(1)
        .map_err(|err| format!("cannot grow JMA sweep table: {err}"))?;
    volume.sweeps.push(sweep);
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
        name: field_name_for_parameter(section[9], section[10]),
        station,
        elevation_deg,
        ray_elevation_deg,
    })
}

/// WMO Code table 4.2, discipline 0 (meteorological), category 15 (radar):
/// 1 = reflectivity (dBZ) -> `DBZH`, 2 = radial velocity (m/s) -> `VRADH`.
/// Anything else is preserved under a `JMA_<category>_<number>` name instead
/// of being dropped.
fn field_name_for_parameter(category: u8, number: u8) -> FieldName {
    match (category, number) {
        (15, 1) => FieldName::Dbzh,
        (15, 2) => FieldName::Vradh,
        (category, number) => FieldName::parse(&format!("JMA_{category}_{number}")),
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

    // -- synthetic JMA GRIB2 + ustar builders ------------------------------

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    /// Section skeleton: 4-byte length placeholder + section number, body
    /// appended by `fill`, length fixed afterwards.
    fn section(number: u8, fill: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0, number];
        fill(&mut body);
        let len = body.len() as u32;
        body[0..4].copy_from_slice(&len.to_be_bytes());
        body
    }

    /// One-sweep JMA GRIB2 message: 2 radials x 3 gates, run-length levels
    /// `[1, 2, 3, 1, 2, 0]` against the level table `[10.5, 20.5, 30.5]`
    /// (decimal scale 1), category/number per `parameter`, 0.50 deg tilt.
    fn synthetic_jma_grib2(station_id: &[u8; 4], station_number: u16, parameter: u8) -> Vec<u8> {
        synthetic_jma_grib2_at_elevation(station_id, station_number, parameter, 50)
    }

    /// [`synthetic_jma_grib2`] with the product elevation in centidegrees
    /// (the per-ray table rides 0.05 deg above it, so "per-ray wins"
    /// stays observable at any tilt).
    fn synthetic_jma_grib2_at_elevation(
        station_id: &[u8; 4],
        station_number: u16,
        parameter: u8,
        elevation_centideg: u16,
    ) -> Vec<u8> {
        let radials = 2usize;
        let gates = 3usize;

        let sec1 = section(1, |body| {
            push_u16(body, 34); // centre: Tokyo
            push_u16(body, 0); // subcentre
            body.extend_from_slice(&[2, 1, 0]); // tables, local tables, time sig
            push_u16(body, 2026); // year
            body.extend_from_slice(&[6, 12, 6, 40, 0]); // mo, dy, hr, mi, se
            body.extend_from_slice(&[0, 0]); // production status, data type
        });
        let sec3 = section(3, |body| {
            body.push(0); // source of grid definition
            push_u32(body, (radials * gates) as u32); // total points
            body.extend_from_slice(&[0, 0]); // optional list octets
            push_u16(body, GRID_TEMPLATE_AZIMUTH_RANGE);
            push_u32(body, gates as u32);
            push_u32(body, radials as u32);
            push_u32(body, 36_500_000); // grid centre lat (micro-deg)
            push_u32(body, 136_500_000); // grid centre lon
            push_u32(body, 500_000); // gate spacing (mm) -> 500 m
            push_u32(body, 0); // range start
            body.push(0); // scan mode: clockwise
            push_u16(body, 4_500); // start azimuth 45.00 deg
        });
        let sec4 = section(4, |body| {
            push_u16(body, 0); // coordinate values
            push_u16(body, PRODUCT_TEMPLATE_RADAR_ELEVATION);
            body.push(15); // parameter category: radar
            body.push(parameter); // 1 = REF, 2 = VEL
            body.extend_from_slice(&[0, 0, 0]); // pad to octet 15
            push_u32(body, 36_512_345); // site lat 36.512345
            push_u32(body, 136_987_654); // site lon 136.987654
            push_u16(body, 1234); // altitude 123.4 m
            body.extend_from_slice(station_id);
            push_u16(body, station_number);
            push_u16(body, 0); // magnetic declination
            push_u32(body, 5_370_000); // tx frequency kHz
            body.extend_from_slice(&[0, 0]); // polarization, operation mode
            body.push(0); // pad to octet 40
            body.extend_from_slice(&[0, 0]); // qc, clutter filter
            push_u16(body, elevation_centideg); // product elevation
            body.push(0); // pad to octet 44
            push_u16(body, 1_000); // representative PRF 1
            push_u16(body, 1_000); // representative PRF 2
            push_u16(body, 1_000); // representative PRF 3
            push_u16(body, 0); // obs start offset
            push_u16(body, 0); // obs end offset
            body.extend_from_slice(&[0; 6]); // pad to octet 60
            for _ in 0..radials {
                push_u16(body, elevation_centideg + 5); // per-ray elevation
                push_u16(body, 1_000); // per-ray PRF
            }
        });
        let sec5 = section(5, |body| {
            push_u32(body, (radials * gates) as u32);
            push_u16(body, DATA_TEMPLATE_RUN_LENGTH);
            body.push(8); // bits per packed value
            push_u16(body, 250); // max level value used (V)
            push_u16(body, 3); // level table size (M)
            body.push(1); // decimal scale factor
            push_u16(body, 105); // level 1 -> 10.5
            push_u16(body, 205); // level 2 -> 20.5
            push_u16(body, 305); // level 3 -> 30.5
        });
        let sec6 = section(6, |body| body.push(255));
        let sec7 = section(7, |body| {
            body.extend_from_slice(&[1, 2, 3, 1, 2, 0]); // literal levels
        });

        let mut msg = Vec::new();
        msg.extend_from_slice(GRIB_MAGIC);
        msg.extend_from_slice(&[0, 0]); // reserved
        msg.push(0); // discipline
        msg.push(2); // edition
        msg.extend_from_slice(&[0; 8]); // total length placeholder
        for sec in [sec1, sec3, sec4, sec5, sec6, sec7] {
            msg.extend_from_slice(&sec);
        }
        msg.extend_from_slice(GRIB_END_MAGIC);
        let total = msg.len() as u64;
        msg[8..16].copy_from_slice(&total.to_be_bytes());
        msg
    }

    fn tar_member_blocks(name: &str, data: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; TAR_BLOCK_LEN];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644"); // mode
        header[108..115].copy_from_slice(b"0000000"); // uid
        header[116..123].copy_from_slice(b"0000000"); // gid
        let size = format!("{:011o}", data.len());
        header[TAR_SIZE_OFFSET..TAR_SIZE_OFFSET + 11].copy_from_slice(size.as_bytes());
        header[136..147].copy_from_slice(b"00000000000"); // mtime
        header[TAR_TYPEFLAG_OFFSET] = b'0';
        header[TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 6].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00"); // version
        // Checksum: header bytes summed with the checksum field as spaces.
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut out = header;
        out.extend_from_slice(data);
        let padding = data.len().div_ceil(TAR_BLOCK_LEN) * TAR_BLOCK_LEN - data.len();
        out.extend(std::iter::repeat_n(0u8, padding));
        out
    }

    fn tar_archive(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in members {
            out.extend_from_slice(&tar_member_blocks(name, data));
        }
        out.extend(std::iter::repeat_n(0u8, TAR_BLOCK_LEN * 2));
        out
    }

    fn member_name(station_number: u16, product: &str) -> String {
        format!(
            "Z__C_RJTD_20260612064000_RDR_JMAGPV_RS{station_number}_Gar0p5km0p7deg_P{product}_ANAL_grib2.bin"
        )
    }

    fn two_station_tar() -> Vec<u8> {
        let alfa = synthetic_jma_grib2(b"ALFA", 47001, 1);
        let brvo = synthetic_jma_grib2(b"BRVO", 47002, 2);
        tar_archive(&[
            (&member_name(47001, "ze"), &alfa),
            (&member_name(47002, "vr"), &brvo),
        ])
    }

    // -- tests ---------------------------------------------------------------

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
        let section = section(3, |body| {
            body.push(0);
            push_u32(body, (MAX_GRID_RADIALS + 1) as u32);
            body.extend_from_slice(&[0, 0]);
            push_u16(body, GRID_TEMPLATE_AZIMUTH_RANGE);
            push_u32(body, 1);
            push_u32(body, (MAX_GRID_RADIALS + 1) as u32);
            push_u32(body, 36_500_000);
            push_u32(body, 136_500_000);
            push_u32(body, 500_000);
            push_u32(body, 0);
            body.push(0);
            push_u16(body, 0);
        });
        let err = parse_grid(&section, "pathological-grid")
            .err()
            .expect("pathological grid must be rejected");
        assert!(
            err.contains("grid dimensions") && err.contains("exceed limits"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn oversized_tar_member_is_rejected_from_its_header() {
        let mut header = vec![0u8; TAR_BLOCK_LEN];
        let name = member_name(47001, "ze");
        header[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}", MAX_JMA_MEMBER_BYTES + 1);
        header[TAR_SIZE_OFFSET..TAR_SIZE_OFFSET + 11].copy_from_slice(size.as_bytes());
        header[TAR_TYPEFLAG_OFFSET] = b'0';
        let err = ustar_members(&header)
            .err()
            .expect("oversized member error");
        assert!(
            err.contains("declares") && err.contains("limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sniffs_jma_tar_bytes() {
        let tar = two_station_tar();
        assert!(looks_like_jma_tar_bytes(&tar));
        // Too short for one header block.
        assert!(!looks_like_jma_tar_bytes(&tar[..TAR_BLOCK_LEN - 1]));
        // ustar magic with a non-JMA member name.
        let other = tar_archive(&[("plain.txt", b"hello".as_slice())]);
        assert!(!looks_like_jma_tar_bytes(&other));
        // Non-tar bytes.
        assert!(!looks_like_jma_tar_bytes(&[0u8; 1024]));
    }

    /// Field report: tilt #00 on JMA radars showed the ~25° cone — the
    /// GRIB members carry sweeps high-tilt-first, and a station split
    /// across tar members restarted its sweep numbering. The ladder must
    /// come back lowest beam first with sequential numbering.
    #[test]
    fn cuts_sort_lowest_elevation_first_across_members() {
        let high = synthetic_jma_grib2_at_elevation(b"ALFA", 47001, 1, 2500); // 25.0 deg
        let low = synthetic_jma_grib2_at_elevation(b"ALFA", 47001, 1, 50); // 0.5 deg
        let tar = tar_archive(&[
            (&member_name(47001, "zeh"), &high),
            (&member_name(47001, "zel"), &low),
        ]);
        let volumes = read_jma_tar_volumes(&tar, None).expect("decode");
        assert_eq!(volumes.len(), 1, "same station must merge");
        let sweeps = &volumes[0].sweeps;
        assert_eq!(sweeps.len(), 2);
        assert_eq!(sweeps[0].fixed_angle_deg, 0.5);
        assert_eq!(sweeps[1].fixed_angle_deg, 25.0);
        assert_eq!(sweeps[0].sweep_number, 0);
        assert_eq!(sweeps[1].sweep_number, 1);
        assert_eq!(sweeps[0].elevation_number, Some(1));
        assert_eq!(sweeps[1].elevation_number, Some(2));
    }

    #[test]
    fn decodes_every_station_in_archive_order() {
        let tar = two_station_tar();
        let volumes = read_jma_tar_volumes(&tar, None).expect("two-station decode");
        assert_eq!(volumes.len(), 2, "every station must come back");
        assert_eq!(volumes[0].attrs.instrument_name, "ALFA");
        assert_eq!(volumes[1].attrs.instrument_name, "BRVO");
        assert_eq!(
            volumes[0].time_reference.to_rfc3339(),
            "2026-06-12T06:40:00+00:00"
        );

        let alfa = &volumes[0];
        assert_eq!(alfa.attrs.site_name.as_deref(), Some("RS47001"));
        assert_eq!(alfa.attrs.wmo.id.as_deref(), Some("47001"));
        assert_eq!(alfa.provenance.source_format, SourceFormat::JmaGrib2);
        let lat = alfa.location.latitude_deg.expect("site latitude");
        let lon = alfa.location.longitude_deg.expect("site longitude");
        assert!((lat - 36.512345).abs() < 1e-9, "lat was {lat}");
        assert!((lon - 136.987654).abs() < 1e-9, "lon was {lon}");
        assert_eq!(alfa.location.altitude_m, Some(f64::from(123.4f32)));
        assert_eq!(alfa.sweeps.len(), 1);

        let sweep = &alfa.sweeps[0];
        assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
        assert_eq!(sweep.fixed_angle_deg, 0.5);
        assert_eq!(sweep.elevation_number, Some(1));
        assert_eq!(sweep.nrays(), 2);
        assert_eq!(sweep.rays.azimuth_deg, vec![45.0, 225.0]);
        assert_eq!(sweep.rays.elevation_deg[0], 0.55); // per-ray table wins
        assert_eq!(sweep.rays.time_s, vec![0.0, 0.0]);
        assert_eq!(sweep.range.spacing_m(), Some(500.0));
        assert_eq!(sweep.range.ngates(), 3);
        assert!(sweep.ray_vars.nyquist_velocity_mps.is_none());

        let field = sweep.field(&FieldName::Dbzh).expect("DBZH field");
        assert!(matches!(field.data, FieldData::F32 { .. }));
        assert_eq!(field.value(0, 0), Some(10.5));
        assert_eq!(field.value(0, 1), Some(20.5));
        assert_eq!(field.value(0, 2), Some(30.5));
        assert_eq!(field.value(1, 1), Some(20.5));
        assert_eq!(field.value(1, 2), None); // level 0 is missing (NaN)

        // The velocity member mapped to VRADH.
        assert!(volumes[1].sweeps[0].field(&FieldName::Vradh).is_some());
    }

    #[test]
    fn site_filter_selects_one_station_by_id_or_number() {
        let tar = two_station_tar();
        for filter in ["BRVO", "brvo", "RS47002", "47002"] {
            let volumes = read_jma_tar_volumes(&tar, Some(filter)).expect("filtered decode");
            assert_eq!(volumes.len(), 1, "filter '{filter}'");
            assert_eq!(
                volumes[0].attrs.instrument_name, "BRVO",
                "filter '{filter}'"
            );
        }
        let err = read_jma_tar_volumes(&tar, Some("NOPE")).unwrap_err();
        assert!(err.to_string().contains("NOPE"), "unexpected error: {err}");
    }

    #[test]
    fn first_station_decode_takes_the_first_member_only() {
        let tar = two_station_tar();
        let volume = read_jma_tar_first_station(&tar).expect("first-station decode");
        assert_eq!(volume.attrs.instrument_name, "ALFA");
    }

    #[test]
    fn station_headers_skip_gate_data_and_dedupe() {
        let alfa = synthetic_jma_grib2(b"ALFA", 47001, 1);
        let tar = tar_archive(&[
            (&member_name(47001, "ze"), alfa.as_slice()),
            (&member_name(47001, "ze"), alfa.as_slice()), // duplicate station
        ]);
        let stations = jma_tar_station_headers(&tar).expect("station headers");
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].id, "ALFA");
        assert_eq!(stations[0].number, 47001);
        assert!((stations[0].latitude_deg - 36.512345).abs() < 1e-9);
        assert!((stations[0].longitude_deg - 136.987654).abs() < 1e-9);
    }

    #[test]
    fn repeated_station_members_merge_into_one_volume() {
        let alfa = synthetic_jma_grib2(b"ALFA", 47001, 1);
        let tar = tar_archive(&[
            (&member_name(47001, "ze"), alfa.as_slice()),
            (&member_name(47001, "ze"), alfa.as_slice()),
        ]);
        let volumes = read_jma_tar_volumes(&tar, None).expect("merged decode");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].sweeps.len(), 2, "sweeps append in arrival order");
        assert_eq!(volumes[0].provenance.decode.decoded_ray_count, 4);
    }

    #[test]
    fn corrupt_member_is_skipped_but_alone_is_an_error() {
        let alfa = synthetic_jma_grib2(b"ALFA", 47001, 1);
        let garbage = vec![0xAAu8; 64];
        let mixed = tar_archive(&[
            (&member_name(47999, "ze"), garbage.as_slice()),
            (&member_name(47001, "ze"), alfa.as_slice()),
        ]);
        let volumes = read_jma_tar_volumes(&mixed, None).expect("good member survives");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].attrs.instrument_name, "ALFA");

        let only_garbage = tar_archive(&[(&member_name(47999, "ze"), garbage.as_slice())]);
        let err = read_jma_tar_volumes(&only_garbage, None).unwrap_err();
        assert!(err.to_string().contains("GRIB"), "unexpected error: {err}");

        let no_members = tar_archive(&[("notes.txt", b"hi".as_slice())]);
        let err = read_jma_tar_volumes(&no_members, None).unwrap_err();
        assert!(
            err.to_string().contains("no Z__C_RJTD"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn truncated_tar_member_is_an_error_not_a_panic() {
        let tar = two_station_tar();
        let err = read_jma_tar_volumes(&tar[..TAR_BLOCK_LEN + 17], None).unwrap_err();
        assert!(
            err.to_string().contains("overruns"),
            "unexpected error: {err}"
        );
    }
}
