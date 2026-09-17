//! Native DORADE sweepfile (`swp.*`) decoder for mobile research radars
//! (DOW6/DOW7/DOW8, COW, RaXPol, and other CSWR/OU sweepfile producers).
//!
//! Decodes directly into [`recast_radar_core::RadarVolume`] with no intermediate
//! volume model: each sweepfile contributes one [`recast_radar_core::ElevationCut`]
//! whose moments live in compact [`recast_radar_core::MomentGrid`] storage (16-bit
//! DORADE integers stay 16-bit, shifted into unsigned space so the grid's
//! `(raw - offset) / scale` matches DORADE's `(raw - bias) / scale`).
//!
//! Format references:
//! - R. Oye and M. Case, "DORADE Data Format" (NCAR/ATD, 1995; revised
//!   2003/2010 by W.-C. Lee, NCAR/EOL) — block layouts and semantics.
//! - lrose-core `DoradeData.hh` (NCAR/EOL) — authoritative struct offsets.
//! - HRD run-length encoding from the NOAA Hurricane Research Division as
//!   described in the DORADE document and implemented in `soloii`/Radx.
//!
//! This is a lift-and-improve of the reference implementation in
//! `gurt-rs/src/dorade.rs` (rustwx work tree, 2026-05-31). Divergences:
//! - **CSFD support**: COW2 and RaXPol sweepfiles carry gate geometry in the
//!   `CSFD` (cell spacing format descriptor) block, not `CELV`; the reference
//!   only parsed `CELV` and silently fell back to extended-PARM fields, which
//!   the 1995-format 104-byte PARM (DOW7) does not have.
//! - **VOLD date offsets fixed**: the reference read the volume date at
//!   offset 32; the standard layout puts `year` at 36 (verified against real
//!   COW2 bytes). SSWB remains the primary time source.
//! - **Transition-ray filtering**: rays with RYIB `ray_status != 0` (antenna
//!   moving between fixed angles) are dropped. A real DOW7 Goshen sweepfile
//!   is 42% transition rays spanning 0.5°-11.4° inside a "0.5°" sweep; the
//!   reference kept them, smearing the PPI. Sweeps whose rays are *all*
//!   flagged in-transition (a writer quirk in the same corpus) keep their
//!   rays instead of erroring.
//! - **RADD layout**: the standard 1995 layout (lat/lon/alt at 80/84/88,
//!   `data_compress` at 68) is parsed directly; the reference parsed a
//!   shifted legacy layout first and patched it afterwards.
//! - **CFAC corrections**: azimuth/elevation/range-delay/lat/lon correction
//!   factors are applied when present (all-zero in the observed corpus, but
//!   cheap and correct; Radx applies them unconditionally too).
//! - **Per-ray times**: RYIB julian day + h/m/s/ms become
//!   `Radial::time_offset_ms`; the reference dropped ray times.
//! - **Binary formats**: 8-bit int, 16-bit int, 32-bit int, and 32-bit float
//!   PARM data are supported; the reference assumed 16-bit everywhere.
//! - **Staggered-PRT Nyquist**: the extended unambiguous velocity falls back
//!   to `λ / (4·(T2 − T1))` (Zrnić and Mahapatra 1985, IEEE Trans. AES-21;
//!   Torres, Dubel, and Zrnić 2004, J. Atmos. Oceanic Technol. 21,
//!   1389–1399) when RADD `eff_unamb_vel` is missing; the reference used
//!   `m·Va_short`, which is only correct for `n − m = 1` stagger ratios.
//!
//! Known limitations (documented, not silent):
//! - Multi-segment CSFD range geometry is flattened to the first segment's
//!   spacing because [`recast_radar_core::GateRange`] models uniform gates only.
//! - Per-ray platform georeferencing (`ASIB`) is ignored: DOW/COW/RaXPol
//!   deployments are parked, so the RADD site position applies to the whole
//!   sweep. Airborne tail radars would need ASIB handling.
//! - RHI sweeps decode as cuts ordered by their fixed angle (the AZIMUTH for
//!   an RHI — `ElevationCut::elevation_deg` holds it); the RADD `scan_mode`
//!   is surfaced as [`recast_radar_core::ScanMode`] in the volume metadata so
//!   displays can render a range-height panel instead of a plan view.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use recast_radar_core::bounded_read::{
    DecodeBudget, check_gate_count, check_sweep_count, volume_moment_capacity_bytes,
};
use recast_radar_core::{
    GateRange, MomentGrid, MomentRow, MomentType, RadarSite, RadarVolume, Radial, ScanMode,
    canonical_moment,
};

use crate::{DoradeError, Result};

const BLOCK_HEADER_LEN: usize = 8;
const DORADE_BAD_F32: f32 = -9999.0;
/// DORADE altitude fields are kilometres MSL.
const KM_TO_M: f64 = 1000.0;
/// Aggregate decoded cells retained while assembling one sweep. The cap is
/// independent of input compression and bounds the combined moment rows.
const MAX_DORADE_CELLS_PER_SWEEP: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn i16(self, bytes: &[u8], offset: usize) -> i16 {
        let raw = [bytes[offset], bytes[offset + 1]];
        match self {
            Self::Little => i16::from_le_bytes(raw),
            Self::Big => i16::from_be_bytes(raw),
        }
    }

    fn i32(self, bytes: &[u8], offset: usize) -> i32 {
        let raw = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        match self {
            Self::Little => i32::from_le_bytes(raw),
            Self::Big => i32::from_be_bytes(raw),
        }
    }

    fn f32(self, bytes: &[u8], offset: usize) -> f32 {
        f32::from_bits(self.i32(bytes, offset) as u32)
    }
}

/// Cheap header peek used to group sweepfiles into volume scans without a
/// full decode. Parsing stops at the first ray.
#[derive(Clone, Debug, PartialEq)]
pub struct DoradeSweepHeader {
    pub instrument: String,
    pub volume_number: i32,
    pub sweep_number: i32,
    pub fixed_angle_deg: f32,
    pub start_time: Option<DateTime<Utc>>,
    pub latitude_deg: f32,
    pub longitude_deg: f32,
    pub altitude_m: f32,
}

/// `true` when the buffer starts with a plausible DORADE descriptor block.
///
/// Sweepfiles written by solo/Radx begin with `COMM`, `SSWB`, or `VOLD`;
/// the 4-byte length that follows must be valid in at least one byte order.
pub fn looks_like_dorade_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < BLOCK_HEADER_LEN {
        return false;
    }
    if !matches!(&bytes[..4], b"COMM" | b"SSWB" | b"VOLD" | b"RADD") {
        return false;
    }
    detect_endian(bytes).is_ok()
}

/// `true` when the file name uses the `swp.*` sweepfile convention.
pub fn looks_like_dorade_name(name: &str) -> bool {
    let file_name = name
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    file_name.starts_with("swp.") || file_name.ends_with(".swp") || file_name.ends_with(".dorade")
}

/// Convenience: path-based variant of [`looks_like_dorade_name`].
pub fn looks_like_dorade_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(looks_like_dorade_name)
}

/// Parse only the descriptor blocks (everything before the first ray).
pub fn peek_dorade_sweep(bytes: &[u8]) -> Result<DoradeSweepHeader> {
    let mut parse = SweepParse::new(detect_endian(bytes)?);
    parse.run(bytes, true)?;
    Ok(DoradeSweepHeader {
        instrument: parse.instrument.clone(),
        volume_number: parse.volume_number,
        sweep_number: parse.sweep_number,
        fixed_angle_deg: parse.fixed_angle_deg,
        start_time: parse.start_time,
        latitude_deg: parse.site_latitude_deg(),
        longitude_deg: parse.site_longitude_deg(),
        altitude_m: parse.site_altitude_m(),
    })
}

/// Decode one sweepfile into a fresh single-cut volume.
pub fn decode_dorade_sweep_volume(bytes: &[u8]) -> Result<RadarVolume> {
    let mut volume = RadarVolume::default();
    append_dorade_sweep(bytes, &mut volume)?;
    finalize_dorade_volume(&mut volume);
    Ok(volume)
}

