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
//! MAN -> `manual_ppi`, AIR -> `elevation_surveillance`, others verbatim);
//! for RHI sweeps the fixed angle is the AZIMUTH. An AIR scan (an airborne
//! tail radar) also sets the volume's `primary_axis` to `axis_y_prime`. Sweeps of a multi-file volume stay in input order (scan
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
//! - **Transition rays flagged, not dropped**: every ray is kept in file
//!   order, and RYIB `ray_status` 1 (antenna moving between fixed angles)
//!   sets FM301 `antenna_transition`, as LROSE Radx does (RadxPrint, LROSE
//!   release 20250811, reads the COW2 fixture as 24 rays, the first 3 with
//!   `antennaTransition: 1`), and as the CfRadial reader keeps a CfRadial
//!   file's flagged rays. A real DOW7 Goshen sweepfile is 42% transition
//!   rays spanning 0.5°-11.4° inside a "0.5°" sweep: a consumer that should
//!   not draw them tests the flag.
//! - **RADD layout**: the standard 1995 layout (lat/lon/alt at 80/84/88,
//!   `data_compress` at 68) is parsed directly; the reference parsed a
//!   shifted legacy layout first and patched it afterwards.
//! - **CFAC corrections**: the azimuth, elevation, range delay, latitude,
//!   longitude and radar altitude corrections are applied when present (all
//!   zero in the ground-based corpus; the N42RF fore sweep has a -37.895 m
//!   range delay, so its first gate is at -37.9 m), and are therefore left
//!   out of the model's `georeferencing_correction`, so a consumer does not
//!   apply them a second time. The other ten apply to the platform
//!   georeference, which the decoder carries as stored (the ASIB values of
//!   the platform track): they are the `georeferencing_correction` items
//!   `pressure_altitude_correction` (the CFAC kilometres in metres),
//!   `eastward_ground_speed_correction`, `northward_ground_speed_correction`,
//!   `vertical_velocity_correction`, `heading_correction`,
//!   `roll_correction`, `pitch_correction`, `drift_correction`,
//!   `rotation_correction` and `tilt_correction`, as LROSE Radx writes them
//!   (RadxConvert writes the pressure altitude correction's kilometre value
//!   under a metre unit). The group holds them when every sweepfile of the
//!   volume has the same ones; each sweep keeps its own CFAC block as the
//!   `dorade_cfac_*` sweep attributes either way. LROSE Radx leaves every
//!   coordinate as stored and applies all sixteen only when asked to apply
//!   the georeference.
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
//! Descriptor and ray block values without a coordinate reach the model
//! (`descriptors.rs`): every SSWB, VOLD, RADD, CFAC, CSFD, CELV,
//! SWIB and COMM field as a `dorade_*` sweep attribute, every PARM field as
//! a `dorade_parm_*` attribute of its field, the CELV distances as the sweep
//! variable `dorade_celv_distance`, and per ray the RYIB `ray_status` (FM301
//! `antenna_transition`, and verbatim as `dorade_ryib_ray_status`),
//! `true_scan_rate` (FM301 `scan_rate`), `sweep_num`, and the ASIB platform
//! position and attitude (the sweep's platform track) with its velocities,
//! winds and change rates (the CfRadial georeference variables). The RADD
//! `radar_type` of an airborne or shipborne radar sets `platform_type` and
//! `platform_is_mobile` (true, as LROSE Radx writes it). A
//! SEDS block (Solo II edit summary) is the sweep attribute
//! `dorade_seds_text` and the volume's `history`. The ASIB mapping is
//! checked against LROSE RadxPrint on two NOAA P-3 N42RF tail radar sweeps
//! (Hurricane Michael, 2018), whose ASIB blocks hold every motion and
//! attitude value. No real file stores a RYIB `true_scan_rate` (every
//! DORADE fixture and both full N42RF sweepfiles hold -9999 or -32768), so
//! its mapping to FM301 `scan_rate` is untested on real data; only the
//! verbatim `dorade_ryib_true_scan_rate` column is checked.
//!
//! The typed slots hold physical values, so a missing-value sentinel (-999,
//! -9999 or -32768: any value at or below -999, or not finite) is NaN there,
//! and a RYIB peak power that is not positive is NaN in the monitoring
//! transmit power. Where a column holds such a value, the column is also
//! kept verbatim as a per-ray variable, so the stored value is not lost:
//! `dorade_ryib_true_scan_rate`, `dorade_ryib_peak_power_kw` and
//! `dorade_asib_<field>` (the ASIB member names, with their units).
//!
//! Known limitation (documented, not silent): the site position is the RADD
//! position with the CFAC corrections; the per-ray ASIB position is carried
//! as the platform track but does not move the gates, so an airborne radar's
//! gates are placed at the RADD site. An airborne ray's `azimuth` and
//! `elevation` are its RYIB values as stored (for the N42RF sweeps, the
//! rotation and tilt relative to the aircraft), not earth-relative angles
//! computed from the attitude; LROSE Radx reads them the same way unless
//! asked to apply the georeference.

use std::path::Path;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use recast_radar_core::bounded_read::{
    DecodeBudget, MAX_GATES_PER_RADIAL, check_gate_count, check_sweep_count,
};
use recast_radar_core::model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldData, FieldName, FloatCoding, FollowMode,
    GateMapping, GeoreferencingCorrection, IntCoding, LinearTransform, Monitoring, PlatformTrack,
    PlatformType, PrimaryAxis, RadarCalibration, RangeCoord, Scalar, SourceFormat, Sweep,
    SweepMode, Volume, floor_to_second,
};

use crate::descriptors::{self, Attrs};
use crate::{DoradeError, Result};

