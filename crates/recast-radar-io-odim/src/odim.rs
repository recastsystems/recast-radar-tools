//! ODIM_H5 polar volume/scan decoder (the European/research HDF5 standard).
//!
//! Information model: D. B. Michelson, R. Lewandowski, M. Szewczykowski,
//! H. Beekhuis, and G. Haase, "EUMETNET OPERA weather radar information
//! model for implementation with the HDF5 file format" (ODIM_H5), EUMETNET
//! OPERA Working Document WD_2008_03 (v2.2, 2014; v2.4, 2021). Layout:
//! `/what` (object, date, time, source), `/where` (lat, lon, height),
//! `/datasetN` per sweep with `where` (elangle, nbins, nrays, rstart,
//! rscale, a1gate), `what` (start/end date and time), `how` (per-ray angle
//! and time arrays, `NI`) and `dataM` per quantity with `what` (quantity,
//! gain, offset, nodata, undetect) and the `data` plane (nrays x nbins).
//!
//! Decodes `PVOL` (polar volume) and `SCAN` (single sweep) objects into the
//! FM301 model ([`Volume`]; `docs/design/fm301-model.md` sections 5.2, 7.2
//! and 8.1), following what xradar's `open_odim_datatree` returns for the
//! same file:
//! - One sweep per `datasetN` in file order; every `dataM` plane is a field
//!   named by its `what/quantity` verbatim (`DBZH`, `TH`, `VRAD`, ...).
//! - Planes keep their stored encoding: `u8`/`u16` with the CF packing
//!   `physical = gain * raw + offset` and `nodata` as `_FillValue`,
//!   `undetect` as `_Undetect` (kept distinct; Table 301-10); `float32` and
//!   `float64` planes verbatim with their sentinels as float codings.
//! - Ray azimuths are `(how/startazA + how/stopazA) / 2` when present, else
//!   the storage-order centres `(i + 0.5) * 360 / nrays`; ray elevations
//!   `(how/startelA + how/stopelA) / 2`, else `how/elangles`, else
//!   `where/elangle`; ray times `(how/startazT + how/stopazT) / 2`, else
//!   spread evenly between `what/starttime` and `endtime` starting at
//!   `where/a1gate` (all rays at `starttime` when the two are equal).
//! - The `range` coordinate holds gate centres: `rstart` (km to the start of
//!   the first bin) plus half a `rscale` (bin spacing in metres). Implausibly
//!   large `rstart` values are reinterpreted as metres — see
//!   [`first_gate_m_from_rstart`] for the writer quirk that requires it.
//! - `nyquist_velocity(time)` broadcasts `how/NI` (dataset, else root).
//! - Planes are stored verbatim (design note 7.2): no rewrite pass, so the
//!   raw arrays hash equal to xradar's. Some IRIS exporters (AEMET Spain,
//!   IRIS 10.3) copy the REFLECTIVITY `what` group onto the velocity plane —
//!   the VRADH `nodata`/`undetect` carry the dBZ sentinels (e.g. 95.5 / -32)
//!   while no-echo velocity gates are filled with the physical `offset`
//!   (0 m/s for gain=1/offset=0). Those fill gates match neither declared
//!   sentinel, so the decoded plane is a spurious 0 m/s wall (as it is in
//!   xradar and Py-ART). [`recover_copied_whatgroup_velocity_nodata`] is the
//!   opt-in post-pass that masks a velocity gate sitting on `offset` when
//!   the co-located reflectivity gate is no-echo, and only when the velocity
//!   sentinels equal the reflectivity sentinels (the copied-what-group
//!   signature) — genuine 0 m/s gates with echo, and conformant writers with
//!   distinct velocity sentinels, are untouched.
//!
//! Known limitations (explicit, not silent): non-polar objects (ELEV/RHI
//! cross-section products, CVOL, IMAGE) are rejected with a clear error;
//! 8/16-bit unsigned and float data planes are supported (the only types
//! OPERA members emit).

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use recast_radar_core::bounded_read::{DecodeBudget, check_gate_count, check_sweep_count};
use recast_radar_core::model::{
    Field, FieldData, FieldName, FloatCoding, FloatWidth, FollowMode, GateMapping, IntCoding,
    LinearTransform, PackedInt, Quantity, RangeCoord, SourceFormat, Sweep, SweepMode, Volume,
    floor_to_second,
};

pub use crate::hdf5lite::looks_like_hdf5_bytes;
use crate::hdf5lite::{H5Attr, H5Data, H5File};
use crate::{OdimError, Result};