/// Decode a set of sweepfiles forming one volume scan.
///
/// Cuts are appended in input order and then sorted by elevation (ties keep
/// input order, which the callers arrange to be scan time). The site
/// position comes from the first sweep's RADD block — mobile radars move
/// between deployments, so the coordinates always come from the file.
pub fn decode_dorade_volume_from_slices<S: AsRef<[u8]>>(sweeps: &[S]) -> Result<RadarVolume> {
    if sweeps.is_empty() {
        return Err(invalid(0, "no DORADE sweeps to decode"));
    }
    let mut volume = RadarVolume::default();
    for sweep in sweeps {
        append_dorade_sweep(sweep.as_ref(), &mut volume)?;
    }
    finalize_dorade_volume(&mut volume);
    Ok(volume)
}

/// Decode a set of sweepfile paths forming one volume scan.
pub fn decode_dorade_volume_from_paths<P: AsRef<Path>>(paths: &[P]) -> Result<RadarVolume> {
    if paths.is_empty() {
        return Err(invalid(0, "no DORADE sweep paths to decode"));
    }
    let mut volume = RadarVolume::default();
    for path in paths {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| DoradeError::Io {
            path: path.display().to_string(),
            source,
        })?;
        append_dorade_sweep(&bytes, &mut volume)?;
    }
    volume.metadata.source_path = Some(paths[0].as_ref().display().to_string());
    finalize_dorade_volume(&mut volume);
    Ok(volume)
}

/// Decode one sweepfile and append it as a cut on `volume`.
///
/// The first appended sweep populates the site, volume time, and metadata;
/// later sweeps must come from the same instrument.
pub fn append_dorade_sweep(bytes: &[u8], volume: &mut RadarVolume) -> Result<()> {
    let mut parse = SweepParse::new(detect_endian(bytes)?);
    parse.run(bytes, false)?;
    check_sweep_count(volume.cuts.len() + 1, "DORADE volume")
        .map_err(DoradeError::LimitExceeded)?;
    // The volume budget covers every sweep appended so far.
    let mut budget = DecodeBudget::volume();
    let existing_radials: usize = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
    budget
        .charge(
            volume_moment_capacity_bytes(volume),
            1,
            "DORADE volume grids",
        )
        .and_then(|()| {
            budget.charge(
                existing_radials,
                size_of::<Radial>(),
                "DORADE volume radials",
            )
        })
        .map_err(DoradeError::LimitExceeded)?;
    parse.finish_into(volume, &mut budget)
}

/// Sort cuts by elevation and refresh volume-level bookkeeping. Called once
/// after the last [`append_dorade_sweep`].
pub fn finalize_dorade_volume(volume: &mut RadarVolume) {
    // Stable sort: same-elevation cuts (single-tilt COW2 sequences) keep
    // their scan-time order.
    volume
        .cuts
        .sort_by(|left, right| left.elevation_deg.total_cmp(&right.elevation_deg));
    volume.metadata.decoded_radial_count = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
}

fn detect_endian(bytes: &[u8]) -> Result<Endian> {
    if bytes.len() < BLOCK_HEADER_LEN {
        return Err(DoradeError::Truncated {
            what: "DORADE block header",
            offset: 0,
            needed: BLOCK_HEADER_LEN,
            available: bytes.len(),
        });
    }
    let le = Endian::Little.i32(bytes, 4);
    let be = Endian::Big.i32(bytes, 4);
    let len = bytes.len() as i64;
    let le_ok = le as i64 >= BLOCK_HEADER_LEN as i64 && le as i64 <= len;
    let be_ok = be as i64 >= BLOCK_HEADER_LEN as i64 && be as i64 <= len;
    match (le_ok, be_ok) {
        (true, false) => Ok(Endian::Little),
        (false, true) => Ok(Endian::Big),
        // Network byte order is the DORADE default; prefer it on a tie.
        (true, true) => Ok(Endian::Big),
        (false, false) => Err(invalid(4, "cannot determine DORADE byte order")),
    }
}

/// One PARM descriptor plus the moment grid it feeds.
struct ParamState {
    name: String,
    scale: f32,
    bias: f32,
    bad_data: i32,
    /// DORADE `binary_format`: 1 = i8, 2 = i16, 3 = i32, 4 = f32.
    binary_format: i16,
    /// Extended (1997+) PARM gate metadata; the 104-byte 1995 PARM lacks it.
    number_cells: Option<usize>,
    first_cell_m: Option<f32>,
    cell_spacing_m: Option<f32>,
    moment: MomentType,
    grid: Option<MomentGrid>,
    /// Decoded row for the in-flight ray, if any.
    pending_row: Option<MomentRow>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cfac {
    azimuth_deg: f32,
    elevation_deg: f32,
    range_delay_m: f32,
    longitude_deg: f32,
    latitude_deg: f32,
    radar_altitude_km: f32,
}

#[derive(Clone, Copy, Debug)]
struct PendingRay {
    azimuth_deg: f32,
    elevation_deg: f32,
    /// RYIB `ray_status`: 0 = normal, 1 = in transition, 2 = bad.
    status: i32,
    time: Option<DateTime<Utc>>,
}

struct SweepParse {
    endian: Endian,
    instrument: String,
    volume_number: i32,
    sweep_number: i32,
    fixed_angle_deg: f32,
    scan_mode: i16,
    compression: i16,
    radd_longitude_deg: f32,
    radd_latitude_deg: f32,
    radd_altitude_km: f32,
    eff_unamb_vel_mps: Option<f32>,
    frequency_ghz: Option<f32>,
    prt1_ms: Option<f32>,
    prt2_ms: Option<f32>,
    num_ipps_trans: Option<i16>,
    cfac: Cfac,
    start_time: Option<DateTime<Utc>>,
    vold_date: Option<NaiveDate>,
    params: Vec<ParamState>,
    /// CELV per-cell ranges or CSFD-derived uniform axis.
    range_first_m: Option<f32>,
    range_spacing_m: Option<f32>,
    range_gate_count: Option<usize>,
    rays: Vec<(PendingRay, Vec<(usize, MomentRow)>)>,
    /// Antenna-transition rays, kept aside so an all-transition sweep (seen
    /// in the 2009 Goshen DOW7 corpus) can still decode instead of erroring.
    transition_rays: Vec<(PendingRay, Vec<(usize, MomentRow)>)>,
    current_ray: Option<PendingRay>,
    skipped_field_blocks: usize,
    decoded_cells: usize,
}

impl SweepParse {
    fn new(endian: Endian) -> Self {
        Self {
            endian,
            instrument: String::new(),
            volume_number: 0,
            sweep_number: 0,
            fixed_angle_deg: f32::NAN,
            scan_mode: 8,
            compression: 0,
            radd_longitude_deg: f32::NAN,
            radd_latitude_deg: f32::NAN,
            radd_altitude_km: f32::NAN,
            eff_unamb_vel_mps: None,
            frequency_ghz: None,
            prt1_ms: None,
            prt2_ms: None,
            num_ipps_trans: None,
            cfac: Cfac::default(),
            start_time: None,
            vold_date: None,
            params: Vec::new(),
            range_first_m: None,
            range_spacing_m: None,
            range_gate_count: None,
            rays: Vec::new(),
            transition_rays: Vec::new(),
            current_ray: None,
            skipped_field_blocks: 0,
            decoded_cells: 0,
        }
    }

    fn site_latitude_deg(&self) -> f32 {
        self.radd_latitude_deg + self.cfac.latitude_deg
    }

    fn site_longitude_deg(&self) -> f32 {
        self.radd_longitude_deg + self.cfac.longitude_deg
    }

    fn site_altitude_m(&self) -> f32 {
        ((self.radd_altitude_km + self.cfac.radar_altitude_km) as f64 * KM_TO_M) as f32
    }