const BLOCK_HEADER_LEN: usize = 8;
/// RADD `scan_mode` of an airborne tail radar (AIR).
const SCAN_MODE_AIR: i16 = 9;
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
    pub(crate) fn i16(self, bytes: &[u8], offset: usize) -> i16 {
        let raw = [bytes[offset], bytes[offset + 1]];
        match self {
            Self::Little => i16::from_le_bytes(raw),
            Self::Big => i16::from_be_bytes(raw),
        }
    }

    pub(crate) fn i32(self, bytes: &[u8], offset: usize) -> i32 {
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

    pub(crate) fn f32(self, bytes: &[u8], offset: usize) -> f32 {
        f32::from_bits(self.i32(bytes, offset) as u32)
    }

    pub(crate) fn f64(self, bytes: &[u8], offset: usize) -> f64 {
        let mut raw = [0; 8];
        raw.copy_from_slice(&bytes[offset..offset + 8]);
        match self {
            Self::Little => f64::from_le_bytes(raw),
            Self::Big => f64::from_be_bytes(raw),
        }
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
    /// The unapplied CFAC corrections of each appended sweep.
    unapplied_cfac: Vec<Option<UnappliedCfac>>,
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
            unapplied_cfac: Vec::new(),
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
        check_sweep_count(self.volume.sweeps.len() + 1, "DORADE volume")
            .map_err(DoradeError::LimitExceeded)?;
        // The volume budget covers every sweep appended so far, then the
        // rays of this sweepfile as they are read.
        let existing_rays: usize = self.volume.sweeps.iter().map(Sweep::nrays).sum();
        let budget = &mut parse.budget;
        budget
            .charge(
                volume_field_capacity_bytes(&self.volume),
                1,
                "DORADE volume fields",
            )
            .and_then(|()| budget.charge(existing_rays, RAY_BYTES, "DORADE volume rays"))
            .map_err(DoradeError::LimitExceeded)?;
        parse.run(bytes, false)?;
        parse.finish_into(self)
    }

    /// Seal the volume: ray-time coverage and invariants, the FM301
    /// `history` from the sweepfiles' SEDS edit summaries, and the CFAC
    /// corrections the decoder does not apply as the
    /// `georeferencing_correction`.
    pub fn finish(self) -> Result<Volume> {
        let Self {
            mut volume,
            unapplied_cfac,
            ..
        } = self;
        volume.attrs.history = seds_history(&volume.sweeps);
        volume.georeferencing_correction = georeferencing_correction(&unapplied_cfac);
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

/// The CfRadial `georeferencing_correction` of a volume: the CFAC corrections
/// the decoder does not apply (the platform georeference ones), when every
/// sweepfile has the same; `None` when a sweepfile has no full CFAC block or
/// they differ (each sweep keeps its own `dorade_cfac_*` attributes). The
/// pressure altitude correction is in metres (CFAC: km). A missing-value
/// sentinel leaves its item out.
fn georeferencing_correction(
    unapplied: &[Option<UnappliedCfac>],
) -> Option<Box<GeoreferencingCorrection>> {
    let first = (*unapplied.first()?)?;
    let bits = |values: &UnappliedCfac| values.map(f32::to_bits);
    if !unapplied
        .iter()
        .all(|values| values.as_ref().map(bits) == Some(bits(&first)))
    {
        return None;
    }
    let item = |index: usize| Some(present(first[index])).filter(|value| value.is_finite());
    Some(Box::new(GeoreferencingCorrection {
        pressure_altitude_correction: item(0).map(|km| (f64::from(km) * KM_TO_M) as f32),
        eastward_ground_speed_correction: item(1),
        northward_ground_speed_correction: item(2),
        vertical_velocity_correction: item(3),
        heading_correction: item(4),
        roll_correction: item(5),
        pitch_correction: item(6),
        drift_correction: item(7),
        rotation_correction: item(8),
        tilt_correction: item(9),
        ..GeoreferencingCorrection::default()
    }))
}

/// The FM301 `history` of a DORADE volume: the SEDS (Solo II edit summary)
/// texts of its sweepfiles in sweep order, each distinct text once, joined
/// by a newline, without trailing whitespace (LROSE Radx's `history` of one
/// sweepfile); `None` when no sweepfile has one. Each sweep keeps its own
/// text verbatim as `dorade_seds_text`.
fn seds_history(sweeps: &[Sweep]) -> Option<String> {
    let mut texts: Vec<&str> = Vec::new();
    for sweep in sweeps {
        for (name, value) in &sweep.other {
            if let AttrValue::Text(text) = value
                && name.starts_with("dorade_seds_text")
            {
                let text = text.trim_end();
                if !texts.contains(&text) {
                    texts.push(text);
                }
            }
        }
    }
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// Bytes a ray occupies in the model, at most: the three coordinates
/// (charged as `f64`); seven 4-byte ray variables (Nyquist velocity, PRT,
/// unambiguous range, pulse width, sample count, calibration index,
/// transmit power); `antenna_transition`; the RYIB status, sweep number
/// and scan rate; the platform track (four `f64` and six `f32` columns);
/// eight georeference `f32` columns; and the verbatim sentinel columns
/// (eighteen ASIB values, the true scan rate and the peak power, `f32`).
const RAY_BYTES: usize = 3 * size_of::<f64>()
    + 7 * size_of::<f32>()
    + size_of::<u8>()
    + 3 * size_of::<i32>()
    + 4 * size_of::<f64>()
    + 6 * size_of::<f32>()
    + 8 * size_of::<f32>()
    + (ASIB_VALUES + 2) * size_of::<f32>();

/// Bytes a ray occupies while the sweepfile is read: its RYIB and ASIB
/// values and the list of its field rows (the rows themselves are bounded
/// by `MAX_DORADE_CELLS_PER_SWEEP`).
const PARSE_RAY_BYTES: usize = size_of::<(PendingRay, Vec<(usize, ParamRow)>)>();

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
                FieldData::I32 { values, .. } => values.capacity().saturating_mul(4),
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
    /// PARM `param_description` and `param_units`, verbatim (empty when
    /// blank); the field's `long_name` and `units`.
    description: String,
    units: String,
    /// PARM `pulse_width` (m), `num_samples` and `recvr_bandwidth` (MHz).
    pulse_width_m: i16,
    num_samples: i16,
    bandwidth_mhz: f32,
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
    /// Every other PARM field ([`descriptors::parm`]), for the field's
    /// attributes.
    attrs: Attrs,
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

    /// Drop words past the described cells (uncompressed RDAT payloads are
    /// padded to a 4-byte boundary: NOXP writes 1002 words for 1001 cells).
    fn truncate(&mut self, gates: usize) {
        match self {
            Self::I8(row) => row.truncate(gates),
            Self::I16(row) => row.truncate(gates),
            Self::F32(row) => row.truncate(gates),
        }
    }
}

/// RADD radar constants (DORADE 1995 layout), `None` where the file writes
/// a missing value.
#[derive(Clone, Copy, Debug, Default)]
struct RaddConstants {
    /// `radar_const` (dB).
    radar_constant_db: Option<f32>,
    /// `peak_power` (kW).
    peak_power_kw: Option<f32>,
    /// `noise_power` (dBm).
    noise_power_dbm: Option<f32>,
    /// `receiver_gain` (dB).
    receiver_gain_db: Option<f32>,
    /// `antenna_gain` (dB).
    antenna_gain_db: Option<f32>,
    /// `system_gain` (dB).
    system_gain_db: Option<f32>,
    /// `horz_beam_width` and `vert_beam_width` (deg).
    beam_width_h_deg: Option<f32>,
    beam_width_v_deg: Option<f32>,
    /// `req_rotat_vel` (deg/s).
    rotation_deg_per_s: Option<f32>,
    /// `eff_unamb_range` (km).
    unambiguous_range_km: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cfac {
    azimuth_deg: f32,
    elevation_deg: f32,
    range_delay_m: f32,
    longitude_deg: f32,
    latitude_deg: f32,
    radar_altitude_km: f32,
    /// The corrections the decoder does not apply, when the block holds all
    /// sixteen (see [`UnappliedCfac`]).
    unapplied: Option<UnappliedCfac>,
}

/// The CFAC corrections of the platform georeference, which the decoder
/// carries as stored and does not correct, in block order: pressure
/// altitude (km, offset 28), east-west and north-south ground speed and
/// vertical velocity (m/s, 36-47), heading, roll, pitch, drift, rotation
/// angle and tilt (degrees, 48-71).
type UnappliedCfac = [f32; 10];

/// CFAC offsets of the [`UnappliedCfac`] values.
const UNAPPLIED_CFAC_OFFSETS: [usize; 10] = [28, 36, 40, 44, 48, 52, 56, 60, 64, 68];

/// The CFAC block length that holds all sixteen corrections.
const CFAC_LEN: usize = 72;

#[derive(Clone, Copy, Debug)]
struct PendingRay {
    azimuth_deg: f32,
    elevation_deg: f32,
    /// RYIB `ray_status`: 0 = normal, 1 = in transition, 2 = bad.
    status: i32,
    time: Option<DateTime<Utc>>,
    /// RYIB `peak_power` (kW), verbatim (-999 in DOW6 files).
    peak_power_kw: f32,
    /// RYIB `sweep_num`.
    sweep_num: i32,
    /// RYIB `true_scan_rate` (deg/s), verbatim.
    true_scan_rate: f32,
    /// The ASIB (platform information block) that follows the RYIB, its 18
    /// values verbatim: longitude, latitude (deg), altitude MSL and AGL
    /// (km), east-west, north-south and vertical velocity (m/s), heading,
    /// roll, pitch, drift, rotation angle and tilt (deg), east-west,
    /// north-south and vertical wind (m/s), heading and pitch change
    /// (deg/s).
    asib: Option<[f32; ASIB_VALUES]>,
}

/// Values of an ASIB block.
const ASIB_VALUES: usize = 18;

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
    radd: RaddConstants,
    /// VOLD `proj_name`, `flight_num` and `gen_facility`, verbatim.
    vold_text: Vec<(&'static str, String)>,
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
    /// Every ray in file order, antenna-transition rays included.
    rays: Vec<(PendingRay, Vec<(usize, ParamRow)>)>,
    /// Every field of the SSWB, VOLD, RADD, CFAC, CSFD, CELV, SWIB, COMM and
    /// SEDS blocks, in block order ([`descriptors`]), for the sweep's
    /// attributes.
    descriptor_attrs: Attrs,
    /// COMM blocks read so far (the second and later are numbered).
    comm_blocks: usize,
    /// SEDS blocks read so far (the second and later are numbered).
    seds_blocks: usize,
    /// Memory budget of the volume this sweepfile joins.
    budget: DecodeBudget,
    /// CELV cell distances (m) as the file stores them.
    celv_distances_m: Option<Vec<f32>>,
    /// RADD `radar_type` (0 ground, 1 airborne fore, 2 aft, 3 tail, 4 lower
    /// fuselage, 5 shipborne).
    radar_type: Option<i16>,
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
            radd: RaddConstants::default(),
            vold_text: Vec::new(),
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
            descriptor_attrs: Vec::new(),
            comm_blocks: 0,
            seds_blocks: 0,
            budget: DecodeBudget::volume(),
            celv_distances_m: None,
            radar_type: None,
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
                b"COMM" => {
                    let attrs = descriptors::comm(self.endian, block, self.comm_blocks);
                    self.comm_blocks += 1;
                    self.descriptor_attrs.extend(attrs);
                }
                b"SEDS" => {
                    let attrs = descriptors::seds(block, self.seds_blocks);
                    self.seds_blocks += 1;
                    self.descriptor_attrs.extend(attrs);
                }
                b"RYIB" => {
                    if stop_at_first_ray {
                        return Ok(());
                    }
                    self.finish_current_ray()?;
                    self.current_ray = Some(self.parse_ryib(block, pos)?);
                }
                b"ASIB" => self.parse_asib(block),
                b"RDAT" => self.parse_rdat(block, pos)?,
                // RKTB (the untrimmed NOXP and N42RF sweepfiles end with one
                // after the NULL block) is the writer's ray index: an angle
                // lookup table and each ray's rotation angle, file offset and
                // size. It is structural and skipped, like the Level II block
                // pointers (descriptors.rs). XSTF, FRIB, FRAD, WAVE, ...: no
                // real sample holds one, so they are skipped.
                _ => {}
            }
            pos = end;
        }
        self.finish_current_ray()
    }

    fn parse_vold(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 48, offset, "VOLD")?;
        self.descriptor_attrs
            .extend(descriptors::vold(self.endian, block));
        self.volume_number = i32::from(self.endian.i16(block, 10));
        // Standard layout: proj_name[20] at 16, then year at 36 (the
        // reference read offset 32, which lands inside proj_name).
        let year = i32::from(self.endian.i16(block, 36));
        let month = self.endian.i16(block, 38);
        let day = self.endian.i16(block, 40);
        let hour = self.endian.i16(block, 42);
        let minute = self.endian.i16(block, 44);
        let second = self.endian.i16(block, 46);
        // proj_name[20] at 16; flight_num[8] at 48 and gen_facility[8] at 56
        // follow the date when the block is long enough.
        self.vold_text = [
            ("proj_name", 16..36),
            ("flight_num", 48..56),
            ("gen_facility", 56..64),
        ]
        .into_iter()
        .filter(|(_, bytes)| bytes.end <= block.len())
        .map(|(name, bytes)| (name, text(&block[bytes])))
        .filter(|(_, value)| !value.is_empty())
        .collect();
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
        self.descriptor_attrs
            .extend(descriptors::radd(self.endian, block));
        self.radar_type = Some(self.endian.i16(block, 48));
        self.instrument = text(&block[8..16]);
        let value = |offset| valid_dorade_f32(self.endian.f32(block, offset));
        self.radd = RaddConstants {
            radar_constant_db: value(16),
            peak_power_kw: value(20).filter(|power| *power > 0.0),
            noise_power_dbm: value(24),
            receiver_gain_db: value(28),
            antenna_gain_db: value(32),
            system_gain_db: value(36),
            beam_width_h_deg: value(40).filter(|width| *width > 0.0),
            beam_width_v_deg: value(44).filter(|width| *width > 0.0),
            rotation_deg_per_s: value(52),
            unambiguous_range_km: value(96).filter(|range| *range > 0.0),
        };
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
        self.descriptor_attrs
            .extend(descriptors::cfac(self.endian, block));
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
            unapplied: (block.len() >= CFAC_LEN)
                .then(|| UNAPPLIED_CFAC_OFFSETS.map(|offset| self.endian.f32(block, offset))),
        };
    }

    fn parse_parm(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 104, offset, "PARM")?;
        let name = text(&block[8..16]);
        let description = text(&block[16..56]);
        let units = text(&block[56..64]);
        let bandwidth_mhz = self.endian.f32(block, 68);
        let pulse_width_m = self.endian.i16(block, 72);
        let num_samples = self.endian.i16(block, 76);
        let binary_format = self.endian.i16(block, 78);
        let scale = self.endian.f32(block, 92);
        let bias = self.endian.f32(block, 96);
        let bad_data = self.endian.i32(block, 100);
        let attrs = descriptors::parm(self.endian, block);
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
            description,
            units,
            pulse_width_m,
            num_samples,
            bandwidth_mhz,
            scale: if scale.abs() > 1.0e-6 { scale } else { 1.0 },
            bias,
            bad_data,
            binary_format,
            number_cells,
            first_cell_m,
            cell_spacing_m,
            field: None,
            pending_row: None,
            attrs,
        });
        Ok(())
    }

    fn parse_celv(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 16, offset, "CELV")?;
        self.descriptor_attrs
            .extend(descriptors::celv(self.endian, block));
        let cells = self.endian.i32(block, 8).max(0) as usize;
        let available = (block.len() - 12) / 4;
        let count = cells.min(available);
        if count == 0 {
            return Ok(());
        }
        validate_gate_count(count, offset, "CELV")?;
        // CELV lists every cell range (uniform in the observed corpus).
        let distances: Vec<f32> = (0..count)
            .map(|cell| self.endian.f32(block, 12 + cell * 4))
            .collect();
        self.celv_distances_m = Some(distances.clone());
        self.range_cells_m = Some(distances);
        Ok(())
    }

    fn parse_csfd(&mut self, block: &[u8], offset: usize) -> Result<()> {
        // CSFD: num_segments (i32 at 8), dist_to_first (f32 at 12),
        // spacing[8] (f32 at 16), num_cells[8] (i16 at 48). 64 bytes.
        require(block, 64, offset, "CSFD")?;
        self.descriptor_attrs
            .extend(descriptors::csfd(self.endian, block));
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
        self.descriptor_attrs
            .extend(descriptors::swib(self.endian, block));
        self.sweep_number = self.endian.i32(block, 16);
        self.fixed_angle_deg = self.endian.f32(block, 32);
        Ok(())
    }

    fn parse_sswb(&mut self, block: &[u8], offset: usize) -> Result<()> {
        require(block, 20, offset, "SSWB")?;
        self.descriptor_attrs
            .extend(descriptors::sswb(self.endian, block));
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
            peak_power_kw: self.endian.f32(block, 32),
            sweep_num: self.endian.i32(block, 8),
            true_scan_rate: self.endian.f32(block, 36),
            asib: None,
        })
    }

    /// ASIB (platform information block): attached to the ray whose RYIB
    /// precedes it. A block shorter than its 80 bytes is ignored.
    fn parse_asib(&mut self, block: &[u8]) {
        let Some(ray) = self.current_ray.as_mut() else {
            return;
        };
        if block.len() < 8 + ASIB_VALUES * 4 {
            return;
        }
        let endian = self.endian;
        ray.asib = Some(std::array::from_fn(|index| {
            endian.f32(block, 8 + index * 4)
        }));
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
        let mut row = match param.binary_format {
            1 => ParamRow::I8(payload.iter().map(|byte| *byte as i8).collect()),
            2 => {
                // One loop per byte order, so each converts without a branch
                // per word.
                let pairs = payload.chunks_exact(2);
                let words: Vec<i16> = match endian {
                    Endian::Little => pairs
                        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                        .collect(),
                    Endian::Big => pairs
                        .map(|pair| i16::from_be_bytes([pair[0], pair[1]]))
                        .collect(),
                };
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
        if let Some(gates) = gate_count {
            row.truncate(gates);
        }
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

    /// Record the in-flight ray with its field rows. Every ray is kept,
    /// whatever its RYIB `ray_status` (0 normal, 1 antenna in transition,
    /// 2 bad): the status is carried per ray.
    fn finish_current_ray(&mut self) -> Result<()> {
        let Some(ray) = self.current_ray.take() else {
            return Ok(());
        };
        self.budget
            .charge(1, PARSE_RAY_BYTES, "DORADE rays")
            .map_err(DoradeError::LimitExceeded)?;
        let rows: Vec<(usize, ParamRow)> = self
            .params
            .iter_mut()
            .enumerate()
            .filter_map(|(index, param)| param.pending_row.take().map(|row| (index, row)))
            .collect();
        self.rays.push((ray, rows));
        Ok(())
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

    fn finish_into(mut self, builder: &mut DoradeVolumeBuilder) -> Result<()> {
        if self.rays.is_empty() {
            return Err(invalid(0, "DORADE sweep contains no rays"));
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
            self.describe_volume(volume);
        } else if volume.attrs.instrument_name != self.instrument {
            return Err(invalid(
                0,
                format!(
                    "DORADE sweep instrument '{}' does not match volume '{}'",
                    self.instrument, volume.attrs.instrument_name
                ),
            ));
        }
        // The reference is at or before every ray: the SSWB start, or the
        // earliest ray when rays run backwards past it (NOXP 2009-05-01
        // stores its rays from 19:02:44 back to 19:02:42).
        let earliest_ray = self.rays.iter().filter_map(|(ray, _)| ray.time).min();
        let sweep_start = match (self.start_time, earliest_ray) {
            (Some(start), Some(ray)) => Some(start.min(ray)),
            (start, ray) => start.or(ray),
        };
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
            // The mean elevation of the rays not in antenna transition (of
            // every ray when all are).
            let scanning: Vec<f32> = self
                .rays
                .iter()
                .filter(|(ray, _)| ray.status != 1)
                .map(|(ray, _)| ray.elevation_deg)
                .collect();
            let elevations = if scanning.is_empty() {
                self.rays.iter().map(|(ray, _)| ray.elevation_deg).collect()
            } else {
                scanning
            };
            elevations.iter().sum::<f32>() / elevations.len() as f32
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
            self.budget
                .charge(nrays, gates.saturating_mul(word_bytes), "DORADE field")
                .map_err(DoradeError::LimitExceeded)?;
            let mut field = new_field(param, u32::try_from(ngates).unwrap_or(u32::MAX));
            field.reserve_rows(nrays);
            param.field = Some(field);
        }
        self.budget
            .charge(nrays, RAY_BYTES, "DORADE sweep rays")
            .map_err(DoradeError::LimitExceeded)?;

        sweep.reserve_rays(nrays);
        let rays = std::mem::take(&mut self.rays);
        let transmit_power_dbm: Vec<f32> = rays
            .iter()
            .map(|(ray, _)| transmit_power_dbm(ray.peak_power_kw))
            .collect();
        let ray_blocks: Vec<PendingRay> = rays.iter().map(|(ray, _)| *ray).collect();
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
        if let Some(range_km) = self.radd.unambiguous_range_km {
            sweep.ray_vars.unambiguous_range_m = Some(vec![range_km * 1000.0; nrays]);
        }
        if let Some(pulse_width_s) = self.common_pulse_width_s() {
            sweep.ray_vars.pulse_width_s = Some(vec![pulse_width_s; nrays]);
        }
        if let Some(samples) = self
            .common_param(|param| param.num_samples)
            .filter(|n| *n > 0)
        {
            sweep.ray_vars.n_samples = Some(vec![i32::from(samples); nrays]);
        }
        sweep.target_scan_rate_deg_per_s = self.radd.rotation_deg_per_s;
        if transmit_power_dbm.iter().any(|power| power.is_finite()) {
            sweep.monitoring = Some(Box::new(Monitoring {
                radar_measured_transmit_power_h_dbm: Some(transmit_power_dbm),
                ..Monitoring::default()
            }));
        }
        if let Some(calibration) = self.calibration() {
            let index = calibration_index(&mut volume.radar_calibration, calibration);
            sweep.ray_vars.calib_index = Some(vec![index; nrays]);
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
        attach_ray_blocks(&mut sweep, &ray_blocks);
        sweep.other = std::mem::take(&mut self.descriptor_attrs);
        if let Some(distances) = self.celv_distances_m.take() {
            sweep.extra_vars.push(ExtraVariable {
                name: "dorade_celv_distance".into(),
                dims: vec!["dorade_cell".into()],
                shape: vec![u32::try_from(distances.len()).unwrap_or(u32::MAX)],
                values: ArrayBuf::F32(distances),
                attrs: vec![
                    (
                        "long_name".into(),
                        AttrValue::text("CELV distance from the radar to each cell"),
                    ),
                    ("units".into(), AttrValue::text("m")),
                    (
                        "comment".into(),
                        AttrValue::text(
                            "DORADE CELV dist_cells, as stored (before the CFAC range delay correction)",
                        ),
                    ),
                ],
            });
        }

        volume.provenance.decode.message_count += 1;
        volume.provenance.decode.skipped_message_count += self.skipped_field_blocks;
        volume.sweeps.push(sweep);
        builder.sweep_starts.push(sweep_start);
        builder.unapplied_cfac.push(self.cfac.unapplied);
        Ok(())
    }
}

impl SweepParse {
    /// Volume-level descriptors from the first sweepfile: RADD beam widths
    /// and antenna gain and the PARM receiver bandwidth
    /// (`radar_parameters`), and the VOLD text fields (global attributes,
    /// verbatim).
    fn describe_volume(&self, volume: &mut Volume) {
        // A moving platform, as LROSE Radx writes `platform_is_mobile`.
        if let Some(platform) = self.radar_type.and_then(platform_type) {
            volume.platform_type = platform;
            volume.attrs.platform_is_mobile = true;
        }
        // An airborne (AIR) scan rotates about the aircraft's longitudinal
        // axis: FM301 `axis_y_prime`, as LROSE Radx sets it for tail radars.
        if self.scan_mode == SCAN_MODE_AIR {
            volume.primary_axis = Some(PrimaryAxis::AxisYPrime);
        }
        let radd = &self.radd;
        let parameters = &mut volume.radar_parameters;
        parameters.beam_width_h_deg = radd.beam_width_h_deg;
        parameters.beam_width_v_deg = radd.beam_width_v_deg;
        // One antenna for both polarizations.
        parameters.antenna_gain_h_db = radd.antenna_gain_db;
        parameters.antenna_gain_v_db = radd.antenna_gain_db;
        parameters.receiver_bandwidth_hz = self
            .common_param(|param| param.bandwidth_mhz.to_bits())
            .map(f32::from_bits)
            .and_then(valid_dorade_f32)
            .filter(|bandwidth| *bandwidth > 0.0)
            .map(|bandwidth| bandwidth * 1e6);
        for (name, value) in &self.vold_text {
            volume
                .attrs
                .other
                .push((Box::from(*name), AttrValue::Text(value.as_str().into())));
        }
    }

    /// This sweepfile's RADD calibration constants as a `radar_calibration`
    /// entry (without an index), with the PARM pulse width they apply to;
    /// `None` when the file records none.
    fn calibration(&self) -> Option<RadarCalibration> {
        let radd = &self.radd;
        let calibration = RadarCalibration {
            radar_constant_h: radd.radar_constant_db,
            xmit_power_h_dbm: radd.peak_power_kw.map(kw_to_dbm),
            noise_hc_dbm: radd.noise_power_dbm,
            receiver_gain_hc_db: radd.receiver_gain_db,
            antenna_gain_h_db: radd.antenna_gain_db,
            antenna_gain_v_db: radd.antenna_gain_db,
            extra: radd
                .system_gain_db
                .map(|gain| {
                    (
                        Box::from("system_gain"),
                        AttrValue::Scalar(Scalar::F32(gain)),
                    )
                })
                .into_iter()
                .collect(),
            ..RadarCalibration::default()
        };
        (calibration != RadarCalibration::default()).then(|| RadarCalibration {
            pulse_width_s: self.common_pulse_width_s(),
            ..calibration
        })
    }

    /// `value` of every PARM when they all agree.
    fn common_param<T: PartialEq + Copy>(&self, value: impl Fn(&ParamState) -> T) -> Option<T> {
        let first = value(self.params.first()?);
        self.params
            .iter()
            .all(|param| value(param) == first)
            .then_some(first)
    }

    /// The PARM pulse width (m) of every field, as a duration (two-way).
    fn common_pulse_width_s(&self) -> Option<f32> {
        const SPEED_OF_LIGHT_M_PER_S: f64 = 299_792_458.0;
        self.common_param(|param| param.pulse_width_m)
            .filter(|metres| *metres > 0)
            .map(|metres| (2.0 * f64::from(metres) / SPEED_OF_LIGHT_M_PER_S) as f32)
    }
}

/// Peak power in kW as dBm (1 kW is 60 dBm).
fn kw_to_dbm(kw: f32) -> f32 {
    10.0 * kw.log10() + 60.0
}

/// A RYIB peak power (kW) as the monitoring transmit power (dBm): NaN where
/// the file writes a missing value (not positive, or not finite).
fn transmit_power_dbm(kw: f32) -> f32 {
    if kw.is_finite() && kw > 0.0 {
        kw_to_dbm(kw)
    } else {
        f32::NAN
    }
}

/// The `calib_index` of `entry` in `calibration`: an equal entry's, else a
/// new entry's.
fn calibration_index(calibration: &mut Vec<RadarCalibration>, entry: RadarCalibration) -> i32 {
    let found = calibration.iter().find(|existing| {
        RadarCalibration {
            calib_index: None,
            ..(*existing).clone()
        } == entry
    });
    if let Some(index) = found.and_then(|existing| existing.calib_index) {
        return index;
    }
    let index = i32::try_from(calibration.len()).unwrap_or(i32::MAX);
    calibration.push(RadarCalibration {
        calib_index: Some(index),
        ..entry
    });
    index
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
    let mut field = Field::new(
        FieldName::parse(&param.name),
        GateMapping::IDENTITY,
        ngates,
        data,
    );
    if !param.description.is_empty() {
        field.attrs.long_name = Some(param.description.clone().into());
    }
    if !param.units.is_empty() {
        field.attrs.units = Some(param.units.clone().into());
    }
    field.attrs.other = param.attrs.clone();
    field
}

/// The RADD `radar_type` as an FM301 `platform_type`: 1 to 4 are the
/// airborne fore, aft, tail and lower fuselage radars, 5 shipborne. Ground
/// radars (0) and unknown codes keep the default.
fn platform_type(code: i16) -> Option<PlatformType> {
    match code {
        1 => Some(PlatformType::AircraftFore),
        2 => Some(PlatformType::AircraftAft),
        3 => Some(PlatformType::AircraftTail),
        4 => Some(PlatformType::AircraftBelly),
        5 => Some(PlatformType::Ship),
        _ => None,
    }
}

/// A DORADE float that is not a missing-value sentinel (-999, -9999,
/// -32768) or NaN, else NaN.
fn present(value: f32) -> f32 {
    if value.is_finite() && value > -999.0 {
        value
    } else {
        f32::NAN
    }
}

fn per_ray_variable(name: &str, values: ArrayBuf, attrs: Vec<(&str, &str)>) -> ExtraVariable {
    ExtraVariable {
        name: name.into(),
        dims: vec!["time".into()],
        shape: vec![u32::try_from(values.len()).unwrap_or(u32::MAX)],
        values,
        attrs: attrs
            .into_iter()
            .map(|(key, value)| (Box::from(key), AttrValue::text(value)))
            .collect(),
    }
}

/// The RYIB and ASIB values of every ray: `ray_status` as FM301
/// `antenna_transition` (status 1) and verbatim, `true_scan_rate` as FM301
/// `scan_rate`, `sweep_num` verbatim, and the ASIB platform position and
/// attitude as the sweep's platform track, with its velocities, winds and
/// change rates as the CfRadial georeference variables. ASIB values are as
/// stored: the CFAC corrections (sweep attributes `dorade_cfac_*`) are not
/// applied to them. A column with a missing-value sentinel is also kept
/// verbatim ([`push_verbatim_sentinels`]).
fn attach_ray_blocks(sweep: &mut Sweep, rays: &[PendingRay]) {
    if rays.is_empty() {
        return;
    }
    sweep.ray_vars.antenna_transition =
        Some(rays.iter().map(|ray| u8::from(ray.status == 1)).collect());
    sweep.extra_vars.push(per_ray_variable(
        "dorade_ryib_ray_status",
        ArrayBuf::I32(rays.iter().map(|ray| ray.status).collect()),
        vec![
            ("long_name", "RYIB ray status"),
            ("comment", "0 normal, 1 antenna in transition, 2 bad"),
        ],
    ));
    sweep.extra_vars.push(per_ray_variable(
        "dorade_ryib_sweep_num",
        ArrayBuf::I32(rays.iter().map(|ray| ray.sweep_num).collect()),
        vec![("long_name", "RYIB sweep number")],
    ));
    let rates: Vec<f32> = rays.iter().map(|ray| present(ray.true_scan_rate)).collect();
    if rates.iter().any(|rate| rate.is_finite()) {
        sweep.ray_vars.scan_rate_deg_per_s = Some(rates);
    }
    push_verbatim_sentinels(
        sweep,
        "dorade_ryib_true_scan_rate",
        "degrees/s",
        "RYIB true scan rate",
        rays.iter().map(|ray| ray.true_scan_rate).collect(),
        present,
    );
    push_verbatim_sentinels(
        sweep,
        "dorade_ryib_peak_power_kw",
        "kW",
        "RYIB peak transmitted power",
        rays.iter().map(|ray| ray.peak_power_kw).collect(),
        transmit_power_dbm,
    );
    if rays.iter().all(|ray| ray.asib.is_none()) {
        return;
    }
    let column = |index: usize| -> Vec<f32> {
        rays.iter()
            .map(|ray| ray.asib.map_or(f32::NAN, |asib| present(asib[index])))
            .collect()
    };
    let optional = |values: Vec<f32>| values.iter().any(|v| v.is_finite()).then_some(values);
    let km_to_m = |values: Vec<f32>| -> Vec<f64> {
        values
            .into_iter()
            .map(|km| f64::from(km) * KM_TO_M)
            .collect()
    };
    let degrees = |values: Vec<f32>| -> Vec<f64> { values.into_iter().map(f64::from).collect() };
    sweep.platform_track = Some(Box::new(PlatformTrack {
        latitude_deg: degrees(column(1)),
        longitude_deg: degrees(column(0)),
        altitude_m: km_to_m(column(2)),
        altitude_agl_m: optional(column(3)).map(km_to_m),
        heading_deg: optional(column(7)),
        roll_deg: optional(column(8)),
        pitch_deg: optional(column(9)),
        drift_deg: optional(column(10)),
        rotation_deg: optional(column(11)),
        tilt_deg: optional(column(12)),
    }));
    for (index, name, units, long_name) in [
        (
            4,
            "eastward_velocity",
            "m/s",
            "ASIB east-west velocity of the platform",
        ),
        (
            5,
            "northward_velocity",
            "m/s",
            "ASIB north-south velocity of the platform",
        ),
        (
            6,
            "vertical_velocity",
            "m/s",
            "ASIB vertical velocity of the platform",
        ),
        (
            13,
            "eastward_wind",
            "m/s",
            "ASIB east-west wind at the platform",
        ),
        (
            14,
            "northward_wind",
            "m/s",
            "ASIB north-south wind at the platform",
        ),
        (
            15,
            "vertical_wind",
            "m/s",
            "ASIB vertical wind at the platform",
        ),
        (
            16,
            "heading_change_rate",
            "degrees/s",
            "ASIB heading change rate",
        ),
        (
            17,
            "pitch_change_rate",
            "degrees/s",
            "ASIB pitch change rate",
        ),
    ] {
        if let Some(values) = optional(column(index)) {
            sweep.extra_vars.push(per_ray_variable(
                name,
                ArrayBuf::F32(values),
                vec![("long_name", long_name), ("units", units)],
            ));
        }
    }
    for (index, (name, units, long_name)) in ASIB_FIELDS.iter().enumerate() {
        let raw = rays
            .iter()
            .map(|ray| ray.asib.map_or(f32::NAN, |asib| asib[index]))
            .collect();
        push_verbatim_sentinels(sweep, name, units, long_name, raw, present);
    }
}

/// ASIB (platform_i, lrose-core `DoradeData.hh`) members in block order:
/// verbatim variable name, units and description.
#[rustfmt::skip]
const ASIB_FIELDS: [(&str, &str, &str); ASIB_VALUES] = [
    ("dorade_asib_longitude_deg", "degrees", "ASIB platform longitude"),
    ("dorade_asib_latitude_deg", "degrees", "ASIB platform latitude"),
    ("dorade_asib_altitude_msl_km", "km", "ASIB platform altitude MSL"),
    ("dorade_asib_altitude_agl_km", "km", "ASIB platform altitude AGL"),
    ("dorade_asib_ew_velocity_mps", "m/s", "ASIB east-west velocity"),
    ("dorade_asib_ns_velocity_mps", "m/s", "ASIB north-south velocity"),
    ("dorade_asib_vert_velocity_mps", "m/s", "ASIB vertical velocity"),
    ("dorade_asib_heading_deg", "degrees", "ASIB heading"),
    ("dorade_asib_roll_deg", "degrees", "ASIB roll"),
    ("dorade_asib_pitch_deg", "degrees", "ASIB pitch"),
    ("dorade_asib_drift_angle_deg", "degrees", "ASIB drift angle"),
    ("dorade_asib_rotation_angle_deg", "degrees", "ASIB rotation angle"),
    ("dorade_asib_tilt_deg", "degrees", "ASIB tilt"),
    ("dorade_asib_ew_horiz_wind_mps", "m/s", "ASIB east-west wind"),
    ("dorade_asib_ns_horiz_wind_mps", "m/s", "ASIB north-south wind"),
    ("dorade_asib_vert_wind_mps", "m/s", "ASIB vertical wind"),
    ("dorade_asib_heading_change_deg_per_s", "degrees/s", "ASIB heading change rate"),
    ("dorade_asib_pitch_change_deg_per_s", "degrees/s", "ASIB pitch change rate"),
];

/// `raw` as the verbatim per-ray variable `name` when a value in it is a
/// number that `typed` turns into NaN (a missing-value sentinel), so the
/// stored value survives the typed slot's NaN.
fn push_verbatim_sentinels(
    sweep: &mut Sweep,
    name: &str,
    units: &str,
    long_name: &str,
    raw: Vec<f32>,
    typed: impl Fn(f32) -> f32,
) {
    if raw
        .iter()
        .any(|value| !value.is_nan() && typed(*value).is_nan())
    {
        sweep.extra_vars.push(per_ray_variable(
            name,
            ArrayBuf::F32(raw),
            vec![
                ("long_name", long_name),
                ("units", units),
                (
                    "comment",
                    "as stored, missing-value sentinels included (NaN: the ray has no such block)",
                ),
            ],
        ));
    }
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
/// AIR, the scan of an airborne tail radar rotating about the aircraft's
/// longitudinal axis, is `elevation_surveillance`, as LROSE Radx reads it
/// (RadxPrint on the NOAA P-3 N42RF tail radar sweeps of Hurricane
/// Michael).
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
        SCAN_MODE_AIR => SweepMode::ElevationSurveillance,
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
    // The context text is built only for the error (this runs per field
    // block of every ray).
    if gates <= MAX_GATES_PER_RADIAL {
        return Ok(());
    }
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
mod tests {
    use super::*;

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

    /// The field with the DORADE parameter name `name`.
    fn field<'s>(sweep: &'s Sweep, name: &str) -> &'s Field {
        sweep
            .field(&FieldName::parse(name))
            .unwrap_or_else(|| panic!("no field {name}"))
    }

    fn assert_gate(field: &Field, ray: usize, gate: usize, expected: Option<f32>) {
        let actual = field.value(ray, gate);
        match (actual, expected) {
            (None, None) => {}
            (Some(actual), Some(expected)) => assert!(
                (actual - expected).abs() < 1e-3,
                "{} [{ray},{gate}]: {actual} != {expected}",
                field.name
            ),
            _ => panic!("{} [{ray},{gate}]: {actual:?} != {expected:?}", field.name),
        }
    }

    /// Missing gates among the sweep's CSFD/CELV cells.
    fn missing_gates(field: &Field, cells: usize, ray: usize) -> usize {
        (0..cells)
            .filter(|gate| field.value(ray, *gate).is_none())
            .count()
    }

    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() <= tolerance
    }

    /// Ray time in milliseconds after the volume time reference.
    fn time_offset_ms(sweep: &Sweep, ray: usize) -> i64 {
        (sweep.rays.time_s[ray] * 1000.0).round() as i64
    }

    fn nyquist(sweep: &Sweep, ray: usize) -> f64 {
        f64::from(
            sweep
                .ray_vars
                .nyquist_velocity_mps
                .as_ref()
                .expect("Nyquist velocity")[ray],
        )
    }

    /// `(first centre, spacing, gates)` of the sweep's range coordinate.
    fn range_layout(sweep: &Sweep) -> (f64, f64, usize) {
        let range = &sweep.range;
        let first = range.center_m(0).expect("first gate");
        let spacing = range.center_m(1).expect("second gate") - first;
        (first, spacing, range.ngates())
    }

    #[test]
    fn decodes_big_endian_real_cow2_sweep() {
        let bytes = corpus(COW2);
        assert!(looks_like_dorade_bytes(&bytes));
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Big);

        let volume = read_dorade_sweep_volume(&bytes).expect("decode COW2");
        // RADD: name COW2, scan mode 8 (SUR), lat 39.739979, lon -103.292664,
        // altitude 1.519 km, HRD RLE (data_compress 1).
        assert_eq!(volume.attrs.instrument_name, "COW2");
        assert_eq!(volume.provenance.source_format, SourceFormat::Dorade);
        assert!(close(
            volume.location.latitude_deg.unwrap(),
            39.739_98,
            1e-5
        ));
        assert!(close(
            volume.location.longitude_deg.unwrap(),
            -103.292_66,
            1e-5
        ));
        assert!(close(volume.location.altitude_m.unwrap(), 1519.0, 0.01));
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some("dorade-hrd-rle")
        );
        // SSWB start 1779404114 = 2026-05-21T22:55:14Z.
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2026, 5, 21, 22, 55, 14).unwrap()
        );
        assert_eq!(volume.sweeps.len(), 1);

        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.sweep_mode, SweepMode::AzimuthSurveillance);
        // SWIB fixed angle 1.0052556, sweep 6; RYIB status [1, 1, 1, 0, ...]:
        // every ray kept, the first three flagged as antenna transition.
        assert_eq!(sweep.fixed_angle_deg, 1.005_255_6);
        assert_eq!(sweep.elevation_number, Some(6));
        assert_eq!(sweep.nrays(), 24);
        let transition = sweep.ray_vars.antenna_transition.as_ref().unwrap();
        assert_eq!(transition[..4], [1, 1, 1, 0]);
        assert_eq!(transition.iter().filter(|flag| **flag == 1).count(), 3);
        assert_eq!(volume.provenance.decode.skipped_message_count, 0);
        // CSFD: one segment, 375 cells, 50 m to the first, 100 m apart.
        assert_eq!(range_layout(sweep), (50.0, 100.0, 375));
        for (ray, (azimuth, time_offset)) in [(3, (73.0, 280)), (4, (73.5, 297)), (23, (83.0, 609))]
        {
            assert_eq!(sweep.rays.azimuth_deg[ray], azimuth, "ray {ray}");
            assert_eq!(sweep.rays.elevation_deg[ray], 0.818_481_45, "ray {ray}");
            assert_eq!(time_offset_ms(sweep, ray), time_offset, "ray {ray}");
            // RADD eff_unamb_vel 68.75974 m/s.
            assert!(close(nyquist(sweep, ray), 68.759_74, 1e-4));
        }

        // PARM DBZHC_F / VEL_F / ZDR_F (scale 100) and RHOHV_F (scale 10000),
        // bias 0, bad -32768, on the first scanning ray (3) and the last ray.
        let reflectivity = field(sweep, "DBZHC_F");
        let velocity = field(sweep, "VEL_F");
        let zdr = field(sweep, "ZDR_F");
        let rhohv = field(sweep, "RHOHV_F");
        assert_gate(reflectivity, 3, 1, Some(-14.22));
        assert_gate(reflectivity, 23, 0, Some(-19.85));
        assert_gate(reflectivity, 23, 1, Some(-11.37));
        assert_gate(reflectivity, 23, 50, None);
        assert_gate(velocity, 3, 1, Some(-49.06));
        assert_gate(velocity, 23, 0, Some(-66.56));
        assert_gate(velocity, 23, 100, Some(54.18));
        assert_gate(velocity, 23, 374, Some(-34.55));
        assert_gate(zdr, 23, 0, Some(6.53));
        assert_gate(rhohv, 23, 1, Some(0.8043));
        // Bad-gate counts of the same rays.
        assert_eq!(missing_gates(reflectivity, 375, 3), 265);
        assert_eq!(missing_gates(velocity, 375, 3), 112);
        assert_eq!(missing_gates(zdr, 375, 23), 335);
        assert_eq!(missing_gates(rhohv, 375, 23), 332);
    }

    #[test]
    fn decodes_little_endian_rle_sweep() {
        // DOW6low RHI: little-endian HRD RLE, CELV 1000 cells from 24.98 m at
        // 49.97 m spacing, 104-byte PARMs, 41 rays of which the first 6 are
        // flagged as antenna transition.
        let bytes = corpus(DOW6_RHI);
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Little);
        let volume = read_dorade_sweep_volume(&bytes).expect("decode DOW6 RHI");
        assert_eq!(volume.attrs.instrument_name, "DOW6low");
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some("dorade-hrd-rle")
        );
        assert!(close(
            volume.location.latitude_deg.unwrap(),
            39.995_46,
            1e-5
        ));
        assert!(close(
            volume.location.longitude_deg.unwrap(),
            -105.191_68,
            1e-5
        ));
        assert!(close(volume.location.altitude_m.unwrap(), 1615.0, 0.01));
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.nrays(), 41);
        let transition = sweep.ray_vars.antenna_transition.as_ref().unwrap();
        assert!(transition[..6].iter().all(|flag| *flag == 1));
        assert!(transition[6..].iter().all(|flag| *flag == 0));
        assert_eq!(volume.provenance.decode.skipped_message_count, 0);
        let (first, spacing, gates) = range_layout(sweep);
        assert!(close(first, 24.98, 0.01), "first gate {first}");
        assert!(close(spacing, 49.97, 0.01), "gate spacing {spacing}");
        assert_eq!(gates, 1000);
        assert!(close(nyquist(sweep, 6), 39.866_02, 1e-4));
        assert_eq!(time_offset_ms(sweep, 6), 1126);
        assert_eq!(time_offset_ms(sweep, 40), 3503);

        // Fields keep their DORADE names: DBZHC and VEL next to their edited
        // DBZHC_F and VEL_F copies.
        let reflectivity = field(sweep, "DBZHC");
        let velocity = field(sweep, "VEL");
        let velocity_f = field(sweep, "VEL_F");
        assert_gate(reflectivity, 6, 0, Some(-12.24));
        assert_gate(reflectivity, 6, 10, Some(-13.07));
        assert_gate(reflectivity, 6, 100, None);
        assert_gate(reflectivity, 40, 100, Some(-6.92));
        assert_gate(velocity, 6, 0, Some(36.76));
        assert_gate(velocity, 6, 10, Some(0.12));
        assert_gate(velocity, 6, 100, Some(-34.92));
        assert_gate(velocity, 6, 500, Some(40.44));
        assert_gate(velocity, 6, 999, Some(-0.53));
        assert_gate(velocity, 40, 500, Some(31.42));
        assert_gate(velocity_f, 6, 0, Some(32.6));
        assert_gate(velocity_f, 6, 10, Some(18.6));
        assert_gate(field(sweep, "RHOHV"), 6, 0, Some(0.6755));
        assert_gate(field(sweep, "PHIDP"), 6, 999, Some(125.2));
        assert_eq!(missing_gates(reflectivity, 1000, 6), 887);
        assert_eq!(missing_gates(field(sweep, "KDP"), 1000, 6), 1000);
    }

    #[test]
    fn decodes_little_endian_uncompressed_sweep() {
        // NOXP sector PPI: little-endian, uncompressed (data_compress 0), CSFD
        // 1001 cells x 150 m from 75 m, RADD scan mode 1, 100 rays.
        let bytes = corpus(NOXP_SECTOR);
        assert_eq!(detect_endian(&bytes).unwrap(), Endian::Little);
        let volume = read_dorade_sweep_volume(&bytes).expect("decode NOXP sector");
        assert_eq!(volume.attrs.instrument_name, "NOXPRVP");
        assert_eq!(
            volume.provenance.compression.as_deref(),
            Some("dorade-uncompressed")
        );
        assert!(close(
            volume.location.latitude_deg.unwrap(),
            34.480_247,
            1e-5
        ));
        assert!(close(
            volume.location.longitude_deg.unwrap(),
            -100.336_24,
            1e-5
        ));
        // SSWB start 20:32:11Z; the rays run back to 20:32:07Z, the earliest
        // of the two and so the time reference (no ray time is negative).
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2009, 5, 25, 20, 32, 7).unwrap()
        );
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.sweep_mode, SweepMode::Sector);
        assert_eq!(sweep.nrays(), 100);
        assert_eq!(sweep.fixed_angle_deg, 0.499_877_93);
        // The RDAT payloads carry one more word than the 1001 cells, padding
        // the block to a 4-byte boundary; only the described cells are kept.
        assert_eq!(range_layout(sweep), (75.0, 150.0, 1001));
        assert!(sweep.fields.iter().all(|field| field.ngates == 1001));
        // RYIB azimuth -160.03235 deg, normalized into [0, 360).
        let azimuths = &sweep.rays.azimuth_deg;
        assert!(close(
            f64::from(azimuths[0]),
            f64::from(360.0 - 160.032_35f32),
            1e-3
        ));
        assert!(close(
            f64::from(azimuths[98]),
            f64::from(360.0 - 62.168_884f32),
            1e-3
        ));
        // Rays run backwards from the SSWB start: ray 0 is 4 s after the
        // reference (the earliest ray) and ray 98 sits on it.
        assert_eq!(time_offset_ms(sweep, 0), 4000);
        assert_eq!(time_offset_ms(sweep, 98), 0);
        assert!(close(nyquist(sweep, 0), 7.576_25, 1e-4));

        let reflectivity = field(sweep, "DZ");
        let velocity = field(sweep, "VR");
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
        let err = read_dorade_sweep_volume(&bytes).expect_err("whole sweep rejected");
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
                39.739_98f64,
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
            assert!(
                close(f64::from(header.latitude_deg), latitude, 1e-5),
                "{id}"
            );
            assert_eq!(peek_dorade_sweep(&bytes).expect("peek full file"), header);
        }
    }

    #[test]
    fn multi_sweep_volume_keeps_input_order_and_rebases_times() {
        // Three sweeps of NOXP volume NOX090610003210 (SWIB fixed angles
        // 0.49987793, 0.99975586, 1.9995117; SSWB starts 00:32:10, 00:32:22
        // and 00:32:26Z; 6 rays each), passed 2.0, 0.5, 1.0 deg.
        let sweeps = [
            corpus(NOXP_0610_20),
            corpus(NOXP_0610_05),
            corpus(NOXP_0610_10),
        ];
        let volume = read_dorade_volume_from_slices(&sweeps).expect("decode");
        // Input (scan) order, numbered in that order.
        let angles: Vec<f32> = volume.sweeps.iter().map(|s| s.fixed_angle_deg).collect();
        assert_eq!(angles, [1.999_511_7, 0.499_877_93, 0.999_755_86]);
        let numbers: Vec<u32> = volume.sweeps.iter().map(|s| s.sweep_number).collect();
        assert_eq!(numbers, [0, 1, 2]);
        assert!(volume.sweeps.iter().all(|sweep| sweep.nrays() == 6));
        assert_eq!(volume.provenance.decode.decoded_ray_count, 18);
        // The earliest SSWB start (the 0.5 deg sweep, 00:32:10Z) is the time
        // reference; the first-decoded 2.0 deg sweep's rays are rebased to it.
        assert_eq!(
            volume.time_reference,
            Utc.with_ymd_and_hms(2009, 6, 10, 0, 32, 10).unwrap()
        );
        let first_ray_s: Vec<f64> = volume.sweeps.iter().map(|s| s.rays.time_s[0]).collect();
        assert_eq!(first_ray_s, [16.0, 0.0, 12.0]);
        // Per-ray elevations of the 1.0 deg sweep: 0.98876953.
        assert!(
            volume.sweeps[2]
                .rays
                .elevation_deg
                .iter()
                .all(|elevation| *elevation == 0.988_769_53)
        );
        // CSFD 1174 cells x 75 m from 37.5 m.
        assert_eq!(range_layout(&volume.sweeps[0]), (37.5, 75.0, 1174));
        assert_gate(field(&volume.sweeps[1], "DZ"), 0, 100, Some(-2.5));
        assert_gate(field(&volume.sweeps[2], "VR"), 5, 100, Some(-13.56));
    }

    #[test]
    fn mismatched_instruments_are_rejected() {
        let err =
            read_dorade_volume_from_slices(&[corpus(COW2), corpus(NOXP_0610_05)]).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert!(err.to_string().contains("NOXPRVP"), "{err}");
    }

    #[test]
    fn rhi_scan_mode_is_detected_from_radd() {
        // DOW6low: RADD scan mode 3 (RHI), SWIB fixed angle 143.99875 deg;
        // after 6 antenna-transition rays, the rays step elevation down from
        // 30.0 to 13.0 deg by 0.5 deg at RYIB azimuths 125.78-126.55 deg
        // (CFAC corrections all zero).
        let volume = read_dorade_sweep_volume(&corpus(DOW6_RHI)).expect("decode");
        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.sweep_mode, SweepMode::Rhi);
        assert_eq!(sweep.fixed_angle_deg, 143.998_75);
        assert_eq!(sweep.nrays(), 41);
        let transition = sweep.ray_vars.antenna_transition.as_ref().unwrap();
        for (index, (azimuth, elevation)) in sweep
            .rays
            .azimuth_deg
            .iter()
            .zip(&sweep.rays.elevation_deg)
            .enumerate()
            .skip(6)
        {
            assert_eq!(transition[index], 0, "ray {index}");
            assert_eq!(*elevation, 30.0 - 0.5 * (index - 6) as f32, "ray {index}");
            assert!((125.7..126.6).contains(azimuth), "ray {index}");
        }
        assert_eq!(transition[..6], [1; 6]);
        assert!(close(
            f64::from(sweep.rays.azimuth_deg[6]),
            125.775_07,
            1e-4
        ));
        assert!(close(
            f64::from(sweep.rays.azimuth_deg[40]),
            126.549_61,
            1e-4
        ));
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
        assert_eq!(sweep_mode_from_radd(9), SweepMode::ElevationSurveillance);
        assert_eq!(sweep_mode_from_radd(0).as_str(), "calibration");
        assert_eq!(sweep_mode_from_radd(99).as_str(), "dorade_scan_mode_99");
    }

    #[test]
    fn i16_fields_keep_dorade_scaling() {
        // COW2 PARM (binary_format 2 = i16): DBZHC_F scale 100, RHOHV_F scale
        // 10000, bias 0, bad_data -32768; the words stay i16 with the
        // (raw - bias) / scale transform and bad_data as the fill value.
        let volume = read_dorade_sweep_volume(&corpus(COW2)).expect("decode");
        let sweep = &volume.sweeps[0];
        for (name, scale) in [
            ("DBZHC_F", 100.0),
            ("VEL_F", 100.0),
            ("ZDR_F", 100.0),
            ("RHOHV_F", 10_000.0),
        ] {
            let FieldData::I16 { coding, .. } = &field(sweep, name).data else {
                panic!("{name} is not int16");
            };
            assert_eq!(
                coding.transform,
                LinearTransform::IcdScaleOffset { scale, offset: 0.0 },
                "{name}"
            );
            assert_eq!(coding.fill_value, Some(-32768), "{name}");
        }
        // Raw word -3030 is gate 0 of the first scanning ray, file ray 3
        // (golden REF).
        let FieldData::I16 { values, .. } = &field(sweep, "DBZHC_F").data else {
            unreachable!()
        };
        assert_eq!(values[3 * 375], -3030);
    }

    fn assert_near(actual: Option<f32>, expected: f64, what: &str) {
        let actual = actual.unwrap_or_else(|| panic!("{what}: missing"));
        assert!(
            close(f64::from(actual), expected, 1e-3 * expected.abs().max(1.0)),
            "{what}: {actual} != {expected}"
        );
    }

    /// Every ray of a per-ray variable holds `expected`.
    fn assert_constant(values: Option<&Vec<f32>>, rays: usize, expected: f64, what: &str) {
        let values = values.unwrap_or_else(|| panic!("{what}: missing"));
        assert_eq!(values.len(), rays, "{what}");
        for value in values {
            assert_near(Some(*value), expected, what);
        }
    }

    const LIGHT_M_PER_S: f64 = 299_792_458.0;

    #[test]
    fn radd_parm_ryib_and_vold_descriptors_reach_the_model() {
        // Golden section `dorade`, keys `<case>.radd_constants`,
        // `<case>.params[*].{description, units, pulse_width, num_samples,
        // recvr_bandwidth}`, `<case>.ryib_peak_power` and `<case>.vold_text`.
        // Missing RADD values are -9999 (DOW6) or -32768 (NOXP), a missing
        // RYIB peak power is -999 (DOW6), and a zero bandwidth is unset.

        let dow6 = read_dorade_sweep_volume(&corpus(DOW6_RHI)).expect("decode");
        let parameters = &dow6.radar_parameters;
        assert_near(parameters.beam_width_h_deg, 1.0, "DOW6 beam width h");
        assert_near(parameters.beam_width_v_deg, 1.0, "DOW6 beam width v");
        assert_near(
            parameters.antenna_gain_h_db,
            44.299_999,
            "DOW6 antenna gain h",
        );
        assert_near(
            parameters.antenna_gain_v_db,
            44.299_999,
            "DOW6 antenna gain v",
        );
        assert_near(parameters.receiver_bandwidth_hz, 2.0e6, "DOW6 bandwidth");
        let [calibration] = dow6.radar_calibration.as_slice() else {
            panic!("DOW6: one calibration entry");
        };
        assert_eq!(calibration.calib_index, Some(0));
        assert_near(
            calibration.pulse_width_s,
            2.0 * 75.0 / LIGHT_M_PER_S,
            "DOW6 calibration pulse width",
        );
        assert_near(
            calibration.radar_constant_h,
            81.051_201,
            "DOW6 radar constant",
        );
        // peak_power 19.952623 kW.
        assert_near(
            calibration.xmit_power_h_dbm,
            10.0 * 19.952_623_367_309_57f64.log10() + 60.0,
            "DOW6 transmit power",
        );
        assert_near(calibration.noise_hc_dbm, -55.072_601, "DOW6 noise power");
        assert_near(
            calibration.receiver_gain_hc_db,
            50.651_699,
            "DOW6 receiver gain",
        );
        assert_near(
            calibration.antenna_gain_h_db,
            44.299_999,
            "DOW6 calibration gain",
        );
        assert_eq!(calibration.extra.len(), 1);
        assert_eq!(&*calibration.extra[0].0, "system_gain");
        let AttrValue::Scalar(Scalar::F32(system_gain)) = calibration.extra[0].1 else {
            panic!("system gain is float32");
        };
        assert_near(Some(system_gain), 42.299_999, "DOW6 system gain");
        assert!(dow6.attrs.other.is_empty(), "DOW6 VOLD text is blank");
        let sweep = &dow6.sweeps[0];
        let rays = sweep.nrays();
        assert_constant(
            sweep.ray_vars.unambiguous_range_m.as_ref(),
            rays,
            59_958.492,
            "DOW6 unambiguous range",
        );
        assert_constant(
            sweep.ray_vars.pulse_width_s.as_ref(),
            rays,
            2.0 * 75.0 / LIGHT_M_PER_S,
            "DOW6 pulse width",
        );
        assert_eq!(sweep.ray_vars.n_samples, Some(vec![256; rays]));
        assert_eq!(sweep.ray_vars.calib_index, Some(vec![0; rays]));
        assert_eq!(
            sweep.target_scan_rate_deg_per_s, None,
            "req_rotat_vel -9999"
        );
        assert!(sweep.monitoring.is_none(), "RYIB peak_power -999");
        let ncp = field(sweep, "NCP");
        assert_eq!(ncp.attrs.long_name.as_deref(), Some("NCP"));
        assert_eq!(ncp.attrs.units, None, "blank PARM units");
        let snr = field(sweep, "SNRHC");
        assert_eq!(snr.attrs.units.as_deref(), Some("dB"));
        assert_eq!(
            field(sweep, "VS1").attrs.long_name.as_deref(),
            Some("VELPS")
        );

        let noxp = read_dorade_sweep_volume(&corpus(NOXP_SECTOR)).expect("decode");
        let parameters = &noxp.radar_parameters;
        assert_near(parameters.beam_width_h_deg, 0.879_999, "NOXP beam width h");
        assert_near(parameters.beam_width_v_deg, 0.879_999, "NOXP beam width v");
        assert_eq!(parameters.antenna_gain_h_db, None, "antenna_gain -32768");
        assert_eq!(parameters.receiver_bandwidth_hz, None, "recvr_bandwidth 0");
        let [calibration] = noxp.radar_calibration.as_slice() else {
            panic!("NOXP: one calibration entry");
        };
        assert_near(
            calibration.radar_constant_h,
            63.709_999,
            "NOXP radar constant",
        );
        assert_near(
            calibration.xmit_power_h_dbm,
            10.0 * 300f64.log10() + 60.0,
            "NOXP transmit power",
        );
        assert_near(calibration.noise_hc_dbm, 26.0, "NOXP noise power");
        assert_eq!(calibration.receiver_gain_hc_db, None);
        assert!(calibration.extra.is_empty(), "system_gain -32768");
        assert_eq!(noxp.attrs.other.len(), 1);
        assert_eq!(&*noxp.attrs.other[0].0, "gen_facility");
        assert_eq!(noxp.attrs.other[0].1, AttrValue::Text("NOXPRVP".into()));
        let sweep = &noxp.sweeps[0];
        let rays = sweep.nrays();
        assert_constant(
            sweep.ray_vars.unambiguous_range_m.as_ref(),
            rays,
            157_784.21,
            "NOXP unambiguous range",
        );
        assert_constant(
            sweep.ray_vars.pulse_width_s.as_ref(),
            rays,
            2.0 * 300.0 / LIGHT_M_PER_S,
            "NOXP pulse width",
        );
        assert_eq!(sweep.ray_vars.n_samples, Some(vec![32; rays]));
        assert_eq!(sweep.ray_vars.calib_index, Some(vec![0; rays]));
        // RYIB peak_power 300 kW on every kept ray.
        let monitoring = sweep.monitoring.as_ref().expect("RYIB peak power");
        assert_constant(
            monitoring.radar_measured_transmit_power_h_dbm.as_ref(),
            rays,
            10.0 * 300f64.log10() + 60.0,
            "NOXP measured transmit power",
        );
        assert_eq!(monitoring.radar_measured_transmit_power_v_dbm, None);
        let reflectivity = field(sweep, "DZ");
        assert_eq!(
            reflectivity.attrs.long_name.as_deref(),
            Some("Reflectivity (1 byte)")
        );
        assert_eq!(reflectivity.attrs.units.as_deref(), Some("dBZ"));
        // PARM units are 8 characters: KDP's "dimensionless" is cut.
        assert_eq!(field(sweep, "KDP").attrs.units.as_deref(), Some("dimensio"));
    }

    #[test]
    fn calibration_entries_follow_the_sweepfile_constants() {
        // The three 2009-06-10 NOXP sweepfiles share their RADD constants
        // (golden `noxp_0610_*.radd_constants`: radar_const 76.72, peak_power
        // 30 kW, PARM pulse_width 150 m) and give one entry. The 2009-05-25
        // sector sweepfile of the same radar has radar_const 63.71, 300 kW
        // and 300 m, so a volume of both gives two entries, and each sweep's
        // rays point at their own (the files are from different days; the
        // builder accepts them because the instrument matches).
        let same = read_dorade_volume_from_slices(&[
            corpus(NOXP_0610_05),
            corpus(NOXP_0610_10),
            corpus(NOXP_0610_20),
        ])
        .expect("decode");
        let [calibration] = same.radar_calibration.as_slice() else {
            panic!("one entry for identical constants");
        };
        assert_eq!(calibration.calib_index, Some(0));
        assert_near(calibration.radar_constant_h, 76.720_001, "radar constant");
        assert_near(
            calibration.xmit_power_h_dbm,
            10.0 * 30f64.log10() + 60.0,
            "transmit power",
        );
        assert_near(
            calibration.pulse_width_s,
            2.0 * 150.0 / LIGHT_M_PER_S,
            "pulse width",
        );
        for sweep in &same.sweeps {
            assert_eq!(sweep.ray_vars.calib_index, Some(vec![0; sweep.nrays()]));
            let power = sweep
                .monitoring
                .as_ref()
                .and_then(|m| m.radar_measured_transmit_power_h_dbm.as_ref());
            assert_constant(power, sweep.nrays(), 10.0 * 30f64.log10() + 60.0, "power");
        }

        let mixed = read_dorade_volume_from_slices(&[
            corpus(NOXP_0610_05),
            corpus(NOXP_SECTOR),
            corpus(NOXP_0610_10),
        ])
        .expect("decode");
        let constants: Vec<(Option<i32>, f32)> = mixed
            .radar_calibration
            .iter()
            .map(|entry| (entry.calib_index, entry.radar_constant_h.unwrap()))
            .collect();
        assert_eq!(constants.len(), 2, "{constants:?}");
        assert_eq!(constants[0].0, Some(0));
        assert_eq!(constants[1].0, Some(1));
        assert!(close(f64::from(constants[0].1), 76.72, 1e-3));
        assert!(close(f64::from(constants[1].1), 63.71, 1e-3));
        let indices: Vec<i32> = mixed
            .sweeps
            .iter()
            .map(|sweep| {
                let index = sweep.ray_vars.calib_index.as_ref().expect("calib_index");
                assert!(index.iter().all(|value| *value == index[0]));
                index[0]
            })
            .collect();
        assert_eq!(indices, [0, 1, 0]);
        // The volume-level parameters and VOLD text come from the first file.
        assert_near(
            mixed.radar_parameters.beam_width_h_deg,
            0.879_999,
            "beam width",
        );
        assert_eq!(mixed.attrs.other.len(), 1);
    }
}