/// Decode an ODIM_H5 PVOL/SCAN byte buffer into the FM301 model.
pub fn read_odim_h5_volume(bytes: &[u8]) -> Result<Volume> {
    let file = H5File::open(bytes)?;
    let object = file
        .attr("/what", "object")
        .and_then(|attr| attr.as_str().map(str::to_owned))
        .ok_or_else(|| {
            invalid(
                "HDF5 file has no /what 'object' attribute — not ODIM_H5 \
                 (CfRadial2/other HDF5 radar formats are not supported yet)",
            )
        })?;
    if object != "PVOL" && object != "SCAN" {
        return Err(invalid(format!(
            "ODIM_H5 object '{object}' unsupported (PVOL and SCAN only)"
        )));
    }

    let source = file
        .attr("/what", "source")
        .and_then(|attr| attr.as_str().map(str::to_owned))
        .unwrap_or_default();
    let identity = site_identity_from_source(&source);
    let nominal_time = parse_datetime(&file, "/what");
    let mut volume = Volume::new(
        identity.id,
        nominal_time.unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
    );
    volume.attrs.site_name = identity.name;
    volume.attrs.source = (!source.is_empty()).then_some(source);
    volume.attrs.wmo.id = identity.wmo;
    volume.attrs.wmo.wsi = identity.wigos;
    volume.location.latitude_deg = attr_f64(&file, "/where", "lat");
    volume.location.longitude_deg = attr_f64(&file, "/where", "lon");
    volume.location.altitude_m = attr_f64(&file, "/where", "height");
    volume.provenance.source_format = SourceFormat::OdimH5;
    volume.provenance.source_version = file
        .attr("/what", "version")
        .and_then(|attr| attr.as_str().map(str::to_owned))
        .or(Some("ODIM_H5".to_owned()));
    volume.provenance.source_conventions = file
        .attr("/", "Conventions")
        .and_then(|attr| attr.as_str().map(str::to_owned));
    volume.provenance.compression = Some("odim-h5".to_owned());
    if let Some(mhz) = odim_radar_frequency_mhz(&file) {
        volume.radar_parameters.frequency_hz = vec![mhz * 1e6];
    }
    let root_nyquist = attr_f64(&file, "/how", "NI");

    let mut dataset_names: Vec<String> = file
        .child_names("/")
        .into_iter()
        .filter(|name| {
            name.strip_prefix("dataset")
                .is_some_and(|rest| rest.parse::<u32>().is_ok())
        })
        .collect();
    dataset_names.sort_by_key(|name| name[7..].parse::<u32>().unwrap_or(u32::MAX));
    if dataset_names.is_empty() {
        return Err(invalid("ODIM_H5 volume has no /datasetN groups"));
    }
    check_sweep_count(dataset_names.len(), "ODIM_H5 volume").map_err(OdimError::LimitExceeded)?;

    let mut budget = DecodeBudget::volume();
    // Absolute ray times (seconds since the Unix epoch) until the reference
    // is known.
    let mut ray_epoch_s: Vec<Vec<f64>> = Vec::with_capacity(dataset_names.len());
    let mut skipped_planes = 0usize;
    for (index, name) in dataset_names.iter().enumerate() {
        let (sweep, times) = decode_sweep(&file, name, index, root_nyquist, &mut budget)?;
        skipped_planes += sweep.skipped_planes;
        ray_epoch_s.push(times);
        volume.sweeps.push(sweep.sweep);
    }

    // Time reference: the nominal volume time, else the earliest ray.
    if nominal_time.is_none() {
        let earliest = ray_epoch_s
            .iter()
            .flatten()
            .copied()
            .filter(|time| time.is_finite())
            .fold(f64::INFINITY, f64::min);
        if earliest.is_finite()
            && let Some(instant) =
                DateTime::<Utc>::from_timestamp_millis((earliest * 1000.0).floor() as i64)
        {
            volume.time_reference = floor_to_second(instant);
        }
    }
    let reference_s = volume.time_reference.timestamp() as f64;
    for (sweep, times) in volume.sweeps.iter_mut().zip(ray_epoch_s) {
        sweep.rays.time_s = times.into_iter().map(|time| time - reference_s).collect();
    }

    volume.provenance.decode.decoded_ray_count = volume.sweeps.iter().map(Sweep::nrays).sum();
    volume.provenance.decode.message_count = dataset_names.len();
    volume.provenance.decode.skipped_message_count = skipped_planes;
    volume.seal().map_err(|err| invalid(err.to_string()))?;
    volume.time_coverage = volume.ray_time_extent();
    Ok(volume)
}

struct DecodedSweep {
    sweep: Sweep,
    skipped_planes: usize,
}

