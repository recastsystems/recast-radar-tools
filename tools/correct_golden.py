#!/usr/bin/env python3
"""Golden values for the recast-radar-correct tests: Py-ART region-based
dealiasing on real Level II sweeps.

For each case in CASES (a manifest id, a Py-ART sweep index and optionally a
model wind fixture), this script

1. resolves the file from the corpus (committed under ``testdata/`` or the
   shared download cache, downloading it from the manifest URL if missing)
   and checks its sha256 against the manifest;
2. reads it with ``pyart.io.read_nexrad_archive`` (Py-ART 2.2.5) and takes
   the sweep with ``Radar.extract_sweeps``;
3. runs ``pyart.correct.dealias_region_based`` with its shipped defaults
   (interval_splits=3, skip_between_rays=skip_along_ray=100, centered=True,
   default GateFilter, per-sweep Nyquist from the file), except that
   ``rays_wrap_around`` is set from the sweep's azimuth coverage: True only
   when the first and last rays are adjacent (a full 360 deg sweep). Py-ART's
   own default (True for every PPI) would join the two ends of a trimmed
   120-radial sector 60 deg apart. Sweeps whose rays all carry Nyquist 0
   (TDWR) are not dealiased (Py-ART raises OverflowError on them); their
   golden has ``dealiased false`` and no folds;
4. with a wind fixture (``crates/recast-radar-bench/fixtures/dealias/``, real
   HRRR/RAP analyses at the site), projects the profile onto every gate
   (4/3-earth beam height, Doviak & Zrnic 1993 eq. 2.28b; u, v linear in
   height and constant beyond the profile ends; v_r = (u sin az + v cos az)
   cos el) and records ``env_offset``, the global fold k for which the most
   valid gates satisfy |dealiased + 2Nk - v_r| <= N (Py-ART anchors each sweep
   by its mean fold, so its output is known only up to one such offset), and
   ``env_within_nyquist``, that gate count;
5. writes ``crates/recast-radar-correct/tests/golden/<case>.txt``.

Fold per gate = round((dealiased - raw) / (2 * nyquist_of_ray)); the script
asserts that the residual is below 0.01 m/s, i.e. that Py-ART moved every gate
by whole Nyquist intervals, and that Py-ART's output mask equals the input
mask.

File format (text):

    # comment lines
    key value                       header: case, id, sha256, sweep,
                                    fixed_angle, rays, first_gate_m,
                                    gate_spacing_m, rays_wrap_around,
                                    nyquist_mps (median), valid_gates,
                                    dealiased, unfolded_gates (|dealiased -
                                    raw| > median Nyquist), [env_fixture,
                                    env_valid_time, env_offset,
                                    env_within_nyquist], file_sweeps,
                                    file_velocity_sweeps (sweeps with any
                                    valid velocity gate)
    rays:
    <ray> <azimuth> <nyquist> <valid> <gate_index_sum> <raw_sum> <runs>

    valid: number of gates with valid velocity; gate_index_sum: sum of their
    gate indices on the Py-ART range axis; raw_sum: sum of their raw
    velocities (m/s). These three are independent of the Rust decoder, so a
    test checks that its decoded rows line up with Py-ART's rays gate for gate
    before using the folds. runs: the Py-ART fold of each valid gate in gate
    order, run-length encoded as space-separated "count*fold".

Run with the venv that has arm_pyart 2.2.5:

    python tools/correct_golden.py [--case NAME ...] [--no-download]
"""

import argparse
import copy
import hashlib
import json
import os
import sys
import tempfile
import tomllib
import urllib.request
import warnings
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
OUT = ROOT / "crates" / "recast-radar-correct" / "tests" / "golden"
ENV_FIXTURES = ROOT / "crates" / "recast-radar-bench" / "fixtures" / "dealias"
# Effective earth radius of the 4/3-earth beam model (Doviak & Zrnic 1993, eq. 2.28b).
EFFECTIVE_EARTH_RADIUS_M = 4.0 / 3.0 * 6371000.0

# name -> (manifest id, Py-ART sweep index[, environmental wind fixture]).
# Sweep indices count every sweep in the file, velocity-less surveillance
# sweeps included, in file order.
CASES = {
    # trimmed split cuts (the Doppler half is sweep 1)
    "kdvn_20200810_trim_s1": ("l2-kdvn-20200810-180401-trim", 1),
    "kbox_20220129_trim_s1": ("l2-kbox-20220129-150537-trim", 1),
    "klix_20210829_trim_s1": ("l2-klix-20210829-180425-trim", 1),
    "ktlx_20130520_trim_s1": ("l2-ktlx-20130520-201643-trim", 1),
    "tstl_20230331_trim_s1": ("l2-tstl-20230331-230314-trim", 1),
    # full volumes
    "klix_20210829_s1": ("l2-klix-20210829-180425", 1, "env_klix_hrrr.json"),
    "klix_20210829_s2": ("l2-klix-20210829-180425", 2, "env_klix_hrrr.json"),
    "klix_20210829_s9": ("l2-klix-20210829-180425", 9, "env_klix_hrrr.json"),
    "klix_20210829_s13": ("l2-klix-20210829-180425", 13, "env_klix_hrrr.json"),
    "kdvn_20200810_s1": ("l2-kdvn-20200810-180401", 1),
    "pahg_20250909_s1": ("l2-pahg-20250909-212549", 1),
    "ktlx_20130520_s1": ("l2-ktlx-20130520-201643", 1, "env_ktlx.json"),
    "ktlx_20130520_s3": ("l2-ktlx-20130520-201643", 3, "env_ktlx.json"),
}


