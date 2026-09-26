"""Cross-library decode benchmark: the Python libraries.

One process decodes one file with one library `--warmup` + `--iters` times and
prints one JSON line (the protocol every harness in tools/xlib-bench shares;
see docs/perf/cross-library.md). Each timed sample is path -> fully decoded
arrays in memory, file read included:

- Py-ART: `read_nexrad_archive`, `read_nexrad_level3`, `aux_io.read_odim_h5`,
  `read_cfradial` (all eager by default: every field is a masked float array).
- MetPy: `Level2File`, `Level3File` (eager: every radial's moments scaled).
- xradar: `open_*_datatree(path).load()` (the backends are lazy; `load()`
  forces every variable of every node).
- wradlib: `read_opera_hdf5` (ODIM), `read_generic_netcdf` (CfRadial).
- radrs: `radrs.xradar.open_datatree(path).load()`.
- h5py / netCDF4: every dataset / variable read in full (container floor).

usage: py_bench.py --lib LIB --format FORMAT FILE [--iters N] [--warmup N]
                   [--wait-stdin]
"""

import argparse
import gc
import json
import os
import sys
import time


def status_kb(key):
    """A /proc/self/status value in KiB ("VmRSS:", "VmHWM:"), or None."""
    try:
        with open("/proc/self/status") as status:
            for line in status:
                if line.startswith(key):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def rss_kb():
    """Current resident set of this process in KiB (0 if unknown)."""
    rss = status_kb("VmRSS:")
    if rss is not None:
        return rss
    try:
        import psutil

        return psutil.Process().memory_info().rss // 1024
    except ImportError:
        return 0


def pin_self():
    """Apply the driver's pinning to this process (XLIB_PIN_CPU,
    XLIB_HIGH_PRIORITY). On Windows a virtual environment's python.exe is a
    launcher that starts the interpreter as a child process, so the driver
    cannot pin the interpreter from outside."""
    cpu = os.environ.get("XLIB_PIN_CPU")
    if not cpu:
        return
    import psutil

    process = psutil.Process()
    process.cpu_affinity([int(cpu)])
    if os.environ.get("XLIB_HIGH_PRIORITY") and hasattr(psutil, "HIGH_PRIORITY_CLASS"):
        process.nice(psutil.HIGH_PRIORITY_CLASS)


def self_peak_kb():
    """This process's peak resident set in KiB (Windows peak working set)."""
    try:
        import psutil

        info = psutil.Process().memory_info()
        peak = getattr(info, "peak_wset", None)
        if peak is not None:
            return peak // 1024
    except ImportError:
        pass
    try:
        import resource

        return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    except ImportError:
        return 0


def count_datatree(tree):
    """(sweeps, variables, elements) of a loaded DataTree."""
    sweeps = variables = elements = 0
    for node in tree.subtree:
        if node.name and node.name.startswith("sweep_"):
            sweeps += 1
        for var in node.ds.data_vars.values():
            variables += 1
            elements += int(var.size)
    return sweeps, variables, elements


def pyart_radar_counts(radar):
    elements = sum(int(field["data"].size) for field in radar.fields.values())
    return radar.nsweeps, len(radar.fields), elements