fn decode_sweep(
    file: &H5File<'_>,
    dataset: &str,
    index: usize,
    root_nyquist: Option<f64>,
    budget: &mut DecodeBudget,
) -> Result<(DecodedSweep, Vec<f64>)> {
    let where_path = format!("/{dataset}/where");
    let what_path = format!("/{dataset}/what");
    let how_path = format!("/{dataset}/how");
    let elangle = attr_f64(file, &where_path, "elangle")
        .ok_or_else(|| invalid(format!("{dataset} has no where/elangle")))?
        as f32;
    let rstart_km = attr_f64(file, &where_path, "rstart").unwrap_or(0.0);
    let rscale_m = attr_f64(file, &where_path, "rscale").unwrap_or(0.0);
    let nyquist = attr_f64(file, &how_path, "NI")
        .or(root_nyquist)
        .map(|value| value as f32)
        .filter(|value| *value > 0.0);

    let mut data_names: Vec<String> = file
        .child_names(&format!("/{dataset}"))
        .into_iter()
        .filter(|name| {
            name.strip_prefix("data")
                .is_some_and(|rest| rest.parse::<u32>().is_ok())
        })
        .collect();
    data_names.sort_by_key(|name| name[4..].parse::<u32>().unwrap_or(u32::MAX));
    if data_names.is_empty() {
        return Err(invalid(format!("{dataset} has no dataM planes")));
    }

    // All planes in a sweep share ray geometry; read the first to size it.
    let first_plane = file.dataset(&format!("/{dataset}/{}/data", data_names[0]))?;
    let (nrays, nbins) = match first_plane.dims.as_slice() {
        [rays, bins] => (*rays, *bins),
        other => {
            return Err(invalid(format!(
                "{dataset} data has rank {} (need 2)",
                other.len()
            )));
        }
    };
    if nrays == 0 || nbins == 0 {
        return Err(invalid(format!("{dataset} data plane is empty")));
    }
    check_gate_count(nbins, dataset).map_err(OdimError::LimitExceeded)?;
    let ngates = u32::try_from(nbins).map_err(|_| invalid(format!("{dataset} nbins overflow")))?;
    let spacing_m = if rscale_m > 0.0 && rscale_m.is_finite() {
        rscale_m
    } else {
        1.0
    };
    let first_center_m = f64::from(first_gate_m_from_rstart(rstart_km)) + spacing_m / 2.0;

    let mut sweep = Sweep::new(index as u32, SweepMode::AzimuthSurveillance, elangle);
    sweep.follow_mode = Some(FollowMode::None);
    sweep.range = RangeCoord::Uniform {
        first_center_m,
        spacing_m,
        ngates,
    };
    budget
        .charge(nrays, 4 * size_of::<f64>(), "ODIM_H5 sweep rays")
        .map_err(OdimError::LimitExceeded)?;

    // Ray coordinates (xradar's rules; module docs).
    sweep.rays.azimuth_deg = match (
        attr_array(file, &how_path, "startazA"),
        attr_array(file, &how_path, "stopazA"),
    ) {
        (Some(start), stop)
            if start.len() == nrays && stop.as_ref().is_none_or(|s| s.len() == nrays) =>
        {
            let stop = stop.unwrap_or_else(|| {
                let mut next: Vec<f64> = start[1..].to_vec();
                next.push(start[0] + 360.0);
                next
            });
            start
                .iter()
                .zip(&stop)
                .map(|(start, stop)| {
                    let stop = if *stop < *start { stop + 360.0 } else { *stop };
                    let mut azimuth = (start + stop) / 2.0;
                    if azimuth >= 360.0 {
                        azimuth -= 360.0;
                    }
                    azimuth as f32
                })
                .collect()
        }
        _ => (0..nrays)
            .map(|ray| ((ray as f32 + 0.5) * 360.0 / nrays as f32).rem_euclid(360.0))
            .collect(),
    };
    sweep.rays.elevation_deg = match (
        attr_array(file, &how_path, "startelA"),
        attr_array(file, &how_path, "stopelA"),
    ) {
        (Some(start), Some(stop)) if start.len() == nrays && stop.len() == nrays => start
            .iter()
            .zip(&stop)
            .map(|(start, stop)| ((start + stop) / 2.0) as f32)
            .collect(),
        _ => match attr_array(file, &how_path, "elangles") {
            Some(angles) if angles.len() == nrays => {
                angles.iter().map(|angle| *angle as f32).collect()
            }
            _ => vec![elangle; nrays],
        },
    };
    let times = match (
        attr_array(file, &how_path, "startazT"),
        attr_array(file, &how_path, "stopazT"),
    ) {
        (Some(start), Some(stop)) if start.len() == nrays && stop.len() == nrays => start
            .iter()
            .zip(&stop)
            .map(|(start, stop)| (start + stop) / 2.0)
            .collect(),
        _ => ray_times_from_what(file, &what_path, &where_path, nrays),
    };
    sweep.rays.time_s = vec![0.0; nrays];
    if let Some(nyquist) = nyquist {
        sweep.ray_vars.nyquist_velocity_mps = Some(vec![nyquist; nrays]);
    }

    let mut skipped_planes = 0usize;
    let mut first_plane = Some(first_plane);
    for (plane_index, plane_name) in data_names.iter().enumerate() {
        let plane_what = format!("/{dataset}/{plane_name}/what");
        let quantity = file
            .attr(&plane_what, "quantity")
            .and_then(|attr| attr.as_str().map(str::to_owned))
            .unwrap_or_else(|| plane_name.to_uppercase());
        let gain = attr_f64(file, &plane_what, "gain").unwrap_or(1.0);
        let gain = if gain.abs() > 1.0e-9 { gain } else { 1.0 };
        let offset = attr_f64(file, &plane_what, "offset").unwrap_or(0.0);
        let nodata = attr_f64(file, &plane_what, "nodata");
        let undetect = attr_f64(file, &plane_what, "undetect");
        let plane = match (plane_index, first_plane.take()) {
            (0, Some(plane)) => plane,
            _ => file.dataset(&format!("/{dataset}/{plane_name}/data"))?,
        };
        if plane.dims.as_slice() != [nrays, nbins] {
            skipped_planes += 1;
            continue;
        }
        let name = FieldName::parse(&quantity);
        if sweep.field(&name).is_some() {
            // A second plane of the same quantity: malformed; the first wins.
            skipped_planes += 1;
            continue;
        }

        let word_bytes = match &plane.data {
            H5Data::U8(_) => 1,
            H5Data::U16(_) => 2,
            H5Data::F32(_) => 4,
            H5Data::F64(_) => 8,
        };
        budget
            .charge(nrays, nbins.saturating_mul(word_bytes), "ODIM_H5 field")
            .map_err(OdimError::LimitExceeded)?;
        let transform = LinearTransform::CfScaleOffset {
            scale_factor: gain,
            add_offset: offset,
            attr_width: FloatWidth::F64,
        };
        // Float planes with the identity packing hold physical values.
        let float_transform = (gain != 1.0 || offset != 0.0).then_some(transform);
        let data = match plane.data {
            H5Data::U8(values) => FieldData::U8 {
                values,
                coding: int_coding(transform, nodata, undetect),
            },
            H5Data::U16(values) => FieldData::U16 {
                values,
                coding: int_coding(transform, nodata, undetect),
            },
            H5Data::F32(values) => FieldData::F32 {
                values,
                coding: FloatCoding {
                    transform: float_transform,
                    fill_value: nodata.map(|value| value as f32),
                    undetect: undetect.map(|value| value as f32),
                },
            },
            H5Data::F64(values) => FieldData::F64 {
                values,
                coding: FloatCoding {
                    transform: float_transform,
                    fill_value: nodata,
                    undetect,
                },
            },
        };
        let mut field = Field::new(name, GateMapping::IDENTITY, ngates, data);
        if field.name == FieldName::Th {
            // ODIM TH is logarithmic total power in dBZ (design note 8.2,
            // note 1), whatever FM301 Table 301-9 says about the spelling.
            field.quantity = Quantity::TotalPower;
            field.attrs.units = Some("dBZ".into());
        }
        sweep
            .add_field(field)
            .map_err(|err| invalid(format!("{dataset}/{plane_name}: {err}")))?;
    }
    Ok((
        DecodedSweep {
            sweep,
            skipped_planes,
        },
        times,
    ))
}

/// Integer types ODIM planes are stored in, with the saturating `as` cast
/// the sentinel attributes need (writers disagree about whether `nodata` is
/// a long or a double).
trait OdimCode: PackedInt {
    fn from_f64(value: f64) -> Self;
}

impl OdimCode for u8 {
    fn from_f64(value: f64) -> Self {
        value as u8
    }
}

impl OdimCode for u16 {
    fn from_f64(value: f64) -> Self {
        value as u16
    }
}

/// Integer plane coding: `nodata` is `_FillValue`, `undetect` is
/// `_Undetect`. A plane that declares only `undetect` uses it as the fill
/// code too (the view needs one for padding).
fn int_coding<T: OdimCode>(
    transform: LinearTransform,
    nodata: Option<f64>,
    undetect: Option<f64>,
) -> IntCoding<T> {
    let nodata = nodata.map(T::from_f64);
    let undetect = undetect.map(T::from_f64);
    IntCoding {
        transform,
        fill_value: nodata.or(undetect),
        undetect,
        range_folded: None,
        valid_range: None,
    }
}