    fn run(&mut self, bytes: &[u8], stop_at_first_ray: bool) -> Result<()> {
        let mut pos = 0usize;
        while let Some(&id) = bytes
            .get(pos..)
            .filter(|rest| rest.len() >= BLOCK_HEADER_LEN)
            .and_then(|rest| rest.first_chunk::<4>())
        {
            let nbytes = self.endian.i32(bytes, pos + 4);
            if nbytes < BLOCK_HEADER_LEN as i32 {
                // NULL terminator blocks or padding: stop cleanly at a
                // recognizable end marker, error otherwise.
                if &id == b"NULL" || id == [0; 4] {
                    break;
                }
                return Err(invalid(pos, format!("invalid DORADE block size {nbytes}")));
            }
            let end = pos + nbytes as usize;
            if end > bytes.len() {
                // Tolerate a truncated trailing ray (partial downloads); the
                // descriptor blocks must be complete for a usable sweep.
                if !self.rays.is_empty() {
                    break;
                }
                return Err(DoradeError::Truncated {
                    what: "DORADE block",
                    offset: pos,
                    needed: nbytes as usize,
                    available: bytes.len() - pos,
                });
            }
            let block = &bytes[pos..end];
            match &id {
                b"VOLD" => self.parse_vold(block, pos)?,
                b"RADD" => self.parse_radd(block, pos)?,
                b"CFAC" => self.parse_cfac(block),
                b"PARM" => self.parse_parm(block, pos)?,
                b"CELV" => self.parse_celv(block, pos)?,
                b"CSFD" => self.parse_csfd(block, pos)?,
                b"SWIB" => self.parse_swib(block, pos)?,
                b"SSWB" => self.parse_sswb(block, pos)?,
                b"RYIB" => {
                    if stop_at_first_ray {
                        return Ok(());
                    }
                    self.finish_current_ray();
                    self.current_ray = Some(self.parse_ryib(block, pos)?);
                }
                b"RDAT" => self.parse_rdat(block, pos)?,
                // COMM, ASIB, XSTF, RKTB, SEDS, FRIB, FRAD, WAVE, ...: skipped.
                _ => {}
            }
            pos = end;
        }
        self.finish_current_ray();
        Ok(())
    }

    fn parse_vold(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 48, offset, "VOLD")?;
        self.volume_number = i32::from(self.endian.i16(block, 10));
        // Standard layout: proj_name[20] at 16, then year at 36 (the
        // reference read offset 32, which lands inside proj_name).
        let year = i32::from(self.endian.i16(block, 36));
        let month = self.endian.i16(block, 38);
        let day = self.endian.i16(block, 40);
        let hour = self.endian.i16(block, 42);
        let minute = self.endian.i16(block, 44);
        let second = self.endian.i16(block, 46);
        if let Some(date) = NaiveDate::from_ymd_opt(year, month.max(0) as u32, day.max(0) as u32) {
            self.vold_date = Some(date);
            if self.start_time.is_none() {
                self.start_time = date
                    .and_hms_opt(
                        hour.max(0) as u32,
                        minute.max(0) as u32,
                        second.max(0) as u32,
                    )
                    .map(|naive| Utc.from_utc_datetime(&naive));
            }
        }
        Ok(())
    }

    fn parse_radd(&mut self, block: &[u8], offset: usize) -> Result<()> {
        // The standard 1995 RADD is 144 bytes; Radx writes a 300-byte
        // extended version with identical leading offsets.
        require(block, 144, offset, "RADD")?;
        self.instrument = text(&block[8..16]);
        self.scan_mode = self.endian.i16(block, 50);
        self.compression = self.endian.i16(block, 68);
        self.radd_longitude_deg = self.endian.f32(block, 80);
        self.radd_latitude_deg = self.endian.f32(block, 84);
        self.radd_altitude_km = self.endian.f32(block, 88);
        self.eff_unamb_vel_mps = valid_dorade_f32(self.endian.f32(block, 92));
        self.num_ipps_trans = Some(self.endian.i16(block, 102));
        self.frequency_ghz = valid_dorade_f32(self.endian.f32(block, 104))
            .filter(|freq| (0.1..=300.0).contains(freq));
        self.prt1_ms = valid_dorade_f32(self.endian.f32(block, 124));
        self.prt2_ms = valid_dorade_f32(self.endian.f32(block, 128));
        Ok(())
    }

    fn parse_cfac(&mut self, block: &[u8]) {
        // CFAC: nine correction floats starting at offset 8 (azimuth,
        // elevation, range delay, longitude, latitude, pressure alt, radar
        // alt, EW ground speed, NS ground speed, ...).
        if block.len() < 36 {
            return;
        }
        self.cfac = Cfac {
            azimuth_deg: self.endian.f32(block, 8),
            elevation_deg: self.endian.f32(block, 12),
            range_delay_m: self.endian.f32(block, 16),
            longitude_deg: self.endian.f32(block, 20),
            latitude_deg: self.endian.f32(block, 24),
            radar_altitude_km: self.endian.f32(block, 32),
        };
    }

