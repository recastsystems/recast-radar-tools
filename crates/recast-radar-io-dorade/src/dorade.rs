//! Native DORADE sweepfile (`swp.*`) decoder for mobile research radars
//! (DOW6/DOW7/DOW8, COW, RaXPol, and other CSWR/OU sweepfile producers).
//!
//! Decodes directly into the FM301 model ([`Volume`]; design note
//! `docs/design/fm301-model.md` sections 6.6, 8.1 and 10): each sweepfile
//! contributes one [`Sweep`] whose fields keep the DORADE parameter names
//! verbatim (`DBZHC_F`, `VEL_F`, `DZ`, ...) and the stored encoding: 8-bit
//! and 16-bit integers stay `i8` / `i16` with the DORADE
//! `(raw - bias) / scale` transform and `bad_data` as `_FillValue`; 32-bit
//! integer and float parameters are expanded to physical `f32` (NaN for
//! bad data), as no packed form of them fits the model. The `range`
//! coordinate holds the CELV cell distances or the CSFD segments (uniform
//! when the cells are evenly spaced, explicit centres otherwise) plus the
//! CFAC range delay. The RADD scan mode maps to `sweep_mode` (SUR ->
//! `azimuth_surveillance`, PPI -> `sector`, RHI -> `rhi`, VER ->
//! `vertical_pointing`, COP -> `coplane`, IDL -> `idle`, TAR -> `pointing`,
//! MAN -> `manual_ppi`, others verbatim); for RHI sweeps the fixed angle is
//! the AZIMUTH. Sweeps of a multi-file volume stay in input order (scan
//! time), the time reference is the earliest sweep start, and every ray's
//! RYIB time is a `time(time)` value relative to it.
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
//! - **Per-ray times**: RYIB julian day + h/m/s/ms become the `time`
//!   coordinate; the reference dropped ray times.
//! - **Binary formats**: 8-bit int, 16-bit int, 32-bit int, and 32-bit float
//!   PARM data are supported; the reference assumed 16-bit everywhere.
//! - **Staggered-PRT Nyquist**: the extended unambiguous velocity falls back
//!   to `λ / (4·(T2 − T1))` (Zrnić and Mahapatra 1985, IEEE Trans. AES-21;
//!   Torres, Dubel, and Zrnić 2004, J. Atmos. Oceanic Technol. 21,
//!   1389–1399) when RADD `eff_unamb_vel` is missing; the reference used
//!   `m·Va_short`, which is only correct for `n − m = 1` stagger ratios.
//!
//! Known limitations (documented, not silent):
//! - Per-ray platform georeferencing (`ASIB`) is ignored: DOW/COW/RaXPol
//!   deployments are parked, so the RADD site position applies to the whole
//!   sweep. Airborne tail radars would need ASIB handling.