/// Ray times from the dataset's `what/startdate,starttime,enddate,endtime`
/// (seconds since the Unix epoch): spread evenly over the rays starting at
/// `where/a1gate`; every ray at `starttime` when start and end are equal.
fn ray_times_from_what(
    file: &H5File<'_>,
    what_path: &str,
    where_path: &str,
    nrays: usize,
) -> Vec<f64> {
    let start = parse_datetime_pair(file, what_path, "startdate", "starttime");
    let end = parse_datetime_pair(file, what_path, "enddate", "endtime").or(start);
    let (Some(start), Some(end)) = (start, end) else {
        return vec![f64::NAN; nrays];
    };
    let start = start.timestamp() as f64;
    let end = end.timestamp() as f64;
    if start == end {
        return vec![start; nrays];
    }
    let delta = (end - start) / nrays as f64;
    let a1gate = file
        .attr(where_path, "a1gate")
        .as_ref()
        .and_then(H5Attr::as_i64)
        .unwrap_or(0)
        .rem_euclid(nrays as i64) as usize;
    (0..nrays)
        .map(|ray| {
            // Storage index `ray` was radiated `(ray - a1gate) mod nrays`-th.
            let order = (ray + nrays - a1gate) % nrays;
            start + delta / 2.0 + order as f64 * delta
        })
        .collect()
}

/// A first bin starting this many km downrange is physically implausible
/// for a PVOL/SCAN: conformant writers put `rstart` at 0 or within a few km
/// (0.0 across the bejab/bewid/norst fixtures, 22 sweeps), and even
/// long-pulse blind ranges are single-digit km. Values beyond this are
/// metre-valued writer output (see `first_gate_m_from_rstart`).
const RSTART_SANE_MAX_KM: f64 = 20.0;

/// Metres to the start of the first bin, from the `where/rstart` attribute.
///
/// ODIM_H5 defines `rstart` in km (Table 5, polar "where"), but AEMET Spain
/// (IRIS 8.13/10.3 exports; all 11 sites surveyed on the OPERA ORD bucket,
/// 2026-07-07) writes it in METRES: observed 125/167/200 across the network.
/// Read as km those would start every ray 125–200 km downrange — past the
/// 150 km extent of the very sweeps they describe (299 bins x 500 m) — while
/// as metres they are classic IRIS range-start values on the same scale as
/// the bin spacing. A physical-sanity rule beats sniffing the IRIS source
/// string: other IRIS exports write conformant km, and any writer whose
/// first bin "starts" > [`RSTART_SANE_MAX_KM`] out is reporting metres.
/// (Metre-valued quirk output below the threshold is indistinguishable from
/// km, but IRIS range starts sit at gate-size scale — hundreds of metres —
/// and 0 reads identically in either unit.)
pub(crate) fn first_gate_m_from_rstart(rstart: f64) -> i32 {
    if rstart > RSTART_SANE_MAX_KM {
        // Reinterpret as metres (writer quirk documented above).
        rstart.round() as i32
    } else {
        (rstart * 1000.0).round() as i32
    }
}

/// A velocity gate within this distance of the plane's physical `offset` is
/// treated as sitting exactly on the collapsed no-data / zero code. The gate
/// spacing of any Doppler quantum (≈0.3 m/s for a 40 m/s Nyquist) is orders of
/// magnitude larger, so this only ever catches the exact `offset` fill.
const VELOCITY_OFFSET_EPS: f32 = 1.0e-6;

/// The `nodata` / `undetect` sentinels and physical offset a field's coding
/// declares, in physical units: the values the plane's `what` group wrote.
fn plane_sentinels(field: &Field) -> (Option<f64>, Option<f64>, f64) {
    fn packed<T: Copy + Into<f64>>(code: Option<T>, transform: LinearTransform) -> Option<f64> {
        code.map(|code| f64::from(transform.apply(code.into())))
    }
    match &field.data {
        FieldData::U8 { coding, .. } => (
            packed(coding.fill_value, coding.transform),
            packed(coding.undetect, coding.transform),
            coding.transform.add_offset(),
        ),
        FieldData::U16 { coding, .. } => (
            packed(coding.fill_value, coding.transform),
            packed(coding.undetect, coding.transform),
            coding.transform.add_offset(),
        ),
        FieldData::I8 { coding, .. } => (
            packed(coding.fill_value, coding.transform),
            packed(coding.undetect, coding.transform),
            coding.transform.add_offset(),
        ),
        FieldData::I16 { coding, .. } => (
            packed(coding.fill_value, coding.transform),
            packed(coding.undetect, coding.transform),
            coding.transform.add_offset(),
        ),
        FieldData::F32 { coding, .. } => {
            let offset = coding.transform.map_or(0.0, LinearTransform::add_offset);
            (
                coding.fill_value.map(f64::from),
                coding.undetect.map(f64::from),
                offset,
            )
        }
        FieldData::F64 { coding, .. } => {
            let offset = coding.transform.map_or(0.0, LinearTransform::add_offset);
            (coding.fill_value, coding.undetect, offset)
        }
    }
}

/// Recover no-echo Doppler-velocity gates that a copied-`what`-group writer
/// (AEMET Spain / IRIS 10.3) leaves decoding to a spurious `offset` (0 m/s).
/// Returns the number of gates masked.
///
/// Such writers stamp the velocity plane's `nodata`/`undetect` with the
/// REFLECTIVITY sentinels while filling no-echo velocity gates with the
/// physical `offset`, so those gates match no declared sentinel and decode
/// as a wall of 0 m/s (in xradar and Py-ART too). There is no per-plane
/// attribute that distinguishes the fill from a genuine 0 m/s reading, so
/// the file's own reflectivity no-echo mask is the only ground truth: a
/// velocity gate on `offset` with no co-located reflectivity echo is the
/// writer's collapsed no-data code; the same value where reflectivity IS
/// present is a real 0 m/s reading and is preserved.
///
/// This is an opt-in post-pass: [`read_odim_h5_volume`] stores every plane
/// verbatim. The velocity and reflectivity planes are the highest-priority
/// ones of each kind ([`canonical_field`]). Guarded by the copied-what-group
/// signature (velocity sentinels equal the reflectivity sentinels) so
/// conformant writers — which give velocity its own distinct sentinels,
/// already masked by the coding — are never touched. Also a no-op for a
/// sweep whose two planes do not share ray/gate geometry.
pub fn recover_copied_whatgroup_velocity_nodata(volume: &mut Volume) -> usize {
    volume
        .sweeps
        .iter_mut()
        .map(recover_sweep_velocity_nodata)
        .sum()
}