    fn parse_parm(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 104, offset, "PARM")?;
        let name = text(&block[8..16]);
        let binary_format = self.endian.i16(block, 78);
        let scale = self.endian.f32(block, 92);
        let bias = self.endian.f32(block, 96);
        let bad_data = self.endian.i32(block, 100);
        // 1997+ extended PARM (216 bytes) carries per-field gate geometry.
        let (number_cells, first_cell_m, cell_spacing_m) = if block.len() >= 212 {
            let number_cells = self.endian.i32(block, 200).max(0) as usize;
            validate_gate_count(number_cells, offset, "PARM")?;
            (
                Some(number_cells),
                Some(self.endian.f32(block, 204)),
                Some(self.endian.f32(block, 208)),
            )
        } else {
            (None, None, None)
        };
        self.params.push(ParamState {
            name,
            scale: if scale.abs() > 1.0e-6 { scale } else { 1.0 },
            bias,
            bad_data,
            binary_format,
            number_cells,
            first_cell_m,
            cell_spacing_m,
            moment: MomentType::Unknown(String::new()),
            grid: None,
            pending_row: None,
        });
        Ok(())
    }

    fn parse_celv(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 16, offset, "CELV")?;
        let cells = self.endian.i32(block, 8).max(0) as usize;
        let available = (block.len() - 12) / 4;
        let count = cells.min(available);
        if count == 0 {
            return Ok(());
        }
        validate_gate_count(count, offset, "CELV")?;
        let first = self.endian.f32(block, 12);
        let spacing = if count >= 2 {
            // CELV lists every cell range; recast_radar_core models uniform gates,
            // so use the lead spacing (uniform in the observed corpus).
            self.endian.f32(block, 16) - first
        } else {
            0.0
        };
        self.range_first_m = Some(first);
        self.range_spacing_m = Some(spacing);
        self.range_gate_count = Some(count);
        Ok(())
    }

    fn parse_csfd(&mut self, block: &[u8], offset: usize) -> Result<()> {
        // CSFD: num_segments (i32 at 8), dist_to_first (f32 at 12),
        // spacing[8] (f32 at 16), num_cells[8] (i16 at 48). 64 bytes.
        require(block, 64, offset, "CSFD")?;
        let segments = self.endian.i32(block, 8).clamp(0, 8) as usize;
        if segments == 0 {
            return Ok(());
        }
        let first = self.endian.f32(block, 12);
        let spacing = self.endian.f32(block, 16);
        let mut total_cells = 0usize;
        for segment in 0..segments {
            total_cells += self.endian.i16(block, 48 + segment * 2).max(0) as usize;
        }
        if total_cells == 0 {
            return Ok(());
        }
        validate_gate_count(total_cells, offset, "CSFD")?;
        // Multi-segment geometry flattens to the first segment's spacing;
        // see module docs.
        self.range_first_m = Some(first);
        self.range_spacing_m = Some(spacing);
        self.range_gate_count = Some(total_cells);
        Ok(())
    }

    fn parse_swib(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 36, offset, "SWIB")?;
        self.sweep_number = self.endian.i32(block, 16);
        self.fixed_angle_deg = self.endian.f32(block, 32);
        Ok(())
    }

    fn parse_sswb(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 20, offset, "SSWB")?;
        let start = self.endian.i32(block, 12);
        if start > 0 {
            self.start_time = DateTime::<Utc>::from_timestamp(i64::from(start), 0);
        }
        Ok(())
    }

    fn parse_ryib(&mut self, block: &[u8], offset: usize) -> Result<PendingRay> {
        require(block, 44, offset, "RYIB")?;
        let julian_day = self.endian.i32(block, 12);
        let hour = self.endian.i16(block, 16);
        let minute = self.endian.i16(block, 18);
        let second = self.endian.i16(block, 20);
        let millisecond = self.endian.i16(block, 22);
        let time = self.ray_time(julian_day, hour, minute, second, millisecond);
        Ok(PendingRay {
            azimuth_deg: self.endian.f32(block, 24) + self.cfac.azimuth_deg,
            elevation_deg: self.endian.f32(block, 28) + self.cfac.elevation_deg,
            status: self.endian.i32(block, 40),
            time,
        })
    }

    fn ray_time(
        &self,
        julian_day: i32,
        hour: i16,
        minute: i16,
        second: i16,
        millisecond: i16,
    ) -> Option<DateTime<Utc>> {
        let base_year = self
            .start_time
            .map(|time| time.date_naive())
            .or(self.vold_date)?
            .year();
        if !(1..=366).contains(&julian_day) {
            return None;
        }
        let date = NaiveDate::from_yo_opt(base_year, julian_day as u32)?;
        let naive = date.and_hms_milli_opt(
            hour.clamp(0, 23) as u32,
            minute.clamp(0, 59) as u32,
            second.clamp(0, 59) as u32,
            millisecond.clamp(0, 999) as u32,
        )?;
        let mut time = Utc.from_utc_datetime(&naive);
        // Year rollover: a sweep started Dec 31 can have rays on Jan 1.
        if let Some(start) = self.start_time {
            if time < start - Duration::days(180) {
                let next = NaiveDate::from_yo_opt(base_year + 1, julian_day as u32)?;
                time = Utc.from_utc_datetime(&next.and_time(naive.time()));
            } else if time > start + Duration::days(180) {
                let previous = NaiveDate::from_yo_opt(base_year - 1, julian_day as u32)?;
                time = Utc.from_utc_datetime(&previous.and_time(naive.time()));
            }
        }
        Some(time)
    }

    fn parse_rdat(&mut self, block: &[u8], offset: usize) -> Result<()> {
        if self.current_ray.is_none() {
            return Ok(());
        }
        require(block, 16, offset, "RDAT")?;
        let name = text(&block[8..16]);
        let Some(param_index) = self.params.iter().position(|param| param.name == name) else {
            self.skipped_field_blocks += 1;
            return Ok(());
        };
        let payload = &block[16..];
        let endian = self.endian;
        let compressed = self.compression == 1;
        let gate_count = self.gate_count_for_param(param_index);
        if let Some(gates) = gate_count {
            validate_gate_count(gates, offset, "RDAT")?;
        }
        let param = &self.params[param_index];
        let stored_gates = match param.binary_format {
            1 => payload.len(),
            2 => payload.len() / 2,
            _ => payload.len() / 4,
        };
        if !(compressed && param.binary_format == 2) {
            validate_gate_count(stored_gates, offset, "RDAT")?;
        }
        let row = match param.binary_format {
            1 => {
                // i8 → u8 storage; +128 keeps (raw − offset)/scale intact.
                let row = payload
                    .iter()
                    .map(|byte| (*byte as i8 as i16 + 128) as u8)
                    .collect();
                MomentRow::U8(row)
            }
            2 => {
                let words: Vec<i16> = payload
                    .chunks_exact(2)
                    .map(|pair| match endian {
                        Endian::Little => i16::from_le_bytes([pair[0], pair[1]]),
                        Endian::Big => i16::from_be_bytes([pair[0], pair[1]]),
                    })
                    .collect();
                let words = if compressed {
                    let gates = gate_count.ok_or_else(|| {
                        invalid(
                            offset,
                            format!("no gate count for compressed DORADE field '{name}'"),
                        )
                    })?;
                    decode_hrd_rle(&words, gates, param.bad_data as i16)?
                } else {
                    words
                };
                // i16 → u16 storage; +32768 keeps (raw − offset)/scale intact.
                MomentRow::U16(
                    words
                        .into_iter()
                        .map(|word| (i32::from(word) + 32768) as u16)
                        .collect(),
                )
            }
            3 => MomentRow::F32(
                payload
                    .chunks_exact(4)
                    .map(|quad| {
                        let raw = endian.i32(quad, 0);
                        if raw == param.bad_data {
                            f32::NAN
                        } else {
                            (raw as f32 - param.bias) / param.scale
                        }
                    })
                    .collect(),
            ),
            4 => MomentRow::F32(
                payload
                    .chunks_exact(4)
                    .map(|quad| {
                        let raw = endian.f32(quad, 0);
                        if raw == param.bad_data as f32 || raw <= DORADE_BAD_F32 {
                            f32::NAN
                        } else {
                            (raw - param.bias) / param.scale
                        }
                    })
                    .collect(),
            ),
            _ => {
                // 16-bit float (format 5) is unobserved in the wild corpus;
                // skip the field rather than failing the sweep.
                self.skipped_field_blocks += 1;
                return Ok(());
            }
        };
        let decoded_cells = self
            .decoded_cells
            .checked_add(row.len())
            .ok_or_else(|| invalid(offset, "DORADE decoded-cell count overflow"))?;
        if decoded_cells > MAX_DORADE_CELLS_PER_SWEEP {
            return Err(DoradeError::LimitExceeded(format!(
                "DORADE sweep exceeds the {MAX_DORADE_CELLS_PER_SWEEP}-cell decode limit"
            )));
        }
        self.decoded_cells = decoded_cells;
        self.params[param_index].pending_row = Some(row);
        Ok(())
    }

    fn gate_count_for_param(&self, param_index: usize) -> Option<usize> {
        self.range_gate_count
            .or_else(|| self.params[param_index].number_cells)
    }

    fn finish_current_ray(&mut self) {
        let Some(ray) = self.current_ray.take() else {
            return;
        };
        let rows: Vec<(usize, MomentRow)> = self
            .params
            .iter_mut()
            .enumerate()
            .filter_map(|(index, param)| param.pending_row.take().map(|row| (index, row)))
            .collect();
        // ray_status: 0 = normal, 1 = antenna in transition, 2 = bad.
        if ray.status != 0 {
            self.transition_rays.push((ray, rows));
        } else {
            self.rays.push((ray, rows));
        }
    }

    fn gate_range(&self) -> Result<GateRange> {
        if let (Some(first), Some(spacing), Some(count)) = (
            self.range_first_m,
            self.range_spacing_m,
            self.range_gate_count,
        ) {
            return Ok(GateRange {
                first_gate_m: (first + self.cfac.range_delay_m).round() as i32,
                gate_spacing_m: spacing.round().max(1.0) as i32,
                gate_count: count,
            });
        }
        let param = self
            .params
            .iter()
            .find(|param| param.number_cells.unwrap_or(0) > 0)
            .ok_or_else(|| invalid(0, "DORADE sweep has no CELV/CSFD/PARM range metadata"))?;
        Ok(GateRange {
            first_gate_m: (param.first_cell_m.unwrap_or(0.0) + self.cfac.range_delay_m).round()
                as i32,
            gate_spacing_m: param.cell_spacing_m.unwrap_or(1000.0).round().max(1.0) as i32,
            gate_count: param.number_cells.unwrap_or(0),
        })
    }

    /// Effective Nyquist (fold) velocity for the recorded velocity field.
    ///
    /// RADD `eff_unamb_vel` is authoritative when present: for staggered-PRT
    /// systems it already holds the extended unambiguous velocity the radar
    /// dealiased to. Otherwise fall back to the wavelength/PRT relations
    /// (Doviak and Zrnić 1993, eq. 3.17; Torres, Dubel, and Zrnić 2004 for
    /// the staggered extension λ/(4·(T2 − T1))).
    fn nyquist_velocity_mps(&self) -> Option<f32> {
        if let Some(value) = self.eff_unamb_vel_mps.filter(|value| *value > 0.0) {
            return Some(value);
        }
        let wavelength_m = 299_792_458.0f32 / (self.frequency_ghz? * 1.0e9);
        let mut prts: Vec<f32> = [self.prt1_ms, self.prt2_ms]
            .into_iter()
            .flatten()
            .filter(|prt| *prt > 0.0)
            .map(|prt| prt / 1000.0)
            .collect();
        prts.sort_by(f32::total_cmp);
        match prts.as_slice() {
            [] => None,
            [short] => Some(wavelength_m / (4.0 * short)),
            [short, long, ..] => {
                if self.num_ipps_trans.unwrap_or(1) >= 2 && (long - short) > f32::EPSILON {
                    Some(wavelength_m / (4.0 * (long - short)))
                } else {
                    Some(wavelength_m / (4.0 * short))
                }
            }
        }
    }

    fn finish_into(mut self, volume: &mut RadarVolume, budget: &mut DecodeBudget) -> Result<()> {
        let mut skipped_transition_rays = self.transition_rays.len();
        if self.rays.is_empty() {
            if self.transition_rays.is_empty() {
                return Err(invalid(0, "DORADE sweep contains no rays"));
            }
            // All-transition sweep (e.g. 2009 Goshen DOW7 v4): the status
            // flag is the only thing wrong with the data, so keep it rather
            // than failing the whole volume/archive.
            self.rays = std::mem::take(&mut self.transition_rays);
            skipped_transition_rays = 0;
        }
        if self.instrument.is_empty() {
            self.instrument = "DORADE".to_owned();
        }
        if volume.site.id.is_empty() {
            volume.site = RadarSite {
                id: self.instrument.clone(),
                name: Some(format!("{} (mobile)", self.instrument)),
                latitude_deg: finite(self.site_latitude_deg()),
                longitude_deg: finite(self.site_longitude_deg()),
                elevation_m: finite(self.site_altitude_m()),
            };
            volume.metadata.archive_version = Some("DORADE".to_owned());
            volume.metadata.compression = Some(
                if self.compression == 1 {
                    "dorade-hrd-rle"
                } else {
                    "dorade-uncompressed"
                }
                .to_owned(),
            );
            volume.metadata.scan_mode = Some(scan_mode_from_radd(self.scan_mode));
            volume.metadata.radar_frequency_mhz = self
                .frequency_ghz
                .filter(|frequency| frequency.is_finite() && *frequency > 0.0)
                .map(|frequency| (frequency * 1000.0).round() as u32);
        } else if volume.site.id != self.instrument {
            return Err(invalid(
                0,
                format!(
                    "DORADE sweep instrument '{}' does not match volume '{}'",
                    self.instrument, volume.site.id
                ),
            ));
        }
        let sweep_start = self.start_time;
        if let Some(start) = sweep_start
            && (volume.cuts.is_empty() || start < volume.volume_time)
        {
            volume.volume_time = start;
        }

        let gate_range = self.gate_range()?;
        let nyquist = self.nyquist_velocity_mps();
        let fixed_angle = if self.fixed_angle_deg.is_finite() {
            self.fixed_angle_deg
        } else {
            let sum: f32 = self.rays.iter().map(|(ray, _)| ray.elevation_deg).sum();
            sum / self.rays.len() as f32
        };

        // Map params to canonical moments; first match per type wins, later
        // duplicates (e.g. DOW corrected fields DCZ/VC next to DZ/VE) keep
        // their DORADE name as MomentType::Unknown so nothing is dropped.
        let mut taken: BTreeSet<MomentType> = BTreeSet::new();
        for param in &mut self.params {
            let canonical = canonical_moment(&param.name);
            param.moment = match canonical {
                Some(moment) if !taken.contains(&moment) => {
                    taken.insert(moment.clone());
                    moment
                }
                _ => MomentType::Unknown(param.name.clone()),
            };
            param.grid = Some(new_grid(param, gate_range.clone()));
        }

        // Charge the finished grids before building them: every row is padded
        // to the widest row of its field, so the retained size follows from
        // the row counts and widths, not from the (possibly compressed) input.
        let mut rows_per_param = vec![(0usize, 0usize); self.params.len()];
        for (_, rows) in &self.rays {
            for (param_index, row) in rows {
                if let Some((count, widest)) = rows_per_param.get_mut(*param_index) {
                    *count += 1;
                    *widest = (*widest).max(row.len());
                }
            }
        }
        for (param, (rows, widest)) in self.params.iter_mut().zip(&rows_per_param) {
            if *rows == 0 {
                continue;
            }
            let word_bytes = match param.binary_format {
                1 => 1,
                2 => 2,
                _ => 4,
            };
            let row_bytes = gate_range
                .gate_count
                .max(*widest)
                .checked_mul(word_bytes)
                .and_then(|bytes| bytes.checked_add(size_of::<usize>()))
                .ok_or_else(|| invalid(0, "DORADE grid size overflow"))?;
            budget
                .charge(*rows, row_bytes, "DORADE moment grid")
                .map_err(DoradeError::LimitExceeded)?;
            if let Some(grid) = param.grid.as_mut() {
                reserve_grid(grid, *rows, gate_range.gate_count.max(*widest));
            }
        }
        budget
            .charge(self.rays.len(), size_of::<Radial>(), "DORADE sweep radials")
            .map_err(DoradeError::LimitExceeded)?;

        let elevation_number = u8::try_from(self.sweep_number.clamp(0, 255)).ok();
        let cut = volume.push_cut(fixed_angle, elevation_number);
        cut.radials.reserve_exact(self.rays.len());
        let rays = std::mem::take(&mut self.rays);
        for (ray, rows) in rays {
            let radial_index = cut.radials.len();
            let time_offset_ms = match (ray.time, sweep_start) {
                (Some(time), Some(start)) => (time - start)
                    .num_milliseconds()
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                    as i32,
                _ => 0,
            };
            cut.radials.push(Radial {
                azimuth_deg: normalize_azimuth(ray.azimuth_deg),
                elevation_deg: ray.elevation_deg,
                time_offset_ms,
                gate_range: gate_range.clone(),
                nyquist_velocity_mps: nyquist,
                radial_status: None,
            });
            for (param_index, row) in rows {
                let param = &mut self.params[param_index];
                if let Some(grid) = param.grid.as_mut() {
                    grid.push_row(radial_index, row)?;
                }
            }
        }
        for param in &mut self.params {
            if let Some(grid) = param.grid.take()
                && grid.radial_count() > 0
            {
                cut.moments.insert(grid.moment.clone(), grid);
            }
        }

        volume.metadata.message_count += 1;
        volume.metadata.skipped_message_count +=
            skipped_transition_rays + self.skipped_field_blocks;
        Ok(())
    }
}

