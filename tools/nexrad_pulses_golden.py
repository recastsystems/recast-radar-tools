"""Level II calibration and pulse golden from independent readers.

Writes testdata/golden/level2/pulses_calibration.json for the committed
Message 31 fixtures, which crates/recast-radar-io-nexrad/tests/
pulses_calibration_real.rs compares with the model and the FM301 view:

- MetPy (Level2File): for every sweep, the elevation number, ray count and,
  run-length encoded over its rays in file order, the VOL block calibration
  constant (dBZ0), system ZDR and initial system PhiDP, and the RAD block
  unambiguous range; the message 5 cut of each elevation number (waveform
  code, surveillance PRF number and pulse count, the three Doppler sectors);
  the VCP pulse width; and the message 18 transmitter pulse widths TAU_SP and
  TAU_LP (ns).
- This script's own frame walk of the metadata record (MetPy does not decode
  message 32): the RDA PRF Data PRFs of each waveform, in mHz (ICD 2620002AA
  Table XVIII).
- LROSE RadxPrint (release 20250811, in the `nexbench` container): the
  calibration it reads (baseDbz1kmHc, zdrCorrectionDb, systemPhidpDeg) and,
  per sweep it prints, the distinct nSamples, pulseWidthUsec and prtSec of
  its rays.

    python tools/nexrad_pulses_golden.py [--container nexbench] [--no-radx]

Nothing is read from this workspace's decoders.
"""

import argparse
import bz2
import json
import struct
import logging
import subprocess
import sys
import warnings
from pathlib import Path

warnings.filterwarnings("ignore")
logging.disable(logging.CRITICAL)

from metpy.io import Level2File  # noqa: E402
import metpy  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = {
    "l2-kdmx-20080525-205148-trim": "KDMX20080525_205148.trim.V06",
    "l2-ktlx-20130520-201643-trim": "KTLX20130520_201643.trim.V06",
    "l2-koax-20140616-205305-trim": "KOAX20140616_205305.trim.V06",
    "l2-kewx-20160413-022531-trim": "KEWX20160413_022531.trim.V06",
    "l2-kdvn-20200810-180401-trim": "KDVN20200810_180401.trim.V06",
    "l2-klix-20210829-180425-trim": "KLIX20210829_180425.trim.V06",
    "l2-kbox-20220129-150537-trim": "KBOX20220129_150537.trim.V06",
    "l2-tstl-20230331-230314-trim": "TSTL20230331_230314.trim.V06",
    "l2-pgua-20230524-030945-trim": "PGUA20230524_030945.trim.V06",
    "l2-kmtx-20240301-212827-trim": "KMTX20240301_212827.trim.V06",
    "l2-ktlx-20240315-000217-trim": "KTLX20240315_000217.trim.V06",
    "l2-kilx-20260418-013553-trim": "KILX20260418_013553.trim.V06",
}
WAVEFORMS = {
    "Contiguous Surveillance": 1,
    "Contig. Doppler with Ambiguity Res.": 2,
    "Contig. Doppler without Ambiguity Res.": 3,
    "Batch": 4,
    "Staggered Pulse Pair": 5,
}


def rle(values):
    """[[value, count], ...] over consecutive equal values."""
    out = []
    for value in values:
        if out and out[-1][0] == value:
            out[-1][1] += 1
        else:
            out.append([value, 1])
    return out


def f32(value):
    return float(value)


def metpy_case(path):
    f = Level2File(str(path))
    sweeps = []
    for sweep in f.sweeps:
        if not sweep:
            continue
        el_num = int(sweep[0][0].el_num)
        vol = []
        unamb = []
        for ray in sweep:
            v = ray[1]
            vol.append(None if v is None else [f32(v.calib_dbz), f32(v.sys_zdr), f32(v.phidp0)])
            r = ray[3]
            unamb.append(None if r is None else f32(r.unamb_range))
        sweeps.append({
            "el_num": el_num,
            "rays": len(sweep),
            "vol_calibration_rle": rle(vol),
            "unamb_range_km_rle": rle(unamb),
        })
    vcp = getattr(f, "vcp_info", None)
    cuts = []
    if vcp is not None:
        for el in vcp.els:
            cuts.append({
                "waveform": WAVEFORMS.get(str(el.waveform), 0),
                "surv_prf_num": int(el.surv_prf_num),
                "surv_pulse_count": int(el.surv_pulse_count),
                "sectors": [
                    [f32(el.sector1_edge), int(el.sector1_doppler_prf_num), int(el.sector1_pulse_count)],
                    [f32(el.sector2_edge), int(el.sector2_doppler_prf_num), int(el.sector2_pulse_count)],
                    [f32(el.sector3_edge), int(el.sector3_doppler_prf_num), int(el.sector3_pulse_count)],
                ],
            })
    rda = getattr(f, "rda", None) or {}
    return {
        "vcp_pulse_width": None if vcp is None else str(vcp.pulse_width),
        "tau_sp_ns": rda.get("TAU_SP"),
        "tau_lp_ns": rda.get("TAU_LP"),
        "cuts": cuts,
        "sweeps": sweeps,
    }