fn recover_sweep_velocity_nodata(sweep: &mut Sweep) -> usize {
    let (Some(velocity_name), Some(reflectivity_name)) = (
        canonical_field(sweep, CanonicalMoment::Velocity),
        canonical_field(sweep, CanonicalMoment::Reflectivity),
    ) else {
        return 0;
    };
    let (Some(velocity_index), Some(reflectivity_index)) = (
        sweep.field_index(&velocity_name),
        sweep.field_index(&reflectivity_name),
    ) else {
        return 0;
    };
    let (vel_nodata, vel_undetect, offset) = plane_sentinels(&sweep.fields[velocity_index]);
    let (ref_nodata, ref_undetect, _) = plane_sentinels(&sweep.fields[reflectivity_index]);
    // Copied-what-group signature: the velocity plane carries the reflectivity
    // plane's no-data sentinels verbatim (and at least one is present).
    let sentinels_copied = vel_nodata == ref_nodata
        && vel_undetect == ref_undetect
        && (vel_nodata.is_some() || vel_undetect.is_some());
    if !sentinels_copied {
        return 0;
    }
    // Mask in place (reflectivity borrowed, velocity taken out for the pass)
    // so no gate-index list proportional to the sweep is built.
    let mut velocity = std::mem::replace(
        &mut sweep.fields[velocity_index],
        Field::new(
            velocity_name.clone(),
            GateMapping::IDENTITY,
            0,
            FieldData::U8 {
                values: Vec::new(),
                coding: IntCoding::new(LinearTransform::IcdScaleOffset {
                    scale: 1.0,
                    offset: 0.0,
                }),
            },
        ),
    );
    let masked = mask_offset_fill_without_echo(
        &mut velocity,
        &sweep.fields[reflectivity_index],
        offset as f32,
    );
    sweep.fields[velocity_index] = velocity;
    masked
}

/// Set velocity gates that sit on the plane's physical `offset` where the
/// reflectivity field has no echo to the velocity field's fill code. A no-op
/// unless both fields share ray/gate geometry. Returns the gates masked.
fn mask_offset_fill_without_echo(velocity: &mut Field, reflectivity: &Field, offset: f32) -> usize {
    let (rows, gates) = velocity.shape();
    if reflectivity.shape() != (rows, gates) {
        return 0; // differing geometry: do not risk mis-masking
    }
    let mut masked = 0;
    for row in 0..rows {
        for gate in 0..gates {
            // Reflectivity no-echo: no value (sentinel) or NaN.
            let ref_no_echo = reflectivity.value(row, gate).is_none_or(|z| !z.is_finite());
            if !ref_no_echo {
                continue;
            }
            if let Some(value) = velocity.value(row, gate)
                && value.is_finite()
                && (value - offset).abs() <= VELOCITY_OFFSET_EPS
            {
                mask_gate_no_data(velocity, row * gates + gate);
                masked += 1;
            }
        }
    }
    masked
}

/// Set one flat gate index to the field's fill code: `_FillValue` for
/// integer storage (unchanged when the plane declares none — nothing
/// transparent to write), `_FillValue` else NaN for float storage.
fn mask_gate_no_data(field: &mut Field, index: usize) {
    match &mut field.data {
        FieldData::U8 { values, coding } => {
            if let (Some(fill), Some(slot)) = (coding.fill_value, values.get_mut(index)) {
                *slot = fill;
            }
        }
        FieldData::U16 { values, coding } => {
            if let (Some(fill), Some(slot)) = (coding.fill_value, values.get_mut(index)) {
                *slot = fill;
            }
        }
        FieldData::I8 { values, coding } => {
            if let (Some(fill), Some(slot)) = (coding.fill_value, values.get_mut(index)) {
                *slot = fill;
            }
        }
        FieldData::I16 { values, coding } => {
            if let (Some(fill), Some(slot)) = (coding.fill_value, values.get_mut(index)) {
                *slot = fill;
            }
        }
        FieldData::F32 { values, coding } => {
            if let Some(slot) = values.get_mut(index) {
                *slot = coding.fill_code();
            }
        }
        FieldData::F64 { values, coding } => {
            if let Some(slot) = values.get_mut(index) {
                *slot = coding.fill_code();
            }
        }
    }
}

/// Canonical moment kinds of ODIM quantity codes (spec Table 16), used to
/// pick the reflectivity and velocity planes of the copied-what-group
/// recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum CanonicalMoment {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    SpecificDifferentialPhase,
}

/// Map ODIM quantity codes (spec Table 16) onto the canonical moment set.
pub(crate) fn canonical_quantity(quantity: &str) -> Option<CanonicalMoment> {
    match quantity {
        "DBZH" | "DBZV" | "TH" | "TV" | "DBZ" => Some(CanonicalMoment::Reflectivity),
        "VRADH" | "VRADV" | "VRAD" | "VRADDH" => Some(CanonicalMoment::Velocity),
        "WRADH" | "WRADV" | "WRAD" => Some(CanonicalMoment::SpectrumWidth),
        // The unfiltered dual-pol spellings come in both orders in the
        // wild: spec-style trailing U (ZDRU) and DWD's leading U (UZDR,
        // URHOHV — live opendata.dwd.de sweep files, 2026-06-12).
        "ZDR" | "ZDRU" | "UZDR" => Some(CanonicalMoment::DifferentialReflectivity),
        "RHOHV" | "RHOHVU" | "URHOHV" => Some(CanonicalMoment::CorrelationCoefficient),
        "PHIDP" | "PHIDPU" | "UPHIDP" => Some(CanonicalMoment::DifferentialPhase),
        "KDP" | "KDPU" => Some(CanonicalMoment::SpecificDifferentialPhase),
        _ => None,
    }
}

