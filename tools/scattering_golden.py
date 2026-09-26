#!/usr/bin/env python3
"""Golden values for the real-input tests of recast-radar-scattering.

The Rust unit tests in ``crates/recast-radar-scattering/src/{lut,p3_table,scheme_psd,
tmatrix_runtime}.rs`` load the committed PyTMatrix 0.3.3 lookup tables and the WRF P3
v5.4 tables from the corpus (``testdata/scattering/manifest.toml``) and compare what the
crate decodes, interpolates and parses against the JSON files this script writes:

    testdata/golden/scattering/tmatrix_luts.json
    testdata/golden/scattering/p3_tables.json
    testdata/golden/scattering/tmatrix_runtime.json
    testdata/golden/scattering/ishmael_source_check.json
    testdata/golden/scattering/ishmael_table_quadrature.json

Every expected value comes from outside the crate:

- LUT bytes are read here with ``struct``/``json`` from the schema-1 layout documented
  in ``crates/recast-radar-scattering/tools/pytmatrix-0.3.3/PACK_FORMAT.md`` and the
  generator ``manifest.json`` (8-byte magic, u16 LE schema, u32 LE header length, UTF-8
  JSON header, f64 LE payload point-major with the last declared axis fastest, nine
  components per node).
- The held-out nodes, the direct PyTMatrix results recomputed for them and the
  validator's own multilinear interpolation come from the post-freeze held-out report
  (corpus id ``tmatrix-held-out-interpolation-report-v10``); this script only checks
  that the report's ``lut_sha256`` pins the committed table and repeats the validator's
  interpolation with numpy on the payload read here.
- Prepared-plan layouts (strides, base index, active axes, upper offsets and fractions)
  are computed from the axis coordinates with the last-axis-fastest rule and
  ``fraction = (x - lower) / (upper - lower)``.
- P3 table records are read from the text with the record layout of the WRF v5.4
  tables (``module_mp_p3.F`` READ statements: 3 or 4 integer indices then 14 or 15 REAL
  fields per main record, ``mass rain rime value value`` collision records), never with
  the Rust parser.

Usage:
    python tools/scattering_golden.py [tmatrix_luts.json p3_tables.json tmatrix_runtime.json
                                       ishmael_source_check.json ishmael_table_quadrature.json]

Files come from the committed corpus (testdata/files/...) or the shared download cache
that recast-radar-testdata fills. The committed files were written with Python 3.13 and
numpy 2.5.3.
"""

import hashlib
import itertools
import json
import os
import struct
import sys
import tomllib
import urllib.request
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden" / "scattering"

COMPONENTS = ["zh", "zv", "hh_vv_covariance_real", "hh_vv_covariance_imaginary", "kdp", "ah", "av",
              "fall_speed_first_moment", "fall_speed_second_moment"]


# ------------------------------------------------------------------ corpus ---