def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"] if (TESTDATA / "manifest.toml").is_file() else []
    paths += sorted(p / "manifest.toml" for p in TESTDATA.iterdir()
                    if p.is_dir() and (p / "manifest.toml").is_file())
    for path in paths:
        with open(path, "rb") as fh:
            files += tomllib.load(fh).get("file", [])
    return {entry["id"]: entry for entry in files}


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    for var, suffix in (("LOCALAPPDATA", ()), ("XDG_CACHE_HOME", ()), ("HOME", (".cache",))):
        if var == "LOCALAPPDATA" and os.name != "nt":
            continue
        if os.environ.get(var):
            return Path(os.environ[var], *suffix, "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def corpus_path(entry, allow_download):
    if entry.get("committed"):
        rel = Path(entry["committed"])
        path = ROOT / rel if rel.parts[0] == "testdata" else TESTDATA / rel
    else:
        path = cache_dir() / entry["id"]
        if not path.is_file():
            if not allow_download:
                raise FileNotFoundError(f"{entry['id']} is not cached at {path}")
            path.parent.mkdir(parents=True, exist_ok=True)
            for url in entry.get("urls", []):
                fd, tmp = tempfile.mkstemp(prefix=f".{entry['id']}.", suffix=".part",
                                           dir=path.parent)
                os.close(fd)
                try:
                    urllib.request.urlretrieve(url, tmp)
                    if sha256_file(tmp) == entry["sha256"]:
                        os.replace(tmp, path)
                        break
                except OSError as error:
                    print(f"  download {url}: {error}", file=sys.stderr)
                finally:
                    if os.path.exists(tmp):
                        os.remove(tmp)
            else:
                raise FileNotFoundError(f"could not download {entry['id']}")
    actual = sha256_file(path)
    if actual != entry["sha256"]:
        raise ValueError(f"{entry['id']}: sha256 {actual} != manifest {entry['sha256']}")
    return path


_STATION_TABLE = None


def read_radar(path):
    """read_nexrad_archive with Py-ART's station table restored first (Py-ART
    2.2.5 converts TDWR station elevations in place on every read)."""
    global _STATION_TABLE
    import pyart
    from pyart.io import nexrad_common

    if _STATION_TABLE is None:
        _STATION_TABLE = copy.deepcopy(nexrad_common.NEXRAD_LOCATIONS)
    nexrad_common.NEXRAD_LOCATIONS.clear()
    nexrad_common.NEXRAD_LOCATIONS.update(copy.deepcopy(_STATION_TABLE))
    return pyart.io.read_nexrad_archive(str(path))


def rays_wrap(azimuth):
    """First and last rays adjacent: angular gap at most 3 typical spacings."""
    rays = len(azimuth)
    if rays < 8:
        return False
    first, last = float(azimuth[0]), float(azimuth[-1])
    gap = min((first - last) % 360.0, (last - first) % 360.0)
    return gap <= 3.0 * 360.0 / rays


def runs(values):
    """Run-length encode a list of ints as "count*value" tokens."""
    out = []
    i = 0
    while i < len(values):
        j = i
        while j < len(values) and values[j] == values[i]:
            j += 1
        out.append(f"{j - i}*{values[i]}")
        i = j
    return " ".join(out)


def environmental_offset(fixture, single, dealiased, raw_mask, nyquist):
    """The whole-sweep branch of Py-ART's output closest to a model wind profile.

    Py-ART anchors each sweep by its own mean fold, so its output is known only
    up to one global 2N offset. Project the profile (u, v at beam height,
    4/3-earth model, linear in height and constant beyond the ends) onto every
    gate, v_env = (u sin az + v cos az) cos el, and return the integer k for
    which the most valid gates satisfy |dealiased + 2Nk - v_env| <= N, with
    that gate count."""
    profile = json.loads((ENV_FIXTURES / fixture).read_text(encoding="utf-8"))
    heights = np.array([level["height_m_arl"] for level in profile["levels"]])
    u = np.array([level["u_mps"] for level in profile["levels"]])
    v = np.array([level["v_mps"] for level in profile["levels"]])
    r = single.range["data"][None, :].astype(np.float64)
    el = np.deg2rad(single.elevation["data"].astype(np.float64))[:, None]
    az = np.deg2rad(single.azimuth["data"].astype(np.float64))[:, None]
    ae = EFFECTIVE_EARTH_RADIUS_M
    height = np.sqrt(r * r + ae * ae + 2.0 * r * ae * np.sin(el)) - ae
    projected = (np.interp(height, heights, u) * np.sin(az)
                 + np.interp(height, heights, v) * np.cos(az)) * np.cos(el)
    n = nyquist.astype(np.float64)[:, None]
    values = dealiased.data.astype(np.float64)
    valid = ~raw_mask
    best = None
    for k in range(-3, 4):
        within = int(np.count_nonzero(valid & (np.abs(values + 2.0 * n * k - projected) <= n)))
        if best is None or within > best[1]:
            best = (k, within)
    return profile, best


def golden_case(name, entry, sweep, fixture, allow_download):
    import pyart

    path = corpus_path(entry, allow_download)
    radar = read_radar(path)
    single = radar.extract_sweeps([sweep])
    azimuth = single.azimuth["data"]
    wraps = rays_wrap(azimuth)
    nyquist = single.instrument_parameters["nyquist_velocity"]["data"]
    raw = single.fields["velocity"]["data"]
    if np.all(nyquist <= 0):
        # TDWR files carry Nyquist 0; dealias_region_based cannot run
        # (OverflowError in _find_sweep_interval_splits). Record the sweep
        # and its raw rows without folds.
        dealiased = None
    else:
        dealiased = pyart.correct.dealias_region_based(single, rays_wrap_around=wraps)["data"]

    raw_mask = np.ma.getmaskarray(raw)
    out_mask = raw_mask if dealiased is None else np.ma.getmaskarray(dealiased)
    if not np.array_equal(raw_mask, out_mask):
        raise AssertionError(f"{name}: Py-ART output mask differs from the input mask")
    ranges = single.range["data"]
    spacing = float(ranges[1] - ranges[0])
    median_nyquist = float(np.median(nyquist))

    lines = []
    valid_total = 0
    unfolded_total = 0
    for ray in range(single.nrays):
        n = float(nyquist[ray])
        folds = []
        valid = 0
        index_sum = 0
        raw_sum = 0.0
        for gate in range(single.ngates):
            if raw_mask[ray, gate]:
                continue
            v = float(raw.data[ray, gate])
            valid += 1
            index_sum += gate
            raw_sum += v
            if dealiased is None:
                continue
            d = float(dealiased.data[ray, gate])
            fold = round((d - v) / (2.0 * n))
            if abs(d - v - 2.0 * n * fold) > 0.01:
                raise AssertionError(f"{name}: ray {ray} gate {gate}: {d} - {v} is not whole folds")
            folds.append(fold)
            if abs(d - v) > median_nyquist:
                unfolded_total += 1
        valid_total += valid
        lines.append(f"{ray} {float(azimuth[ray]):.3f} {n:.3f} {valid} {index_sum} {raw_sum:.1f} {runs(folds)}")

    header = [
        "# Py-ART region-based dealiasing golden, generated by tools/correct_golden.py; do not edit.",
        f"# pyart {pyart.__version__}, numpy {np.__version__}",
        f"case {name}",
        f"id {entry['id']}",
        f"sha256 {entry['sha256']}",
        f"sweep {sweep}",
        f"fixed_angle {float(radar.fixed_angle['data'][sweep]):.3f}",
        f"rays {single.nrays}",
        f"first_gate_m {float(ranges[0]):.1f}",
        f"gate_spacing_m {spacing:.1f}",
        f"rays_wrap_around {str(wraps).lower()}",
        f"nyquist_mps {median_nyquist:.3f}",
        f"valid_gates {valid_total}",
        f"dealiased {str(dealiased is not None).lower()}",
        f"unfolded_gates {unfolded_total}",
    ]
    if fixture is not None:
        profile, (k, within) = environmental_offset(fixture, single, dealiased, raw_mask, nyquist)
        header += [
            f"env_fixture {fixture}",
            f"env_valid_time {profile['valid_time']}",
            f"env_offset {k}",
            f"env_within_nyquist {within}",
        ]
    velocity_sweeps = sum(
        1 for s in range(radar.nsweeps)
        if np.ma.count(radar.fields["velocity"]["data"][radar.get_slice(s)]) > 0
    )
    header += [f"file_sweeps {radar.nsweeps}", f"file_velocity_sweeps {velocity_sweeps}", "rays:"]
    OUT.mkdir(parents=True, exist_ok=True)
    target = OUT / f"{name}.txt"
    target.write_text("\n".join(header + lines) + "\n", encoding="utf-8", newline="\n")
    print(f"{name}: {entry['id']} sweep {sweep} ({float(radar.fixed_angle['data'][sweep]):.2f} deg), "
          f"{single.nrays} rays, wraps {wraps}, Nyquist {median_nyquist:.2f}, "
          f"{unfolded_total} of {valid_total} gates unfolded -> {target.relative_to(ROOT)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--case", action="append", choices=sorted(CASES))
    parser.add_argument("--no-download", action="store_true")
    args = parser.parse_args()
    manifest = load_manifest()
    for name in args.case or list(CASES):
        entry_id, sweep, *fixture = CASES[name]
        golden_case(name, manifest[entry_id], sweep, fixture[0] if fixture else None,
                    not args.no_download)


if __name__ == "__main__":
    main()