pub(crate) fn canonical_quantity_priority(quantity: &str) -> u8 {
    match quantity {
        // Prefer filtered reflectivity when a file carries both DBZH and
        // unfiltered TH/TV. ODIM writers do not guarantee plane order.
        "DBZH" | "DBZV" => 30,
        "DBZ" => 25,
        "TH" | "TV" => 10,
        // Prefer filtered dual-pol spellings over explicitly unfiltered ones.
        "ZDR" | "RHOHV" | "PHIDP" | "KDP" => 30,
        "ZDRU" | "UZDR" | "RHOHVU" | "URHOHV" | "PHIDPU" | "UPHIDP" | "KDPU" => 10,
        _ => 20,
    }
}

/// The field of `sweep` that best represents `moment`: the plane with the
/// highest [`canonical_quantity_priority`], earliest first among equals.
pub(crate) fn canonical_field(sweep: &Sweep, moment: CanonicalMoment) -> Option<FieldName> {
    let mut best: Option<(u8, &FieldName)> = None;
    for field in &sweep.fields {
        let name = field.name.as_str();
        if canonical_quantity(name) != Some(moment) {
            continue;
        }
        let priority = canonical_quantity_priority(name);
        if best.is_none_or(|(existing, _)| priority > existing) {
            best = Some((priority, &field.name));
        }
    }
    best.map(|(_, name)| name.clone())
}

/// Identifiers from the `/what` `source` attribute.
pub(crate) struct SiteIdentity {
    pub id: String,
    pub name: Option<String>,
    pub wmo: Option<String>,
    pub wigos: Option<String>,
}

/// Pick site id + display name out of the `/what` `source` attribute:
/// comma-separated "TYP:value" identifier pairs (spec Table 3), e.g.
/// "WMO:02606,RAD:SE50,PLC:Karlskrona,NOD:sekkr". Preference is
/// NOD > RAD > WMO regardless of pair order — operational files (RMI
/// Belgium, met.no) list WMO first but NOD is the canonical OPERA site
/// code (validated against bejab/norst sample volumes).
pub(crate) fn site_identity_from_source(source: &str) -> SiteIdentity {
    let (mut nod, mut rad, mut wmo, mut name, mut wigos) = (None, None, None, None, None);
    for pair in source.split(',') {
        let Some((key, value)) = pair.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "NOD" => nod = Some(value.to_uppercase()),
            "RAD" => rad = Some(value.to_owned()),
            "WMO" => wmo = Some(value.to_owned()),
            "PLC" => name = Some(value.to_owned()),
            "WIGOS" => wigos = Some(value.to_owned()),
            _ => {}
        }
    }
    let id = nod
        .or(rad)
        .or_else(|| wmo.clone())
        .unwrap_or_else(|| "ODIM".to_owned());
    SiteIdentity {
        id,
        name,
        wmo,
        wigos,
    }
}

fn parse_datetime(file: &H5File<'_>, group: &str) -> Option<DateTime<Utc>> {
    parse_datetime_pair(file, group, "date", "time")
}

fn parse_datetime_pair(
    file: &H5File<'_>,
    group: &str,
    date_attr: &str,
    time_attr: &str,
) -> Option<DateTime<Utc>> {
    let date = file.attr(group, date_attr)?.as_str()?.to_owned();
    let time = file.attr(group, time_attr)?.as_str()?.to_owned();
    let date = NaiveDate::parse_from_str(&date, "%Y%m%d").ok()?;
    let time = NaiveTime::parse_from_str(&time, "%H%M%S").ok()?;
    Some(Utc.from_utc_datetime(&NaiveDateTime::new(date, time)))
}

fn attr_f64(file: &H5File<'_>, path: &str, name: &str) -> Option<f64> {
    file.attr(path, name).as_ref().and_then(H5Attr::as_f64)
}

/// A numeric array attribute as f64 (a scalar counts as a one-element array).
fn attr_array(file: &H5File<'_>, path: &str, name: &str) -> Option<Vec<f64>> {
    match file.attr(path, name)? {
        H5Attr::F64Array(values) => Some(values),
        H5Attr::I64Array(values) => Some(values.into_iter().map(|v| v as f64).collect()),
        H5Attr::F64(value) => Some(vec![value]),
        H5Attr::I64(value) => Some(vec![value as f64]),
        H5Attr::Str(_) => None,
    }
}

fn odim_radar_frequency_mhz(file: &H5File<'_>) -> Option<f64> {
    for name in ["frequency", "freq", "radar_frequency", "radar_frequency_hz"] {
        if let Some(value) = attr_f64(file, "/how", name)
            && let Some(mhz) = normalize_frequency_mhz(value)
        {
            return Some(mhz);
        }
    }
    for name in ["wavelength", "radar_wavelength", "wavelength_cm"] {
        if let Some(value) = attr_f64(file, "/how", name)
            && let Some(mhz) = frequency_mhz_from_wavelength(value)
        {
            return Some(mhz);
        }
    }
    None
}

fn normalize_frequency_mhz(value: f64) -> Option<f64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let mhz = if value > 1.0e6 {
        value / 1.0e6
    } else if value > 1000.0 {
        value
    } else {
        value * 1000.0
    };
    (1000.0..=12_000.0).contains(&mhz).then_some(mhz)
}

fn frequency_mhz_from_wavelength(value: f64) -> Option<f64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let meters = if value > 1.0 { value / 100.0 } else { value };
    let mhz = 299.792_458 / meters;
    (1000.0..=12_000.0).contains(&mhz).then_some(mhz)
}