use std::path::Path;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use recast_radar_core::bounded_read::{DecodeBudget, check_gate_count, check_sweep_count};
use recast_radar_core::model::{
    Field, FieldData, FieldName, FloatCoding, FollowMode, GateMapping, IntCoding, LinearTransform,
    RangeCoord, SourceFormat, Sweep, SweepMode, Volume, floor_to_second,
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
pub(crate) enum Endian {
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

/// Decode one sweepfile into a fresh single-sweep volume.
pub fn read_dorade_sweep_volume(bytes: &[u8]) -> Result<Volume> {
    let mut builder = DoradeVolumeBuilder::new();
    builder.append(bytes)?;
    builder.finish()
}

/// Decode a set of sweepfiles forming one volume scan.
///
/// Sweeps are appended in input order, which the callers arrange to be scan
/// time. The site position comes from the first sweep's RADD block — mobile
/// radars move between deployments, so the coordinates always come from the
/// file.
pub fn read_dorade_volume_from_slices<S: AsRef<[u8]>>(sweeps: &[S]) -> Result<Volume> {
    if sweeps.is_empty() {
        return Err(invalid(0, "no DORADE sweeps to decode"));
    }
    let mut builder = DoradeVolumeBuilder::new();
    for sweep in sweeps {
        builder.append(sweep.as_ref())?;
    }
    builder.finish()
}

/// Decode a set of sweepfile paths forming one volume scan.
pub fn read_dorade_volume_from_paths<P: AsRef<Path>>(paths: &[P]) -> Result<Volume> {
    if paths.is_empty() {
        return Err(invalid(0, "no DORADE sweep paths to decode"));
    }
    let mut builder = DoradeVolumeBuilder::new();
    for path in paths {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| DoradeError::Io {
            path: path.display().to_string(),
            source,
        })?;
        builder.append(&bytes)?;
    }
    let mut volume = builder.finish()?;
    volume.provenance.source_path = Some(paths[0].as_ref().display().to_string());
    Ok(volume)
}

/// A DORADE volume assembled sweepfile by sweepfile.
///
/// The first appended sweep populates the site, time reference and
/// provenance; later sweeps must come from the same instrument.
#[derive(Debug)]
pub struct DoradeVolumeBuilder {
    volume: Volume,
    /// SSWB/VOLD start time of each appended sweep.
    sweep_starts: Vec<Option<DateTime<Utc>>>,
}

impl Default for DoradeVolumeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DoradeVolumeBuilder {
    pub fn new() -> Self {
        Self {
            volume: Volume::new("", DateTime::<Utc>::UNIX_EPOCH),
            sweep_starts: Vec::new(),
        }
    }

    /// Sweeps appended so far.
    pub fn sweep_count(&self) -> usize {
        self.volume.sweeps.len()
    }

    /// Start time (SSWB, else VOLD) of each appended sweep.
    pub fn sweep_starts(&self) -> &[Option<DateTime<Utc>>] {
        &self.sweep_starts
    }

    /// Decode one sweepfile and append it as a sweep.
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let mut parse = SweepParse::new(detect_endian(bytes)?);
        parse.run(bytes, false)?;
        check_sweep_count(self.volume.sweeps.len() + 1, "DORADE volume")
            .map_err(DoradeError::LimitExceeded)?;
        // The volume budget covers every sweep appended so far.
        let mut budget = DecodeBudget::volume();
        let existing_rays: usize = self.volume.sweeps.iter().map(Sweep::nrays).sum();
        budget
            .charge(
                volume_field_capacity_bytes(&self.volume),
                1,
                "DORADE volume fields",
            )
            .and_then(|()| budget.charge(existing_rays, RAY_BYTES, "DORADE volume rays"))
            .map_err(DoradeError::LimitExceeded)?;
        parse.finish_into(self, &mut budget)
    }

    /// Seal the volume: ray-time coverage and invariants.
    pub fn finish(self) -> Result<Volume> {
        let Self { mut volume, .. } = self;
        volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
        volume.seal().map_err(|err| invalid(0, err.to_string()))?;
        volume.time_coverage = volume.ray_time_extent();
        Ok(volume)
    }

    /// Move the time reference earlier, rebasing every ray time.
    fn rebase(&mut self, reference: DateTime<Utc>) {
        if reference >= self.volume.time_reference && !self.volume.sweeps.is_empty() {
            return;
        }
        let shift = (self.volume.time_reference - reference).num_milliseconds() as f64 / 1000.0;
        for sweep in &mut self.volume.sweeps {
            for time in &mut sweep.rays.time_s {
                *time += shift;
            }
        }
        self.volume.time_reference = reference;
    }
}

/// Bytes a ray occupies in the model (three coordinates plus a Nyquist and
/// a PRT value).
const RAY_BYTES: usize = 3 * size_of::<f64>() + 2 * size_of::<f32>();

/// Allocated bytes of every field's value buffer in a volume.
fn volume_field_capacity_bytes(volume: &Volume) -> usize {
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

/// One PARM descriptor plus the field it feeds.
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
    field: Option<Field>,
    /// Decoded row for the in-flight ray, if any.
    pending_row: Option<ParamRow>,
}

/// One decoded RDAT row in its storage type.
enum ParamRow {
    I8(Vec<i8>),
    I16(Vec<i16>),
    /// Physical values (32-bit integer and float parameters).
    F32(Vec<f32>),
}