def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"] if (TESTDATA / "manifest.toml").is_file() else []
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
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        return Path(os.environ["LOCALAPPDATA"], "recast-radar-tools", "testdata")
    if os.environ.get("XDG_CACHE_HOME"):
        return Path(os.environ["XDG_CACHE_HOME"], "recast-radar-tools", "testdata")
    if os.environ.get("HOME"):
        return Path(os.environ["HOME"], ".cache", "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_bytes(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        path = TESTDATA / entry["committed"].removeprefix("testdata/")
    else:
        path = cache_dir() / entry_id
        if not path.is_file():
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(entry["urls"][0], timeout=600) as r:
                path.write_bytes(r.read())
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise ValueError(f"{entry_id}: sha256/size mismatch ({digest}, {len(data)})")
    return data


# ---------------------------------------------------------------- LUTs ---

def read_lut(data):
    assert data[:8] == b"BRSLUT01", data[:8]
    schema, header_length = struct.unpack("<HI", data[8:14])
    assert schema == 1
    header_json = data[14:14 + header_length]
    header = json.loads(header_json.decode("utf-8"))
    payload = data[14 + header_length:]
    points = int(np.prod([len(axis["coordinates"]) for axis in header["axes"]]))
    values = np.frombuffer(payload, dtype="<f8").reshape(points, 9)
    return header, header_json, payload, values


def strides(axes):
    out = [1] * len(axes)
    for index in range(len(axes) - 2, -1, -1):
        out[index] = out[index + 1] * len(axes[index + 1]["coordinates"])
    return out


def bracket(coordinates, value):
    """(lower, upper, fraction) as the documented locate rule: exact nodes and
    singleton axes bracket to themselves with fraction 0."""
    first, last = coordinates[0], coordinates[-1]
    if value < first or value > last:
        raise ValueError(f"{value} outside [{first}, {last}]")
    if len(coordinates) == 1 or value == first:
        return 0, 0, 0.0
    if value == last:
        return len(coordinates) - 1, len(coordinates) - 1, 0.0
    upper = int(np.searchsorted(np.asarray(coordinates), value, side="left"))
    if coordinates[upper] == value:
        return upper, upper, 0.0
    lower = upper - 1
    return lower, upper, (value - coordinates[lower]) / (coordinates[upper] - coordinates[lower])


def multilinear(header, values, coordinates):
    """Reference multilinear interpolation, corner order lower-first per active axis."""
    axes = header["axes"]
    st = strides(axes)
    brackets = [bracket(axis["coordinates"], coordinates[axis["kind"]]) for axis in axes]
    active = [i for i, (lo, up, _) in enumerate(brackets) if lo != up]
    out = np.zeros(9)
    for corner in range(1 << len(active)):
        index = 0
        weight = 1.0
        bit = 0
        for axis_index, (lo, up, fraction) in enumerate(brackets):
            if lo == up:
                coordinate_index = lo
            else:
                if (corner >> bit) & 1:
                    weight *= fraction
                    coordinate_index = up
                else:
                    weight *= 1.0 - fraction
                    coordinate_index = lo
                bit += 1
            index += coordinate_index * st[axis_index]
        out += weight * values[index]
    return out


def prepared_plan(header, coordinates):
    axes = header["axes"]
    st = strides(axes)
    base = 0
    offsets = []
    fractions = []
    for axis_index, axis in enumerate(axes):
        lo, up, fraction = bracket(axis["coordinates"], coordinates[axis["kind"]])
        base += lo * st[axis_index]
        if lo != up:
            offsets.append((up - lo) * st[axis_index])
            fractions.append(fraction)
    return {"strides": st, "base_point_index": base, "active_axis_count": len(offsets),
            "corner_count": 1 << len(offsets), "upper_point_offsets": offsets, "upper_fractions": fractions}


def lut_golden(table_id, config_id, manifest_id, report):
    data = corpus_bytes(table_id)
    config = corpus_bytes(config_id)
    manifest = json.loads(corpus_bytes(manifest_id))
    header, header_json, payload, values = read_lut(data)
    lut_sha = hashlib.sha256(data).hexdigest()
    assert manifest["lut_sha256"] == lut_sha
    assert manifest["payload_sha256"] == hashlib.sha256(payload).hexdigest()
    assert manifest["generator_config_sha256"] == hashlib.sha256(config).hexdigest()
    assert header["config_sha256"] == hashlib.sha256(config).hexdigest()
    assert header["generator_config_utf8"].encode("utf-8") == config
    assert manifest["grid_point_count"] == values.shape[0]
    table = next(t for t in report["tables"] if t["lut_sha256"] == lut_sha)
    held_out = []
    for node in table["nodes"]:
        coordinates = node["coordinates"]
        reference = multilinear(header, values, coordinates)
        validator = np.array([node["lut_multilinear_interpolation"][c] for c in COMPONENTS])
        assert np.allclose(reference, validator, rtol=1e-9, atol=0), (coordinates, reference, validator)
        held_out.append({
            "coordinates": [coordinates[axis["kind"]] for axis in header["axes"]],
            "direct_pytmatrix": [node["direct_pytmatrix"][c] for c in COMPONENTS],
            "validator_interpolation": [node["lut_multilinear_interpolation"][c] for c in COMPONENTS],
            "within_thresholds": node["within_predeclared_interpolation_thresholds"],
            "node_index": node["node_index"],
        })
    axes = [{"kind": axis["kind"], "unit": axis["unit"], "count": len(axis["coordinates"]),
             "first": axis["coordinates"][0], "last": axis["coordinates"][-1]} for axis in header["axes"]]
    # Probe nodes: first, last and a middle point of the payload.
    probes = []
    for index in (0, values.shape[0] // 2, values.shape[0] - 1):
        multi = []
        rem = index
        for s in strides(header["axes"]):
            multi.append(rem // s)
            rem %= s
        probes.append({"index": index, "coordinate_indices": multi,
                       "coordinates": [header["axes"][a]["coordinates"][m] for a, m in enumerate(multi)],
                       "components": values[index].tolist()})
    # Prepared plans for two queries: one bracketing the first two non-singleton
    # axes, one on exact nodes (all singleton brackets).
    diameters = header["axes"][0]["coordinates"]
    ratios = header["axes"][1]["coordinates"]
    between = {header["axes"][0]["kind"]: diameters[1] + 0.3 * (diameters[2] - diameters[1]),
               header["axes"][1]["kind"]: (ratios[0] + ratios[1]) / 2.0}
    exact_last = {}
    for axis in header["axes"]:
        between.setdefault(axis["kind"], axis["coordinates"][0])
        exact_last[axis["kind"]] = axis["coordinates"][-1]
    exact_first = {axis["kind"]: axis["coordinates"][0] for axis in header["axes"]}
    plans = {}
    for name, query in (("between", between), ("exact_first", exact_first), ("exact_last", exact_last)):
        plans[name] = {"query": [query[axis["kind"]] for axis in header["axes"]],
                       "plan": prepared_plan(header, query),
                       "interpolation": multilinear(header, values, query).tolist()}
    return {
        "id": table_id, "lut_sha256": lut_sha, "config_sha256": header["config_sha256"],
        "payload_sha256": header["payload_sha256"], "payload_byte_length": len(payload),
        "header_byte_length": len(header_json), "grid_point_count": values.shape[0],
        "table_id": manifest["table_id"], "generator": header["generator"], "science": header["science"],
        "axes": axes, "probes": probes, "held_out": held_out, "plans": plans,
        "config_terminal_velocity": json.loads(config)["terminal_velocity"],
    }


def tmatrix_luts():
    report = json.loads(corpus_bytes("tmatrix-held-out-interpolation-report-v10"))
    nodes = json.loads(corpus_bytes("tmatrix-held-out-nodes-v10"))
    assert report["node_request_sha256"] == hashlib.sha256(
        corpus_bytes("tmatrix-held-out-nodes-v10")).hexdigest()
    out = {"report_id": report["report_id"], "thresholds": report["thresholds"],
           "selection_seed": report["selection_seed"], "held_out_node_count": report["held_out_node_count"],
           "tables": {}}
    for key, ids in (("rain", ("tmatrix-lut-rain-sband-pytmatrix-0.3.3",
                               "tmatrix-lut-rain-sband-pytmatrix-0.3.3-config",
                               "tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest")),
                     ("dry_ice", ("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3",
                                  "tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config",
                                  "tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest"))):
        out["tables"][key] = lut_golden(*ids, report)
    del nodes
    return out


# ------------------------------------------------- research runtime queries ---

TRIMMED_TABLES = {
    "property_dry": "tmatrix-lut-property-dry-oblate-sband-trim",
    "property_wet": "tmatrix-lut-property-wet-oblate-sband-trim",
    "property_rain": "tmatrix-lut-property-rain-sband-trim",
}


def table_record(entry_id, header, data, config):
    return {"id": entry_id, "lut_sha256": hashlib.sha256(data).hexdigest(),
            "table_id": json.loads(config)["table_id"],
            "axes": [{"kind": a["kind"], "coordinates": a["coordinates"]} for a in header["axes"]]}


def query_record(header, values, query):
    return {"query": [query[a["kind"]] for a in header["axes"]],
            "plan": prepared_plan(header, query),
            "interpolation": multilinear(header, values, query).tolist()}


def tmatrix_runtime():
    """Expected table values for the research-runtime tests (tmatrix_runtime.rs): the
    committed conventional dry-ice table and three tables trimmed from the property
    bundle (tools/trim_tmatrix_lut.py), read here from their bytes, with the reference
    multilinear interpolation at the queries the tests make."""
    out = {"tables": {}}
    data = corpus_bytes("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3")
    header, _, _, values = read_lut(data)
    frequency = header["axes"][2]["coordinates"][0]
    out["tables"]["dry_ice"] = table_record("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3", header, data,
                                            corpus_bytes("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config"))
    # closed_hail(7 mm) with axis ratio 0.9 at the singleton frequency and 0 deg.
    out["dry_ice_hail_7mm"] = query_record(header, values, {
        "equivolume_diameter": 0.007, "minor_to_major_axis_ratio": 0.9,
        "frequency": frequency, "radar_elevation": 0.0})

    tables = {}
    for key, entry_id in TRIMMED_TABLES.items():
        data = corpus_bytes(entry_id)
        config = corpus_bytes(entry_id + "-config")
        trim = json.loads(corpus_bytes(entry_id + "-trim"))
        header, _, payload, values = read_lut(data)
        assert trim["lut_sha256"] == hashlib.sha256(data).hexdigest()
        assert header["config_sha256"] == hashlib.sha256(config).hexdigest()
        assert header["generator_config_utf8"].encode("utf-8") == config
        assert header["payload_sha256"] == hashlib.sha256(payload).hexdigest()
        out["tables"][key] = table_record(entry_id, header, data, config)
        tables[key] = (header, values)

    header, values = tables["property_dry"]
    frequency = header["axes"][4]["coordinates"][0]
    # The dry particle node of the tests: 260 K, 1 mm, 400 kg m-3, axis ratio 0.8,
    # 1 deg elevation.
    out["property_dry_node"] = query_record(header, values, {
        "equivolume_diameter": 1.0e-3, "temperature": 260.0, "bulk_density": 400.0,
        "minor_to_major_axis_ratio": 0.8, "frequency": frequency, "radar_elevation": 1.0})

    header, values = tables["property_rain"]
    frequency = header["axes"][3]["coordinates"][0]
    for name, temperature in (("property_rain_273k", 273.15), ("property_rain_225k", 225.0)):
        # closed_rain: 1 mm drops, axis ratio 0.9, 1 deg elevation.
        out[name] = query_record(header, values, {
            "equivolume_diameter": 1.0e-3, "temperature": temperature,
            "minor_to_major_axis_ratio": 0.9, "frequency": frequency, "radar_elevation": 1.0})

    # The wet category of the tests (P3 category 1 at 272.15 K paired with rain: wet
    # mass 2e-4 kg/kg, effective density 802.16 kg m-3, 1e6 particles per kg, liquid
    # mass fraction 0.5, axis ratio 0.855) sits at D = cbrt(6 q / (pi rho N)) = 7.807e-5 m
    # and condensed volume fraction rho ((1 - w) / 917 + w / 999.84) = 0.8385. The box of
    # nodes around it bounds the interpolated reflectivity.
    header, values = tables["property_wet"]
    frequency = header["axes"][5]["coordinates"][0]
    rho, w, q, n = 802.1612656558998, 0.5, 2.0e-4, 1.0e6
    query = {"equivolume_diameter": (6.0 * q / (np.pi * rho * n)) ** (1.0 / 3.0),
             "temperature": 272.15, "condensed_volume_fraction": rho * ((1 - w) / 917.0 + w / 999.84),
             "liquid_mass_fraction": w, "minor_to_major_axis_ratio": 0.855,
             "frequency": frequency, "radar_elevation": 1.0}
    st = strides(header["axes"])
    brackets = [bracket(a["coordinates"], query[a["kind"]]) for a in header["axes"]]
    corners = []
    for combo in itertools.product(*[sorted({lo, up}) for lo, up, _ in brackets]):
        corners.append(float(values[sum(i * s for i, s in zip(combo, st))][0]))
    out["property_wet_box"] = {"query": [query[a["kind"]] for a in header["axes"]],
                               "brackets": [[lo, up] for lo, up, _ in brackets],
                               "zh_min": min(corners), "zh_max": max(corners), "corners": len(corners)}
    return out


# ------------------------------------------- ISHMAEL source-checked state ---

def ishmael_source_check():
    """Platform-independent reference for scheme_psd.rs
    exact_wrf_cold_aggregate_replays_source_final_check: the ISHMAEL
    reconstruction (IshmaelPsd::reconstruct), the WRF qnsmall/qasmall floors and
    var_check (module_mp_jensen_ishmael.F, as the Rust module documents them) and
    the cold-aggregate final check, evaluated with mpmath at 50 significant digits
    with the exact log-gamma. Constants and inputs are the exact binary64 values the
    Rust code uses; results are rounded to the nearest binary64 at the end, so they
    do not depend on any platform's libm."""
    import mpmath as mp

    mp.mp.dps = 50
    f = mp.mpf
    M = f(0.1e-6)
    SHAPE = f(4.0)
    DELTA = (f(0.55), f(1.30))
    DENS = (f(50.0), f(920.0))
    SRC_TOL = f(16) * f(2) ** -23
    MIN_AXIS, MIN_N, MIN_MOM = f(2.0e-6), f(1.25e-7), f(1.0e-24)
    VAR_MAX_AXIS, AGG_MAX_A = f(1.0e-3), f(0.5e-3)
    AGG_RATIO, COLD_DENS, MELT = f(0.2), f(50.0), f(273.15)

    def gamma_moment(n, scale, shape, power):
        return n * mp.exp(power * mp.log(scale) + mp.loggamma(shape + power) - mp.loggamma(shape))

    def mean_volume(a, delta):
        return f(4) / 3 * mp.pi * M ** (1 - delta) * gamma_moment(1, a, SHAPE, 2 + delta)

    def a_for_mass(mass, rho, delta):
        coefficient = (mp.log(f(4) / 3 * mp.pi) + (1 - delta) * mp.log(M)
                       + mp.loggamma(SHAPE + 2 + delta) - mp.loggamma(SHAPE))
        return mp.exp((mp.log(mass) - mp.log(rho) - coefficient) / (2 + delta))

    def c_for_a(a, delta):
        return M ** (1 - delta) * a ** delta

    def bounded(value, bounds):
        tol = SRC_TOL * max(abs(bounds[0]), abs(bounds[1]), f(1))
        if not (bounds[0] - tol <= value <= bounds[1] + tol):
            raise ValueError(f"{value} outside {bounds}")
        clamped = min(max(value, bounds[0]), bounds[1])
        return clamped, value - clamped

    def var_check(qice, n, a, c, delta):
        excursion = f(0)
        if delta < DELTA[0]:
            excursion = delta - DELTA[0]
            delta = DELTA[0]
            a = (c / M ** (1 - delta)) ** (1 / delta)
        elif delta > DELTA[1]:
            excursion = delta - DELTA[1]
            delta = DELTA[1]
            c = c_for_a(a, delta)
        mass = qice / n
        rho_raw = mass / mean_volume(a, delta) if a > MIN_AXIS else DENS[1]
        tol = SRC_TOL * DENS[1]
        projected = rho_raw < DENS[0] - tol or rho_raw > DENS[1] + tol
        rho = min(max(rho_raw, DENS[0]), DENS[1]) if projected else bounded(rho_raw, DENS)[0]
        if projected:
            a = a_for_mass(mass, rho, delta)
            c = c_for_a(a, delta)
        ratio = mp.exp(mp.loggamma(SHAPE + 2 + delta) - mp.loggamma(SHAPE))
        radius = mp.cbrt(qice / (n * rho * f(4) / 3 * mp.pi * ratio))
        small = radius < MIN_AXIS
        if small:
            n = qice / (rho * f(4) / 3 * mp.pi * MIN_AXIS ** 3 * ratio)
            a = a_for_mass(qice / n, rho, delta)
            c = c_for_a(a, delta)
        large = max(a, c) > VAR_MAX_AXIS
        if large:
            if a >= c:
                a = VAR_MAX_AXIS
                c = c_for_a(a, delta)
            else:
                c = VAR_MAX_AXIS
                a = (c / M ** (1 - delta)) ** (1 / delta)
            n = qice / (rho * mean_volume(a, delta))
        return {"n": n, "a": a, "c": c, "delta": delta, "rho": rho, "excursion": excursion,
                "projected": projected, "small": small, "large": large}

    def reconstruct(qice, n, qv, qa):
        a_src = mp.cbrt(qv * qv / (qa * n))
        c_raw = mp.cbrt(qa * qa / (qv * n))
        delta, _ = bounded(mp.log(c_raw / M) / mp.log(a_src / M), DELTA)
        rho_raw = (qice / n) / mean_volume(a_src, delta)
        tol = SRC_TOL * DENS[1]
        projected = rho_raw < DENS[0] - tol or rho_raw > DENS[1] + tol
        rho = min(max(rho_raw, DENS[0]), DENS[1]) if projected else bounded(rho_raw, DENS)[0]
        scale = (rho_raw / rho) ** (1 / (2 + delta)) if projected else f(1)
        a = a_src * scale
        return {"n": n, "a": a, "c": c_for_a(a, delta), "delta": delta, "rho": rho}

    def var_checked(qice, n, qv, qa):
        floors = n < MIN_N or qv < MIN_MOM or qa < MIN_MOM
        n, qv, qa = max(n, MIN_N), max(qv, MIN_MOM), max(qa, MIN_MOM)
        a = mp.cbrt(qv * qv / (qa * n))
        c = mp.cbrt(qa * qa / (qv * n))
        floors = floors or a < MIN_AXIS or c < MIN_AXIS
        a, c = max(a, MIN_AXIS), max(c, MIN_AXIS)
        state = var_check(qice, n, a, c, mp.log(c / M) / mp.log(a / M))
        if not (floors or state["excursion"] != 0 or state["projected"] or state["small"]
                or state["large"]):
            return reconstruct(qice, n, qv, qa)
        return state

    bits = (0x2ED0F2C7, 0x3D4B5185, 0x27E50322, 0x26855DBD)
    qice, qnice, qvoli, qaoli = (f(float(np.frombuffer(struct.pack("<I", b), dtype="<f4")[0]))
                                 for b in bits)
    temperature = f(244.504_241_943_359_38)
    incoming = var_checked(qice, qnice, qvoli, qaoli)
    assert temperature <= MELT
    gamma_ratio = mp.exp(mp.loggamma(SHAPE) - mp.loggamma(SHAPE - 1 + incoming["delta"]))
    c_after = AGG_RATIO * incoming["a"] * gamma_ratio
    if incoming["a"] > f(1.1) * M and c_after > f(1.1) * M:
        delta = min(mp.log(c_after / M) / mp.log(incoming["a"] / M), f(1))
    else:
        delta = f(1)
    mass = qice / incoming["n"]
    a = max(a_for_mass(mass, COLD_DENS, delta), MIN_AXIS)
    capped = a > AGG_MAX_A
    n = incoming["n"]
    if capped:
        a = AGG_MAX_A
        n = max(qice / (COLD_DENS * mean_volume(a, delta)), MIN_N)
    final = var_check(qice, n, a, c_for_a(a, delta), delta)
    qv_final = final["n"] * final["a"] ** 2 * final["c"]
    qa_final = final["n"] * final["a"] * final["c"] ** 2
    d6 = 64 * M ** (2 * (1 - final["delta"])) * gamma_moment(1, final["a"], SHAPE, 4 + 2 * final["delta"])
    values = {
        "a_scale_m": final["a"],
        "c_at_a_scale_m": final["c"],
        "aspect_power_delta": final["delta"],
        "bulk_density_kg_m3": final["rho"],
        "qvoli_source_projection_relative_change": (qv_final - qvoli) / qvoli,
        "qaoli_source_projection_relative_change": (qa_final - qaoli) / qaoli,
        "mean_equivolume_diameter_sixth_m6": d6,
    }
    return {
        "method": "mpmath 50-digit evaluation of the documented reconstruction, var_check "
                  "and cold-aggregate final check (exact log-gamma)",
        "input_f32_bits": [f"0x{b:08x}" for b in bits],
        "temperature_k": float(temperature),
        "flags": {"size_cap_applied": bool(capped), "final_small_ice": bool(final["small"]),
                  "final_large_ice": bool(final["large"]),
                  "final_density_projected": bool(final["projected"])},
        "values": {k: float(v) for k, v in values.items()},
        "digits": {k: mp.nstr(v, 30) for k, v in values.items()},
    }


# ------------------------------------------ ISHMAEL PSD over the dry-ice LUT ---

def ishmael_table_quadrature():
    """Platform-independent reference for scheme_psd.rs
    prepared_cpu_finish_matches_the_reference_quadrature: the ISHMAEL gamma PSD
    (planar ice, a_n 0.5 mm, c_n 0.425 mm, 400 kg m^-3, 200 per kg, dry air 1.2
    kg m^-3) integrated over the committed PyTMatrix dry-ice table with the
    quadrature the module documents, every step in mpmath at 50 digits:

    - the tail cutoff x where the regularized upper incomplete gamma Q(s, x) of the
      number, mass and D6 shapes (4, 6 + delta, 8 + 2 delta) is 1e-10;
    - the table-support interval in scaled size a/a_n: equivalent-volume diameter
      inside the table's diameter axis and minor/major axis ratio inside its ratio
      axis, for the oblate and the prolate branch of the power-law habit;
    - composite 8-point Gauss-Legendre rules on 8 (coarse) and 16 (refined) equal
      panels over [0, x], split at the support-interval ends, with the abscissae
      and weights as the module's binary64 constants;
    - at every node inside the table, the node's equivalent-volume diameter and
      axis ratio looked up by multilinear interpolation of the payload read here,
      weighted by the node's number density.

    The distribution parameters, the tail fraction and the rule constants are the
    binary64 values the Rust test uses; the reconstruction from them is exact here,
    where the crate rounds. Reports the refined sums, the coarse/refined convergence
    errors as the module defines them (|coarse - refined| / max(|coarse|, |refined|,
    absolute floor)) and the number/mass/D6 closure errors |sum + tail - 1|, which
    are the rule's truncation error."""
    import mpmath as mp

    mp.mp.dps = 50
    f = mp.mpf
    M = f(0.1e-6)
    SHAPE = f(4.0)
    table_id = "tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3"
    header, _, _, values = read_lut(corpus_bytes(table_id))
    axes = header["axes"]
    assert [axis["kind"] for axis in axes][:2] == ["equivolume_diameter", "minor_to_major_axis_ratio"]
    assert all(len(axis["coordinates"]) == 1 for axis in axes[2:])
    diameters = [f(v) for v in axes[0]["coordinates"]]
    ratios = [f(v) for v in axes[1]["coordinates"]]
    st = strides(axes)
    payload = [[f(float(x)) for x in row] for row in values]

    def locate(coordinates, value):
        if value < coordinates[0] or value > coordinates[-1]:
            raise ValueError(f"{value} outside the axis")
        if value == coordinates[0]:
            return 0, 0, f(0)
        if value == coordinates[-1]:
            return len(coordinates) - 1, len(coordinates) - 1, f(0)
        upper = next(i for i, c in enumerate(coordinates) if c >= value)
        if coordinates[upper] == value:
            return upper, upper, f(0)
        return upper - 1, upper, (value - coordinates[upper - 1]) / (coordinates[upper] - coordinates[upper - 1])

    def interpolate(diameter, ratio):
        brackets = (locate(diameters, diameter), locate(ratios, ratio))
        out = [f(0)] * 9
        for corner in itertools.product((0, 1), repeat=2):
            weight, index = f(1), 0
            for axis, ((lower, upper, fraction), take_upper) in enumerate(zip(brackets, corner)):
                if lower == upper:
                    if take_upper:
                        weight = f(0)
                    index += lower * st[axis]
                elif take_upper:
                    weight *= fraction
                    index += upper * st[axis]
                else:
                    weight *= 1 - fraction
                    index += lower * st[axis]
            if weight != 0:
                out = [o + weight * v for o, v in zip(out, payload[index])]
        return out

    a_scale, c_scale, density = f(0.5e-3), f(0.425e-3), f(400.0)
    number_per_kg, air_density = f(200.0), f(1.2)
    delta = mp.log(c_scale / M) / mp.log(a_scale / M)
    number_density = number_per_kg * air_density

    def gamma_moment(power):
        return mp.exp(power * mp.log(a_scale) + mp.loggamma(SHAPE + power) - mp.loggamma(SHAPE))

    def q(shape, x):
        return mp.gammainc(shape, x, mp.inf, regularized=True)

    shapes = (SHAPE, SHAPE + 2 + delta, SHAPE + 4 + 2 * delta)
    tail_fraction = f(1.0e-10)
    # Q grows with the shape, so the D6 shape sets the cutoff.
    cutoff = mp.findroot(lambda x: q(shapes[2], x) - tail_fraction, 40)
    assert all(q(shape, cutoff) <= tail_fraction * (1 + f(10) ** -40) for shape in shapes)
    tails = [q(shape, cutoff) for shape in shapes]

    diameter_at_scale = 2 * mp.cbrt(a_scale * a_scale * c_scale)

    def scaled_for_diameter(diameter):
        return mp.exp(3 * (mp.log(diameter) - mp.log(diameter_at_scale)) / (2 + delta))

    log_ratio_at_scale = mp.log(c_scale / a_scale)
    intervals = []
    for log_bounds in ((mp.log(ratios[0]), mp.log(ratios[-1])),      # oblate: c/a = ratio
                       (-mp.log(ratios[-1]), -mp.log(ratios[0]))):   # prolate: c/a = 1/ratio
        ends = [mp.exp((bound - log_ratio_at_scale) / (delta - 1)) for bound in log_bounds]
        lower = max(scaled_for_diameter(diameters[0]), min(ends), f(0))
        upper = min(scaled_for_diameter(diameters[-1]), max(ends), cutoff)
        if upper > lower:
            intervals.append((lower, upper))
    assert len(intervals) == 1

    gl_abscissae = [f(v) for v in (-0.960_289_856_497_536_3, -0.796_666_477_413_626_7,
                                   -0.525_532_409_916_329, -0.183_434_642_495_649_8,
                                   0.183_434_642_495_649_8, 0.525_532_409_916_329,
                                   0.796_666_477_413_626_7, 0.960_289_856_497_536_3)]
    gl_weights = [f(v) for v in (0.101_228_536_290_376_3, 0.222_381_034_453_374_5,
                                 0.313_706_645_877_887_3, 0.362_683_783_378_362,
                                 0.362_683_783_378_362, 0.313_706_645_877_887_3,
                                 0.222_381_034_453_374_5, 0.101_228_536_290_376_3)]
    mean_mass = density * f(4) / 3 * mp.pi * M ** (1 - delta) * gamma_moment(2 + delta)
    mean_d6 = 64 * M ** (2 * (1 - delta)) * gamma_moment(4 + 2 * delta)

    def rule(panels):
        breakpoints = sorted([i * cutoff / panels for i in range(panels + 1)]
                             + [end for interval in intervals for end in interval])
        segments = [breakpoints[0]]
        for point in breakpoints[1:]:
            if point - segments[-1] > 32 * f(2) ** -52 * max(abs(point), f(1)):
                segments.append(point)
        sums, fractions, evaluated = [f(0)] * 9, [f(0)] * 3, 0
        for left, right in zip(segments, segments[1:]):
            half, middle = (right - left) / 2, (right + left) / 2
            for abscissa, weight in zip(gl_abscissae, gl_weights):
                x = middle + half * abscissa
                number_fraction = half * weight * x ** 3 * mp.exp(-x) / mp.gamma(SHAPE)
                a = a_scale * x
                c = M ** (1 - delta) * a ** delta
                diameter = 2 * mp.cbrt(a * a * c)
                ratio = min(c / a, a / c)
                fractions[0] += number_fraction
                fractions[1] += number_fraction * density * f(4) / 3 * mp.pi * a * a * c / mean_mass
                fractions[2] += number_fraction * diameter ** 6 / mean_d6
                if diameters[0] <= diameter <= diameters[-1] and ratios[0] <= ratio <= ratios[-1]:
                    evaluated += 1
                    weight_m3 = number_density * number_fraction
                    sums = [total + weight_m3 * value
                            for total, value in zip(sums, interpolate(diameter, ratio))]
        return sums, fractions, evaluated, len(segments) - 1

    coarse, _, coarse_nodes, coarse_segments = rule(8)
    refined, fractions, refined_nodes, refined_segments = rule(16)
    absolute_floors = [1.0e-10, 1.0e-10, 1.0e-10, 1.0e-10, 1.0e-8, 1.0e-8, 1.0e-8, 1.0e-10, 1.0e-10]
    convergence = [abs(c - r) / max(abs(c), abs(r), f(floor))
                   for c, r, floor in zip(coarse, refined, absolute_floors)]
    closure = [abs(fraction + tail - 1) for fraction, tail in zip(fractions, tails)]
    components = ["zh", "zv", "covariance_re", "covariance_im", "kdp", "ah", "av",
                  "fall_speed_first", "fall_speed_second"]
    worst = max(range(9), key=lambda index: convergence[index])
    return {
        "method": "mpmath 50-digit evaluation of the documented ISHMAEL table quadrature "
                  "(exact tail cutoff, support interval, composite GL8, multilinear lookup)",
        "table": {"id": table_id, "sha256": hashlib.sha256(corpus_bytes(table_id)).hexdigest()},
        "distribution": {"category": "planar", "a_scale_m": 0.5e-3, "c_at_a_scale_m": 0.425e-3,
                         "bulk_density_kg_m3": 400.0, "number_per_kg": 200.0,
                         "dry_air_density_kg_m3": 1.2, "maximum_tail_fraction": 1.0e-10},
        "upper_scaled_a": float(cutoff),
        "support_interval_scaled_a": [float(v) for v in intervals[0]],
        "segments": {"coarse": coarse_segments, "refined": refined_segments},
        "nodes_evaluated": {"coarse": coarse_nodes, "refined": refined_nodes},
        "components": components,
        "coarse": [float(v) for v in coarse],
        "refined": [float(v) for v in refined],
        "refined_digits": [mp.nstr(v, 30) for v in refined],
        "convergence_errors": [float(v) for v in convergence],
        "maximum_convergence_component": worst,
        "closure_errors": {"number": float(closure[0]), "mass": float(closure[1]),
                           "d6": float(closure[2])},
    }


# ------------------------------------------------------------ P3 tables ---

def p3_records(text, indices, fields):
    """Main records of a P3 table text: list of (index tuple, field list) read
    from the text in file order, skipping the collision records (which have
    exactly 5 tokens: mass rain rime value value)."""
    lines = text.split("\n")
    header, separator = lines[0], lines[1]
    main = []
    collision = 0
    for line in lines[2:]:
        if not line:
            continue
        tokens = line.split()
        if len(tokens) == indices + fields:
            main.append((tuple(int(t) for t in tokens[:indices]),
                         [float(np.float32(t)) for t in tokens[indices:]]))
        elif len(tokens) == 5:
            collision += 1
        else:
            raise ValueError(f"unexpected record {line!r}")
    return header, separator, main, collision


def p3_golden(entry_id, indices, fields, lambda_field, mu_field, density_field=None):
    text = corpus_bytes(entry_id).decode("ascii")
    header, separator, main, collision = p3_records(text, indices, fields)
    samples = []
    for position in (0, 1, 49, len(main) // 2, len(main) - 1):
        index, values = main[position]
        sample = {"position": position, "index": list(index), "inverse_qmin": values[6],
                  "inverse_qmax": values[7], "lambda": values[lambda_field], "mu": values[mu_field]}
        if density_field is not None:
            sample["mean_density"] = values[density_field]
        samples.append(sample)
    inverse = [v[6] for _, v in main]
    return {"id": entry_id, "header": header, "separator": separator, "main_records": len(main),
            "collision_records": collision, "data_rows": len(main) + collision,
            "lines": text.count("\n"), "bytes": len(text.encode("ascii")),
            "first_line": text.split("\n")[2], "samples": samples,
            "inverse_qmin_min": min(inverse), "inverse_qmin_max": max(inverse),
            "all_qmin_ge_qmax": all(v[6] >= v[7] for _, v in main),
            "all_qmin_positive": all(v[6] > 0.0 for _, v in main)}


def p3_tables():
    return {
        "two_moment_first_block": p3_golden("wrf-p3-lookup-table-1-v5.4-2momI-first-block", 3, 14, 12, 13),
        "three_moment_first_block": p3_golden("wrf-p3-lookup-table-1-v5.4-3momI-first-block", 4, 15, 13, 14, 11),
        "two_moment": p3_golden("wrf-p3-lookup-table-1-v5.4-2momI", 3, 14, 12, 13),
        "three_moment": p3_golden("wrf-p3-lookup-table-1-v5.4-3momI", 4, 15, 13, 14, 11),
    }


def main(argv):
    GOLDEN.mkdir(parents=True, exist_ok=True)
    builds = (("tmatrix_luts.json", tmatrix_luts), ("p3_tables.json", p3_tables),
              ("tmatrix_runtime.json", tmatrix_runtime),
              ("ishmael_source_check.json", ishmael_source_check),
              ("ishmael_table_quadrature.json", ishmael_table_quadrature))
    for name, build in builds:
        if argv and name not in argv:
            continue
        path = GOLDEN / name
        with open(path, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(build(), fh, indent=1, sort_keys=True)
            fh.write("\n")
        print(f"wrote {path} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main(sys.argv[1:])