def message_32(path):
    """PRFs (mHz) per waveform code of the first message 32 in the metadata
    record, or None: the first LDM record's 2432-byte frames."""
    data = path.read_bytes()
    pos = 24
    length = abs(struct.unpack(">i", data[pos:pos + 4])[0])
    record = data[pos + 4:pos + 4 + length]
    if record[:3] == b"BZh":
        record = bz2.decompress(record)
    else:
        record = data[24:24 + 134 * 2432]
    for frame in range(len(record) // 2432):
        header = record[frame * 2432 + 12:frame * 2432 + 28]
        if header[3] != 32:
            continue
        body = record[frame * 2432 + 28:(frame + 1) * 2432]
        waveforms = struct.unpack(">H", body[0:2])[0]
        at = 4
        out = {}
        for _ in range(waveforms):
            waveform, count = struct.unpack(">HH", body[at:at + 4])
            at += 4
            out[str(waveform)] = list(struct.unpack(f">{count}I", body[at:at + 4 * count]))
            at += 4 * count
        return out
    return None


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def radxprint_case(container, path):
    remote = f"/tmp/nexrad_pulses_golden/{path.name}"
    run(["docker", "exec", container, "mkdir", "-p", "/tmp/nexrad_pulses_golden"])
    run(["docker", "cp", str(path), f"{container}:{remote}"])
    output = run([
        "docker", "exec", container, "bash", "-c",
        "export PATH=/usr/local/lrose/bin:$PATH LD_LIBRARY_PATH=/usr/local/lrose/lib; "
        f"RadxPrint -f '{remote}' -rays",
    ])
    calibration = {}
    sweeps = {}
    in_ray = False
    ray = {}

    def finish_ray():
        if ray:
            entry = sweeps.setdefault(str(ray.get("sweepNum")), {
                "n_samples": set(), "pulse_width_us": set(), "prt_s": set(), "rays": 0,
            })
            entry["rays"] += 1
            entry["n_samples"].add(ray.get("nSamples"))
            entry["pulse_width_us"].add(ray.get("pulseWidthUsec"))
            entry["prt_s"].add(ray.get("prtSec"))

    for line in output.splitlines():
        stripped = line.strip()
        for key in ("baseDbz1kmHc", "zdrCorrectionDb", "systemPhidpDeg"):
            if stripped.startswith(f"<{key}>"):
                calibration[key] = float(stripped[len(key) + 2:stripped.index("</")])
        if "=== RadxRay ===" in line:
            finish_ray()
            ray = {}
            in_ray = True
            continue
        if line.startswith("=====") or line.startswith("=========="):
            in_ray = False
            continue
        if in_ray and line.startswith("  ") and not line.startswith("    ") and ":" in line:
            key, _, text = stripped.partition(":")
            if key in ("sweepNum", "nSamples", "pulseWidthUsec", "prtSec"):
                ray[key] = float(text) if key != "sweepNum" else int(text)
    finish_ray()
    return {
        "calibration": calibration,
        "sweeps": {
            key: {
                "rays": value["rays"],
                "n_samples": sorted(value["n_samples"]),
                "pulse_width_us": sorted(value["pulse_width_us"]),
                "prt_s": sorted(value["prt_s"]),
            }
            for key, value in sorted(sweeps.items(), key=lambda item: int(item[0]))
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", default="nexbench")
    parser.add_argument("--no-radx", action="store_true")
    parser.add_argument("--out", default=str(ROOT / "testdata/golden/level2/pulses_calibration.json"))
    args = parser.parse_args()
    cases = {}
    for entry, name in FIXTURES.items():
        path = ROOT / "testdata/files/level2" / name
        case = {"metpy": metpy_case(path), "message_32_prf_mhz": message_32(path)}
        if not args.no_radx:
            case["radxprint"] = radxprint_case(args.container, path)
        cases[entry] = case
        print(f"{entry}: {len(case['metpy']['sweeps'])} sweeps", file=sys.stderr)
    golden = {
        "source": f"tools/nexrad_pulses_golden.py; MetPy {metpy.__version__} Level2File and "
                  "LROSE RadxPrint -rays (release 20250811)",
        "cases": cases,
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(golden, indent=1) + "\n", encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