pub(crate) fn invalid(reason: impl Into<String>) -> OdimError {
    OdimError::InvalidMessage {
        offset: 0,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantity_codes_map_to_moments() {
        assert_eq!(
            canonical_quantity("DBZH"),
            Some(CanonicalMoment::Reflectivity)
        );
        assert_eq!(canonical_quantity("VRADH"), Some(CanonicalMoment::Velocity));
        assert_eq!(
            canonical_quantity("WRADH"),
            Some(CanonicalMoment::SpectrumWidth)
        );
        assert_eq!(
            canonical_quantity("RHOHV"),
            Some(CanonicalMoment::CorrelationCoefficient)
        );
        assert_eq!(canonical_quantity("QIND"), None);
    }

    /// Met Eireann Shannon carries DBZH, TH and VRADH on every sweep: the
    /// filtered DBZH is the reflectivity whatever the dataset order (the real
    /// sweep with its fields reversed), VRADH the velocity, and no quantity
    /// is spectrum width.
    #[test]
    fn filtered_odim_quantities_win_duplicate_canonical_moments() {
        let bytes = recast_radar_testdata::bytes("odim-iesha-20260305-0115-pvol")
            .unwrap_or_else(|err| panic!("{err}"));
        let volume = read_odim_h5_volume(&bytes).unwrap();
        let mut sweep = volume.sweeps[0].clone();
        let names: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["DBZH", "TH", "VRADH"]);
        for reversed in [false, true] {
            if reversed {
                sweep.fields.reverse();
                assert_eq!(sweep.fields[1].name.as_str(), "TH");
            }
            assert_eq!(
                canonical_field(&sweep, CanonicalMoment::Reflectivity),
                Some(FieldName::Dbzh)
            );
            assert_eq!(
                canonical_field(&sweep, CanonicalMoment::Velocity),
                Some(FieldName::Vradh)
            );
            assert_eq!(
                canonical_field(&sweep, CanonicalMoment::SpectrumWidth),
                None
            );
        }
        assert!(canonical_quantity_priority("DBZH") > canonical_quantity_priority("TH"));
    }

    #[test]
    fn site_identity_prefers_nod_over_wmo_regardless_of_pair_order() {
        // Real operational source strings put WMO first; NOD must win.
        let identity = site_identity_from_source(
            "WMO:06410,RAD:BX42,PLC:Jabbeke,NOD:bejab,CTY:605,CMT:bejab_scan_v3_Z_dBZ",
        );
        assert_eq!(identity.id, "BEJAB");
        assert_eq!(identity.name.as_deref(), Some("Jabbeke"));
        assert_eq!(identity.wmo.as_deref(), Some("06410"));
        // No NOD: fall back RAD, then WMO; empty values are skipped.
        let identity = site_identity_from_source("RAD:AU40,PLC:CapFlat,CTY:500,STN:70341");
        assert_eq!(identity.id, "AU40");
        let identity = site_identity_from_source("WMO:01104,NOD:");
        assert_eq!(identity.id, "01104");
        let identity = site_identity_from_source("CMT:whatever");
        assert_eq!(identity.id, "ODIM");
    }

    #[test]
    fn rstart_beyond_sane_range_reinterprets_as_metres() {
        // Spec-conformant km values pass through unchanged.
        assert_eq!(first_gate_m_from_rstart(0.0), 0);
        assert_eq!(first_gate_m_from_rstart(0.05), 50);
        assert_eq!(first_gate_m_from_rstart(5.0), 5_000);
        // Just under the physical-sanity bound: still km.
        assert_eq!(first_gate_m_from_rstart(19.9), 19_900);
        assert_eq!(first_gate_m_from_rstart(20.0), 20_000);
        // AEMET metre-valued rstart (125/167/200 observed across the
        // network, 2026-07-07): reinterpreted as metres, not 125+ km.
        assert_eq!(first_gate_m_from_rstart(125.0), 125);
        assert_eq!(first_gate_m_from_rstart(167.0), 167);
        assert_eq!(first_gate_m_from_rstart(200.0), 200);
    }

    #[test]
    fn integer_coding_keeps_nodata_and_undetect_apart() {
        let coding: IntCoding<u8> = int_coding(
            LinearTransform::CfScaleOffset {
                scale_factor: 0.5,
                add_offset: -32.0,
                attr_width: FloatWidth::F64,
            },
            Some(255.0),
            Some(0.0),
        );
        assert_eq!(coding.fill_value, Some(255));
        assert_eq!(coding.undetect, Some(0));
        // Only `undetect` declared: it doubles as the fill code.
        let coding: IntCoding<u16> = int_coding(
            LinearTransform::CfScaleOffset {
                scale_factor: 1.0,
                add_offset: 0.0,
                attr_width: FloatWidth::F64,
            },
            None,
            Some(3.0),
        );
        assert_eq!(coding.fill_value, Some(3));
        assert_eq!(coding.undetect, Some(3));
    }

    // ----- copied-what-group velocity recovery on real AEMET planes --------
    //
    // Input: corpus entry `odim-espdg-20260707-1927-pvol-dbzh-vradh` (AEMET
    // Perdiguera, IRIS 10.3 export). Both datasets stamp the DBZH `what`
    // sentinels (nodata 95.5, undetect -32.0, offset 0.0, gain 1.0) onto
    // VRADH, whose no-echo gates hold the offset (0 m/s). The decoder keeps
    // the datasets in file order: sweep 0 is dataset1 (1.5 deg), sweep 1 is
    // dataset2 (0.5 deg).
    //
    // Expected values: tools/golden_io_formats.py, section `odim`, keys
    // `espdg_recovery` (h5py raw planes: a fill gate is DBZH no-echo and VRADH
    // on offset; a genuine zero is DBZH echo and VRADH on offset) and
    // `espdg_distinct_sentinel_mutation` (the file offset of dataset2 VRADH
    // what/nodata, found by editing candidates and reading them back with
    // libhdf5; the v2 object-header checksum recomputed with lookup3; h5py
    // reads the edited file).

    const ESPDG: &str = "odim-espdg-20260707-1927-pvol-dbzh-vradh";
    /// dataset2 VRADH what/nodata f64 value (95.5) and its OHDR checksum.
    const VRADH_NODATA_OFFSET: usize = 101_591;
    const VRADH_OHDR_CHECKSUM_OFFSET: usize = 101_629;
    const ORIGINAL_CHECKSUM: u32 = 320_803_438;
    const DISTINCT_NODATA: f64 = -9999.0;
    const DISTINCT_CHECKSUM: u32 = 3_075_319_015;
    /// Sweep indexes of the two datasets.
    const DATASET1: usize = 0;
    const DATASET2: usize = 1;

    fn espdg_bytes() -> Vec<u8> {
        recast_radar_testdata::bytes(ESPDG).unwrap_or_else(|err| panic!("{err}"))
    }

    /// The same file with dataset2 (0.5 deg) VRADH what/nodata rewritten to a
    /// value no gate holds: a writer that gives velocity its own sentinels.
    fn espdg_with_distinct_velocity_nodata() -> Vec<u8> {
        let mut bytes = espdg_bytes();
        let nodata = &bytes[VRADH_NODATA_OFFSET..VRADH_NODATA_OFFSET + 8];
        assert_eq!(f64::from_le_bytes(nodata.try_into().unwrap()), 95.5);
        let checksum = &bytes[VRADH_OHDR_CHECKSUM_OFFSET..VRADH_OHDR_CHECKSUM_OFFSET + 4];
        assert_eq!(
            u32::from_le_bytes(checksum.try_into().unwrap()),
            ORIGINAL_CHECKSUM
        );
        bytes[VRADH_NODATA_OFFSET..VRADH_NODATA_OFFSET + 8]
            .copy_from_slice(&DISTINCT_NODATA.to_le_bytes());
        bytes[VRADH_OHDR_CHECKSUM_OFFSET..VRADH_OHDR_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&DISTINCT_CHECKSUM.to_le_bytes());
        bytes
    }

    fn velocity(sweep: &Sweep) -> &Field {
        sweep.field(&FieldName::Vradh).expect("VRADH")
    }

    /// (valid gates, missing flat-index sum, missing flat-index square sum,
    /// 0 m/s gates, 0 m/s flat-index sum) of a plane, with flat index
    /// `ray * ngates + gate`.
    fn velocity_summary(field: &Field) -> (usize, u64, u64, usize, u64) {
        let ngates = field.ngates as usize;
        let mut summary = (0usize, 0u64, 0u64, 0usize, 0u64);
        for ray in 0..field.nrays as usize {
            for gate in 0..ngates {
                let index = (ray * ngates + gate) as u64;
                match field.value(ray, gate) {
                    Some(value) => {
                        summary.0 += 1;
                        if value == 0.0 {
                            summary.3 += 1;
                            summary.4 += index;
                        }
                    }
                    None => {
                        summary.1 += index;
                        summary.2 += index * index;
                    }
                }
            }
        }
        summary
    }

    /// golden espdg_recovery.dataset2: 89237 fill gates masked (index sum
    /// 4625669116, square sum 326315664938928), 12869 genuine 0 m/s gates
    /// with echo kept (index sum 821953830), 18403 valid gates left.
    const DATASET2_RECOVERED: (usize, u64, u64, usize, u64) = (
        18_403,
        4_625_669_116,
        326_315_664_938_928,
        12_869,
        821_953_830,
    );
    /// golden espdg_recovery.dataset1: 90846 fill gates, 8141 genuine zeros,
    /// 16794 valid gates.
    const DATASET1_RECOVERED: (usize, u64, u64, usize, u64) = (
        16_794,
        4_709_191_624,
        333_031_625_592_866,
        8_141,
        511_192_353,
    );

    #[test]
    fn copied_whatgroup_recovery_masks_only_no_echo_offset_gates() {
        // The decoder stores the planes verbatim: the 0.5 deg VRADH plane
        // decodes with the full 0 m/s wall, 107640 valid gates, 102106 of
        // them zero (89237 fill + 12869 genuine).
        let mut volume = read_odim_h5_volume(&espdg_bytes()).expect("decode espdg");
        assert!((volume.sweeps[DATASET1].fixed_angle_deg - 1.5).abs() < 0.01);
        let low = &volume.sweeps[DATASET2];
        assert!((low.fixed_angle_deg - 0.5).abs() < 0.01);
        let before = velocity_summary(velocity(low));
        assert_eq!((before.0, before.1, before.3), (107_640, 0, 102_106));
        // Velocity carries the reflectivity sentinels: the copied-what-group
        // signature.
        let dbzh = low.field(&FieldName::Dbzh).expect("DBZH");
        assert_eq!(plane_sentinels(velocity(low)), plane_sentinels(dbzh));
        assert_eq!(plane_sentinels(dbzh), (Some(95.5), Some(-32.0), 0.0));

        let masked = recover_copied_whatgroup_velocity_nodata(&mut volume);

        assert_eq!(masked, 89_237 + 90_846);
        let low = velocity(&volume.sweeps[DATASET2]);
        assert_eq!(velocity_summary(low), DATASET2_RECOVERED);
        // First fill gate (0,0) masked; first genuine zero (0,32) kept.
        assert_eq!(low.value(0, 0), None);
        assert_eq!(low.value(0, 32), Some(0.0));
        assert_eq!(
            velocity_summary(velocity(&volume.sweeps[DATASET1])),
            DATASET1_RECOVERED
        );
        // A second pass finds nothing left to mask.
        assert_eq!(recover_copied_whatgroup_velocity_nodata(&mut volume), 0);
    }

    #[test]
    fn distinct_velocity_sentinels_are_never_reflectivity_gated() {
        // dataset2 VRADH now declares nodata -9999.0 while DBZH keeps 95.5: a
        // conformant writer. golden espdg_distinct_sentinel_mutation: h5py
        // reads 107640 non-sentinel VRADH gates, 102106 of them 0 m/s.
        let edited = espdg_with_distinct_velocity_nodata();
        let mut volume = read_odim_h5_volume(&edited).expect("decode edited espdg");
        let low = &volume.sweeps[DATASET2];
        let summary = velocity_summary(velocity(low));
        assert_eq!((summary.0, summary.1, summary.3), (107_640, 0, 102_106));
        assert_eq!(plane_sentinels(velocity(low)).0, Some(DISTINCT_NODATA));
        // Co-located no-echo reflectivity at (0,0) does not mask velocity.
        assert_eq!(low.field(&FieldName::Dbzh).expect("DBZH").value(0, 0), None);
        assert_eq!(velocity(low).value(0, 0), Some(0.0));

        // The recovery leaves the edited sweep alone and still recovers the
        // unedited 1.5 deg sweep, which carries the copied sentinels.
        let masked = recover_copied_whatgroup_velocity_nodata(&mut volume);
        assert_eq!(masked, 90_846);
        assert_eq!(
            velocity_summary(velocity(&volume.sweeps[DATASET2])),
            summary
        );
        assert_eq!(
            velocity_summary(velocity(&volume.sweeps[DATASET1])),
            DATASET1_RECOVERED
        );
    }
}