impl ParamRow {
    fn len(&self) -> usize {
        match self {
            Self::I8(row) => row.len(),
            Self::I16(row) => row.len(),
            Self::F32(row) => row.len(),
        }
    }
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
    /// CELV per-cell ranges or CSFD-derived gate centres (metres, before the
    /// CFAC range delay).
    range_cells_m: Option<Vec<f32>>,
    rays: Vec<(PendingRay, Vec<(usize, ParamRow)>)>,
    /// Antenna-transition rays, kept aside so an all-transition sweep (seen
    /// in the 2009 Goshen DOW7 corpus) can still decode instead of erroring.
    transition_rays: Vec<(PendingRay, Vec<(usize, ParamRow)>)>,
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
            range_cells_m: None,
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
            field: None,
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
        // CELV lists every cell range (uniform in the observed corpus).
        self.range_cells_m = Some(
            (0..count)
                .map(|cell| self.endian.f32(block, 12 + cell * 4))
                .collect(),
        );
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
        let mut total_cells = 0usize;
        for segment in 0..segments {
            total_cells += self.endian.i16(block, 48 + segment * 2).max(0) as usize;
        }
        if total_cells == 0 {
            return Ok(());
        }
        validate_gate_count(total_cells, offset, "CSFD")?;
        // Cell centres segment by segment: each segment continues from the
        // previous one at its own spacing.
        let mut cells = Vec::with_capacity(total_cells);
        let mut center = f64::from(first);
        for segment in 0..segments {
            let spacing = f64::from(self.endian.f32(block, 16 + segment * 4));
            let count = self.endian.i16(block, 48 + segment * 2).max(0) as usize;
            for _ in 0..count {
                cells.push(center as f32);
                center += spacing;
            }
        }
        self.range_cells_m = Some(cells);
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
            1 => ParamRow::I8(payload.iter().map(|byte| *byte as i8).collect()),
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
                ParamRow::I16(words)
            }
            3 => ParamRow::F32(
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
            4 => ParamRow::F32(
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
        self.range_cells_m
            .as_ref()
            .map(Vec::len)
            .or_else(|| self.params[param_index].number_cells)
    }

    fn finish_current_ray(&mut self) {
        let Some(ray) = self.current_ray.take() else {
            return;
        };
        let rows: Vec<(usize, ParamRow)> = self
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

    /// The sweep's `range` coordinate: CELV / CSFD cell centres plus the
    /// CFAC range delay (uniform when evenly spaced within 1% of a gate: DOW6
    /// CELV tables carry float32 accumulation of 0.3% of a gate),
    /// else the extended PARM geometry.
    fn range_coordinate(&self) -> Result<RangeCoord> {
        let delay = f64::from(self.cfac.range_delay_m);
        if let Some(cells) = &self.range_cells_m {
            let centers: Vec<f64> = cells.iter().map(|cell| f64::from(*cell) + delay).collect();
            let ngates = u32::try_from(centers.len()).map_err(|_| invalid(0, "gate overflow"))?;
            if centers.len() >= 2 {
                let first = centers[0];
                let spacing = (centers[centers.len() - 1] - first) / (centers.len() - 1) as f64;
                let uniform = spacing > 0.0
                    && spacing.is_finite()
                    && centers.iter().enumerate().all(|(gate, center)| {
                        (center - (first + gate as f64 * spacing)).abs() <= 1e-2 * spacing
                    });
                if uniform {
                    return Ok(RangeCoord::Uniform {
                        first_center_m: first,
                        spacing_m: spacing,
                        ngates,
                    });
                }
                return Ok(RangeCoord::Explicit {
                    centers_m: centers.iter().map(|c| *c as f32).collect(),
                });
            }
            return Ok(RangeCoord::Uniform {
                first_center_m: centers.first().copied().unwrap_or(0.0),
                spacing_m: 1.0,
                ngates,
            });
        }
        let param = self
            .params
            .iter()
            .find(|param| param.number_cells.unwrap_or(0) > 0)
            .ok_or_else(|| invalid(0, "DORADE sweep has no CELV/CSFD/PARM range metadata"))?;
        let ngates = u32::try_from(param.number_cells.unwrap_or(0))
            .map_err(|_| invalid(0, "gate overflow"))?;
        let spacing = f64::from(param.cell_spacing_m.unwrap_or(1000.0));
        Ok(RangeCoord::Uniform {
            first_center_m: f64::from(param.first_cell_m.unwrap_or(0.0)) + delay,
            spacing_m: if spacing > 0.0 && spacing.is_finite() {
                spacing
            } else {
                1.0
            },
            ngates,
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

    fn finish_into(
        mut self,
        builder: &mut DoradeVolumeBuilder,
        budget: &mut DecodeBudget,
    ) -> Result<()> {
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
        let volume = &mut builder.volume;
        if volume.sweeps.is_empty() {
            volume.attrs.instrument_name = self.instrument.clone();
            volume.location.latitude_deg = finite(self.site_latitude_deg()).map(f64::from);
            volume.location.longitude_deg = finite(self.site_longitude_deg()).map(f64::from);
            volume.location.altitude_m = finite(self.site_altitude_m()).map(f64::from);
            volume.provenance.source_format = SourceFormat::Dorade;
            volume.provenance.source_version = Some("DORADE".to_owned());
            volume.provenance.compression = Some(
                if self.compression == 1 {
                    "dorade-hrd-rle"
                } else {
                    "dorade-uncompressed"
                }
                .to_owned(),
            );
            volume.radar_parameters.frequency_hz = self
                .frequency_ghz
                .filter(|frequency| frequency.is_finite() && *frequency > 0.0)
                .map(|frequency| vec![f64::from(frequency) * 1e9])
                .unwrap_or_default();
        } else if volume.attrs.instrument_name != self.instrument {
            return Err(invalid(
                0,
                format!(
                    "DORADE sweep instrument '{}' does not match volume '{}'",
                    self.instrument, volume.attrs.instrument_name
                ),
            ));
        }
        let sweep_start = self.start_time;
        if let Some(start) = sweep_start {
            builder.rebase(floor_to_second(start));
        }
        let volume = &mut builder.volume;
        let reference = volume.time_reference;

        let range = self.range_coordinate()?;
        let ngates = range.ngates();
        let nyquist = self.nyquist_velocity_mps();
        let prt_s = self
            .prt1_ms
            .filter(|prt| *prt > 0.0)
            .map(|prt| prt / 1000.0);
        let fixed_angle = if self.fixed_angle_deg.is_finite() {
            self.fixed_angle_deg
        } else {
            let sum: f32 = self.rays.iter().map(|(ray, _)| ray.elevation_deg).sum();
            sum / self.rays.len() as f32
        };

        let mut sweep = Sweep::new(
            volume.sweeps.len() as u32,
            sweep_mode_from_radd(self.scan_mode),
            fixed_angle,
        );
        sweep.follow_mode = Some(FollowMode::None);
        sweep.elevation_number =
            u16::try_from(self.sweep_number.clamp(0, i32::from(u16::MAX))).ok();
        sweep.range = range;

        // Charge the finished fields before building them: every row is
        // padded to the widest row of its field, so the retained size follows
        // from the row counts and widths, not from the (possibly compressed)
        // input.
        let mut rows_per_param = vec![(0usize, 0usize); self.params.len()];
        for (_, rows) in &self.rays {
            for (param_index, row) in rows {
                if let Some((count, widest)) = rows_per_param.get_mut(*param_index) {
                    *count += 1;
                    *widest = (*widest).max(row.len());
                }
            }
        }
        let nrays = self.rays.len();
        for (param, (rows, widest)) in self.params.iter_mut().zip(&rows_per_param) {
            if *rows == 0 {
                continue;
            }
            let word_bytes = match param.binary_format {
                1 => 1,
                2 => 2,
                _ => 4,
            };
            let gates = ngates.max(*widest);
            budget
                .charge(nrays, gates.saturating_mul(word_bytes), "DORADE field")
                .map_err(DoradeError::LimitExceeded)?;
            let mut field = new_field(param, u32::try_from(ngates).unwrap_or(u32::MAX));
            field.reserve_rows(nrays);
            param.field = Some(field);
        }
        budget
            .charge(nrays, RAY_BYTES, "DORADE sweep rays")
            .map_err(DoradeError::LimitExceeded)?;

        sweep.reserve_rays(nrays);
        let rays = std::mem::take(&mut self.rays);
        for (ray, rows) in rays {
            let time_s = ray.time.map_or(f64::NAN, |time| {
                (time - reference).num_milliseconds() as f64 / 1000.0
            });
            let index = sweep.push_ray(
                time_s,
                normalize_azimuth(ray.azimuth_deg),
                ray.elevation_deg,
            );
            for (param_index, row) in rows {
                let param = &mut self.params[param_index];
                let Some(field) = param.field.as_mut() else {
                    continue;
                };
                let pushed = match row {
                    ParamRow::I8(row) => field.push_row_i8(index, &row),
                    ParamRow::I16(row) => field.push_row_i16(index, &row),
                    ParamRow::F32(row) => field.push_row_f32(index, &row),
                };
                pushed.map_err(|err| invalid(0, format!("field {}: {err}", param.name)))?;
            }
        }
        if let Some(nyquist) = nyquist {
            sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyquist; nrays]);
        }
        if let Some(prt_s) = prt_s {
            sweep.ray_vars.prt_s = Some(vec![prt_s; nrays]);
        }
        for param in &mut self.params {
            if let Some(field) = param.field.take()
                && field.nrays > 0
                && sweep.add_field(field).is_err()
            {
                // A second PARM with the same name: the first wins.
                self.skipped_field_blocks += 1;
            }
        }

        volume.provenance.decode.message_count += 1;
        volume.provenance.decode.skipped_message_count +=
            skipped_transition_rays + self.skipped_field_blocks;
        volume.sweeps.push(sweep);
        builder.sweep_starts.push(sweep_start);
        Ok(())
    }
}

/// An empty field for a PARM: `i8` / `i16` verbatim with the DORADE
/// `(raw - bias) / scale` transform and `bad_data` as the fill; physical
/// `f32` for 32-bit parameters.
fn new_field(param: &ParamState, ngates: u32) -> Field {
    let transform = LinearTransform::IcdScaleOffset {
        scale: param.scale,
        offset: param.bias,
    };
    let data = match param.binary_format {
        1 => FieldData::I8 {
            values: Vec::new(),
            coding: IntCoding {
                transform,
                fill_value: i8::try_from(param.bad_data).ok(),
                undetect: None,
                range_folded: None,
                valid_range: None,
            },
        },
        2 => FieldData::I16 {
            values: Vec::new(),
            coding: IntCoding {
                transform,
                fill_value: i16::try_from(param.bad_data).ok(),
                undetect: None,
                range_folded: None,
                valid_range: None,
            },
        },
        _ => FieldData::F32 {
            values: Vec::new(),
            coding: FloatCoding::default(),
        },
    };
    Field::new(
        FieldName::parse(&param.name),
        GateMapping::IDENTITY,
        ngates,
        data,
    )
}

/// The DORADE RADD `scan_mode` code as an FM301 `sweep_mode` (design note
/// section 10).
///
/// Code values per the DORADE format document (R. Oye and M. Case, "DORADE
/// Data Format", NCAR/ATD 1995; revised by W.-C. Lee, NCAR/EOL) and the
/// authoritative lrose-core `DoradeData.hh` enum: 0 = CAL (calibration),
/// 1 = PPI (sector), 2 = COP (coplane), 3 = RHI, 4 = VER (vertical
/// pointing), 5 = TAR (target/stationary), 6 = MAN (manual), 7 = IDL (idle),
/// 8 = SUR (360° surveillance), 9 = AIR (airborne), 10 = HOR (horizontal).
pub fn sweep_mode_from_radd(code: i16) -> SweepMode {
    match code {
        0 => SweepMode::Other("calibration".into()),
        1 => SweepMode::Sector,
        2 => SweepMode::Coplane,
        3 => SweepMode::Rhi,
        4 => SweepMode::VerticalPointing,
        5 => SweepMode::Pointing,
        6 => SweepMode::ManualPpi,
        7 => SweepMode::Idle,
        8 => SweepMode::AzimuthSurveillance,
        9 => SweepMode::Other("airborne".into()),
        10 => SweepMode::Other("horizontal".into()),
        other => SweepMode::Other(format!("dorade_scan_mode_{other}").into()),
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

pub(crate) fn invalid(offset: usize, reason: impl Into<String>) -> DoradeError {
    DoradeError::InvalidMessage {
        offset,
        reason: reason.into(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use recast_radar_core::model::FieldName;

    pub(crate) fn put_i16(block: &mut [u8], offset: usize, value: i16, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        block[offset..offset + 2].copy_from_slice(&bytes);
    }

    pub(crate) fn put_i32(block: &mut [u8], offset: usize, value: i32, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        block[offset..offset + 4].copy_from_slice(&bytes);
    }

    pub(crate) fn put_f32(block: &mut [u8], offset: usize, value: f32, endian: Endian) {
        put_i32(block, offset, value.to_bits() as i32, endian);
    }

    pub(crate) fn base_block(id: &[u8; 4], len: usize, endian: Endian) -> Vec<u8> {
        let mut block = vec![0u8; len];
        block[..4].copy_from_slice(id);
        put_i32(&mut block, 4, len as i32, endian);
        block
    }

    pub(crate) struct Synth {
        pub endian: Endian,
        pub compressed: bool,
    }

    impl Synth {
        pub(crate) fn build(&self, rays: &[(f32, f32, i32, &[i16])]) -> Vec<u8> {
            let endian = self.endian;
            let mut bytes = Vec::new();

            let mut sswb = base_block(b"SSWB", 200, endian);
            put_i32(&mut sswb, 12, 1_779_404_114, endian); // 2026-05-21T22:55:14Z
            put_i32(&mut sswb, 16, 1_779_404_126, endian);
            bytes.extend(sswb);

            let mut vold = base_block(b"VOLD", 72, endian);
            put_i16(&mut vold, 10, 42, endian); // volume number
            put_i16(&mut vold, 36, 2026, endian);
            put_i16(&mut vold, 38, 5, endian);
            put_i16(&mut vold, 40, 21, endian);
            put_i16(&mut vold, 42, 22, endian);
            put_i16(&mut vold, 44, 55, endian);
            put_i16(&mut vold, 46, 14, endian);
            bytes.extend(vold);

            let mut radd = base_block(b"RADD", 144, endian);
            radd[8..12].copy_from_slice(b"TST1");
            put_i16(&mut radd, 50, 8, endian); // scan mode SUR
            put_i16(&mut radd, 68, if self.compressed { 1 } else { 0 }, endian);
            put_f32(&mut radd, 80, -103.2927, endian); // lon
            put_f32(&mut radd, 84, 39.74, endian); // lat
            put_f32(&mut radd, 88, 1.519, endian); // alt km
            put_f32(&mut radd, 92, 68.76, endian); // eff unamb vel
            put_i16(&mut radd, 102, 2, endian); // num ipps
            put_f32(&mut radd, 104, 5.45, endian); // freq GHz
            put_f32(&mut radd, 124, 0.4, endian); // prt1 ms
            put_f32(&mut radd, 128, 0.6, endian); // prt2 ms
            bytes.extend(radd);

            let mut parm = base_block(b"PARM", 216, endian);
            parm[8..11].copy_from_slice(b"DBZ");
            put_i16(&mut parm, 78, 2, endian); // 16-bit
            put_f32(&mut parm, 92, 100.0, endian); // scale
            put_f32(&mut parm, 96, 0.0, endian); // bias
            put_i32(&mut parm, 100, -32768, endian); // bad
            put_i32(&mut parm, 200, 4, endian); // cells
            put_f32(&mut parm, 204, 50.0, endian);
            put_f32(&mut parm, 208, 100.0, endian);
            bytes.extend(parm);

            let mut csfd = base_block(b"CSFD", 64, endian);
            put_i32(&mut csfd, 8, 1, endian); // one segment
            put_f32(&mut csfd, 12, 50.0, endian); // first cell m
            put_f32(&mut csfd, 16, 100.0, endian); // spacing m
            put_i16(&mut csfd, 48, 4, endian); // cells
            bytes.extend(csfd);

            let mut swib = base_block(b"SWIB", 40, endian);
            put_i32(&mut swib, 16, 6, endian); // sweep number
            put_i32(&mut swib, 20, rays.len() as i32, endian);
            put_f32(&mut swib, 32, 1.0, endian); // fixed angle
            bytes.extend(swib);

            for (azimuth, elevation, status, gates) in rays {
                let mut ryib = base_block(b"RYIB", 44, self.endian);
                put_i32(&mut ryib, 8, 6, endian); // sweep number
                put_i32(&mut ryib, 12, 141, endian); // julian day (May 21)
                put_i16(&mut ryib, 16, 22, endian);
                put_i16(&mut ryib, 18, 55, endian);
                put_i16(&mut ryib, 20, 15, endian);
                put_i16(&mut ryib, 22, 250, endian);
                put_f32(&mut ryib, 24, *azimuth, endian);
                put_f32(&mut ryib, 28, *elevation, endian);
                put_i32(&mut ryib, 40, *status, endian);
                bytes.extend(ryib);

                let words: Vec<i16> = if self.compressed {
                    let mut encoded = Vec::new();
                    encoded.push((0x8000u16 | gates.len() as u16) as i16);
                    encoded.extend_from_slice(gates);
                    encoded.push(1); // end sentinel
                    encoded
                } else {
                    gates.to_vec()
                };
                let mut rdat = base_block(b"RDAT", 16 + words.len() * 2, endian);
                rdat[8..11].copy_from_slice(b"DBZ");
                for (index, word) in words.iter().enumerate() {
                    put_i16(&mut rdat, 16 + index * 2, *word, endian);
                }
                bytes.extend(rdat);
            }
            bytes
        }
    }

    pub(crate) fn synth_rays() -> Vec<(f32, f32, i32, &'static [i16])> {
        vec![
            (45.0, 1.0, 0, &[1000, 2000, -32768, 500][..]),
            (46.0, 1.0, 0, &[1500, -32768, 700, 800][..]),
            (47.0, 9.5, 1, &[1, 2, 3, 4][..]), // transition ray
        ]
    }

    pub(crate) fn find_block(bytes: &[u8], id: &[u8; 4]) -> usize {
        bytes
            .windows(4)
            .position(|window| window == id)
            .expect("block present")
    }

    #[test]
    fn decodes_big_endian_synthetic_sweep() {
        let bytes = Synth {
            endian: Endian::Big,
            compressed: false,
        }
        .build(&synth_rays());
        assert!(looks_like_dorade_bytes(&bytes));

        let volume = read_dorade_sweep_volume(&bytes).expect("decode");
        assert_eq!(volume.attrs.instrument_name, "TST1");
        assert_eq!(volume.provenance.source_format, SourceFormat::Dorade);
        assert_eq!(volume.location.latitude_deg, Some(f64::from(39.74f32)));
        assert_eq!(volume.location.longitude_deg, Some(f64::from(-103.2927f32)));
        assert!((volume.location.altitude_m.unwrap() - 1519.0).abs() < 0.5);
        assert_eq!(
            volume.radar_parameters.frequency_hz,
            vec![f64::from(5.45f32) * 1e9]
        );
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
        );
        assert_eq!(volume.sweeps.len(), 1);

        let sweep = &volume.sweeps[0];
        // RADD scan mode 8 (SUR) is azimuth surveillance.
        assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
        assert_eq!(sweep.fixed_angle_deg, 1.0);
        assert_eq!(sweep.elevation_number, Some(6));
        // Transition ray dropped.
        assert_eq!(sweep.nrays(), 2);
        assert_eq!(sweep.rays.azimuth_deg[0], 45.0);
        assert_eq!(
            sweep.ray_vars.nyquist_velocity_mps.as_deref(),
            Some(&[68.76, 68.76][..])
        );
        assert_eq!(
            sweep.range,
            RangeCoord::Uniform {
                first_center_m: 50.0,
                spacing_m: 100.0,
                ngates: 4
            }
        );
        // RYIB time 22:55:15.250 - SSWB start 22:55:14 = 1.25 s.
        assert_eq!(sweep.rays.time_s[0], 1.25);

        // DBZ stays int16 with the DORADE transform; bad data is the fill.
        let field = sweep.field(&FieldName::Dbz).expect("DBZ");
        let FieldData::I16 { values, coding } = &field.data else {
            panic!("DBZ is int16");
        };
        assert_eq!(values, &[1000, 2000, -32768, 500, 1500, -32768, 700, 800]);
        assert_eq!(coding.fill_value, Some(-32768));
        assert_eq!(
            coding.transform,
            LinearTransform::IcdScaleOffset {
                scale: 100.0,
                offset: 0.0
            }
        );
        assert_eq!(field.value(0, 0), Some(10.0));
        assert_eq!(field.value(0, 1), Some(20.0));
        assert_eq!(field.value(0, 2), None); // bad gate
        assert_eq!(field.value(1, 2), Some(7.0));
    }

    #[test]
    fn decodes_little_endian_rle_sweep() {
        let bytes = Synth {
            endian: Endian::Little,
            compressed: true,
        }
        .build(&synth_rays());
        assert!(looks_like_dorade_bytes(&bytes));

        let volume = read_dorade_sweep_volume(&bytes).expect("decode");
        let field = volume.sweeps[0].field(&FieldName::Dbz).expect("DBZ");
        assert_eq!(field.value(0, 0), Some(10.0));
        assert_eq!(field.value(0, 3), Some(5.0));
        assert_eq!(field.value(1, 1), None);
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
        let mut block = base_block(b"PARM", 216, Endian::Big);
        block[8..11].copy_from_slice(b"DBZ");
        put_i16(&mut block, 78, 2, Endian::Big);
        put_f32(&mut block, 92, 100.0, Endian::Big);
        put_i32(&mut block, 200, i32::MAX, Endian::Big);

        let mut sweep = SweepParse::new(Endian::Big);
        let err = sweep
            .parse_parm(&block, 0)
            .expect_err("absurd gate count must be rejected");
        assert!(err.to_string().contains("gates per radial"));
    }

    #[test]
    fn peek_reads_grouping_metadata_without_rays() {
        let bytes = Synth {
            endian: Endian::Big,
            compressed: false,
        }
        .build(&synth_rays());
        let header = peek_dorade_sweep(&bytes).expect("peek");
        assert_eq!(header.instrument, "TST1");
        assert_eq!(header.volume_number, 42);
        assert_eq!(header.sweep_number, 6);
        assert_eq!(header.fixed_angle_deg, 1.0);
        assert_eq!(
            header.start_time,
            Some(Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap())
        );
        assert!((header.latitude_deg - 39.74).abs() < 1e-5);
    }

    #[test]
    fn multi_sweep_volume_keeps_input_order_and_rebases_times() {
        let synth = Synth {
            endian: Endian::Big,
            compressed: false,
        };
        let high = {
            let mut rays = synth_rays();
            for ray in &mut rays {
                ray.1 = 2.4;
            }
            let mut bytes = synth.build(&rays);
            let swib_pos = find_block(&bytes, b"SWIB");
            put_f32(&mut bytes[swib_pos..], 32, 2.4, Endian::Big);
            // Starts 10 s later than the low sweep.
            let sswb_pos = find_block(&bytes, b"SSWB");
            put_i32(&mut bytes[sswb_pos..], 12, 1_779_404_124, Endian::Big);
            bytes
        };
        let low = synth.build(&synth_rays());
        let volume = read_dorade_volume_from_slices(&[high, low]).expect("decode");
        assert_eq!(volume.sweeps.len(), 2);
        // Input (scan) order, numbered in that order; the earliest sweep start
        // is the time reference.
        assert_eq!(volume.sweeps[0].fixed_angle_deg, 2.4);
        assert_eq!(volume.sweeps[1].fixed_angle_deg, 1.0);
        assert_eq!(volume.sweeps[1].sweep_number, 1);
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
        );
        assert_eq!(volume.sweeps[0].rays.time_s[0], 1.25);
        assert_eq!(volume.sweeps[1].rays.time_s[0], 1.25);
        assert_eq!(volume.provenance.decode.decoded_ray_count, 4);
    }

    #[test]
    fn mismatched_instruments_are_rejected() {
        let synth = Synth {
            endian: Endian::Big,
            compressed: false,
        };
        let first = synth.build(&synth_rays());
        let mut second = synth.build(&synth_rays());
        let radd_pos = find_block(&second, b"RADD");
        second[radd_pos + 8..radd_pos + 12].copy_from_slice(b"TST2");
        let err = read_dorade_volume_from_slices(&[first, second]).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn rhi_scan_mode_is_detected_from_radd() {
        // An RHI sweep: RADD scan mode 3, fixed azimuth, elevation-swept rays.
        let rays: Vec<(f32, f32, i32, &[i16])> = vec![
            (271.0, 0.5, 0, &[1000, 2000, 1500, 500][..]),
            (271.0, 1.5, 0, &[1500, 1200, 700, 800][..]),
            (271.0, 2.5, 0, &[900, 1100, 600, 400][..]),
        ];
        let mut bytes = Synth {
            endian: Endian::Big,
            compressed: false,
        }
        .build(&rays);
        let radd_pos = find_block(&bytes, b"RADD");
        put_i16(&mut bytes[radd_pos..], 50, 3, Endian::Big); // RHI per DORADE doc
        let volume = read_dorade_sweep_volume(&bytes).expect("decode");
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.sweep_mode, SweepMode::Rhi);
        // Per-ray elevations carry the sweep; azimuth is fixed.
        assert_eq!(sweep.nrays(), 3);
        assert!(sweep.rays.azimuth_deg.iter().all(|az| *az == 271.0));
        assert_eq!(sweep.rays.elevation_deg[0], 0.5);
        assert_eq!(sweep.rays.elevation_deg[2], 2.5);
    }

    #[test]
    fn radd_scan_mode_codes_map_to_sweep_modes() {
        // Codes per Oye & Case 1995 / lrose DoradeData.hh.
        assert_eq!(sweep_mode_from_radd(1), SweepMode::Sector);
        assert_eq!(sweep_mode_from_radd(8), SweepMode::AzimuthSurveillance);
        assert_eq!(sweep_mode_from_radd(3), SweepMode::Rhi);
        assert_eq!(sweep_mode_from_radd(4), SweepMode::VerticalPointing);
        assert_eq!(sweep_mode_from_radd(2), SweepMode::Coplane);
        assert_eq!(sweep_mode_from_radd(6), SweepMode::ManualPpi);
        assert_eq!(sweep_mode_from_radd(0).as_str(), "calibration");
        assert_eq!(sweep_mode_from_radd(99).as_str(), "dorade_scan_mode_99");
    }

    #[test]
    fn csfd_segments_become_explicit_centres_when_spacings_differ() {
        let mut block = base_block(b"CSFD", 64, Endian::Big);
        put_i32(&mut block, 8, 2, Endian::Big); // two segments
        put_f32(&mut block, 12, 100.0, Endian::Big); // first cell
        put_f32(&mut block, 16, 100.0, Endian::Big); // segment 0 spacing
        put_f32(&mut block, 20, 250.0, Endian::Big); // segment 1 spacing
        put_i16(&mut block, 48, 3, Endian::Big);
        put_i16(&mut block, 50, 2, Endian::Big);
        let mut sweep = SweepParse::new(Endian::Big);
        sweep.parse_csfd(&block, 0).unwrap();
        assert_eq!(
            sweep.range_cells_m,
            Some(vec![100.0, 200.0, 300.0, 400.0, 650.0])
        );
        assert_eq!(
            sweep.range_coordinate().unwrap(),
            RangeCoord::Explicit {
                centers_m: vec![100.0, 200.0, 300.0, 400.0, 650.0]
            }
        );
    }
}