fn new_grid(param: &ParamState, gate_range: GateRange) -> MomentGrid {
    match param.binary_format {
        1 => MomentGrid::new_u8(
            param.moment.clone(),
            gate_range,
            param.scale,
            param.bias + 128.0,
            i32_to_u8_sentinel(param.bad_data),
            None,
        ),
        2 => MomentGrid::new_u16(
            param.moment.clone(),
            gate_range,
            param.scale,
            param.bias + 32768.0,
            i32_to_u16_sentinel(param.bad_data),
            None,
        ),
        _ => MomentGrid {
            moment: param.moment.clone(),
            gate_range,
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices: Vec::new(),
            storage: recast_radar_core::MomentStorage::F32(Vec::new()),
        },
    }
}

/// Reserve exactly `rows` rows of `gates` values (the widest row, which the
/// grid pads every row to), so pushing them never reallocates.
fn reserve_grid(grid: &mut MomentGrid, rows: usize, gates: usize) {
    grid.radial_indices.reserve_exact(rows);
    let values = rows.saturating_mul(gates);
    match &mut grid.storage {
        recast_radar_core::MomentStorage::U8(storage) => storage.reserve_exact(values),
        recast_radar_core::MomentStorage::U16(storage) => storage.reserve_exact(values),
        recast_radar_core::MomentStorage::F32(storage) => storage.reserve_exact(values),
    }
}

fn i32_to_u8_sentinel(bad_data: i32) -> Option<u8> {
    u8::try_from(bad_data + 128).ok()
}

fn i32_to_u16_sentinel(bad_data: i32) -> Option<u16> {
    u16::try_from(i64::from(bad_data) + 32768).ok()
}

/// Map the DORADE RADD `scan_mode` code onto the shared scan-mode enum.
///
/// Code values per the DORADE format document (R. Oye and M. Case, "DORADE
/// Data Format", NCAR/ATD 1995; revised by W.-C. Lee, NCAR/EOL) and the
/// authoritative lrose-core `DoradeData.hh` enum: 0 = CAL (calibration),
/// 1 = PPI (sector), 2 = COP (coplane), 3 = RHI, 4 = VER (vertical
/// pointing), 5 = TAR (target/stationary), 6 = MAN (manual), 7 = IDL (idle),
/// 8 = SUR (360° surveillance), 9 = AIR (airborne), 10 = HOR (horizontal).
fn scan_mode_from_radd(code: i16) -> ScanMode {
    match code {
        1 | 8 => ScanMode::Ppi,
        3 => ScanMode::Rhi,
        4 => ScanMode::VerticalPointing,
        _ => ScanMode::Other,
    }
}