def make_decoder(lib, fmt):
    """A function path -> (sweeps, variables, elements)."""
    if lib == "pyart":
        import pyart

        readers = {
            "l2": pyart.io.read_nexrad_archive,
            "l3": pyart.io.read_nexrad_level3,
            "odim": pyart.aux_io.read_odim_h5,
            "cfrad1": pyart.io.read_cfradial,
            # Py-ART has no CfRadial 2 reader of its own (read_cfradial
            # expects the CfRadial 1 layout); its route is the xradar wrapper
            # pyart.xradar.Xradar, i.e. xradar's decode.
            "cfrad2": pyart.io.read_cfradial,
        }
        reader = readers[fmt]
        return lambda path: pyart_radar_counts(reader(path))
    if lib == "metpy":
        from metpy.io import Level2File, Level3File

        if fmt == "l2":

            def level2(path):
                f = Level2File(path)
                elements = 0
                for sweep in f.sweeps:
                    for ray in sweep:
                        for _, (_, data) in ray[4].items():
                            elements += int(data.size)
                return len(f.sweeps), 0, elements

            return level2
        if fmt == "l3":

            def level3(path):
                f = Level3File(path)
                elements = 0
                # A tabular-only product (storm structure, radar coded
                # message) has no symbology block, so no `sym_block`.
                sym_block = getattr(f, "sym_block", None)
                for packet in sym_block[0] if sym_block else []:
                    data = packet.get("data")
                    if data is not None:
                        elements += sum(len(row) for row in data)
                return 1, 1, elements

            return level3
    if lib == "xradar":
        import xradar

        openers = {
            "l2": xradar.io.open_nexradlevel2_datatree,
            "odim": xradar.io.open_odim_datatree,
            "cfrad1": xradar.io.open_cfradial1_datatree,
            "cfrad2": xradar.io.open_cfradial2_datatree,
        }
        opener = openers[fmt]
        return lambda path: count_datatree(opener(path).load())
    if lib == "radrs" and fmt == "l2":
        import radrs.xradar

        return lambda path: count_datatree(radrs.xradar.open_datatree(path).load())
    if lib == "wradlib":
        import wradlib

        if fmt in ("odim", "odim-cart"):

            def opera(path):
                data = wradlib.io.read_opera_hdf5(path)
                elements = sum(
                    int(v.size) for v in data.values() if hasattr(v, "size") and v.ndim == 2
                )
                return 0, len(data), elements

            return opera
        if fmt == "cfrad1":

            def generic_netcdf(path):
                data = wradlib.io.read_generic_netcdf(path)
                variables = data.get("variables", {})
                elements = sum(
                    int(v["data"].size)
                    for v in variables.values()
                    if hasattr(v.get("data"), "size")
                )
                return 0, len(variables), elements

            return generic_netcdf
    if lib == "h5py" and fmt in ("odim", "odim-cart"):
        import h5py

        def read_all(path):
            counts = [0, 0]

            with h5py.File(path, "r") as f:

                def visit(_, obj):
                    if isinstance(obj, h5py.Dataset):
                        counts[0] += 1
                        counts[1] += int(obj[()].size)

                f.visititems(visit)
            return 0, counts[0], counts[1]

        return read_all
    if lib == "netcdf4" and fmt in ("cfrad1", "cfrad2"):
        import netCDF4

        def read_all(path):
            variables = elements = 0
            with netCDF4.Dataset(path) as ds:
                ds.set_auto_mask(False)
                groups = [ds]
                while groups:
                    group = groups.pop()
                    groups.extend(group.groups.values())
                    for var in group.variables.values():
                        variables += 1
                        # String variables read as `str`, not arrays.
                        elements += int(getattr(var[...], "size", 1))
            return 0, variables, elements

        return read_all
    raise SystemExit(f"py_bench: no {lib} reader for format {fmt}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lib", required=True)
    parser.add_argument("--format", required=True)
    parser.add_argument("--iters", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--wait-stdin", action="store_true")
    parser.add_argument("file")
    args = parser.parse_args()

    import warnings

    warnings.filterwarnings("ignore")
    decode = make_decoder(args.lib, args.format)
    if args.wait_stdin:
        sys.stdin.readline()
    pin_self()
    gc.collect()
    rss_before = rss_kb()
    samples = []
    counts = (0, 0, 0)
    for iteration in range(args.warmup + args.iters):
        started = time.perf_counter()
        counts = decode(args.file)
        elapsed = (time.perf_counter() - started) * 1000.0
        if iteration >= args.warmup:
            samples.append(elapsed)
        gc.collect()
    ordered = sorted(samples)
    print(
        json.dumps(
            {
                "lib": args.lib,
                "format": args.format,
                "iters": args.iters,
                "median_ms": round(ordered[len(ordered) // 2], 3) if ordered else None,
                "min_ms": round(ordered[0], 3) if ordered else None,
                "samples_ms": [round(s, 3) for s in samples],
                "sweeps": counts[0],
                "fields": counts[1],
                "gates": counts[2],
                "rss_before_kb": rss_before,
                "self_peak_rss_kb": self_peak_kb(),
                # Linux: the peak resident set since exec, the cross-check of
                # the driver's GNU time figure.
                "self_hwm_kb": status_kb("VmHWM:"),
                "python": sys.version.split()[0],
                "platform": sys.platform,
                "pid": os.getpid(),
            }
        )
    )


if __name__ == "__main__":
    main()
