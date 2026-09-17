#!/usr/bin/env python3
"""Golden values for the real-data tests of recast-radar-retrieve.

The Rust tests decode real corpus files with the workspace readers and compare what the
retrieval algorithms compute against the JSON files this script writes:

    testdata/golden/retrieve/availability.json  crates/recast-radar-retrieve/tests/availability_real.rs
    testdata/golden/retrieve/detect.json        crates/recast-radar-retrieve/tests/detect_real.rs
    testdata/golden/retrieve/gbvtd.json         crates/recast-radar-retrieve/tests/gbvtd_real.rs
    testdata/golden/retrieve/shear.json         crates/recast-radar-retrieve/tests/shear_real.rs
    testdata/golden/retrieve/sweep.json         crates/recast-radar-retrieve/tests/sweep_real.rs
    testdata/golden/retrieve/volume.json        crates/recast-radar-retrieve/tests/volume_real.rs
    testdata/golden/retrieve/vwp.json           crates/recast-radar-retrieve/tests/vwp_real.rs

Every input value comes from a reader that is independent of recast-radar-tools:

- NEXRAD Level II: MetPy 1.7.1 ``metpy.io.Level2File`` (ray azimuth, elevation and Nyquist
  velocity, moment gate geometry and scaled gate values) and Py-ART 2.2.5
  ``pyart.io.read_nexrad_archive`` (fields, ray times, radar position),
  ``pyart.correct.dealias_region_based``, ``pyart.retrieve.compute_cdr``,
  ``pyart.retrieve.kdp_vulpiani`` and ``pyart.retrieve.vad_browning``.
- Published storm data: the NHC HURDAT2 best track (``hurdat2-1851-2024-040425.txt``) for
  the hurricane centres, intensities and radii of maximum wind, and the SPC tornado database
  (``1950-2024_actual_tornadoes.csv``) for tornado path start/end points and start times.
  The rows used are copied into the golden files verbatim.
- DORADE: a block walker (RADD/PARM/CELV/CSFD/SWIB/RYIB/RDAT) written from the format
  description, not from the Rust reader.

The expected outputs are computed here with numpy from those inputs, by reference
implementations of the documented algorithms (module docs of the Rust sources):

- LLSD velocity derivatives (Smith and Elmore 2004): least-squares slope of radial velocity
  over a 3 radial x 3 gate window against cross-radial arc distance (azimuthal shear) or
  along-radial distance (radial divergence), at least 4 samples, x1000;
- the PHIDP phase bundle: RHOHV/reflectivity gating, 360-degree unwrapping across gaps of at
  most 2 gates, linear fill of those gaps, a Hampel filter (half window 3, 3 sigma), and a
  Huber-weighted (k = 1.5, 3 iterations) linear fit over a 3 km (13 gate) window; PHIF is
  the fitted intercept and KDP half the slope, kept inside the S-band bounds [-2, 14] deg/km;
- the velocity range gradient: central difference over the nearest finite gates within 2 on
  each side, with the difference wrapped to the ray's +-Nyquist interval;
- the circular depolarization ratio (Matrosov 2004), as Py-ART's ``compute_cdr``;
- column products: the lowest reflectivity tilt's azimuths and ground ranges walked through
  every reflectivity tilt (nearest azimuth, nearest ground-range gate, 4/3-Earth beam
  geometry), giving the column maximum, echo base/top (Z >= threshold) and echo depth;
- the VAD wind (Browning and Wexler 1968) exactly as documented in ``vwp.rs``: one median per
  radial over the range annulus, one sample per integer azimuth degree, a first-harmonic fit
  with intercept, robust trimming at 3 robust sigma clamped to [3, 12] m/s, and a refit;
- the GBVTD ring fit (Lee et al. 1999): 72 samples per ring about a known centre, nearest
  gate/radial sampling, least squares for the axisymmetric VT/VR after removing the
  beam-projected storm motion, and the wavenumber-1 tangential terms fit on the residual.

Where the Rust code computes in f32 (azimuths, wrapped deltas, Hampel medians), the
reference does the same arithmetic in float32.

Test files are read from the committed corpus (testdata/files) and from the shared download
cache that recast-radar-testdata fills (%LOCALAPPDATA%\\recast-radar-tools\\testdata, or
$RECAST_RADAR_TESTDATA); every file is checked against its manifest sha256. Run
``cargo test -p recast-radar-retrieve`` once to download the full volumes. The HURDAT2 and
SPC files are downloaded into the cache directory on first use.

Usage:
    python tools/retrieve_golden.py [availability|detect|gbvtd|shear|sweep|volume|vwp ...]

With no arguments every golden file is regenerated. The committed files were written with
Python 3.13, numpy 2.5.3, MetPy 1.7.1 and Py-ART 2.2.5.
"""

import csv
import gzip
import hashlib
import io
import json
import logging
import math
import os
import struct
import sys
import tomllib
import urllib.request
import warnings
from datetime import datetime, timedelta, timezone
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")
logging.getLogger("metpy.io.nexrad").setLevel(logging.ERROR)

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden"

F32 = np.float32
RNG_SEED = 20260916

HURDAT2_URL = "https://www.nhc.noaa.gov/data/hurdat/hurdat2-1851-2024-040425.txt"
SPC_URL = "https://www.spc.noaa.gov/wcm/data/1950-2024_actual_tornadoes.csv"


# ----------------------------------------------------------------- corpus ---

def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"]
    paths += sorted(p / "manifest.toml" for p in TESTDATA.iterdir()
                    if p.is_dir() and (p / "manifest.toml").is_file())
    for path in paths:
        with open(path, "rb") as fh:
            files += tomllib.load(fh).get("file", [])
    return {entry["id"]: entry for entry in files}