/// Decompress one HRD run-length-encoded 16-bit field row.
///
/// Marker word semantics (DORADE document, "compression scheme" appendix):
/// high bit set → `count` data words follow verbatim; high bit clear →
/// `count` gates of missing data; a bare `1` terminates the row.
fn decode_hrd_rle(words: &[i16], gates: usize, bad_data: i16) -> Result<Vec<i16>> {
    validate_gate_count(gates, 0, "HRD RLE")?;
    let mut out = vec![bad_data; gates];
    let mut input = 0usize;
    let mut output = 0usize;
    while input < words.len() && output < gates {
        let marker = words[input] as u16;
        input += 1;
        let count = (marker & 0x7fff) as usize;
        if marker == 1 {
            // End-of-row sentinel.
            break;
        }
        if count == 0 {
            continue;
        }
        if marker & 0x8000 != 0 {
            let take = count
                .min(gates - output)
                .min(words.len().saturating_sub(input));
            out[output..output + take].copy_from_slice(&words[input..input + take]);
            input += count.min(words.len().saturating_sub(input));
            output += take;
        } else {
            output += count.min(gates - output);
        }
    }
    Ok(out)
}

fn normalize_azimuth(azimuth_deg: f32) -> f32 {
    let normalized = azimuth_deg.rem_euclid(360.0);
    if normalized.is_finite() {
        normalized
    } else {
        0.0
    }
}

fn valid_dorade_f32(value: f32) -> Option<f32> {
    (value.is_finite() && value > DORADE_BAD_F32 && value != 0.0).then_some(value)
}

