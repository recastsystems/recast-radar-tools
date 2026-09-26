"""DORADE golden from LROSE RadxPrint, an independent DORADE reader.

Writes testdata/golden/dorade/radxprint.json.

For each committed DORADE sweepfile (`files`): the ray count, the sweep mode,
platform type and primary axis, and per ray what `RadxPrint -rays` prints for
it (time, azimuth, elevation, antenna transition flag, true scan rate,
measured transmit power, Nyquist velocity) and for its georeference (every
ASIB value: position, platform velocities, heading, roll, pitch, drift,
rotation, tilt, winds and the heading and pitch change rates). LROSE keeps
antenna-transition rays and flags them (`antennaTransition: 1`), and reads the
georeference as stored (no CFAC corrections without `-apply_georefs`), which
is what the Rust decoder is checked against in
crates/recast-radar-io-dorade/tests/radxprint_real.rs.

For every file, also what `RadxConvert -cfradial` writes: the global attribute
`platform_is_mobile` and the sixteen geometry correction variables
(`azimuth_correction` ... `tilt_correction`), which Radx fills from the CFAC
block without applying them (none for a sweepfile without a CFAC block).

For each full airborne sweepfile of which only a head trim is committed
(`full_files`): the ray count, sweep mode, platform type, primary axis and the
`history` Radx reads from the SEDS block at the end of the file. These files
are manifest download entries; the script takes them from the testdata cache
(RECAST_RADAR_TESTDATA, else %LOCALAPPDATA%/recast-radar-tools/testdata or
~/.cache/recast-radar-tools/testdata) or downloads them from the manifest URL,
and checks their SHA-256 either way.

RadxPrint runs in a Docker container with LROSE installed (the `nexbench`
container of this project, LROSE release 20250811):

    python tools/dorade_radx_golden.py [--container nexbench] [--out PATH]

The files are copied into the container with `docker cp`; nothing is read
from this workspace's decoders.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = {
    "dorade-cow2-20260521-225514-sur-head24": "swp.1260521225514.COW2.229.1.0_SUR_v215.head24",
    "dorade-dow6-20211230-222139-rhi-head41": "swp.1211230222139.DOW6low.648.144.0_RHI_v169.head41",
    "dorade-noxp-20090501-190244-ppi": "swp.1090501190244.NOXPRVP.0.0.5_PPI_v1",
    "dorade-noxp-20090525-203211-sector": "swp.1090525203211.NOXPRVP.0.0.5_PPI_v1",
    "dorade-noxp-20090610-003210-ppi-head6": "swp.1090610003210.NOXPRVP.0.0.5_PPI_v1.head6",
    "dorade-n42rf-ts-20181010-122951-air-head24":
        "swp.1181010122951.N42RF-TS.196.-20.0_AIR_v3394.head24",
    "dorade-n42rf-tm-20181010-123925-air-head48":
        "swp.1181010123925.N42RF-TM.137.20.0_AIR_v3532.head48",
}
FULL_FILES = [
    "dorade-n42rf-ts-20181010-122951-air",
    "dorade-n42rf-tm-20181010-123925-air",
]
RAY_KEYS = {
    "timeSecs": "time",
    "az": "azimuth_deg",
    "elev": "elevation_deg",
    "antennaTransition": "antenna_transition",
    "trueScanRate": "true_scan_rate_deg_per_s",
    "measXmitPowerDbmH": "transmit_power_h_dbm",
    "nyquistMps": "nyquist_mps",
}
# RadxGeoref members DoradeRadxFile fills from the ASIB (platform_i), in
# ASIB order.
GEOREF_KEYS = {
    "longitude": "longitude_deg",
    "latitude": "latitude_deg",
    "altitudeKmMsl": "altitude_msl_km",
    "altitudeKmAgl": "altitude_agl_km",
    "ewVelocity": "ew_velocity_mps",
    "nsVelocity": "ns_velocity_mps",
    "vertVelocity": "vert_velocity_mps",
    "heading": "heading_deg",
    "roll": "roll_deg",
    "pitch": "pitch_deg",
    "drift": "drift_deg",
    "rotation": "rotation_deg",
    "tilt": "tilt_deg",
    "ewWind": "ew_wind_mps",
    "nsWind": "ns_wind_mps",
    "vertWind": "vert_wind_mps",
    "headingRate": "heading_rate_deg_per_s",
    "pitchRate": "pitch_rate_deg_per_s",
}


# The CfRadial geometry correction variables RadxConvert writes, CFAC order.
CORRECTIONS = [
    "azimuth_correction",
    "elevation_correction",
    "range_correction",
    "longitude_correction",
    "latitude_correction",
    "pressure_altitude_correction",
    "altitude_correction",
    "eastward_velocity_correction",
    "northward_velocity_correction",
    "vertical_velocity_correction",
    "heading_correction",
    "roll_correction",
    "pitch_correction",
    "drift_correction",
    "rotation_correction",
    "tilt_correction",
]


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def radxprint(container, path):
    remote = f"/tmp/dorade_radx_golden/{path.name}"
    run(["docker", "exec", container, "mkdir", "-p", "/tmp/dorade_radx_golden"])
    run(["docker", "cp", str(path), f"{container}:{remote}"])
    return run([
        "docker", "exec", container, "bash", "-c",
        "export PATH=/usr/local/lrose/bin:$PATH LD_LIBRARY_PATH=/usr/local/lrose/lib; "
        f"RadxPrint -f '{remote}' -rays",
    ])


def radxconvert(container, path):
    """`platform_is_mobile` and the correction variables of the CfRadial file
    RadxConvert writes from `path`, as ncdump prints them (no corrections for
    a sweepfile without a CFAC block)."""
    remote = f"/tmp/dorade_radx_golden/{path.name}"
    outdir = f"/tmp/dorade_radx_golden/cfradial_{path.name}"
    run(["docker", "exec", container, "mkdir", "-p", "/tmp/dorade_radx_golden"])
    run(["docker", "cp", str(path), f"{container}:{remote}"])
    output = run([
        "docker", "exec", container, "bash", "-c",
        "export PATH=/usr/local/lrose/bin:$PATH LD_LIBRARY_PATH=/usr/local/lrose/lib; "
        f"rm -rf '{outdir}' && RadxConvert -f '{remote}' -outdir '{outdir}' -cfradial >/dev/null && "
        f"nc=$(find '{outdir}' -name '*.nc' | head -1) && "
        "ncdump -h \"$nc\" | grep ':platform_is_mobile' && "
        f"ncdump -v {','.join(CORRECTIONS)} \"$nc\" | sed -n '/^data:/,$p'",
    ])
    result = {"corrections": {}}
    for line in output.splitlines():
        line = line.strip().rstrip(";").strip()
        if line.startswith(":platform_is_mobile"):
            result["platform_is_mobile"] = line.split("=", 1)[1].strip().strip('"')
            continue
        name, _, text = line.partition(" = ")
        if name in CORRECTIONS:
            result["corrections"][name] = value(text)
    # Radx writes the corrections only for a file with a CFAC block: all or none.
    missing = [name for name in CORRECTIONS if name not in result["corrections"]]
    if (result["corrections"] and missing) or "platform_is_mobile" not in result:
        raise ValueError(f"{path.name}: RadxConvert output lacks {missing or 'platform_is_mobile'}")
    return result


def value(text):
    text = text.strip()
    try:
        return float(text) if any(c in text for c in ".e") else int(text)
    except ValueError:
        return text


def parse_rays(output):
    """Per-ray dictionaries: a RadxRay block and the RadxGeoref after it."""
    rays = []
    block = None
    for line in output.splitlines():
        if "=== RadxRay ===" in line:
            block = "ray"
            rays.append({})
            continue
        if "=== RadxGeoref ===" in line:
            block = "georef"
            continue
        if line.startswith("====="):
            block = None
            continue
        if block is None or not rays or ":" not in line:
            continue
        # Top-level members only: two-space indent (ray) or any (georef).
        key, _, text = line.strip().partition(":")
        if block == "ray" and line.startswith("  ") and not line.startswith("    "):
            if key in RAY_KEYS:
                rays[-1][RAY_KEYS[key]] = text.strip() if key == "timeSecs" else value(text)
        elif block == "georef" and key in GEOREF_KEYS:
            rays[-1][GEOREF_KEYS[key]] = value(text)
    return rays


def member(section, key):
    """The value of `  key: value` in a RadxPrint section."""
    for line in section.splitlines():
        if line.startswith(f"  {key}: "):
            return line[len(key) + 4:].strip()
    raise KeyError(key)


def parse_volume(output):
    """Volume-level values: the RadxVol history (it may span lines, up to the
    `comment` member that RadxVol prints after it), the platform type and
    primary axis of RadxPlatform, and the sweep mode of the (one) RadxSweep."""
    volume = output.split("=============== RadxVol ===============", 1)[1]
    platform = volume.split("--------------- RadxPlatform ---------------", 1)[1]
    sweeps = output.split("=============== RadxSweep ===============")[1:]
    if len(sweeps) != 1:
        raise ValueError(f"{len(sweeps)} RadxSweep sections")
    start = volume.index("\n  history: ") + len("\n  history: ")
    end = volume.index("\n  comment: ", start)
    return {
        "sweep_mode": member(sweeps[0], "sweepMode"),
        "platform_type": member(platform, "platformType"),
        "primary_axis": member(platform, "primaryAxis"),
        "history": volume[start:end],
    }


def manifest_entry(entry_id):
    text = (ROOT / "testdata/other/manifest.toml").read_text(encoding="utf-8")
    for entry in tomllib.loads(text)["file"]:
        if entry["id"] == entry_id:
            return entry
    raise KeyError(entry_id)


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    base = os.environ.get("LOCALAPPDATA") if os.name == "nt" else None
    base = base or os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(base) / "recast-radar-tools" / "testdata"


def full_file(entry_id, scratch):
    """A manifest download entry: the cached copy, else a fresh download; its
    SHA-256 is checked against the manifest either way."""
    entry = manifest_entry(entry_id)
    path = cache_dir() / entry_id
    if not path.is_file():
        path = Path(scratch) / entry_id
        with urllib.request.urlopen(entry["urls"][0]) as response:
            path.write_bytes(response.read())
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        raise ValueError(f"{entry_id}: sha256 {digest}, manifest {entry['sha256']}")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", default="nexbench")
    parser.add_argument("--out", default=str(ROOT / "testdata/golden/dorade/radxprint.json"))
    args = parser.parse_args()
    files = {}
    for entry, name in FIXTURES.items():
        path = ROOT / "testdata/files/other/dorade" / name
        output = radxprint(args.container, path)
        rays = parse_rays(output)
        volume = parse_volume(output)
        files[entry] = {
            "n_rays": len(rays),
            "sweep_mode": volume["sweep_mode"],
            "platform_type": volume["platform_type"],
            "primary_axis": volume["primary_axis"],
            **radxconvert(args.container, path),
            "rays": rays,
        }
        print(f"{entry}: {len(rays)} rays", file=sys.stderr)
    full_files = {}
    with tempfile.TemporaryDirectory() as scratch:
        for entry in FULL_FILES:
            path = full_file(entry, scratch)
            output = radxprint(args.container, path)
            full_files[entry] = {"n_rays": len(parse_rays(output)), **parse_volume(output),
                                 **radxconvert(args.container, path)}
            print(f"{entry}: {full_files[entry]['n_rays']} rays", file=sys.stderr)
    golden = {
        "source": "LROSE RadxPrint -rays and RadxConvert -cfradial (release 20250811), "
                  "tools/dorade_radx_golden.py",
        "note": "values as RadxPrint prints them (about six significant digits); "
                "-9999 and -32768 are missing values; history is RadxVol history "
                "(the SEDS text without trailing whitespace, empty without a SEDS); "
                "platform_is_mobile and corrections are what RadxConvert writes to CfRadial "
                "(ncdump), pressure_altitude_correction in the CFAC's kilometres",
        "files": files,
        "full_files": full_files,
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(golden, indent=1) + "\n", encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