MANIFEST = load_manifest()


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    for var, suffix in (("LOCALAPPDATA", ()), ("XDG_CACHE_HOME", ()), ("HOME", (".cache",))):
        if var == "LOCALAPPDATA" and os.name != "nt":
            continue
        if os.environ.get(var):
            return Path(os.environ[var], *suffix, "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_path(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        rel = Path(entry["committed"])
        path = ROOT / rel if rel.parts[0] == "testdata" else TESTDATA / rel
    else:
        path = cache_dir() / entry_id
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        raise SystemExit(f"{entry_id}: sha256 {digest} != manifest {entry['sha256']}")
    return path


def reference_file(url):
    """A published reference table, downloaded once into the cache directory."""
    path = cache_dir() / "reference" / url.rsplit("/", 1)[1]
    if not path.is_file():
        path.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(url, timeout=120) as response:
            path.write_bytes(response.read())
    return path


def format_json(value, indent=0):
    """JSON with objects indented one key per line and arrays of numbers (or of short
    arrays) on a single line."""
    pad = " " * indent
    if isinstance(value, dict):
        if not value:
            return "{}"
        items = [f'{pad} {json.dumps(key)}: {format_json(item, indent + 1)}'
                 for key, item in value.items()]
        return "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(value, list) and any(isinstance(item, dict) for item in value):
        items = [f"{pad} {format_json(item, indent + 1)}" for item in value]
        return "[\n" + ",\n".join(items) + "\n" + pad + "]"
    return json.dumps(value, allow_nan=False, separators=(",", ":"))


def write_golden(relative, payload):
    path = GOLDEN / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    text = format_json(payload)
    json.loads(text)
    path.write_text(text + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {path.relative_to(ROOT)} ({len(text)} bytes)")


def jf(value, digits=None):
    """JSON float: None for NaN."""
    if value is None:
        return None
    value = float(value)
    if not math.isfinite(value):
        return None
    return round(value, digits) if digits is not None else value


def jlist(values, digits=None):
    return [jf(v, digits) for v in values]


def sample_indices(count, limit, seed_offset=0):
    if count <= limit:
        return np.arange(count)
    rng = np.random.default_rng(RNG_SEED + seed_offset)
    return np.sort(rng.choice(count, size=limit, replace=False))


# ------------------------------------------------------- f32 arithmetic ---

def rem_euclid32(x, m=360.0):
    """Rust f32::rem_euclid."""
    x = np.asarray(x, dtype=F32)
    m = F32(m)
    r = np.fmod(x, m)
    return np.where(r < 0, (r + m).astype(F32), r).astype(F32)


def ang_dist32(a, b):
    """volume.rs angular_distance: min(d, 360 - d) with d = |a - b|.rem_euclid(360), f32."""
    d = rem_euclid32(np.abs(np.asarray(a, dtype=F32) - np.asarray(b, dtype=F32)))
    return np.minimum(d, (F32(360.0) - d).astype(F32)).astype(F32)


def wrapped_delta32(delta, period):
    """sweep.rs wrapped_delta: (delta + period/2).rem_euclid(period) - period/2 in f32."""
    period = F32(period)
    half = F32(0.5) * period
    return (rem_euclid32(np.asarray(delta, dtype=F32) + half, period) - half).astype(F32)


def median32(values):
    """sweep.rs median_f32_mut: sorted f32 median, mean of the middle pair for even counts."""
    values = np.sort(np.asarray(values, dtype=F32))
    n = len(values)
    if n == 0:
        return None
    if n % 2 == 0:
        return F32(F32(0.5) * (values[n // 2 - 1] + values[n // 2]))
    return values[n // 2]


# ------------------------------------------------------------ beam geometry ---

EARTH_RADIUS_M = 6_371_000.0
AE_M = EARTH_RADIUS_M * 4.0 / 3.0


def beam_height_m(slant_m, elevation_deg):
    """Doviak and Zrnic (1993) eq. 2.28b, 4/3-Earth beam-centre height above the radar."""
    theta = math.radians(elevation_deg)
    return math.sqrt(slant_m * slant_m + AE_M * AE_M + 2.0 * slant_m * AE_M * math.sin(theta)) - AE_M


def beam_ground_range_m(slant_m, elevation_deg):
    """Doviak and Zrnic (1993) eq. 2.28c."""
    theta = math.radians(elevation_deg)
    h = beam_height_m(slant_m, elevation_deg)
    return AE_M * math.asin((slant_m * math.cos(theta)) / (AE_M + h))


# --------------------------------------------------------- Level II (MetPy) ---

MSG31_NAMES = {b"REF": "REF", b"VEL": "VEL", b"SW ": "SW", b"SW": "SW", b"ZDR": "ZDR",
               b"PHI": "PHI", b"RHO": "RHO", b"CFP": "CFP"}


def level2_sweeps(entry_id):
    """Sweeps of a Level II file as read by MetPy: per sweep the ray azimuths and elevations
    (f32 values of the file's angle fields), the Nyquist velocity of each ray, and per moment
    the rows (ray indices) carrying it, the gate geometry in metres and the scaled values
    (NaN below code 2, i.e. no data and range folded)."""
    from metpy.io import Level2File

    raw = corpus_path(entry_id).read_bytes()
    if raw[:2] == bytes((0x1F, 0x8B)):
        raw = gzip.decompress(raw)
    f = Level2File(io.BytesIO(raw))
    sweeps = []
    for rays in f.sweeps:
        az, el, nyq = [], [], []
        moments = {}
        for index, ray in enumerate(rays):
            header = ray[0]
            az.append(header.az_angle)
            el.append(header.el_angle)
            if len(ray) == 5:
                blocks = {MSG31_NAMES.get(k, k.decode().strip()): v for k, v in ray[4].items()}
                nyq.append(ray[3].nyq_vel)
            else:
                blocks = dict(ray[1])
                nyq.append(header.nyq_vel)
            for name, (hdr, data) in blocks.items():
                m = moments.setdefault(name, {"rows": [], "data": [], "first": [], "spacing": []})
                m["rows"].append(index)
                m["data"].append(np.asarray(data, dtype=np.float64))
                m["first"].append(int(round(hdr.first_gate * 1000.0)))
                m["spacing"].append(int(round(hdr.gate_width * 1000.0)))
        for name, m in moments.items():
            if len(set(m["first"])) != 1 or len(set(m["spacing"])) != 1:
                raise SystemExit(f"{entry_id}: {name} gate geometry changes inside a sweep")
            gates = max(len(d) for d in m["data"])
            grid = np.full((len(m["rows"]), gates), np.nan)
            for row, d in enumerate(m["data"]):
                grid[row, :len(d)] = d
            m.update(first_gate_m=m["first"][0], gate_spacing_m=m["spacing"][0],
                     gate_count=gates, values=grid, rows=np.asarray(m["rows"]))
            del m["data"], m["first"], m["spacing"]
        sweeps.append({"az": np.asarray(az, dtype=F32), "el": np.asarray(el, dtype=F32),
                       "nyquist": np.asarray(nyq, dtype=np.float64), "moments": moments})
    return sweeps, f


def pyart_radar(entry_id):
    import pyart

    path = corpus_path(entry_id)
    raw = path.read_bytes()
    if raw[:2] == bytes((0x1F, 0x8B)):
        # Py-ART detects gzip from the file name; the cache file has no extension.
        return pyart.io.read_nexrad_archive(io.BytesIO(gzip.decompress(raw)))
    return pyart.io.read_nexrad_archive(str(path))


def pyart_volume_time(radar):
    units = radar.time["units"]
    return datetime.strptime(units, "seconds since %Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def pyart_sweep_field(radar, sweep, field):
    """A sweep of a Py-ART field as float64 with masked gates NaN."""
    return np.ma.filled(radar.get_field(sweep, field).astype(np.float64), np.nan)


def pyart_dealiased(radar):
    """Py-ART region-based dealiasing of the whole volume, masked gates NaN."""
    import pyart

    de = pyart.correct.dealias_region_based(radar, vel_field="velocity", keep_original=False)
    return np.ma.filled(de["data"].astype(np.float64), np.nan)


def radar_xy_km(radar, lat, lon):
    """Radar-relative east/north km of a point (azimuthal equidistant, Py-ART)."""
    import pyart

    x, y = pyart.core.geographic_to_cartesian_aeqd(
        np.asarray([lon]), np.asarray([lat]),
        float(radar.longitude["data"][0]), float(radar.latitude["data"][0]))
    return float(x[0]) / 1000.0, float(y[0]) / 1000.0


def az_range_from_xy(x_km, y_km):
    return math.degrees(math.atan2(x_km, y_km)) % 360.0, math.hypot(x_km, y_km)


# ------------------------------------------------------------- DORADE walker ---

def dorade_sweep(entry_id, fields):
    """One DORADE sweep file: RYIB ray azimuth/elevation (plus CFAC corrections), the
    CELV/CSFD gate geometry, the RADD scan mode, and each of `fields` decoded from RDAT
    (16-bit words, raw or HRD run-length) as stored / scale - bias with the PARM bad-data
    flag as NaN."""
    raw = corpus_path(entry_id).read_bytes()
    endian = "<" if struct.unpack_from("<i", raw, 4)[0] < 65536 else ">"

    def i16(b, o):
        return struct.unpack_from(endian + "h", b, o)[0]

    def i32(b, o):
        return struct.unpack_from(endian + "i", b, o)[0]

    def f32(b, o):
        return struct.unpack_from(endian + "f", b, o)[0]

    out = {"rays": [], "params": {}}
    cfac = (0.0, 0.0, 0.0)
    offset = 0
    current = None
    while offset + 8 <= len(raw):
        name = raw[offset:offset + 4].decode("latin-1")
        length = i32(raw, offset + 4)
        if length < 8 or offset + length > len(raw):
            break
        block = raw[offset:offset + length]
        if name == "RADD":
            out["scan_mode"] = i16(block, 50)
            out["compression"] = i16(block, 68)
        elif name == "PARM":
            pname = block[8:16].decode("latin-1").strip("\x00 ")
            out["params"][pname] = {"format": i16(block, 78), "scale": f32(block, 92),
                                    "bias": f32(block, 96), "bad": i32(block, 100)}
        elif name == "CELV":
            count = i32(block, 8)
            cells = np.frombuffer(block, dtype=endian + "f4", count=count, offset=12)
            out["first_cell_m"] = float(cells[0])
            out["cell_spacing_m"] = float(cells[1] - cells[0])
            out["cell_count"] = count
        elif name == "CSFD":
            out["first_cell_m"] = f32(block, 12)
            out["cell_spacing_m"] = f32(block, 16)
            out["cell_count"] = i16(block, 48)
        elif name == "CFAC":
            cfac = (f32(block, 8), f32(block, 12), f32(block, 16))
        elif name == "SWIB":
            out["fixed_angle_deg"] = f32(block, 32)
        elif name == "RYIB":
            current = {"azimuth_deg": F32(f32(block, 24)) + F32(cfac[0]),
                       "elevation_deg": F32(f32(block, 28)) + F32(cfac[1]),
                       "status": i32(block, 40), "data": {}}
            out["rays"].append(current)
        elif name == "RDAT" and current is not None:
            pname = block[8:16].decode("latin-1").strip("\x00 ")
            if pname in fields:
                current["data"][pname] = block[16:]
        offset += length
    cells = out["cell_count"]
    for ray in out["rays"]:
        ray["values"] = {}
        for field in fields:
            param = out["params"][field]
            words = np.frombuffer(ray["data"][field], dtype=endian + "i2")
            if out["compression"] == 1:
                decoded = []
                i = 0
                while i < len(words) and len(decoded) < cells:
                    control = int(words[i]) & 0xFFFF
                    i += 1
                    if control in (0, 1):
                        break
                    if control & 0x8000:
                        n = control & 0x7FFF
                        decoded.extend(int(w) for w in words[i:i + n])
                        i += n
                    else:
                        decoded.extend([param["bad"]] * control)
                words = np.asarray(decoded[:cells] + [param["bad"]] * (cells - len(decoded)))
            words = np.asarray(words[:cells], dtype=np.int64)
            values = words / param["scale"] - param["bias"]
            ray["values"][field] = np.where(words == param["bad"], np.nan, values)
        del ray["data"]
    return out


# -------------------------------------------------------------- availability ---

def section_availability():
    """Moment names and radial counts per sweep, from MetPy."""
    payload = {
        "source": "tools/retrieve_golden.py availability; MetPy 1.7.1 Level2File",
        "files": [],
    }
    for entry in ("l2-ktlx-20240315-000217-trim", "l2-ktlx-19990503-230052"):
        sweeps, _ = level2_sweeps(entry)
        payload["files"].append({
            "id": entry,
            "sweeps": [{
                "index": index,
                "radials": int(len(s["az"])),
                "elevation_deg": jf(s["el"][0], 3),
                "moments": {name: int(len(m["rows"])) for name, m in sorted(s["moments"].items())},
            } for index, s in enumerate(sweeps)],
        })
    write_golden("retrieve/availability.json", payload)


# --------------------------------------------------------------------- detect ---

def spc_tornado(om, year):
    """One row of the SPC tornado database by its `om` number and year."""
    with open(reference_file(SPC_URL), newline="") as fh:
        for row in csv.DictReader(fh):
            if row["om"] == str(om) and row["yr"] == str(year):
                return row
    raise SystemExit(f"SPC tornado {om}/{year} not found")


def tornado_case(entry, om, year, duration_min):
    """Expected tornado position at the time the lowest Doppler sweep scanned it: the SPC
    path start/end interpolated at constant speed over `duration_min` (the survey's
    duration), evaluated at the Py-ART ray time of the sweep-1 ray nearest that position."""
    radar = pyart_radar(entry)
    row = spc_tornado(om, year)
    # SPC times are CST (tz code 3) regardless of daylight saving.
    start = datetime.strptime(f"{row['date']} {row['time']}", "%Y-%m-%d %H:%M:%S")
    start = start.replace(tzinfo=timezone(timedelta(hours=-6)))
    volume_time = pyart_volume_time(radar)
    sweep = next(i for i in range(radar.nsweeps)
                 if not np.all(np.ma.getmaskarray(radar.get_field(i, "velocity"))))
    az = radar.get_azimuth(sweep)
    times = radar.time["data"][radar.get_slice(sweep)]
    slat, slon, elat, elon = (float(row[k]) for k in ("slat", "slon", "elat", "elon"))
    ray_time = volume_time
    for _ in range(3):
        fraction = min(max((ray_time - start).total_seconds() / (duration_min * 60.0), 0.0), 1.0)
        lat = slat + fraction * (elat - slat)
        lon = slon + fraction * (elon - slon)
        x, y = radar_xy_km(radar, lat, lon)
        azimuth, rng = az_range_from_xy(x, y)
        ray = int(np.argmin(np.abs(((az - azimuth) + 180.0) % 360.0 - 180.0)))
        ray_time = volume_time + timedelta(seconds=float(times[ray]))
    return {
        "id": entry,
        "radar": {"latitude_deg": float(radar.latitude["data"][0]),
                  "longitude_deg": float(radar.longitude["data"][0])},
        "volume_time": volume_time.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "doppler_sweep": sweep,
        "doppler_sweep_elevation_deg": jf(radar.get_elevation(sweep).mean(), 2),
        "spc": {k: row[k] for k in ("om", "yr", "mo", "dy", "date", "time", "tz", "st", "mag",
                                    "slat", "slon", "elat", "elon", "len", "wid")},
        "assumed_duration_min": duration_min,
        "ray_time": ray_time.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "path_fraction": jf(fraction, 4),
        "latitude_deg": jf(lat, 4),
        "longitude_deg": jf(lon, 4),
        "east_km": jf(x, 2),
        "north_km": jf(y, 2),
        "azimuth_deg": jf(azimuth, 2),
        "range_km": jf(rng, 2),
    }


def section_detect():
    import pyart

    payload = {
        "source": "tools/retrieve_golden.py detect; SPC 1950-2024_actual_tornadoes.csv, "
                  "Py-ART 2.2.5 read_nexrad_archive, MetPy 1.7.1 Level2File",
        "tornadoes": [
            # Rolling Fork - Silver City, MS, EF4 (NWS Jackson): SPC om 622315, on the ground
            # 00:57Z-02:08Z 2023-03-25 (71 min per the NWS survey).
            tornado_case("l2-kdgx-20230325-010651", 622315, 2023, 71),
            # Stanton, NE, EF4 (NWS Omaha; the first of the Pilger family): SPC om 514013,
            # 20:42Z-21:14Z 2014-06-16 (32 min per the NWS survey).
            tornado_case("l2-koax-20140616-205305", 514013, 2014, 32),
            # Moore, OK, EF5 (NWS Norman): SPC om 451537, 19:56Z-20:35Z 2013-05-20 (39 min).
            tornado_case("l2-ktlx-20130520-201643", 451537, 2013, 39),
        ],
        "quiet": [],
        "single_doppler_tilt": None,
    }
    for entry in ("l2-ktlx-20240515-000014", "l2-kmaf-20230331-230843"):
        radar = pyart_radar(entry)
        ref = radar.fields["reflectivity"]["data"]
        velocity_sweeps = [i for i in range(radar.nsweeps)
                           if not np.all(np.ma.getmaskarray(radar.get_field(i, "velocity")))]
        payload["quiet"].append({
            "id": entry,
            "vcp": int(radar.metadata.get("vcp_pattern", 0)),
            "sweeps": int(radar.nsweeps),
            "velocity_sweeps": len(velocity_sweeps),
            "max_reflectivity_dbz": jf(ref.max(), 2),
            "gates_ge_20_dbz": int((ref >= 20.0).sum()),
        })
    sweeps, _ = level2_sweeps("l2-ktlx-20130520-201643-trim")
    payload["single_doppler_tilt"] = {
        "id": "l2-ktlx-20130520-201643-trim",
        "sweeps": len(sweeps),
        "velocity_sweeps": sum(1 for s in sweeps if "VEL" in s["moments"]),
        "radials": [int(len(s["az"])) for s in sweeps],
    }
    write_golden("retrieve/detect.json", payload)


# ---------------------------------------------------------------------- gbvtd ---

def hurdat2_fixes(storm_id):
    """Best-track fixes of one HURDAT2 storm: (time, lat, lon, wind kt, rmw nmi or None)."""
    fixes = []
    inside = False
    with open(reference_file(HURDAT2_URL), encoding="latin-1") as fh:
        for line in fh:
            parts = [p.strip() for p in line.split(",")]
            if parts[0][:2] in ("AL", "EP", "CP"):
                inside = parts[0] == storm_id
                continue
            if not inside:
                continue
            when = datetime.strptime(parts[0] + parts[1], "%Y%m%d%H%M").replace(tzinfo=timezone.utc)
            lat = float(parts[4][:-1]) * (1 if parts[4].endswith("N") else -1)
            lon = float(parts[5][:-1]) * (1 if parts[5].endswith("E") else -1)
            wind = int(parts[6])
            rmw = int(parts[20]) if len(parts) > 20 and parts[20] not in ("", "-999") else None
            fixes.append({"time": when, "record": parts[2], "status": parts[3], "lat": lat,
                          "lon": lon, "wind_kt": wind, "rmw_nmi": rmw, "line": line.rstrip()})
    if not fixes:
        raise SystemExit(f"HURDAT2 storm {storm_id} not found")
    return fixes


def interpolate_fix(fixes, when):
    """Linear interpolation of position between the bracketing fixes, the storm motion from
    those two fixes (m/s east/north), and the wind/RMW of the nearest fix."""
    before = max((f for f in fixes if f["time"] <= when), key=lambda f: f["time"])
    after = min((f for f in fixes if f["time"] > when), key=lambda f: f["time"], default=before)
    span = (after["time"] - before["time"]).total_seconds()
    t = (when - before["time"]).total_seconds() / span if span > 0 else 0.0
    lat = before["lat"] + t * (after["lat"] - before["lat"])
    lon = before["lon"] + t * (after["lon"] - before["lon"])
    if span > 0:
        mean_lat = math.radians(0.5 * (before["lat"] + after["lat"]))
        east = math.radians(after["lon"] - before["lon"]) * EARTH_RADIUS_M * math.cos(mean_lat)
        north = math.radians(after["lat"] - before["lat"]) * EARTH_RADIUS_M
        motion = (east / span, north / span)
    else:
        motion = (0.0, 0.0)
    nearest = min((before, after), key=lambda f: abs((f["time"] - when).total_seconds()))
    return before, after, lat, lon, motion, nearest


class RingSampler:
    """gbvtd.rs Sampler: nearest gate by range, nearest radial by circular azimuth
    distance (binary search on the sorted azimuths, tie to the lower neighbour)."""

    def __init__(self, azimuths_deg, first_gate_m, gate_spacing_m, values):
        az = rem_euclid32(azimuths_deg)
        finite = np.isfinite(az)
        order = np.argsort(az[finite], kind="stable")
        self.sorted_az = az[finite][order]
        self.sorted_idx = np.flatnonzero(finite)[order]
        self.first = F32(first_gate_m)
        self.spacing = F32(gate_spacing_m)
        self.values = values.astype(F32)
        self.gates = values.shape[1]

    def sample(self, x_km, y_km):
        x_km = F32(x_km)
        y_km = F32(y_km)
        range_m = F32(np.sqrt(x_km * x_km + y_km * y_km) * F32(1000.0))
        gate_f = np.round(F32((range_m - self.first) / self.spacing))
        if not np.isfinite(gate_f) or gate_f < 0 or gate_f >= self.gates:
            return None
        gate = int(gate_f)
        az = rem_euclid32(F32(np.degrees(np.arctan2(x_km, y_km))))
        n = len(self.sorted_az)
        pos = int(np.searchsorted(self.sorted_az, az, side="left"))
        lo = (pos + n - 1) % n
        hi = pos % n
        dl = abs(F32(rem_euclid32(F32(self.sorted_az[lo] - az + F32(180.0))) - F32(180.0)))
        dh = abs(F32(rem_euclid32(F32(self.sorted_az[hi] - az + F32(180.0))) - F32(180.0)))
        radial = self.sorted_idx[lo] if dl <= dh else self.sorted_idx[hi]
        value = self.values[radial, gate]
        return float(value) if np.isfinite(value) else None


MIN_TANGENTIAL_OBSERVABILITY = 0.08
MIN_RING_CONDITION = 0.20
MIN_ASYMMETRY_CONDITION = 0.05


def fit_ring(sampler, center_km, radius_km, n_azimuths, wind_ms):
    """gbvtd.rs fit_ring: the axisymmetric (VT, VR) least squares on one ring with the
    beam-projected storm motion removed, then the wavenumber-1 tangential terms fit on the
    residual. Returns None where the Rust code returns None."""
    cx, cy = F32(center_km[0]), F32(center_km[1])
    phi0 = F32(np.arctan2(cy, cx))
    saa = sac = scc = sad = scd = 0.0
    terms = []
    for k in range(n_azimuths):
        beta = F32(F32(2.0 * np.pi) * F32(k) / F32(n_azimuths))
        sb, cb = F32(np.sin(beta)), F32(np.cos(beta))
        px = F32(cx + F32(radius_km) * cb)
        py = F32(cy + F32(radius_km) * sb)
        rho = F32(np.sqrt(px * px + py * py))
        if rho < 1e-3:
            continue
        vd = sampler.sample(px, py)
        if vd is None:
            continue
        bx, by = F32(px / rho), F32(py / rho)
        a = float(F32(F32(-sb) * bx + cb * by))
        c = float(F32(cb * bx + sb * by))
        env = wind_ms[0] * float(bx) + wind_ms[1] * float(by)
        d = vd - env
        theta = F32(beta - phi0)
        st, ct = F32(np.sin(theta)), F32(np.cos(theta))
        ac, as_ = a * float(ct), a * float(st)
        saa += a * a
        sac += a * c
        scc += c * c
        sad += a * d
        scd += c * d
        terms.append((a, c, d, ac, as_))
    samples = len(terms)
    if samples < 8:
        return None
    det = saa * scc - sac * sac
    mean_a2 = saa / samples
    rho = det / (saa * scc) if saa > 0.0 and scc > 0.0 else 0.0
    if mean_a2 < MIN_TANGENTIAL_OBSERVABILITY or rho < MIN_RING_CONDITION:
        return None
    vt = (sad * scc - scd * sac) / det
    vr = (saa * scd - sac * sad) / det
    m11 = m12 = m22 = r1 = r2 = sse = 0.0
    for a, c, d, ac, as_ in terms:
        e = a * vt + c * vr - d
        sse += e * e
        target = -e
        m11 += ac * ac
        m12 += ac * as_
        m22 += as_ * as_
        r1 += ac * target
        r2 += as_ * target
    vt1_cos = vt1_sin = 0.0
    det2 = m11 * m22 - m12 * m12
    if m11 > 0.0 and m22 > 0.0 and det2 / (m11 * m22) >= MIN_ASYMMETRY_CONDITION:
        vt1_cos = (r1 * m22 - r2 * m12) / det2
        vt1_sin = (m11 * r2 - m12 * r1) / det2
    vt1_cos, vt1_sin = F32(vt1_cos), F32(vt1_sin)
    return {
        "radius_km": jf(radius_km),
        "vt": jf(F32(vt), 4),
        "vr": jf(F32(vr), 4),
        "vt1_cos": jf(vt1_cos, 4),
        "vt1_sin": jf(vt1_sin, 4),
        "vt1_amp": jf(F32(np.hypot(vt1_cos, vt1_sin)), 4),
        "vt1_phase_deg": jf(F32(np.degrees(np.arctan2(vt1_sin, vt1_cos))), 3),
        "samples": samples,
        "rms": jf(F32(np.sqrt(sse / samples)), 4),
    }


def hurricane_case(entry, storm_id, doppler_sweep, radii_km, n_azimuths):
    radar = pyart_radar(entry)
    fixes = hurdat2_fixes(storm_id)
    volume_time = pyart_volume_time(radar)
    times = radar.time["data"][radar.get_slice(doppler_sweep)]
    sweep_time = volume_time + timedelta(seconds=float(np.median(times)))
    before, after, lat, lon, motion, nearest = interpolate_fix(fixes, sweep_time)
    x, y = radar_xy_km(radar, lat, lon)
    azimuth, rng = az_range_from_xy(x, y)
    dealiased = pyart_dealiased(radar)
    sl = radar.get_slice(doppler_sweep)
    field = dealiased[sl]
    az = radar.get_azimuth(doppler_sweep)
    rg = radar.range["data"]
    sampler = RingSampler(az, rg[0], rg[1] - rg[0], field)
    rings = [fit_ring(sampler, (x, y), r, n_azimuths, motion) for r in radii_km]
    nyquist = radar.instrument_parameters["nyquist_velocity"]["data"][sl]
    return {
        "id": entry,
        "storm": storm_id,
        "radar": {"latitude_deg": float(radar.latitude["data"][0]),
                  "longitude_deg": float(radar.longitude["data"][0]),
                  "altitude_m": float(radar.altitude["data"][0])},
        "volume_time": volume_time.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "doppler_sweep": doppler_sweep,
        "doppler_sweep_elevation_deg": jf(radar.get_elevation(doppler_sweep).mean(), 2),
        "doppler_sweep_nyquist_mps": jf(np.median(nyquist), 2),
        "doppler_sweep_rays": int(sl.stop - sl.start),
        "sweep_time": sweep_time.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "hurdat2": [f["line"] for f in (before, after)],
        "best_track": {
            "latitude_deg": jf(lat, 4),
            "longitude_deg": jf(lon, 4),
            "east_km": jf(x, 2),
            "north_km": jf(y, 2),
            "azimuth_deg": jf(azimuth, 2),
            "range_km": jf(rng, 2),
            "wind_kt": nearest["wind_kt"],
            "wind_mps": jf(nearest["wind_kt"] * 0.514444, 2),
            "rmw_nmi": nearest["rmw_nmi"],
            "rmw_km": jf(nearest["rmw_nmi"] * 1.852, 2) if nearest["rmw_nmi"] else None,
            "motion_east_mps": jf(motion[0], 3),
            "motion_north_mps": jf(motion[1], 3),
        },
        "rings": {
            "n_azimuths": n_azimuths,
            "radii_km": jlist(radii_km),
            "fits": rings,
        },
    }


def section_gbvtd():
    radii = [float(r) for r in range(8, 81, 4)]
    payload = {
        "source": "tools/retrieve_golden.py gbvtd; NHC HURDAT2 hurdat2-1851-2024-040425.txt, "
                  "Py-ART 2.2.5 read_nexrad_archive + dealias_region_based, numpy ring fits",
        "cases": [
            hurricane_case("l2-klix-20210829-180425", "AL092021", 1, radii, 72),
            hurricane_case("l2-tjua-20220918-190621", "AL072022", 1, radii, 72),
        ],
    }
    write_golden("retrieve/gbvtd.json", payload)


# ---------------------------------------------------------------------- shear ---

def llsd_derivative(azimuths_deg, values, first_gate_m, spacing_m, axis):
    """shear.rs llsd_velocity_derivative: per gate, the least-squares slope of velocity
    against x over rows-1..rows+1 (no azimuth wrap) and gates-1..gates+1, where x is the
    cross-radial arc r * daz (azimuthal) or the along-radial offset (radial), in metres;
    at least 4 finite samples, |denominator| >= 1e-6; output x1000 (f32)."""
    rows, gates = values.shape
    az = rem_euclid32(azimuths_deg)
    spacing = F32(spacing_m)
    r_m = (F32(first_gate_m) + np.arange(gates, dtype=F32) * spacing).astype(F32)
    out = np.full((rows, gates), np.nan, dtype=F32)
    sx = np.zeros((rows, gates))
    sv = np.zeros((rows, gates))
    sxx = np.zeros((rows, gates))
    sxv = np.zeros((rows, gates))
    n = np.zeros((rows, gates), dtype=np.int64)
    finite = np.isfinite(values)
    v64 = np.where(finite, values, 0.0)
    for dr in (-1, 0, 1):
        rr = np.arange(rows) + dr
        ok_row = (rr >= 0) & (rr < rows)
        rr_c = np.clip(rr, 0, rows - 1)
        daz = np.radians((az[rr_c] - az).astype(F32)).astype(F32)
        daz = np.where(daz > F32(np.pi), (daz - F32(2 * np.pi)).astype(F32), daz)
        daz = np.where(daz < F32(-np.pi), (daz + F32(2 * np.pi)).astype(F32), daz).astype(F32)
        for dg in (-1, 0, 1):
            gg = np.arange(gates) + dg
            ok_gate = (gg >= 0) & (gg < gates)
            gg_c = np.clip(gg, 0, gates - 1)
            ok = ok_row[:, None] & ok_gate[None, :] & finite[rr_c[:, None], gg_c[None, :]]
            if axis == "azimuthal":
                x = (r_m[None, :] * daz[:, None]).astype(F32)
            else:
                x = np.broadcast_to((F32(dg) * spacing).astype(F32), (rows, gates))
            x = x.astype(np.float64)
            v = v64[rr_c[:, None], gg_c[None, :]]
            sx += np.where(ok, x, 0.0)
            sv += np.where(ok, v, 0.0)
            sxx += np.where(ok, x * x, 0.0)
            sxv += np.where(ok, x * v, 0.0)
            n += ok
    nf = n.astype(np.float64)
    denom = nf * sxx - sx * sx
    valid = (n >= 4) & (np.abs(denom) >= 1e-6) & (r_m[None, :] > 0)
    with np.errstate(invalid="ignore", divide="ignore"):
        slope = (nf * sxv - sx * sv) / np.where(valid, denom, 1.0)
    out[valid] = (slope[valid].astype(F32) * F32(1000.0)).astype(F32)
    return out


def grid_summary(grid, base_label):
    """Whole-grid checks: per-row finite counts and sums, plus a fixed sample of cells."""
    finite = np.isfinite(grid)
    rows, gates = grid.shape
    # 400 finite cells plus 50 cells anywhere (so no-data cells are checked too).
    finite_cells = np.flatnonzero(finite.ravel())
    cells = np.union1d(finite_cells[sample_indices(len(finite_cells), 400)],
                       sample_indices(rows * gates, 50, seed_offset=1))
    return {
        "rows": rows,
        "gates": gates,
        "valid": int(finite.sum()),
        "row_valid": [int(v) for v in finite.sum(axis=1)],
        "row_sum": jlist(np.where(finite, grid.astype(np.float64), 0.0).sum(axis=1), 3),
        "cells": [[int(c // gates), int(c % gates), jf(grid.flat[c], 4)] for c in cells],
        "base": base_label,
    }


def grid_extreme(grid, azimuths, first_gate_m, spacing_m, largest=True):
    finite = np.isfinite(grid)
    if not finite.any():
        return None
    masked = np.where(finite, grid, -np.inf if largest else np.inf)
    row, gate = np.unravel_index(np.argmax(masked) if largest else np.argmin(masked), grid.shape)
    return {"row": int(row), "gate": int(gate), "value": jf(grid[row, gate], 4),
            "azimuth_deg": jf(azimuths[row], 3),
            "range_km": jf((first_gate_m + gate * spacing_m) / 1000.0, 3)}


def shear_case(entry, sweep_index, axis):
    """LLSD derivative on the raw MetPy velocity (exactly what the `_from_dealiased` entry
    point computes from the raw grid) and on the Py-ART region-based dealiased velocity."""
    sweeps, _ = level2_sweeps(entry)
    s = sweeps[sweep_index]
    vel = s["moments"]["VEL"]
    az = s["az"][vel["rows"]]
    raw = llsd_derivative(az, vel["values"], vel["first_gate_m"], vel["gate_spacing_m"], axis)
    radar = pyart_radar(entry)
    # Py-ART pads every field to the longest gate axis of the volume.
    dealiased = pyart_dealiased(radar)[radar.get_slice(sweep_index)][:, :vel["gate_count"]]
    pyart_az = radar.get_azimuth(sweep_index)
    if len(pyart_az) != len(az) or np.max(np.abs(rem_euclid32(pyart_az) - rem_euclid32(az))) > 1e-3:
        raise SystemExit(f"{entry}: MetPy and Py-ART ray order differ on sweep {sweep_index}")
    de = llsd_derivative(az, dealiased, vel["first_gate_m"], vel["gate_spacing_m"], axis)
    folded = np.isfinite(vel["values"]) & np.isfinite(dealiased) \
        & (np.abs(vel["values"] - dealiased) > 0.5 * np.median(s["nyquist"]))
    return {
        "id": entry,
        "sweep": sweep_index,
        "axis": axis,
        "elevation_deg": jf(s["el"][0], 3),
        "nyquist_mps": jf(np.median(s["nyquist"]), 3),
        "first_gate_m": vel["first_gate_m"],
        "gate_spacing_m": vel["gate_spacing_m"],
        "velocity_valid": int(np.isfinite(vel["values"]).sum()),
        "pyart_unfolded_gates": int(folded.sum()),
        "raw": grid_summary(raw, "MetPy raw velocity"),
        "raw_max": grid_extreme(raw, az, vel["first_gate_m"], vel["gate_spacing_m"], True),
        "raw_min": grid_extreme(raw, az, vel["first_gate_m"], vel["gate_spacing_m"], False),
        "dealiased": grid_summary(de, "Py-ART dealias_region_based velocity"),
        "dealiased_max": grid_extreme(de, az, vel["first_gate_m"], vel["gate_spacing_m"], True),
        "dealiased_min": grid_extreme(de, az, vel["first_gate_m"], vel["gate_spacing_m"], False),
    }


def section_shear():
    jma = "jma-n6-20191012-090000-rs47773"
    payload = {
        "source": "tools/retrieve_golden.py shear; MetPy 1.7.1 Level2File, Py-ART 2.2.5 "
                  "dealias_region_based, numpy LLSD (Smith and Elmore 2004)",
        "cases": [
            shear_case("l2-ktlx-20130520-201643-trim", 1, "azimuthal"),
            shear_case("l2-kdvn-20200810-180401-trim", 1, "radial"),
            shear_case("l2-kdvn-20200810-180401-trim", 1, "azimuthal"),
        ],
        # The JMA N6 velocity product carries no Nyquist velocity (staggered PRF); the
        # curation walker recorded 13 sweeps for the TAKA member.
        "no_nyquist": {"id": jma, "sweeps": 13},
    }
    write_golden("retrieve/shear.json", payload)


# ---------------------------------------------------------------------- sweep ---

KDP_WINDOW_KM = 3.0
KDP_MIN_WINDOW = 7
KDP_MAX_WINDOW = 41
KDP_MIN_VALID = 5
KDP_MAX_GAP = 2
KDP_PERIOD_DEG = F32(360.0)
KDP_MIN_RHO = F32(0.80)
KDP_MIN_DBZ = F32(-10.0)
HAMPEL_HALF = 3
HAMPEL_SIGMA = F32(3.0)
HUBER_K = 1.5
S_BAND_KDP_BOUNDS = (-2.0, 14.0)


def scaled32(values, scale, offset):
    """The Rust reader's f32 scaling of integer codes: (code - offset) / scale."""
    codes = np.rint(values * scale + offset)
    out = ((codes.astype(F32) - F32(offset)) / F32(scale)).astype(F32)
    return np.where(np.isfinite(values), out, np.nan).astype(F32)


def regression_window_gates(spacing_km):
    """sweep.rs regression_window_gates for the default KdpConfig."""
    minimum = max(KDP_MIN_WINDOW, 3)
    maximum = max(KDP_MAX_WINDOW, minimum)
    gates = int(round(max(KDP_WINDOW_KM, spacing_km) / spacing_km))
    gates = min(max(gates, minimum), maximum)
    if gates % 2 == 0:
        gates = gates + 1 if gates < maximum else max(gates - 1, 3)
    return gates


def unwrap_phase(values):
    """sweep.rs unwrap_phase_in_place (f32): remove 360-degree wraps between consecutive
    finite gates, restarting after a gap longer than KDP_MAX_GAP."""
    out = values.astype(F32).copy()
    previous = None
    gap = 0
    for i in range(len(out)):
        if not np.isfinite(out[i]):
            gap += 1
            continue
        if gap > KDP_MAX_GAP:
            previous = None
        if previous is not None:
            wraps = np.round(F32((out[i] - previous) / KDP_PERIOD_DEG))
            out[i] = F32(out[i] - F32(wraps * KDP_PERIOD_DEG))
        previous = out[i]
        gap = 0
    return out


def fill_short_gaps(values):
    """sweep.rs fill_short_gaps_in_place (f32 linear interpolation across gaps of at most
    KDP_MAX_GAP gates bounded by finite values)."""
    out = values.astype(F32).copy()
    n = len(out)
    i = 0
    while i < n:
        if np.isfinite(out[i]):
            i += 1
            continue
        start = i
        while i < n and not np.isfinite(out[i]):
            i += 1
        gap = i - start
        if start == 0 or i >= n or gap > KDP_MAX_GAP:
            continue
        left, right = out[start - 1], out[i]
        for offset in range(gap):
            fraction = F32(F32(offset + 1) / F32(gap + 1))
            out[start + offset] = F32(left + F32(fraction * F32(right - left)))
    return out


def hampel(values):
    """sweep.rs hampel_filter (f32 medians): a finite gate more than 3 robust sigma from
    the median of its 7-gate neighbourhood is replaced by that median."""
    out = values.astype(F32).copy()
    n = len(values)
    for i in range(n):
        v = values[i]
        if not np.isfinite(v):
            continue
        window = values[max(i - HAMPEL_HALF, 0):min(i + HAMPEL_HALF + 1, n)]
        window = window[np.isfinite(window)]
        median = median32(window)
        mad = median32(np.abs(window - median).astype(F32))
        robust_sigma = F32(F32(1.4826) * mad)
        deviation = F32(abs(F32(v - median)))
        if robust_sigma > 1.0e-4:
            outlier = deviation > F32(HAMPEL_SIGMA * robust_sigma)
        else:
            outlier = deviation > F32(3.0)
        if outlier:
            out[i] = median
    return out


def robust_linear_fit(xs, ys):
    """sweep.rs robust_linear_fit: three Huber-weighted (k = 1.5) least-squares passes."""
    weights = np.ones_like(xs)
    slope = intercept = 0.0
    for _ in range(3):
        sw = weights.sum()
        sx = (xs * weights).sum()
        sy = (ys * weights).sum()
        sxx = (xs * xs * weights).sum()
        sxy = (xs * ys * weights).sum()
        denominator = sw * sxx - sx * sx
        if not np.isfinite(denominator) or abs(denominator) <= 1.0e-12 or sw <= 0.0:
            return None
        slope = (sw * sxy - sx * sy) / denominator
        intercept = (sy - slope * sx) / sw
        if not (np.isfinite(slope) and np.isfinite(intercept)):
            return None
        residuals = ys - (intercept + slope * xs)
        median_residual = np.median(residuals)
        sigma = 1.4826 * np.median(np.abs(residuals - median_residual))
        if not np.isfinite(sigma) or sigma <= 1.0e-8:
            break
        cutoff = max(HUBER_K, 0.1) * sigma
        magnitude = np.abs(residuals)
        weights = np.where(magnitude <= cutoff, 1.0, cutoff / np.where(magnitude > 0, magnitude, 1.0))
    return slope, intercept


def phase_bundle(phi, rho, ref, spacing_m, bounds=S_BAND_KDP_BOUNDS, fill_gaps=True):
    """sweep.rs derive_phase_bundle on one sweep whose PHI, RHO and REF grids share their
    gate geometry (so RHO/REF are sampled at the same gate index). Returns
    (PHIF, KDP, out_of_bounds mask, valid-after-QC mask)."""
    rows, gates = phi.shape
    spacing_km = spacing_m / 1000.0
    window = regression_window_gates(F32(spacing_km))
    half = window // 2
    phif = np.full((rows, gates), np.nan, dtype=F32)
    kdp = np.full((rows, gates), np.nan, dtype=F32)
    out_of_bounds = np.zeros((rows, gates), dtype=bool)
    valid_all = np.zeros((rows, gates), dtype=bool)
    for row in range(rows):
        values = phi[row].astype(F32).copy()
        valid = np.isfinite(values)
        # A gate is dropped only where the RHO/REF sample exists and fails the floor;
        # a missing sample (no data, or no gate at that range) does not gate PHIDP.
        if rho is not None:
            r = rho[row, :gates]
            valid &= ~(np.isfinite(r) & (r < KDP_MIN_RHO))
        if ref is not None:
            z = ref[row, :gates]
            valid &= ~(np.isfinite(z) & (z < KDP_MIN_DBZ))
        values[~valid] = np.nan
        valid_all[row] = valid
        values = unwrap_phase(values)
        if fill_gaps:
            values = fill_short_gaps(values)
        values = hampel(values)
        finite = np.isfinite(values)
        for gate in np.flatnonzero(valid):
            start = max(gate - half, 0)
            end = min(gate + half + 1, gates)
            idx = np.arange(start, end)[finite[start:end]]
            if len(idx) < KDP_MIN_VALID:
                continue
            xs = (idx.astype(np.float64) - gate) * spacing_km
            ys = values[idx].astype(np.float64)
            fit = robust_linear_fit(xs, ys)
            if fit is None:
                continue
            slope, intercept = fit
            phif[row, gate] = F32(intercept)
            k = F32(0.5 * slope)
            if np.isfinite(k) and bounds[0] <= k <= bounds[1]:
                kdp[row, gate] = k
            elif np.isfinite(k):
                out_of_bounds[row, gate] = True
    return phif, kdp, out_of_bounds, valid_all


def range_gradient(values, spacing_m, periods):
    """sweep.rs range_gradient_grid_with_period: difference between the nearest finite
    gates within 2 on each side (or the centre and its one finite side), wrapped to the
    row's period when it has one, per km."""
    rows, gates = values.shape
    spacing_km = F32(spacing_m / 1000.0)
    out = np.full((rows, gates), np.nan, dtype=F32)
    for row in range(rows):
        v = values[row].astype(F32)
        period = periods[row]
        finite = np.isfinite(v)
        for gate in range(gates):
            left = next(((gate - d, v[gate - d]) for d in (1, 2) if gate - d >= 0 and finite[gate - d]), None)
            right = next(((gate + d, v[gate + d]) for d in (1, 2) if gate + d < gates and finite[gate + d]), None)
            if left is not None and right is not None:
                delta = F32(right[1] - left[1])
                span = right[0] - left[0]
            elif left is not None and finite[gate]:
                delta = F32(v[gate] - left[1])
                span = gate - left[0]
            elif right is not None and finite[gate]:
                delta = F32(right[1] - v[gate])
                span = right[0] - gate
            else:
                continue
            if period is not None:
                delta = wrapped_delta32(delta, period)
            gradient = F32(delta / F32(F32(span) * spacing_km))
            if np.isfinite(gradient):
                out[row, gate] = gradient
    return out


def cdr_reference(zdr_db, rho):
    """Circular depolarization ratio, Py-ART compute_cdr / sweep.rs (f32)."""
    zdr = np.power(F32(10.0), F32(0.1) * zdr_db.astype(F32)).astype(F32)
    zdr = np.maximum(zdr, F32(1.0e-6))
    inv = np.sqrt(F32(1.0) / zdr).astype(F32)
    num = (F32(1.0) + F32(1.0) / zdr - F32(2.0) * rho.astype(F32) * inv).astype(F32)
    den = (F32(1.0) + F32(1.0) / zdr + F32(2.0) * rho.astype(F32) * inv).astype(F32)
    ok = np.isfinite(zdr_db) & np.isfinite(rho) & (num > 0) & (den > 0)
    with np.errstate(invalid="ignore", divide="ignore"):
        cdr = (F32(10.0) * np.log10((num / den).astype(F32))).astype(F32)
    return np.where(ok, cdr, np.nan).astype(F32)


def cells_where(mask, limit, seed_offset):
    idx = np.flatnonzero(mask.ravel())
    gates = mask.shape[1]
    return [[int(c // gates), int(c % gates)] for c in idx[sample_indices(len(idx), limit, seed_offset)]]


def section_sweep():
    import pyart

    entry = "l2-ktlx-20130520-201643-trim"
    sweeps, _ = level2_sweeps(entry)
    s = sweeps[0]
    phi_m, rho_m, ref_m, zdr_m = (s["moments"][k] for k in ("PHI", "RHO", "REF", "ZDR"))
    phi = scaled32(phi_m["values"], 2.8361001014709473, 2.0)
    rho = scaled32(rho_m["values"], 300.0, -60.5)
    ref = scaled32(ref_m["values"], 2.0, 66.0)
    zdr = scaled32(zdr_m["values"], 16.0, 128.0)
    spacing = phi_m["gate_spacing_m"]
    gates = phi_m["gate_count"]
    az = s["az"][phi_m["rows"]]

    phif, kdp, oob, valid = phase_bundle(phi, rho, ref, spacing)
    phif_nofill, kdp_nofill, _, _ = phase_bundle(phi, rho, ref, spacing, fill_gaps=False)

    # Short internal PHIDP gaps after QC: 1-2 missing gates bounded by valid gates.
    gaps = []
    for row in range(valid.shape[0]):
        v = valid[row]
        g = 0
        while g < gates:
            if v[g]:
                g += 1
                continue
            start = g
            while g < gates and not v[g]:
                g += 1
            if 0 < start and g < gates and g - start <= KDP_MAX_GAP \
                    and np.isfinite(kdp[row, start - 1]) and np.isfinite(kdp[row, g]) \
                    and np.isfinite(kdp_nofill[row, start - 1]) and np.isfinite(kdp_nofill[row, g]):
                gaps.append({"row": row, "gates": [int(start), int(g - 1)],
                             "left_kdp": jf(kdp[row, start - 1], 5),
                             "right_kdp": jf(kdp[row, g], 5),
                             "left_kdp_without_fill": jf(kdp_nofill[row, start - 1], 5),
                             "right_kdp_without_fill": jf(kdp_nofill[row, g], 5)})
    gaps = [gaps[i] for i in sample_indices(len(gaps), 60, 3)]

    # The Moore hail core (69.5 dBZ at 23 km): mean KDP over the core rays where the
    # reflectivity is at least 50 dBZ within 40 km. Py-ART's Vulpiani KDP (windsize 10,
    # 10 iterations) is recorded for context: its heavily smoothed estimate runs 2-3x
    # lower than a 3 km windowed regression on this noisy PHIDP, so only the sign and
    # magnitude class (heavy precipitation, KDP of order 1 deg/km and more at S band) are
    # comparable, not gate values.
    radar = pyart_radar(entry)
    kdp_vulpiani, _ = pyart.retrieve.kdp_vulpiani(radar, psidp_field="differential_phase",
                                                  band="S", windsize=10, n_iter=10)
    vulpiani = np.ma.filled(kdp_vulpiani["data"][radar.get_slice(0)][:, :gates].astype(np.float64), np.nan)
    core_rows = np.flatnonzero((az >= 258.0) & (az <= 278.0))
    core = np.zeros_like(kdp, dtype=bool)
    core[core_rows, :] = True
    core &= np.isfinite(kdp) & (ref[:, :gates] >= 50.0)         & ((phi_m["first_gate_m"] + np.arange(gates) * spacing) < 40_000)
    moore = {
        "rows": [int(r) for r in core_rows],
        "azimuth_range_deg": [258.0, 278.0],
        "core_gates": int(core.sum()),
        "reference_mean_kdp": jf(kdp[core].mean(), 4),
        "reference_median_kdp": jf(np.median(kdp[core]), 4),
        "vulpiani_mean_kdp": jf(np.nanmean(vulpiani[core]), 4),
        "core_cells": cells_where(core, 300, 4),
    }

    # RHO gating aligned by physical range: the RHO grid subsampled to 500 m gates.
    rho_coarse = rho[:, ::2]
    rho_resampled = np.full_like(rho, np.nan)
    for gate in range(gates):
        position = (gate * spacing) / (2 * spacing)
        rounded = math.floor(position + 0.5)
        if rounded < rho_coarse.shape[1] and abs(position - rounded) <= 0.55:
            rho_resampled[:, gate] = rho_coarse[:, rounded]
    _, kdp_coarse_rho, _, valid_coarse = phase_bundle(phi, rho_resampled, ref, spacing)
    changed = valid != valid_coarse

    # CDR from Py-ART on the same sweep.
    cdr_pyart = pyart.retrieve.compute_cdr(radar)
    cdr = np.ma.filled(cdr_pyart["data"][radar.get_slice(0)][:, :gates].astype(np.float64), np.nan)
    cdr32 = cdr_reference(zdr, rho)
    cdr_delta = np.nanmax(np.abs(cdr - cdr32))

    payload = {
        "source": "tools/retrieve_golden.py sweep; MetPy 1.7.1 Level2File, Py-ART 2.2.5 "
                  "kdp_vulpiani and compute_cdr, numpy phase bundle / range gradient",
        "kdp": {
            "id": entry,
            "sweep": 0,
            "elevation_deg": jf(s["el"][0], 3),
            "gate_spacing_m": spacing,
            "first_gate_m": phi_m["first_gate_m"],
            "window_gates": regression_window_gates(F32(spacing / 1000.0)),
            "phi_scale": 2.8361001014709473,
            "phi_offset": 2.0,
            "phi_valid": int(np.isfinite(phi).sum()),
            "qc_valid": int(valid.sum()),
            "phif": grid_summary(phif, "numpy phase bundle intercept"),
            "kdp": grid_summary(kdp, "numpy phase bundle half slope, S-band bounds"),
            "kdp_max": grid_extreme(kdp, az, phi_m["first_gate_m"], spacing, True),
            "out_of_bounds_count": int(oob.sum()),
            "out_of_bounds_cells": cells_where(oob, 100, 5),
            "gaps": gaps,
            "moore_core": moore,
            "rho_500m": {
                "qc_changed_gates": int(changed.sum()),
                "kdp": grid_summary(kdp_coarse_rho, "numpy phase bundle with RHO on 500 m gates"),
                "changed_cells": cells_where(changed & (np.isfinite(kdp) != np.isfinite(kdp_coarse_rho)), 100, 6),
            },
            "unknown_band": {
                "phif": grid_summary(phase_bundle(phi, rho, ref, spacing, bounds=(-np.inf, np.inf))[0],
                                     "numpy phase bundle intercept, unbounded"),
            },
        },
        "cdr": {
            "id": entry,
            "sweep": 0,
            "pyart_vs_f32_max_abs_delta": jf(cdr_delta, 6),
            "grid": grid_summary(cdr, "Py-ART compute_cdr"),
        },
        "velocity_range_gradient": None,
        "native_kdp": None,
    }

    # Velocity range gradient on the derecho Doppler sweep (folded velocity, Nyquist 21).
    entry = "l2-kdvn-20200810-180401-trim"
    sweeps, _ = level2_sweeps(entry)
    s = sweeps[1]
    vel_m = s["moments"]["VEL"]
    vel = scaled32(vel_m["values"], 2.0, 129.0)
    periods = [2.0 * abs(n) if np.isfinite(n) and n > 0 else None for n in s["nyquist"][vel_m["rows"]]]
    wrapped = range_gradient(vel, vel_m["gate_spacing_m"], periods)
    linear = range_gradient(vel, vel_m["gate_spacing_m"], [None] * len(periods))
    differs = np.isfinite(wrapped) & np.isfinite(linear) & (np.abs(wrapped - linear) > 1e-3)
    payload["velocity_range_gradient"] = {
        "id": entry,
        "sweep": 1,
        "nyquist_mps": jf(np.median(s["nyquist"]), 3),
        "grid": grid_summary(wrapped, "numpy wrapped range gradient"),
        "wrap_changed_gates": int(differs.sum()),
        "wrap_changed_cells": [[int(r), int(g), jf(wrapped[r, g], 4), jf(linear[r, g], 4)]
                               for r, g in cells_where(differs, 100, 7)],
    }

    # A real sweep that carries a native KDP field: the NOXP sector, whose KDP is entirely
    # the bad-data flag in the file.
    entry = "dorade-noxp-20090525-203211-sector"
    sweep = dorade_sweep(entry, ["KDP", "DB_PHIDP"])
    payload["native_kdp"] = {
        "id": entry,
        "rays": len(sweep["rays"]),
        "cells": int(sweep["cell_count"]),
        "kdp_param": {k: jf(v) if isinstance(v, float) else v for k, v in sweep["params"]["KDP"].items()},
        "kdp_finite": int(sum(np.isfinite(r["values"]["KDP"]).sum() for r in sweep["rays"])),
        "phidp_finite": int(sum(np.isfinite(r["values"]["DB_PHIDP"]).sum() for r in sweep["rays"])),
    }
    write_golden("retrieve/sweep.json", payload)


# --------------------------------------------------------------------- volume ---

def last_le_search(sorted_values, targets):
    """Rust slice::binary_search_by on sorted values: Ok(i) for the LAST equal element,
    else Err(insertion point). Returns (found, index) arrays."""
    idx = np.searchsorted(sorted_values, targets, side="right") - 1
    found = np.zeros(np.shape(targets), dtype=bool)
    ok = idx >= 0
    found[ok] = sorted_values[idx[ok]] == np.asarray(targets)[ok]
    insertion = np.where(found, idx, idx + 1)
    return found, insertion


def volume_columns(sweeps, moment):
    """volume.rs CutSampler for every tilt carrying `moment`, sorted by elevation: the
    tilt elevation is the first radial's, gate centres first + i * spacing, 4/3-Earth
    ground range and height per gate, and azimuth -> row lookup."""
    cols = []
    for index, s in enumerate(sweeps):
        m = s["moments"].get(moment)
        if m is None:
            continue
        elevation = float(s["el"][0])
        slant = np.asarray([float(m["first_gate_m"]) + g * float(m["gate_spacing_m"])
                            for g in range(m["gate_count"])])
        az = rem_euclid32(s["az"][m["rows"]])
        order = np.argsort(az, kind="stable")
        cols.append({
            "sweep": index, "elevation": elevation, "rows": m["rows"],
            "sorted_az": az[order], "sorted_rows": order, "row_az": az,
            "ground": np.asarray([beam_ground_range_m(r, elevation) for r in slant]),
            "height": np.asarray([beam_height_m(r, elevation) for r in slant]),
            "values": scaled32(m["values"], 2.0, 66.0), "slant": slant,
        })
    cols.sort(key=lambda col: F32(col["elevation"]))
    return cols


def nearest_row(col, az):
    """volume.rs CutSampler::nearest_row (binary search; on a miss the closer of the two
    neighbours by f32 angular distance, ties to the lower)."""
    sorted_az = col["sorted_az"]
    az = F32(az)
    found, i = last_le_search(sorted_az, np.asarray([az]))
    i = int(i[0])
    if found[0]:
        return int(col["sorted_rows"][i])
    n = len(sorted_az)
    lo = n - 1 if i == 0 else i - 1
    hi = 0 if i >= n else i
    if ang_dist32(sorted_az[lo], az) <= ang_dist32(sorted_az[hi], az):
        return int(col["sorted_rows"][lo])
    return int(col["sorted_rows"][hi])


def gate_for_ground_range(col, s):
    """volume.rs CutSampler::gate_for_ground_range: nearest gate by ground range, none
    more than half a gate outside the first/last gate centres."""
    g = col["ground"]
    n = len(g)
    half = 0.5 * abs(g[1] - g[0]) if n > 1 else 0.0
    if s < g[0] - half or s > g[n - 1] + half:
        return None
    found, i = last_le_search(g, np.asarray([s]))
    i = int(i[0])
    if found[0]:
        return i
    if i == 0:
        return 0
    if i >= n:
        return n - 1
    return i - 1 if (s - g[i - 1]) <= (g[i] - s) else i


def column_profile(cols, az, ground_m):
    samples = []
    for col in cols:
        gate = gate_for_ground_range(col, ground_m)
        if gate is None:
            continue
        row = nearest_row(col, az)
        value = col["values"][row, gate]
        if np.isfinite(value):
            samples.append((col["height"][gate], float(value), col["sweep"]))
    samples.sort(key=lambda sample: sample[0])
    return samples


def section_volume():
    entry = "l2-kewx-20160413-022531"
    sweeps, _ = level2_sweeps(entry)
    cols = volume_columns(sweeps, "REF")
    base = min(cols, key=lambda col: (F32(col["elevation"]), col["sweep"]))
    rows = len(base["rows"])
    gates = base["values"].shape[1]
    threshold = 18.3
    # Sampled base cells: 300 random plus the 30 x 30 block around the hail core
    # (lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km).
    core_row = int(np.argmin(np.abs(base["row_az"] - F32(254.7))))
    core_gate = int(np.argmin(np.abs(base["slant"] - 57_625.0)))
    picks = set((int(c // gates), int(c % gates)) for c in sample_indices(rows * gates, 300, 8))
    for r in range(core_row - 6, core_row + 7):
        for g in range(core_gate - 12, core_gate + 13, 2):
            picks.add((r % rows, g))
    cells = []
    upper_wins = 0
    for row, gate in sorted(picks):
        samples = column_profile(cols, base["row_az"][row], base["ground"][gate])
        if not samples:
            cells.append({"row": row, "gate": gate, "cmax": None, "cmax_sweep": None,
                          "base_value": None, "echo_base_m": None, "echo_top_m": None,
                          "depth_m": None, "samples": 0})
            continue
        cmax = max(samples, key=lambda s: s[1])
        heights = [h for h, v, _ in samples if v >= threshold]
        base_value = float(base["values"][row, gate]) if np.isfinite(base["values"][row, gate]) else None
        if base_value is not None and cmax[1] > base_value:
            upper_wins += 1
        cells.append({
            "row": row, "gate": gate,
            "azimuth_deg": jf(base["row_az"][row], 3),
            "ground_range_m": jf(base["ground"][gate], 1),
            "samples": len(samples),
            "base_value": jf(base_value, 3),
            "cmax": jf(cmax[1], 3),
            "cmax_sweep": int(cmax[2]),
            "cmax_height_m": jf(cmax[0], 2),
            "echo_base_m": jf(min(heights), 2) if heights else None,
            "echo_top_m": jf(max(heights), 2) if heights else None,
            "depth_m": jf(max(max(heights) - min(heights), 0.0), 2) if heights else None,
        })
    core = column_profile(cols, base["row_az"][core_row], base["ground"][core_gate])
    payload = {
        "source": "tools/retrieve_golden.py volume; MetPy 1.7.1 Level2File, numpy column walk "
                  "(volume.rs CutSampler rules, 4/3-Earth beam geometry)",
        "id": entry,
        "reflectivity_sweeps": [{"sweep": c["sweep"], "elevation_deg": jf(c["elevation"], 3),
                                 "radials": int(len(c["rows"])), "gates": int(c["values"].shape[1])}
                                for c in cols],
        "base_sweep": int(base["sweep"]),
        "base_elevation_deg": jf(base["elevation"], 3),
        "rows": rows,
        "gates": gates,
        "echo_threshold_dbz": threshold,
        "hail_core": {
            "row": core_row, "gate": core_gate,
            "azimuth_deg": jf(base["row_az"][core_row], 3),
            "ground_range_m": jf(base["ground"][core_gate], 1),
            "lowest_sweep_dbz": jf(base["values"][core_row, core_gate], 2),
            "column": [{"height_m": jf(h, 2), "dbz": jf(v, 2), "sweep": int(s)} for h, v, s in core],
        },
        "upper_tilt_exceeds_base": upper_wins,
        "cells": cells,
    }
    write_golden("retrieve/volume.json", payload)


# ------------------------------------------------------------------------ vwp ---

VAD_SECTORS = 12
VAD_MIN_SAMPLES = 60
VAD_MIN_SECTORS = 8
VAD_GOOD_MIN_SECTORS = 10
VAD_MAX_GAP = 120.0
VAD_GOOD_MAX_GAP = 60.0
VAD_GOOD_MAX_RMS = 3.1
VAD_MAX_RMS = 5.2
VAD_GOOD_MAX_OUTLIERS = 0.30
VAD_MAX_OUTLIERS = 0.55
VAD_GOOD_MAX_STD_ERROR = 1.5
VAD_TRIM_SIGMA = 3.0
VAD_TRIM_MIN = 3.0
VAD_TRIM_MAX = 12.0
VAD_DEFAULT = {"min_slant_m": 5_000.0, "max_slant_m": 150_000.0, "annulus_m": 2_000.0,
               "max_mismatch_m": 300.0}


def median32_or_none(values):
    values = np.asarray(values, dtype=F32)
    values = values[np.isfinite(values)]
    return None if len(values) == 0 else float(median32(values))


def circular_mean_deg(values):
    radians = np.radians(np.asarray(values, dtype=np.float64))
    s, c = np.sin(radians).sum(), np.cos(radians).sum()
    if s == 0.0 and c == 0.0:
        return None
    return F32(np.degrees(np.arctan2(s, c)) % 360.0)


def vad_coverage(azimuths):
    if len(azimuths) == 0:
        return 0, 360.0
    az = np.asarray(azimuths, dtype=F32) % F32(360.0)
    sectors = len(set(min(int(np.floor(a / F32(360.0 / VAD_SECTORS))), VAD_SECTORS - 1) for a in az))
    az = np.sort(az)
    gaps = np.diff(az).tolist() if len(az) > 1 else []
    gaps.append(float(az[0] + F32(360.0) - az[-1]))
    return sectors, float(max(gaps)) if gaps else 0.0


def vad_fit(samples):
    """vwp.rs fit_samples: Vr = bias + u sin(az) cos(el) + v cos(az) cos(el) by normal
    equations; None when the 3x3 system is singular."""
    if len(samples) < 3:
        return None
    x = np.asarray([[1.0, math.sin(math.radians(az)) * math.cos(math.radians(el)),
                     math.cos(math.radians(az)) * math.cos(math.radians(el))]
                    for az, el, _, _ in samples])
    y = np.asarray([v for _, _, v, _ in samples], dtype=np.float64)
    normal = x.T @ x
    if abs(np.linalg.det(normal)) < 1e-10:
        return None
    inverse = np.linalg.inv(normal)
    bias, u, v = inverse @ (x.T @ y)
    residuals = y - x @ np.asarray([bias, u, v])
    rms = math.sqrt((residuals ** 2).sum() / len(samples))
    dof = max(len(samples) - 3, 1)
    variance = (residuals ** 2).sum() / dof
    std_error = math.sqrt(max(inverse[1, 1] * variance, 0.0) + max(inverse[2, 2] * variance, 0.0))
    return {"bias": bias, "u": u, "v": v, "rms": rms, "std_error": std_error,
            "predict": lambda az, el: bias + u * math.sin(math.radians(az)) * math.cos(math.radians(el))
            + v * math.cos(math.radians(az)) * math.cos(math.radians(el))}


def vad_candidate(cut_index, azimuths, elevations, nyquist, values, first_gate_m, spacing_m,
                  target_m, config):
    """vwp.rs candidate_for_height on one tilt: the annulus around the gate whose
    4/3-Earth beam height is nearest `target_m`, one median per radial, one sample per
    integer azimuth degree, the robust two-pass fit and the QC labels. Returns None (no
    candidate), or a dict with "accepted" or "rejected" and the diagnostics."""
    gates = values.shape[1]
    elevation = median32_or_none(elevations)
    if elevation is None or not (-1.0 <= elevation < 89.0):
        return None
    ranges = max(first_gate_m, 0) + np.arange(gates, dtype=np.float64) * spacing_m
    heights = np.asarray([beam_height_m(r, elevation) for r in ranges])
    in_range = (ranges >= config["min_slant_m"]) & (ranges <= config["max_slant_m"])
    if not in_range.any():
        return None
    errors = np.where(in_range, np.abs(heights - target_m), np.inf)
    center = int(np.argmin(errors))
    if errors[center] > config["max_mismatch_m"]:
        return None
    radius = int(round(config["annulus_m"] / spacing_m))
    start, end = max(center - radius, 0), min(center + radius, gates - 1)
    bins = [[] for _ in range(360)]
    for row in range(values.shape[0]):
        az, el = float(azimuths[row]), float(elevations[row])
        if not (np.isfinite(az) and np.isfinite(el)):
            continue
        median = median32_or_none(values[row, start:end + 1])
        if median is None:
            continue
        az = F32(az) % F32(360.0)
        n = nyquist[row]
        bins[min(int(np.floor(az)), 359)].append((az, el, median, np.isfinite(n) and n > 0))
    samples = []
    for b in bins:
        if not b:
            continue
        samples.append((circular_mean_deg([s[0] for s in b]), median32_or_none([s[1] for s in b]),
                        median32_or_none([s[2] for s in b]),
                        sum(1 for s in b if s[3]) * 2 >= len(b)))
    sectors, gap = vad_coverage([s[0] for s in samples])
    diagnostics = {"cut_index": cut_index, "height_m_agl": float(heights[center]),
                   "slant_range_m": float(ranges[center]), "elevation_deg": elevation,
                   "samples_total": len(samples), "samples_used": len(samples),
                   "azimuth_sectors": sectors, "max_azimuth_gap_deg": gap,
                   "outlier_fraction": 0.0, "rms_mps": None}

    def rejected(reason, stage):
        return {"rejected": reason, "stage": stage, "diagnostics": diagnostics}

    if len(samples) < VAD_MIN_SAMPLES:
        return rejected("InsufficientSamples", 1)
    if sectors < VAD_MIN_SECTORS or gap > VAD_MAX_GAP:
        return rejected("InsufficientAzimuthCoverage", 2)
    initial = vad_fit(samples)
    if initial is None:
        return rejected("IllConditionedFit", 3)
    residuals = np.asarray([s[2] - initial["predict"](s[0], s[1]) for s in samples], dtype=F32)
    center_res = median32(residuals)
    deviations = np.abs(residuals - center_res).astype(F32)
    robust_sigma = F32(median32(deviations) * F32(1.4826))
    limit = min(max(F32(VAD_TRIM_SIGMA) * robust_sigma, VAD_TRIM_MIN), VAD_TRIM_MAX)
    retained = [s for s, r in zip(samples, residuals) if abs(F32(r - center_res)) <= limit]
    outlier_fraction = 1.0 - len(retained) / len(samples)
    sectors, gap = vad_coverage([s[0] for s in retained])
    diagnostics.update(samples_used=len(retained), azimuth_sectors=sectors,
                       max_azimuth_gap_deg=gap, outlier_fraction=outlier_fraction)
    if len(retained) < VAD_MIN_SAMPLES:
        return rejected("InsufficientSamples", 4)
    if sectors < VAD_MIN_SECTORS or gap > VAD_MAX_GAP:
        return rejected("InsufficientAzimuthCoverage", 5)
    if outlier_fraction > VAD_MAX_OUTLIERS:
        return rejected("ExcessiveOutliers", 6)
    fit = vad_fit(retained)
    if fit is None:
        return rejected("IllConditionedFit", 7)
    diagnostics["rms_mps"] = fit["rms"]
    if fit["rms"] > VAD_MAX_RMS:
        return rejected("ResidualTooLarge", 8)
    good = (sectors >= VAD_GOOD_MIN_SECTORS and gap <= VAD_GOOD_MAX_GAP
            and fit["rms"] <= VAD_GOOD_MAX_RMS and outlier_fraction <= VAD_GOOD_MAX_OUTLIERS
            and fit["std_error"] <= VAD_GOOD_MAX_STD_ERROR)
    return {"accepted": {"u": fit["u"], "v": fit["v"], "bias": fit["bias"],
                         "std_error": fit["std_error"], "quality": "Good" if good else "Marginal",
                         "speed": math.hypot(fit["u"], fit["v"]),
                         "direction": math.degrees(math.atan2(-fit["u"], -fit["v"])) % 360.0},
            "diagnostics": diagnostics}


def choose_level(candidates, target_m):
    """vwp.rs compare_wind_candidates / compare_rejected_candidates."""
    accepted = [c for c in candidates if "accepted" in c]
    if accepted:
        best = min(accepted, key=lambda c: (
            0 if c["accepted"]["quality"] == "Good" else 1,
            c["diagnostics"]["rms_mps"], c["accepted"]["std_error"],
            -c["diagnostics"]["azimuth_sectors"],
            abs(c["diagnostics"]["height_m_agl"] - target_m), c["diagnostics"]["slant_range_m"]))
        return "retrieved", best
    rejected = [c for c in candidates if "rejected" in c]
    if rejected:
        best = max(rejected, key=lambda c: (
            c["stage"], c["diagnostics"]["samples_used"], c["diagnostics"]["azimuth_sectors"],
            -(c["diagnostics"]["rms_mps"] if c["diagnostics"]["rms_mps"] is not None else math.inf)))
        return "rejected", best
    return "no_coverage", None


def vwp_case(entry, targets_m, config=VAD_DEFAULT, pyart_levels=True):
    """The documented VAD profile from Py-ART's dealiased volume, plus Py-ART's own
    vad_browning wind at the retrieved heights on the same sweep."""
    import pyart

    radar = pyart_radar(entry)
    dealiased = pyart_dealiased(radar)
    sweeps, _ = level2_sweeps(entry)
    levels = []
    for target in targets_m:
        candidates = []
        for index, s in enumerate(sweeps):
            vel = s["moments"].get("VEL")
            if vel is None:
                continue
            values = dealiased[radar.get_slice(index)][:, :vel["gate_count"]]
            rows = vel["rows"]
            candidate = vad_candidate(index, s["az"][rows], s["el"][rows], s["nyquist"][rows],
                                      values, vel["first_gate_m"], vel["gate_spacing_m"], target, config)
            if candidate is not None:
                candidates.append(candidate)
        outcome, best = choose_level(candidates, target)
        level = {"target_m": target, "outcome": outcome}
        if best is not None:
            d = best["diagnostics"]
            level["diagnostics"] = {k: (jf(v, 4) if isinstance(v, float) else v) for k, v in d.items()}
            level["rejection"] = best.get("rejected")
            if "accepted" in best:
                a = best["accepted"]
                level["wind"] = {k: jf(v, 4) if isinstance(v, float) else v for k, v in a.items()}
                if pyart_levels:
                    one = radar.extract_sweeps([d["cut_index"]])
                    one.add_field("dealiased", {"data": np.ma.masked_invalid(
                        dealiased[radar.get_slice(d["cut_index"])])}, replace_existing=True)
                    z = np.asarray([d["height_m_agl"] - 100.0, d["height_m_agl"], d["height_m_agl"] + 100.0])
                    profile = pyart.retrieve.vad_browning(one, "dealiased", z_want=z)
                    level["pyart_vad_browning"] = {"u": jf(profile.u_wind[1], 4),
                                                   "v": jf(profile.v_wind[1], 4)}
        levels.append(level)
    doppler = [i for i, s in enumerate(sweeps) if "VEL" in s["moments"]]
    return {
        "id": entry,
        "sweeps": len(sweeps),
        "velocity_sweeps": doppler,
        "radar_altitude_m": float(radar.altitude["data"][0]),
        "config": config,
        "levels": levels,
    }


def section_vwp():
    payload = {
        "source": "tools/retrieve_golden.py vwp; Py-ART 2.2.5 read_nexrad_archive, "
                  "dealias_region_based and vad_browning, MetPy 1.7.1 Level2File, numpy VAD "
                  "(Browning and Wexler 1968) as documented in vwp.rs",
        "cases": [
            vwp_case("l2-kbox-20220129-150537", [float(h) for h in range(500, 4001, 500)]),
            vwp_case("l2-pahg-20250909-212549", [float(h) for h in range(500, 6001, 500)]),
            vwp_case("l2-kilx-20260418-013553", [float(h) for h in range(500, 6001, 250)]),
        ],
    }
    # The truncated-coverage case: the lowest split cut only (480 radials of 720), so a
    # 20 km level has no beam within 150 km slant range.
    sweeps, _ = level2_sweeps("l2-ktlx-20240315-000217-trim")
    doppler = next(i for i, s in enumerate(sweeps) if "VEL" in s["moments"])
    s = sweeps[doppler]
    elevation = float(np.median(s["el"]))
    payload["single_tilt"] = {
        "id": "l2-ktlx-20240315-000217-trim",
        "doppler_sweep": doppler,
        "elevation_deg": jf(elevation, 3),
        "radials": int(len(s["az"])),
        "max_beam_height_m_at_150km": jf(beam_height_m(150_000.0, elevation), 1),
        "levels": vwp_case("l2-ktlx-20240315-000217-trim", [1000.0, 20000.0],
                           pyart_levels=False)["levels"],
    }
    sector = dorade_sweep("dorade-noxp-20090525-203211-sector", ["VR"])
    az = np.asarray([float(r["azimuth_deg"]) for r in sector["rays"]])
    payload["sector"] = {
        "id": "dorade-noxp-20090525-203211-sector",
        "scan_mode": int(sector["scan_mode"]),
        "rays": len(sector["rays"]),
        "azimuth_min_deg": jf(az.min(), 2),
        "azimuth_max_deg": jf(az.max(), 2),
        "elevation_deg": jf(np.median([float(r["elevation_deg"]) for r in sector["rays"]]), 3),
        "first_cell_m": jf(sector["first_cell_m"], 1),
        "cell_spacing_m": jf(sector["cell_spacing_m"], 1),
    }
    write_golden("retrieve/vwp.json", payload)


SECTIONS = {
    "availability": section_availability,
    "detect": section_detect,
    "gbvtd": section_gbvtd,
    "shear": section_shear,
    "sweep": section_sweep,
    "volume": section_volume,
    "vwp": section_vwp,
}


def main(argv):
    names = argv or list(SECTIONS)
    for name in names:
        if name not in SECTIONS:
            raise SystemExit(f"unknown section {name}; choose from {', '.join(SECTIONS)}")
    for name in names:
        SECTIONS[name]()


if __name__ == "__main__":
    main(sys.argv[1:])