fn finite(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

fn require(block: &[u8], needed: usize, offset: usize, what: &'static str) -> Result<()> {
    if block.len() < needed {
        return Err(DoradeError::Truncated {
            what,
            offset,
            needed,
            available: block.len(),
        });
    }
    Ok(())
}

fn validate_gate_count(gates: usize, offset: usize, descriptor: &'static str) -> Result<()> {
    check_gate_count(gates, &format!("{descriptor} at offset {offset}"))
        .map_err(DoradeError::LimitExceeded)
}

fn invalid(offset: usize, reason: impl Into<String>) -> DoradeError {
    DoradeError::InvalidMessage {
        offset,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recast_radar_core::MomentStorage;

    // Real inputs (corpus ids below). Expected values:
    // tools/golden_io_formats.py, section `dorade` (a DORADE block walker and
    // HRD run-length decoder written from the DORADE format document and the
    // lrose-core DoradeData.hh offsets; physical = (word - bias) / scale).
    const COW2: &str = "dorade-cow2-20260521-225514-sur-head24";
    const DOW6_RHI: &str = "dorade-dow6-20211230-222139-rhi-head41";
    const NOXP_SECTOR: &str = "dorade-noxp-20090525-203211-sector";
    const NOXP_0610_05: &str = "dorade-noxp-20090610-003210-ppi-head6";
    const NOXP_0610_10: &str = "dorade-noxp-20090610-003222-ppi-head6";
    const NOXP_0610_20: &str = "dorade-noxp-20090610-003226-ppi-head6";

    fn corpus(id: &str) -> Vec<u8> {
        recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
    }

    fn assert_gate(grid: &MomentGrid, row: usize, gate: usize, expected: Option<f32>) {
        let actual = grid.scaled_value(row, gate);
        match (actual, expected) {
            (None, None) => {}
            (Some(actual), Some(expected)) => assert!(
                (actual - expected).abs() < 1e-3,
                "{} [{row},{gate}]: {actual} != {expected}",
                grid.moment
            ),
            _ => panic!("{} [{row},{gate}]: {actual:?} != {expected:?}", grid.moment),
        }
    }

    /// Missing gates among the sweep's CSFD/CELV cells. (Uncompressed NOXP
    /// RDAT payloads carry one more word than the 1001 cells, padding the
    /// block to a 4-byte boundary; only the described cells are counted.)
    fn missing_gates(grid: &MomentGrid, cells: usize, row: usize) -> usize {
        (0..cells)
            .filter(|gate| grid.scaled_value(row, *gate).is_none())
            .count()
    }

    fn close(actual: f32, expected: f32, tolerance: f32) -> bool {
        (actual - expected).abs() <= tolerance
    }

    #[test]
    fn decodes_big_endian_real_cow2_sweep() {
        let bytes = corpus(COW2);
        assert!(looks_like_dorade_bytes(&bytes));
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Big);

        let volume = decode_dorade_sweep_volume(&bytes).expect("decode COW2");
        // RADD: name COW2, scan mode 8 (SUR), lat 39.739979, lon -103.292664,
        // altitude 1.519 km, HRD RLE (data_compress 1).
        assert_eq!(volume.site.id, "COW2");
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
        assert!(close(volume.site.latitude_deg.unwrap(), 39.739_98, 1e-5));
        assert!(close(volume.site.longitude_deg.unwrap(), -103.292_66, 1e-5));
        assert!(close(volume.site.elevation_m.unwrap(), 1519.0, 0.01));
        assert_eq!(
            volume.metadata.compression.as_deref(),
            Some("dorade-hrd-rle")
        );
        // SSWB start 1779404114 = 2026-05-21T22:55:14Z.
        assert_eq!(
            volume.volume_time,
            Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
        );
        assert_eq!(volume.cuts.len(), 1);

        let cut = &volume.cuts[0];
        // SWIB fixed angle 1.0052556, sweep 6; RYIB status [1, 1, 1, 0, ...]:
        // three transition rays dropped, 21 kept.
        assert_eq!(cut.elevation_deg, 1.005_255_6);
        assert_eq!(cut.elevation_number, Some(6));
        assert_eq!(cut.radials.len(), 21);
        assert_eq!(volume.metadata.skipped_message_count, 3);
        for (row, (azimuth, time_offset_ms)) in
            [(0, (73.0, 280)), (1, (73.5, 297)), (20, (83.0, 609))]
        {
            let radial = &cut.radials[row];
            assert_eq!(radial.azimuth_deg, azimuth, "ray {row}");
            assert_eq!(radial.elevation_deg, 0.818_481_45, "ray {row}");
            assert_eq!(radial.time_offset_ms, time_offset_ms, "ray {row}");
            // CSFD: one segment, 375 cells, 50 m to the first, 100 m apart.
            assert_eq!(
                (
                    radial.gate_range.first_gate_m,
                    radial.gate_range.gate_spacing_m,
                    radial.gate_range.gate_count
                ),
                (50, 100, 375)
            );
            // RADD eff_unamb_vel 68.75974 m/s.
            assert!(close(radial.nyquist_velocity_mps.unwrap(), 68.759_74, 1e-4));
        }

        // PARM DBZHC_F / VEL_F / ZDR_F (scale 100) and RHOHV_F (scale 10000),
        // bias 0, bad -32768, on the first and last kept rays.
        let reflectivity = &cut.moments[&MomentType::Reflectivity];
        let velocity = &cut.moments[&MomentType::Velocity];
        let zdr = &cut.moments[&MomentType::DifferentialReflectivity];
        let rhohv = &cut.moments[&MomentType::CorrelationCoefficient];
        assert_gate(reflectivity, 0, 1, Some(-14.22));
        assert_gate(reflectivity, 20, 0, Some(-19.85));
        assert_gate(reflectivity, 20, 1, Some(-11.37));
        assert_gate(reflectivity, 20, 50, None);
        assert_gate(velocity, 0, 1, Some(-49.06));
        assert_gate(velocity, 20, 0, Some(-66.56));
        assert_gate(velocity, 20, 100, Some(54.18));
        assert_gate(velocity, 20, 374, Some(-34.55));
        assert_gate(zdr, 20, 0, Some(6.53));
        assert_gate(rhohv, 20, 1, Some(0.8043));
        // Bad-gate counts of the first and last kept rays.
        assert_eq!(missing_gates(reflectivity, 375, 0), 265);
        assert_eq!(missing_gates(velocity, 375, 0), 112);
        assert_eq!(missing_gates(zdr, 375, 20), 335);
        assert_eq!(missing_gates(rhohv, 375, 20), 332);
    }

    #[test]
    fn decodes_little_endian_rle_sweep() {
        // DOW6low RHI: little-endian HRD RLE, CELV 1000 cells from 24.98 m at
        // 49.97 m spacing, 104-byte PARMs, 41 rays of which the first 6 are
        // transition rays.
        let bytes = corpus(DOW6_RHI);
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Little);
        let volume = decode_dorade_sweep_volume(&bytes).expect("decode DOW6 RHI");
        assert_eq!(volume.site.id, "DOW6low");
        assert_eq!(
            volume.metadata.compression.as_deref(),
            Some("dorade-hrd-rle")
        );
        assert!(close(volume.site.latitude_deg.unwrap(), 39.995_46, 1e-5));
        assert!(close(volume.site.longitude_deg.unwrap(), -105.191_68, 1e-5));
        assert!(close(volume.site.elevation_m.unwrap(), 1615.0, 0.01));
        let cut = &volume.cuts[0];
        assert_eq!(cut.radials.len(), 35);
        assert_eq!(volume.metadata.skipped_message_count, 6);
        let first = &cut.radials[0];
        assert_eq!(
            (
                first.gate_range.first_gate_m,
                first.gate_range.gate_spacing_m,
                first.gate_range.gate_count
            ),
            (25, 50, 1000)
        );
        assert!(close(first.nyquist_velocity_mps.unwrap(), 39.866_02, 1e-4));
        assert_eq!(first.time_offset_ms, 1126);
        assert_eq!(cut.radials[34].time_offset_ms, 3503);

        // First canonical match wins: DBZHC -> Reflectivity, VEL -> Velocity;
        // the edited VEL_F copy keeps its DORADE name.
        let reflectivity = &cut.moments[&MomentType::Reflectivity];
        let velocity = &cut.moments[&MomentType::Velocity];
        let velocity_f = &cut.moments[&MomentType::Unknown("VEL_F".to_owned())];
        assert_gate(reflectivity, 0, 0, Some(-12.24));
        assert_gate(reflectivity, 0, 10, Some(-13.07));
        assert_gate(reflectivity, 0, 100, None);
        assert_gate(reflectivity, 34, 100, Some(-6.92));
        assert_gate(velocity, 0, 0, Some(36.76));
        assert_gate(velocity, 0, 10, Some(0.12));
        assert_gate(velocity, 0, 100, Some(-34.92));
        assert_gate(velocity, 0, 500, Some(40.44));
        assert_gate(velocity, 0, 999, Some(-0.53));
        assert_gate(velocity, 34, 500, Some(31.42));
        assert_gate(velocity_f, 0, 0, Some(32.6));
        assert_gate(velocity_f, 0, 10, Some(18.6));
        assert_gate(
            &cut.moments[&MomentType::CorrelationCoefficient],
            0,
            0,
            Some(0.6755),
        );
        assert_gate(
            &cut.moments[&MomentType::DifferentialPhase],
            0,
            999,
            Some(125.2),
        );
        assert_eq!(missing_gates(reflectivity, 1000, 0), 887);
        assert_eq!(
            missing_gates(
                &cut.moments[&MomentType::SpecificDifferentialPhase],
                1000,
                0
            ),
            1000
        );
    }

    #[test]
    fn decodes_little_endian_uncompressed_sweep() {
        // NOXP sector PPI: little-endian, uncompressed (data_compress 0), CSFD
        // 1001 cells x 150 m from 75 m, RADD scan mode 1, 100 rays.
        let bytes = corpus(NOXP_SECTOR);
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Little);
        let volume = decode_dorade_sweep_volume(&bytes).expect("decode NOXP sector");
        assert_eq!(volume.site.id, "NOXPRVP");
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Ppi));
        assert_eq!(
            volume.metadata.compression.as_deref(),
            Some("dorade-uncompressed")
        );
        assert!(close(volume.site.latitude_deg.unwrap(), 34.480_247, 1e-5));
        assert!(close(volume.site.longitude_deg.unwrap(), -100.336_24, 1e-5));
        assert_eq!(
            volume.volume_time,
            Utc.with_ymd_and_hms(2009, 5, 25, 20, 32, 11).unwrap()
        );
        let cut = &volume.cuts[0];
        assert_eq!(cut.radials.len(), 100);
        assert_eq!(cut.elevation_deg, 0.499_877_93);
        let first = &cut.radials[0];
        assert_eq!(
            (
                first.gate_range.first_gate_m,
                first.gate_range.gate_spacing_m,
                first.gate_range.gate_count
            ),
            (75, 150, 1001)
        );
        // RYIB azimuth -160.03235 deg, normalized into [0, 360).
        assert!(close(first.azimuth_deg, 360.0 - 160.032_35, 1e-3));
        assert!(close(cut.radials[98].azimuth_deg, 360.0 - 62.168_884, 1e-3));
        assert_eq!(cut.radials[98].time_offset_ms, -4000);
        assert!(close(first.nyquist_velocity_mps.unwrap(), 7.576_25, 1e-4));

        let reflectivity = &cut.moments[&MomentType::Reflectivity]; // DZ
        let velocity = &cut.moments[&MomentType::Velocity]; // VR
        assert_gate(reflectivity, 0, 0, Some(6.5));
        assert_gate(reflectivity, 0, 50, Some(40.0));
        assert_gate(reflectivity, 0, 100, Some(-2.0));
        assert_gate(reflectivity, 0, 300, None);
        assert_gate(reflectivity, 0, 900, Some(39.5));
        assert_gate(reflectivity, 98, 300, Some(25.5));
        assert_gate(velocity, 0, 100, Some(-4.18));
        assert_gate(velocity, 0, 900, Some(-6.14));
        assert_gate(velocity, 98, 300, Some(0.18));
        assert_eq!(missing_gates(reflectivity, 1001, 0), 679);
        assert_eq!(missing_gates(velocity, 1001, 0), 718);
        assert_eq!(missing_gates(reflectivity, 1001, 99), 1001);
    }

    #[test]
    fn rle_run_of_missing_gates_pads_with_bad() {
        // 2 missing gates, then 2 literal words, end sentinel.
        let words = [2i16, (0x8000u16 | 2) as i16, 700, 800, 1];
        let out = decode_hrd_rle(&words, 6, -32768).unwrap();
        assert_eq!(out, vec![-32768, -32768, 700, 800, -32768, -32768]);
    }

    #[test]
    fn rejects_extended_parm_with_absurd_gate_count() {
        // COW2 PARM DBZHC_F: 216-byte extended block at offset 1080,
        // number_cells 375 at block offset 200 (big-endian).
        let mut bytes = corpus(COW2);
        const PARM: usize = 1080;
        assert_eq!(&bytes[PARM..PARM + 4], b"PARM");
        assert_eq!(Endian::Big.i32(&bytes, PARM + 4), 216);
        assert_eq!(Endian::Big.i32(&bytes, PARM + 200), 375);
        {
            let mut sweep = SweepParse::new(Endian::Big);
            sweep
                .parse_parm(&bytes[PARM..PARM + 216], PARM)
                .expect("real PARM parses");
            assert_eq!(sweep.params[0].name, "DBZHC_F");
            assert_eq!(sweep.params[0].number_cells, Some(375));
        }

        bytes[PARM + 200..PARM + 204].copy_from_slice(&i32::MAX.to_be_bytes());
        let mut sweep = SweepParse::new(Endian::Big);
        let err = sweep
            .parse_parm(&bytes[PARM..PARM + 216], PARM)
            .expect_err("absurd gate count must be rejected");
        assert!(err.to_string().contains("gates per radial"), "{err}");
        let err = decode_dorade_sweep_volume(&bytes).expect_err("whole sweep rejected");
        assert!(err.to_string().contains("gates per radial"), "{err}");
    }

    #[test]
    fn peek_reads_grouping_metadata_without_rays() {
        // (id, instrument, VOLD volume, SWIB sweep, fixed angle, SSWB start,
        // RADD latitude, first RYIB offset)
        for (id, instrument, volume_number, sweep_number, fixed, start, latitude, first_ray) in [
            (
                COW2,
                "COW2",
                215,
                6,
                1.005_255_6f32,
                (2026, 5, 21, 22, 55, 14),
                39.739_98f32,
                2048,
            ),
            (
                DOW6_RHI,
                "DOW6low",
                169,
                0,
                143.998_75,
                (2021, 12, 30, 22, 21, 39),
                39.995_46,
                10_372,
            ),
            (
                NOXP_0610_05,
                "NOXPRVP",
                1,
                1,
                0.499_877_93,
                (2009, 6, 10, 0, 32, 10),
                37.597_79,
                3196,
            ),
        ] {
            let bytes = corpus(id);
            // Only the descriptor blocks before the first ray are needed.
            let header = peek_dorade_sweep(&bytes[..first_ray]).expect("peek");
            assert_eq!(header.instrument, instrument, "{id}");
            assert_eq!(header.volume_number, volume_number, "{id}");
            assert_eq!(header.sweep_number, sweep_number, "{id}");
            assert_eq!(header.fixed_angle_deg, fixed, "{id}");
            let (y, mo, d, h, mi, s) = start;
            assert_eq!(
                header.start_time,
                Some(Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()),
                "{id}"
            );
            assert!(close(header.latitude_deg, latitude, 1e-5), "{id}");
            assert_eq!(peek_dorade_sweep(&bytes).expect("peek full file"), header);
        }
    }

    #[test]
    fn multi_sweep_volume_sorts_cuts_by_elevation() {
        // Three sweeps of NOXP volume NOX090610003210 (SWIB fixed angles
        // 0.49987793, 0.99975586, 1.9995117; 6 rays each), passed out of
        // order.
        let sweeps = [
            corpus(NOXP_0610_20),
            corpus(NOXP_0610_05),
            corpus(NOXP_0610_10),
        ];
        let volume = decode_dorade_volume_from_slices(&sweeps).expect("decode");
        let angles: Vec<f32> = volume.cuts.iter().map(|cut| cut.elevation_deg).collect();
        assert_eq!(angles, [0.499_877_93, 0.999_755_86, 1.999_511_7]);
        assert!(volume.cuts.iter().all(|cut| cut.radials.len() == 6));
        assert_eq!(volume.metadata.decoded_radial_count, 18);
        // Earliest SSWB start (the 0.5 deg sweep, 00:32:10Z) is the volume time.
        assert_eq!(
            volume.volume_time,
            Utc.with_ymd_and_hms(2009, 6, 10, 0, 32, 10).unwrap()
        );
        // Per-ray elevations of the 1.0 deg sweep: 0.98876953.
        assert!(
            volume.cuts[1]
                .radials
                .iter()
                .all(|r| r.elevation_deg == 0.988_769_53)
        );
        // CSFD 1174 cells x 75 m from 37.5 m.
        let gates = &volume.cuts[2].radials[0].gate_range;
        assert_eq!(
            (gates.first_gate_m, gates.gate_spacing_m, gates.gate_count),
            (38, 75, 1174)
        );
        assert_gate(
            &volume.cuts[0].moments[&MomentType::Reflectivity],
            0,
            100,
            Some(-2.5),
        );
        assert_gate(
            &volume.cuts[1].moments[&MomentType::Velocity],
            5,
            100,
            Some(-13.56),
        );
    }

    #[test]
    fn mismatched_instruments_are_rejected() {
        let err =
            decode_dorade_volume_from_slices(&[corpus(COW2), corpus(NOXP_0610_05)]).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert!(err.to_string().contains("NOXPRVP"), "{err}");
    }

    #[test]
    fn rhi_scan_mode_is_detected_from_radd() {
        // DOW6low: RADD scan mode 3 (RHI), SWIB fixed angle 143.99875 deg;
        // kept rays step elevation down from 30.0 to 13.0 deg by 0.5 deg at
        // RYIB azimuths 125.78-126.55 deg (CFAC corrections all zero).
        let volume = decode_dorade_sweep_volume(&corpus(DOW6_RHI)).expect("decode");
        assert_eq!(volume.metadata.scan_mode, Some(ScanMode::Rhi));
        let cut = &volume.cuts[0];
        assert_eq!(cut.elevation_deg, 143.998_75);
        assert_eq!(cut.radials.len(), 35);
        for (index, radial) in cut.radials.iter().enumerate() {
            assert_eq!(
                radial.elevation_deg,
                30.0 - 0.5 * index as f32,
                "ray {index}"
            );
            assert!((125.7..126.6).contains(&radial.azimuth_deg), "ray {index}");
        }
        assert!(close(cut.radials[0].azimuth_deg, 125.775_07, 1e-4));
        assert!(close(cut.radials[34].azimuth_deg, 126.549_61, 1e-4));
    }

    #[test]
    fn radd_scan_mode_codes_map_to_shared_enum() {
        // Codes per Oye & Case 1995 / lrose DoradeData.hh.
        assert_eq!(scan_mode_from_radd(1), ScanMode::Ppi); // PPI sector
        assert_eq!(scan_mode_from_radd(8), ScanMode::Ppi); // SUR
        assert_eq!(scan_mode_from_radd(3), ScanMode::Rhi);
        assert_eq!(scan_mode_from_radd(4), ScanMode::VerticalPointing);
        for other in [0i16, 2, 5, 6, 7, 9, 10, 99] {
            assert_eq!(scan_mode_from_radd(other), ScanMode::Other);
        }
    }

    #[test]
    fn canonical_moment_maps_observed_corpus_names() {
        // COW2 (Radx _F names), RaXPol, DOW7 solo-era names.
        assert_eq!(canonical_moment("DBZHC_F"), Some(MomentType::Reflectivity));
        assert_eq!(canonical_moment("VEL_F"), Some(MomentType::Velocity));
        assert_eq!(
            canonical_moment("ZDR_F"),
            Some(MomentType::DifferentialReflectivity)
        );
        assert_eq!(
            canonical_moment("RHOHV_F"),
            Some(MomentType::CorrelationCoefficient)
        );
        assert_eq!(canonical_moment("DBZ"), Some(MomentType::Reflectivity));
        assert_eq!(canonical_moment("WIDTH"), Some(MomentType::SpectrumWidth));
        assert_eq!(canonical_moment("DZ"), Some(MomentType::Reflectivity));
        assert_eq!(canonical_moment("VE"), Some(MomentType::Velocity));
        assert_eq!(canonical_moment("SW"), Some(MomentType::SpectrumWidth));
        assert_eq!(canonical_moment("NCP"), None);
        assert_eq!(canonical_moment("DM"), None);
    }

    #[test]
    fn duplicate_canonical_names_keep_original_field() {
        // DOW7 carries DZ (raw) and DCZ/VC (corrected); first match wins and
        // later candidates stay addressable under their DORADE names.
        let mut taken = BTreeSet::new();
        let mut resolved = Vec::new();
        for name in ["DZ", "DCZ", "VE", "VC"] {
            let canonical = canonical_moment(name);
            let moment = match canonical {
                Some(moment) if !taken.contains(&moment) => {
                    taken.insert(moment.clone());
                    moment
                }
                _ => MomentType::Unknown(name.to_owned()),
            };
            resolved.push(moment);
        }
        assert_eq!(resolved[0], MomentType::Reflectivity);
        assert_eq!(resolved[1], MomentType::Unknown("DCZ".to_owned()));
        assert_eq!(resolved[2], MomentType::Velocity);
        assert_eq!(resolved[3], MomentType::Unknown("VC".to_owned()));
    }

    #[test]
    fn u16_grids_preserve_dorade_scaling() {
        // COW2 PARM (binary_format 2 = i16): DBZHC_F scale 100, RHOHV_F scale
        // 10000, bias 0, bad_data -32768; i16 words shift into u16 storage.
        let volume = decode_dorade_sweep_volume(&corpus(COW2)).expect("decode");
        let cut = &volume.cuts[0];
        for (moment, scale) in [
            (MomentType::Reflectivity, 100.0),
            (MomentType::Velocity, 100.0),
            (MomentType::DifferentialReflectivity, 100.0),
            (MomentType::CorrelationCoefficient, 10_000.0),
        ] {
            let grid = &cut.moments[&moment];
            assert!(matches!(grid.storage, MomentStorage::U16(_)), "{moment}");
            assert_eq!(grid.scale, scale, "{moment}");
            assert_eq!(grid.offset, 32768.0, "{moment}");
            assert_eq!(grid.nodata, Some(0), "{moment}");
        }
        // Raw word -3030 (golden REF gate 0 of the first kept ray) is stored as
        // -3030 + 32768.
        let MomentStorage::U16(words) = &cut.moments[&MomentType::Reflectivity].storage else {
            unreachable!()
        };
        assert_eq!(words[0], 29_738);
    }
}
