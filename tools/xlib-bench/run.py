"""Cross-library radar decode benchmark driver (docs/perf/cross-library.md).

Stdlib only; runs on Linux (the `nexbench` container) and on Windows.

    run.py stage --out DATA                 copy the corpus into DATA
    run.py bench --data DATA --bin BIN --out RESULTS.jsonl [options]
    run.py table RESULTS.jsonl [...]        Markdown tables of the results
    run.py compare RESULTS.jsonl [...] --a LIB --b LIB
                                            one table of LIB b against LIB a
    run.py rss-threads --data DATA --bin BIN --out RESULTS.jsonl [options]
                                            Level II peak RSS of recast and the
                                            nexrad crate per thread count
    run.py rss-table RESULTS.jsonl [...]    its table: range over repetitions
    run.py ratios RESULTS.jsonl [...]       every library over recast and over
                                            recast-f32, per case and mode

Every harness (decode_bench, py_bench.py, rsl_bench, lrose_bench,
go_nexrad_bench, nexrad_crate_bench) decodes one file `warmup + iters` times
in one process and prints one JSON line; a sample is path -> decoded arrays,
file read included. This driver runs them in rounds, rotating the order of
files and libraries every round, and adds the process's own peak RSS (GNU time
on Linux, GetProcessMemoryInfo on Windows; see run_one).

Modes:
  pinned  one CPU (taskset on Linux; SetProcessAffinityMask on Windows after
          the harness started and before it reads its go line, optionally at
          HIGH_PRIORITY_CLASS), one thread:
          RAYON_NUM_THREADS=1 / --threads 1, OMP/BLAS/numexpr at 1 thread.
  multi   no affinity, every library's default threading.
  rss     one decode (warmup 0, iters 1) per process, pinned, for peak RSS;
          recast and the nexrad crate also once with their default pools.
  pinned-mem, multi-mem
          the pinned and multi runs again (same samples, one round is enough),
          for the peak RSS column of the pinned and multi tables only: `table`
          takes that column from these rows when they exist.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

# name, format, source (testdata id, repo-relative path or
# corpus:<path under ~/radar-corpus>), libs
CASES = [
    ("KTLX20240315_000217_V06", "l2", "l2-ktlx-20240315-000217",
     ["recast", "recast-f32", "recast-paired", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
    ("KILX20260418_013553_V06", "l2", "l2-kilx-20260418-013553",
     ["recast", "recast-f32", "recast-paired", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
    ("KTLX20130520_201643_V06.gz", "l2", "l2-ktlx-20130520-201643",
     ["recast", "recast-f32", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
    ("KLIX20050829_130035.gz", "l2", "l2-klix-20050829-130035",
     ["recast", "recast-f32", "nexrad-crate", "rsl", "lrose", "pyart", "metpy", "xradar"]),
    # Whole-file bzip2: no published Level II file in the corpus uses it, so
    # the KTLX 2013 gzip volume's bytes are recompressed (`bzip2 -9`).
    ("KTLX20130520_201643_V06.bz2", "l2", "bzip2:l2-ktlx-20130520-201643",
     ["recast", "recast-f32", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
    ("TLX_N0B_20260622_0806", "l3", "testdata/files/level3/l3-tlx-n0b-20260622-080623",
     ["recast", "recast-f32", "pyart", "metpy", "lrose"]),
    ("TLX_N0Q_20130520_2016", "l3", "testdata/files/level3/l3-tlx-n0q-20130520-2016",
     ["recast", "recast-f32", "pyart", "metpy", "lrose"]),
    ("TLX_N0U_20220503_0052", "l3", "testdata/files/level3/l3-tlx-n0u-20220503-005231",
     ["recast", "recast-f32", "pyart", "metpy", "lrose"]),
    ("TLX_DPR_20260622_0806", "l3", "testdata/files/level3/l3-tlx-dpr-20260622-080623",
     ["recast", "recast-f32", "pyart", "metpy", "lrose"]),
    # Level III products without radials (fourth fix pass): a raster
    # (composite reflectivity, code 37) and a digital precipitation array
    # (code 81, packet 17) decode to volumes; the graphic (storm tracking 58,
    # mesocyclone 141) and tabular (storm structure 62) products have no
    # data array, so recast decodes them as messages (`l3-product`), as
    # MetPy's Level3File parses every product. Py-ART reads none of them.
    ("TLX_NCR_20260622_0806", "l3", "testdata/files/level3/l3-tlx-ncr-20260622-080623",
     ["recast", "recast-f32", "pyart", "metpy"]),
    ("TLX_DPA_20260629_1736", "l3", "testdata/files/level3/l3-tlx-dpa-20260629-173638",
     ["recast", "recast-f32", "pyart", "metpy"]),
    ("TLX_NST_20260622_0806", "l3-product", "testdata/files/level3/l3-tlx-nst-20260622-080623",
     ["recast", "metpy"]),
    ("TLX_NMD_20260622_0806", "l3-product", "testdata/files/level3/l3-tlx-nmd-20260622-080623",
     ["recast", "metpy"]),
    ("TLX_NSS_20220503_0052", "l3-product", "testdata/files/level3/l3-tlx-nss-20220503-005231",
     ["recast", "metpy"]),
    ("iesha.pvol.20260305T0115.h5", "odim", "testdata/files/other/odim/iesha.pvol.20260305T0115.dbzh_th_vradh.h5",
     ["recast", "recast-f32", "xradar", "wradlib", "pyart", "lrose", "h5py"]),
    ("dkrom.pvol.20260820T1130.h5", "odim", "testdata/files/other/odim/dkrom.pvol.20260820T1130.dualpol.h5",
     ["recast", "recast-f32", "xradar", "wradlib", "pyart", "lrose", "h5py"]),
    ("bejab.pvol.hdf", "odim", "testdata/files/other/odim/bejab.pvol.hdf",
     ["recast", "recast-f32", "xradar", "wradlib", "pyart", "lrose", "h5py"]),
    # An ODIM_H5 SCAN object (one sweep per file, as DWD publishes its
    # volumes): Boostedt sweep 00, TH (fourth fix pass).
    ("deboo.scan.20260924T2130.th00.hd5", "odim",
     "corpus:feeds/dwd/boo/ras07-vol5minng01_sweeph5onem_th_00-2026092421305800-boo-10132-hd5",
     ["recast", "recast-f32", "xradar", "wradlib", "pyart", "lrose", "h5py"]),
    # ODIM_H5 Cartesian MAX products (IMGW POLRAD). xradar, Py-ART and LROSE
    # read polar ODIM only.
    ("imgw.ram.KDP.max.h5", "odim-cart", "testdata/files/other/odim/imgw_polrad/2026071100150601KDP.max.h5",
     ["recast", "wradlib", "h5py"]),
    ("imgw.ram.RhoHV.max.h5", "odim-cart", "testdata/files/other/odim/imgw_polrad/2026071100150601RhoHV.max.h5",
     ["recast", "wradlib", "h5py"]),
    ("cfrad.SPOL_20080604_002217.classic.nc", "cfrad1", "classic:cfrad1-spol-20080604-002217-sur",
     ["recast", "recast-f32", "xradar", "pyart", "wradlib", "lrose", "netcdf4"]),
    ("cfrad.IRENE_CPOL_sweeps0-1.nc", "cfrad1", "testdata/files/other/cfradial/cfrad.20110827_120420.760_CPOLRVP_IRENE_WINDS_SUR.sweeps0-1_DBZ_VEL.nc",
     ["recast", "recast-f32", "xradar", "pyart", "wradlib", "lrose", "netcdf4"]),
    ("cfrad.DOW8_RHI.trim3.nc", "cfrad1", "testdata/files/other/cfradial/cfrad.20211011_223602_DOW8_RHI.trim3.nc",
     ["recast", "recast-f32", "xradar", "pyart", "wradlib", "lrose", "netcdf4"]),
    ("cfrad2.SPOL_20080604_002217.nc", "cfrad2", "cfrad2-spol-20080604-002217-sur",
     ["xradar", "netcdf4", "pyart", "lrose"]),
    ("swp.NOXP_20090501_190244_PPI", "dorade", "testdata/files/other/dorade/swp.1090501190244.NOXPRVP.0.0.5_PPI_v1",
     ["recast", "recast-f32", "rsl", "lrose"]),
    ("swp.NOXP_20090525_203211_SEC", "dorade", "testdata/files/other/dorade/swp.1090525203211.NOXPRVP.0.0.5_PPI_v1",
     ["recast", "recast-f32", "rsl", "lrose"]),
    ("swp.DOW6_20211230_RHI.head41", "dorade", "testdata/files/other/dorade/swp.1211230222139.DOW6low.648.144.0_RHI_v169.head41",
     ["recast", "recast-f32", "rsl", "lrose"]),
    # Airborne DORADE (fourth fix pass): the NOAA P-3 N42RF tail radar in
    # Hurricane Michael, scan mode AIR, 17 fields.
    ("swp.N42RF-TM_20181010_123925_AIR", "dorade", "dorade-n42rf-tm-20181010-123925-air",
     ["recast", "recast-f32", "rsl", "lrose"]),
    ("JMA_N5_20191012_0900.tar", "jma-all", "jma-n5-20191012-090000", ["recast", "recast-f32"]),
    ("JMA_N5_20191012_0900.RS47773.tar", "jma", "testdata/files/other/jma/Z__C_RJTD_20191012090000_RDR_JMAGPV_N5_grib2.RS47773.tar",
     ["recast", "recast-f32"]),
    # Real-time Level II: the chunks of KIWA volume 307 as a real-time client
    # holds them, the Start chunk and the first intermediate chunks
    # concatenated in sequence order (all 70 are l2-kiwa-20260917-003629).
    ("KIWA307_chunks001-003", "l2chunk", "chunks:l2chunk-kiwa-307-20260917-003629:1-3",
     ["recast", "recast-f32", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
    ("KIWA307_chunks001-035", "l2chunk", "chunks:l2chunk-kiwa-307-20260917-003629:1-35",
     ["recast", "recast-f32", "nexrad-crate", "go-nexrad", "rsl", "lrose", "radrs", "pyart", "metpy", "xradar"]),
]

# Inputs this branch's testdata manifest does not list, pinned by SHA-256
# (`stage` checks them): the DWD sweep (from the rolling
# opendata.dwd.de/weather/radar/sites/sweep_vol_z/boo/unfiltered/ directory,
# copied into ~/radar-corpus/feeds; the hdf5-netcdf stream commits it as
# `odim-deboo-20260924-2130-sweep-th-00`) and the N42RF sweep (the
# metadata-complete stream's testdata id, fetched from GitHub
# Alex-DesRosiers/radarqc_scans at 0dc45a2).
PINNED_SHA256 = {
    "corpus:feeds/dwd/boo/ras07-vol5minng01_sweeph5onem_th_00-2026092421305800-boo-10132-hd5":
        "491252444c433f40940cc649201d5a9bd2b6bc0a385df79a137fa62195ffaea9",
    "dorade-n42rf-tm-20181010-123925-air":
        "7766bc200ab520660071157f23233927055bb4e67653e27dfe2d837872cf5748",
}

# The byte router (`decode_bench --format auto`: sniff, unwrap, dispatch)
# against the format's own reader, one case per format it routes.
ROUTER_CASES = ["KTLX20240315_000217_V06", "KTLX20130520_201643_V06.gz",
                "KIWA307_chunks001-003", "TLX_N0B_20260622_0806", "iesha.pvol.20260305T0115.h5",
                "cfrad.IRENE_CPOL_sweeps0-1.nc", "swp.NOXP_20090501_190244_PPI",
                "JMA_N5_20191012_0900.RS47773.tar"]
CASES = [(name, fmt, source, libs + (["recast-auto"] if name in ROUTER_CASES else []))
         for name, fmt, source, libs in CASES]

# The harnesses read real-time chunk concatenations as Level II.
HARNESS_FORMAT = {"l2chunk": "l2", "l3-product": "l3"}

# Linux peak RSS: GNU time (see run_one).
GNU_TIME = "/usr/bin/time"

PY_LIBS = {"pyart", "metpy", "xradar", "radrs", "wradlib", "h5py", "netcdf4"}
RUST_THREADED = {"recast", "recast-f32", "recast-paired", "recast-auto", "nexrad-crate"}


def testdata_cache():
    # The same lookup as recast_radar_testdata::cache_dir.
    env = os.environ.get("RECAST_RADAR_TESTDATA")
    if env:
        return Path(env)
    base = os.environ.get("LOCALAPPDATA") if os.name == "nt" else None
    base = Path(base) if base else Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return base / "recast-radar-tools" / "testdata"


def stage(out, corpus=None):
    import hashlib

    corpus = Path(corpus) if corpus else Path.home() / "radar-corpus"
    out.mkdir(parents=True, exist_ok=True)
    cache = testdata_cache()
    for name, _fmt, source, _libs in CASES:
        target = out / name
        if target.exists():
            continue
        if source.startswith("chunks:"):
            # chunks:<id prefix>:<first>-<last>: the cached chunk entries
            # <prefix>-NNN-{s,i,e}, concatenated in sequence order.
            prefix, _, span = source[7:].rpartition(":")
            first, last = (int(n) for n in span.split("-"))
            with open(target, "wb") as dst:
                for number in range(first, last + 1):
                    chunk = next(p for p in (cache / f"{prefix}-{number:03d}-{kind}" for kind in "sie")
                                 if p.exists())
                    dst.write(chunk.read_bytes())
        elif source.startswith("bzip2:"):
            # A whole-file gzip volume's bytes as one bzip2 stream, level 9.
            import bz2
            import gzip

            with gzip.open(cache / source[6:], "rb") as src:
                target.write_bytes(bz2.compress(src.read(), 9))
        elif source.startswith("classic:"):
            # CfRadial 1 netCDF-4 -> classic (the model reader reads classic
            # netCDF only); same variables, same data.
            import netCDF4

            with netCDF4.Dataset(cache / source[8:]) as src, netCDF4.Dataset(
                target, "w", format="NETCDF3_64BIT_OFFSET"
            ) as dst:
                dst.setncatts({k: src.getncattr(k) for k in src.ncattrs()})
                for dim_name, dim in src.dimensions.items():
                    dst.createDimension(dim_name, None if dim.isunlimited() else len(dim))
                for var_name, var in src.variables.items():
                    dtype = var.dtype
                    if dtype == str or getattr(dtype, "kind", "") in ("O", "U"):
                        continue
                    if dtype.kind == "i" and dtype.itemsize == 8:
                        dtype = "i4"
                    fill = var.getncattr("_FillValue") if "_FillValue" in var.ncattrs() else None
                    new = dst.createVariable(var_name, dtype, var.dimensions, fill_value=fill)
                    new.setncatts({k: var.getncattr(k) for k in var.ncattrs() if k != "_FillValue"})
                    var.set_auto_maskandscale(False)
                    new.set_auto_maskandscale(False)
                    new[...] = var[...]
        elif source.startswith("testdata/"):
            shutil.copyfile(REPO / source, target)
        elif source.startswith("corpus:"):
            if not (corpus / source[7:]).exists():
                print(f"skipped {name}: no {corpus / source[7:]}")
                continue
            shutil.copyfile(corpus / source[7:], target)
        else:
            shutil.copyfile(cache / source, target)
        if source in PINNED_SHA256:
            digest = hashlib.sha256(target.read_bytes()).hexdigest()
            if digest != PINNED_SHA256[source]:
                target.unlink()
                raise SystemExit(f"{source}: SHA-256 {digest}, expected {PINNED_SHA256[source]}")
        print(f"staged {name} ({target.stat().st_size} bytes)")


def iters_for(lib, fmt):
    fmt = HARNESS_FORMAT.get(fmt, fmt)
    small = fmt in ("l3", "odim-cart")
    if lib in PY_LIBS:
        slow = lib in ("pyart", "metpy", "xradar") and fmt in ("l2", "cfrad1", "cfrad2", "jma-all")
        return (3 if slow else 5) if not small else 30
    return 10 if not small else 200


def command(lib, fmt, path, bin_dir, python, iters, warmup, threads_one):
    """The argv of one harness run and extra environment."""
    env = {}
    exe = ".exe" if os.name == "nt" else ""
    # decode_bench has its own l3-product mode; every other harness reads
    # those products as Level III.
    fmt = fmt if lib.startswith("recast") and fmt == "l3-product" else HARNESS_FORMAT.get(fmt, fmt)
    if lib.startswith("recast"):
        binary = "decode_bench_paired" if lib == "recast-paired" else "decode_bench"
        if lib == "recast-auto":
            fmt = "auto"
        argv = [str(Path(bin_dir) / (binary + exe)), "--format", fmt, str(path), "--iters", str(iters),
                "--warmup", str(warmup), "--from-path", "--wait-stdin"]
        if threads_one:
            argv += ["--threads", "1"]
        if lib == "recast-f32":
            argv.append("--physical")
        return argv, env
    if lib in PY_LIBS:
        argv = [python, str(HERE / "py_bench.py"), "--lib", lib, "--format", fmt, "--iters", str(iters),
                "--warmup", str(warmup), "--wait-stdin", str(path)]
        # A Windows venv python.exe is a launcher with the interpreter as its
        # child: the harness pins itself and reports its own peak.
        env["XLIB_SELF_PIN"] = "1"
        return argv, env
    binary = {"nexrad-crate": "nexrad_crate_bench", "go-nexrad": "go_nexrad_bench",
              "rsl": "rsl_bench", "lrose": "lrose_bench"}[lib]
    argv = [str(Path(bin_dir) / (binary + exe)), fmt, str(path), str(iters), str(warmup), "1"]
    if lib == "nexrad-crate" and threads_one:
        env["RAYON_NUM_THREADS"] = "1"
    if lib == "go-nexrad" and threads_one:
        env["GOMAXPROCS"] = "1"
    return argv, env


ONE_THREAD_ENV = {
    "OMP_NUM_THREADS": "1", "OPENBLAS_NUM_THREADS": "1", "MKL_NUM_THREADS": "1",
    "NUMEXPR_NUM_THREADS": "1", "VECLIB_MAXIMUM_THREADS": "1", "BLOSC_NTHREADS": "1",
}


def win_peak_and_pin(proc, cpu, high_priority=False):
    import ctypes
    from ctypes import wintypes

    class PMC(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]

    handle = int(proc._handle)  # noqa: SLF001 - the Popen process handle
    if cpu is not None:
        ctypes.windll.kernel32.SetProcessAffinityMask(wintypes.HANDLE(handle), ctypes.c_size_t(1 << cpu))
        if high_priority:
            # HIGH_PRIORITY_CLASS: normal-priority work on the same core
            # does not preempt the pinned harness.
            ctypes.windll.kernel32.SetPriorityClass(wintypes.HANDLE(handle), 0x80)

    def peak():
        counters = PMC()
        counters.cb = ctypes.sizeof(PMC)
        ctypes.windll.psapi.GetProcessMemoryInfo(wintypes.HANDLE(handle), ctypes.byref(counters), counters.cb)
        return counters.PeakWorkingSetSize // 1024, counters.PageFaultCount

    return peak


def run_one(argv, env_extra, cpu, timeout, high_priority=False):
    """Run one harness process; its JSON line plus the process's own peak RSS.

    Linux: the harness runs under GNU time, which forks it from its own
    process of about 1 MiB and reports the child's ru_maxrss. wait4 on a
    process this driver starts is not the child's own peak: the kernel
    records the old address space's high-water mark at exec, and a
    subprocess.Popen child (vfork, or fork) execs from this driver's address
    space, so every peak below the driver's size (11-22 MiB here) read as the
    driver's size. Under GNU time the floor is GNU time's own ~1 MiB instead.
    The harnesses also report VmHWM from /proc/self/status (`self_hwm_kb`),
    the high-water mark of the address space exec created, as a cross-check.
    """
    env = dict(os.environ)
    env.update(env_extra)
    if cpu is not None:
        env.update(ONE_THREAD_ENV)
    time_out = None
    if os.name != "nt":
        import tempfile

        handle, time_out = tempfile.mkstemp(prefix="xlib-time-")
        os.close(handle)
        argv = [GNU_TIME, "-o", time_out, "-f", "%M %R %F"] + argv
        if cpu is not None:
            argv = ["taskset", "-c", str(cpu)] + argv
    self_pin = os.name == "nt" and env_extra.get("XLIB_SELF_PIN")
    if self_pin and cpu is not None:
        env["XLIB_PIN_CPU"] = str(cpu)
        if high_priority:
            env["XLIB_HIGH_PRIORITY"] = "1"
    started = time.time()
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    peak = None
    if os.name == "nt":
        peak = win_peak_and_pin(proc, cpu, high_priority)
    proc.stdin.write(b"go\n")
    proc.stdin.flush()
    peak_kb = faults = None
    if os.name == "nt":
        out, err = proc.communicate(timeout=timeout)
        peak_kb, faults = peak()
    else:
        stdout_chunks = []
        import threading

        def pump(stream, sink):
            sink.append(stream.read())

        err_chunks = []
        threads = [threading.Thread(target=pump, args=(proc.stdout, stdout_chunks)),
                   threading.Thread(target=pump, args=(proc.stderr, err_chunks))]
        for t in threads:
            t.start()
        proc.stdin.close()
        _, status, _usage = os.wait4(proc.pid, 0)
        proc.returncode = os.waitstatus_to_exitcode(status)
        for t in threads:
            t.join()
        out, err = stdout_chunks[0], err_chunks[0]
        # GNU time writes "Command exited with non-zero status N" first when
        # the harness fails; the format line is the last.
        try:
            with open(time_out) as report:
                lines = [line.split() for line in report if line.strip()]
            maxrss, minor, major = (int(value) for value in lines[-1])
            peak_kb, faults = maxrss, minor + major
        except (OSError, ValueError, IndexError):
            peak_kb = faults = None
        finally:
            os.unlink(time_out)
    wall = time.time() - started
    line = next((l for l in out.decode(errors="replace").splitlines() if l.startswith("{")), None)
    if proc.returncode != 0 or line is None:
        return {"error": err.decode(errors="replace")[-600:], "returncode": proc.returncode}
    result = json.loads(line)
    if self_pin and result.get("self_peak_rss_kb"):
        # The measured process was the launcher; the harness's own peak is the one.
        result["launcher_peak_rss_kb"] = peak_kb
        peak_kb = result["self_peak_rss_kb"]
    method = "gnu-time" if os.name != "nt" else (
        "self-peak-working-set" if self_pin and result.get("self_peak_rss_kb") else "peak-working-set")
    result.update({"peak_rss_kb": peak_kb, "rss_method": method, "page_faults": faults,
                   "process_wall_s": round(wall, 3)})
    return result


def loadavg():
    try:
        return os.getloadavg()[0]
    except (AttributeError, OSError):
        return None


def bench(args):
    data = Path(args.data)
    cases = [c for c in CASES if (not args.formats or c[1] in args.formats)
             and (not args.files or any(f in c[0] for f in args.files))]
    modes = args.modes.split(",")
    out = open(args.out, "a")
    for round_index in range(args.first_round, args.first_round + args.rounds):
        if args.max_load and loadavg() is not None:
            while loadavg() > args.max_load:
                print(f"load {loadavg():.1f} > {args.max_load}; waiting", flush=True)
                time.sleep(30)
        ordered = cases if round_index % 2 == 0 else list(reversed(cases))
        for name, fmt, _source, libs in ordered:
            libs = [l for l in libs if not args.libs or l in args.libs]
            rotation = round_index % max(1, len(libs))
            libs = libs[rotation:] + libs[:rotation]
            path = data / name
            for mode in modes:
                for lib in libs:
                    runs = []
                    if mode in ("pinned", "pinned-mem"):
                        runs.append((iters_for(lib, fmt), 1, args.cpu, True, mode))
                    elif mode in ("multi", "multi-mem"):
                        runs.append((iters_for(lib, fmt), 1, None, False, mode))
                    elif mode == "rss":
                        if round_index > 0:
                            continue
                        runs.append((1, 0, args.cpu, True, "rss-1thread"))
                        if lib in RUST_THREADED:
                            runs.append((1, 0, None, False, "rss-default"))
                    for iters, warmup, cpu, one, label in runs:
                        argv, env = command(lib, fmt, path, args.bin, args.python, iters, warmup, one)
                        result = run_one(argv, env, cpu, args.timeout, args.high_priority)
                        result.update({"case": name, "lib": lib, "mode": label, "round": round_index,
                                       "loadavg": loadavg(), "host": sys.platform, "cpu": cpu,
                                       "time": time.strftime("%Y-%m-%dT%H:%M:%S")})
                        out.write(json.dumps(result) + "\n")
                        out.flush()
                        status = result.get("median_ms", result.get("error", "?"))
                        if isinstance(status, str):
                            status = status.strip().splitlines()[-1][:120] if status.strip() else "error"
                        print(f"r{round_index} {label:12} {name:40} {lib:14} {status} "
                              f"rss={result.get('peak_rss_kb')}", flush=True)
    out.close()


FORMAT_TITLES = {
    "l2": "Level II", "l2chunk": "Level II real-time chunks", "l3": "Level III",
    "l3-product": "Level III graphic and tabular products", "odim": "ODIM_H5",
    "odim-cart": "ODIM_H5 Cartesian", "cfrad1": "CfRadial 1",
    "cfrad2": "CfRadial 2", "dorade": "DORADE", "jma": "JMA GRIB2 (one station)",
    "jma-all": "JMA GRIB2 (20 stations)",
}


def short_error(text):
    """The last informative line of a harness's stderr."""
    lines = [l.strip() for l in (text or "").splitlines() if l.strip() and not set(l.strip()) <= set("^~ ")]
    last = lines[-1] if lines else "failed"
    return last[:110].replace("|", "/")


def valid_rss(row):
    """Peak RSS of a row in KiB, or None when it was not the process's own:
    Linux rows from before the GNU time wrapper (no `rss_method`) recorded
    the driver's high-water mark whenever the harness's peak was lower."""
    if row.get("host") == "linux" and row.get("rss_method") != "gnu-time":
        return None
    return row.get("peak_rss_kb")


def read_rows(path):
    """JSON lines of a results file, plain or gzip-compressed."""
    import gzip

    opener = gzip.open if str(path).endswith(".gz") else open
    with opener(path, "rt") as lines:
        return [json.loads(line) for line in lines if line.strip()]


def table(paths, modes=("pinned", "multi", "rss-1thread", "rss-default")):
    """Markdown tables per format and mode: median of the round medians
    (MoRM), the minimum sample, peak RSS in MiB, the ratio to recast and
    whether the library's round medians lie apart from recast's (`yes`: its
    slowest round is faster than recast's fastest, or its fastest slower
    than recast's slowest; `no`: the two ranges overlap, so the ratio does
    not rank the two). The pinned and multi tables take peak RSS from the
    `pinned-mem` and `multi-mem` rows of the same case and library when
    there are any."""
    from collections import defaultdict
    from statistics import median

    rows = [row for p in paths for row in read_rows(p)]
    case_format = {name: fmt for name, fmt, _source, _libs in CASES}
    # One result per (case, mode, library, round): a round redone with
    # --first-round (in a later file) replaces the earlier attempt, and a
    # successful run wins over a failed one.
    latest = {}
    for r in rows:
        key = (r["case"], r["mode"], r["lib"], r.get("round"))
        if r.get("median_ms") is not None or key not in latest or latest[key].get("median_ms") is None:
            latest[key] = r
    groups = defaultdict(list)
    cases = []
    for r in latest.values():
        groups[(r["case"], r["mode"], r["lib"])].append(r)
        if r["case"] not in cases:
            cases.append(r["case"])
    for fmt in FORMAT_TITLES:
        fmt_cases = [c for c in cases if case_format.get(c) == fmt]
        if not fmt_cases:
            continue
        for mode in modes:
            lines = []
            for case in fmt_cases:
                entries = [(lib, g) for (c, m, lib), g in groups.items() if c == case and m == mode]
                if not entries:
                    continue
                stats, base, base_meds = [], None, None
                for lib, g in entries:
                    ok = [r for r in g if "median_ms" in r and r.get("median_ms") is not None]
                    if not ok:
                        # The most informative failure: one with a message.
                        texts = [r.get("error") for r in g if (r.get("error") or "").strip()]
                        reason = short_error(texts[-1]) if texts else f"exit code {g[-1].get('returncode')}"
                        stats.append((lib, None, None, None, reason))
                        continue
                    meds = [r["median_ms"] for r in ok]
                    morm = median(meds)
                    low = min(r["min_ms"] for r in ok)
                    mem = [r for r in groups.get((case, f"{mode}-mem", lib), []) if valid_rss(r)]
                    peaks = [valid_rss(r) for r in (mem or ok)]
                    rss = max(peaks) / 1024 if all(peaks) else None
                    # What the library decoded: sweeps and gates (rays for
                    # the nexrad crate harness, which reports no gates).
                    last = ok[-1]
                    content = f"{last.get('sweeps')} sw, " + (
                        f"{last.get('gates'):,} gates" if last.get("gates")
                        else f"{last.get('rays') or 0:,} rays")
                    stats.append((lib, meds, morm, low, (rss, content)))
                    if lib == "recast":
                        base, base_meds = morm, meds
                stats.sort(key=lambda s: (s[2] is None, s[2] or 0))
                for lib, meds, morm, low, extra in stats:
                    if meds is None:
                        lines.append(f"| {case} | {lib} | fails: {extra} | | | | | | |")
                        continue
                    rss, content = extra
                    ratio = f"{morm / base:.2f}" if base else ""
                    apart = ""
                    if base_meds and lib != "recast" and len(meds) > 1 and len(base_meds) > 1:
                        separate = max(meds) < min(base_meds) or min(meds) > max(base_meds)
                        apart = "yes" if separate else "no"
                    rounds = " / ".join(f"{m:.1f}" for m in meds)
                    rss_text = f"{rss:.1f}" if rss is not None else "n/m"
                    lines.append(f"| {case} | {lib} | {rounds} | {morm:.1f} | {low:.1f} | {rss_text} | {ratio} "
                                 f"| {apart} | {content} |")
            if not lines:
                continue
            print(f"\n#### {FORMAT_TITLES[fmt]}, {mode}\n")
            print("| file | library | round medians ms | MoRM ms | min ms | peak RSS MiB | / recast | apart | decoded |")
            print("|---|---|---|---:|---:|---:|---:|---|---|")
            print("\n".join(lines))


def compare(paths, lib_a, lib_b, modes=("pinned", "rss-1thread")):
    """One Markdown table of `lib_b` against `lib_a` on every case both ran:
    MoRM of each, their ratio, and each one's peak RSS (as `table` takes
    it). A failed library shows its last error line."""
    from collections import defaultdict
    from statistics import median

    rows = [row for p in paths for row in read_rows(p)]
    latest = {}
    for r in rows:
        key = (r["case"], r["mode"], r["lib"], r.get("round"))
        if r.get("median_ms") is not None or key not in latest or latest[key].get("median_ms") is None:
            latest[key] = r
    groups = defaultdict(list)
    for r in latest.values():
        groups[(r["case"], r["mode"], r["lib"])].append(r)
    order = [name for name, _fmt, _source, _libs in CASES]

    def summary(case, mode, lib):
        runs = groups.get((case, mode, lib), [])
        ok = [r for r in runs if r.get("median_ms") is not None]
        if not ok:
            texts = [r.get("error") for r in runs if (r.get("error") or "").strip()]
            return None, short_error(texts[-1]) if texts else "no run"
        peaks = [valid_rss(r) for r in ok]
        peak = f"{max(peaks) / 1024:.1f}" if all(peaks) else "n/m"
        return median(r["median_ms"] for r in ok), peak

    print(f"| file | mode | {lib_a} MoRM ms | {lib_b} MoRM ms | {lib_b} / {lib_a} | "
          f"{lib_a} peak RSS MiB | {lib_b} peak RSS MiB |")
    print("|---|---|---:|---:|---:|---:|---:|")
    cases = sorted({c for c, _m, _l in groups}, key=lambda c: order.index(c) if c in order else len(order))
    for case in cases:
        for mode in modes:
            if (case, mode, lib_a) not in groups and (case, mode, lib_b) not in groups:
                continue
            a, a_peak = summary(case, mode, lib_a)
            b, b_peak = summary(case, mode, lib_b)
            if a is None or b is None:
                reason = a_peak if a is None else b_peak
                who = lib_a if a is None else lib_b
                print(f"| {case} | {mode} | {'' if a is None else f'{a:.1f}'} | "
                      f"{'' if b is None else f'{b:.1f}'} | {who} fails: {reason} | | |")
                continue
            print(f"| {case} | {mode} | {a:.1f} | {b:.1f} | {b / a:.2f} | {a_peak} | {b_peak} |")


def ratios(paths, bases=("recast", "recast-f32"), modes=("pinned", "multi")):
    """For every case and mode, each library's MoRM over the MoRM of each
    `bases` row, and whether their round medians lie apart (as `table`
    decides it), for the Summary of docs/perf/cross-library.md: which recast
    row a ratio divides by matters, because `recast` returns packed fields
    and `recast-f32` float32 physical values, the work of the readers that
    return float arrays (Py-ART, xradar, MetPy, wradlib, radrs)."""
    from collections import defaultdict
    from statistics import median

    rows = [row for p in paths for row in read_rows(p)]
    case_format = {name: fmt for name, fmt, _source, _libs in CASES}
    latest = {}
    for r in rows:
        key = (r["case"], r["mode"], r["lib"], r.get("round"))
        if r.get("median_ms") is not None or key not in latest or latest[key].get("median_ms") is None:
            latest[key] = r
    meds = defaultdict(list)
    for r in latest.values():
        if r.get("median_ms") is not None:
            meds[(r["case"], r["mode"], r["lib"])].append(r["median_ms"])
    order = [name for name, _fmt, _source, _libs in CASES]
    cases = sorted({c for c, _m, _l in meds}, key=lambda c: order.index(c) if c in order else len(order))
    head = " | ".join(f"/ {b} | apart" for b in bases)
    print(f"| format | file | mode | library | {head} |")
    print("|---|---|---|---|" + "---:|---|" * len(bases))
    for case in cases:
        for mode in modes:
            libs = sorted({l for c, m, l in meds if c == case and m == mode})
            for lib in libs:
                if lib in bases:
                    continue
                values = meds[(case, mode, lib)]
                cells = []
                for base in bases:
                    base_values = meds.get((case, mode, base))
                    if not base_values:
                        cells.append(" | ")
                        continue
                    ratio = median(values) / median(base_values)
                    apart = ""
                    if len(values) > 1 and len(base_values) > 1:
                        separate = max(values) < min(base_values) or min(values) > max(base_values)
                        apart = "yes" if separate else "no"
                    cells.append(f"{ratio:.2f} | {apart}")
                print(f"| {case_format.get(case, '')} | {case} | {mode} | {lib} | " + " | ".join(cells) + " |")


def rss_threads(args):
    """Peak RSS of one Level II decode per process, recast (`--threads N`)
    against the nexrad crate (`RAYON_NUM_THREADS=N`), for every thread count
    and file, `--reps` times. Repetition r runs the files in order (reversed
    on odd r), and for each file the thread counts and the two libraries in
    an order that alternates, so drift in host load reaches both alike.
    Thread count 0 is each library's default pool (rayon: one thread per
    logical CPU, or what the process may use: Rust's available_parallelism
    counts a cgroup CPU quota, 12 in nexbench). No affinity: a pool of N
    threads needs N CPUs. With --bin-b, a second recast build (`recast-b`,
    decode_bench from that directory) joins the rotation, for before/after
    comparisons; --libs picks the libraries."""
    data = Path(args.data)
    threads = [int(n) for n in args.threads.split(",")]
    out = open(args.out, "a")
    for rep_index in range(args.reps):
        files = args.files if rep_index % 2 == 0 else list(reversed(args.files))
        for name in files:
            for n in (threads if rep_index % 2 == 0 else list(reversed(threads))):
                rotation = (rep_index + n) % len(args.libs)
                for lib in args.libs[rotation:] + args.libs[:rotation]:
                    bin_dir = args.bin_b if lib == "recast-b" else args.bin
                    argv, env = command("recast" if lib == "recast-b" else lib, "l2", data / name, bin_dir,
                                        args.python, 1, 0, False)
                    if n and lib.startswith("recast"):
                        argv += ["--threads", str(n)]
                    elif n:
                        env["RAYON_NUM_THREADS"] = str(n)
                    result = run_one(argv, env, None, args.timeout)
                    result.update({"case": name, "lib": lib, "mode": "rss-threads", "threads_asked": n,
                                   "rep": rep_index, "loadavg": loadavg(), "host": sys.platform,
                                   "time": time.strftime("%Y-%m-%dT%H:%M:%S")})
                    out.write(json.dumps(result) + "\n")
                    out.flush()
                    print(f"rep{rep_index} {name:32} threads={n:<3} {lib:13} rss={result.get('peak_rss_kb')}",
                          flush=True)
    out.close()


def rss_table(paths):
    """Markdown table of `rss-threads` rows: per file and thread count, each
    library's peak RSS as min-max over the repetitions (MiB) and the
    repetition count."""
    from collections import defaultdict

    rows = [row for p in paths for row in read_rows(p) if row.get("mode") == "rss-threads"]
    libs = list(dict.fromkeys(r["lib"] for r in rows))
    peaks = defaultdict(list)
    for r in rows:
        if valid_rss(r):
            peaks[(r["case"], r["threads_asked"], r["lib"])].append(valid_rss(r) / 1024)
    cases = list(dict.fromkeys(r["case"] for r in rows))
    counts = sorted({r["threads_asked"] for r in rows}, key=lambda n: (n == 0, n))

    def span(values):
        if not values:
            return "fails"
        low, high = min(values), max(values)
        return f"{low:.1f}" if high - low < 0.05 else f"{low:.1f}-{high:.1f}"

    print(f"| file | threads | {' | '.join(f'{lib} MiB' for lib in libs)} | runs |")
    print(f"|---|---:|{'---:|' * len(libs)}---:|")
    for case in cases:
        for n in counts:
            values = [peaks.get((case, n, lib), []) for lib in libs]
            if not any(values):
                continue
            label = "default" if n == 0 else str(n)
            print(f"| {case} | {label} | {' | '.join(span(v) for v in values)} | "
                  f"{min(len(v) for v in values)} |")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p_stage = sub.add_parser("stage")
    p_stage.add_argument("--out", required=True, type=Path)
    p_stage.add_argument("--corpus", default=str(Path.home() / "radar-corpus"),
                         help="root of the corpus: sources")
    p_bench = sub.add_parser("bench")
    p_bench.add_argument("--data", required=True)
    p_bench.add_argument("--bin", required=True)
    p_bench.add_argument("--python", default=sys.executable)
    p_bench.add_argument("--out", required=True)
    p_bench.add_argument("--rounds", type=int, default=3)
    p_bench.add_argument("--first-round", type=int, default=0,
                         help="index of the first round (its file and library order), to redo a lost round")
    p_bench.add_argument("--modes", default="pinned,multi,rss",
                         help="pinned, multi, rss, pinned-mem, multi-mem (comma-separated)")
    p_bench.add_argument("--cpu", type=int, default=26)
    p_bench.add_argument("--formats", nargs="*")
    p_bench.add_argument("--files", nargs="*")
    p_bench.add_argument("--libs", nargs="*")
    p_bench.add_argument("--timeout", type=int, default=1800)
    p_bench.add_argument("--max-load", type=float, default=0)
    p_bench.add_argument("--high-priority", action="store_true",
                         help="Windows: pinned harnesses run at HIGH_PRIORITY_CLASS")
    p_table = sub.add_parser("table")
    p_table.add_argument("results", nargs="+")
    p_table.add_argument("--modes", default="pinned,multi,rss-1thread,rss-default")
    p_compare = sub.add_parser("compare")
    p_compare.add_argument("results", nargs="+")
    p_compare.add_argument("--a", required=True)
    p_compare.add_argument("--b", required=True)
    p_compare.add_argument("--modes", default="pinned,rss-1thread")
    p_rss = sub.add_parser("rss-threads")
    p_rss.add_argument("--data", required=True)
    p_rss.add_argument("--bin", required=True)
    p_rss.add_argument("--python", default=sys.executable)
    p_rss.add_argument("--out", required=True)
    p_rss.add_argument("--reps", type=int, default=5)
    p_rss.add_argument("--threads", default="1,2,4,8,16,32,0")
    p_rss.add_argument("--files", nargs="+", default=["KTLX20240315_000217_V06", "KILX20260418_013553_V06"])
    p_rss.add_argument("--timeout", type=int, default=600)
    p_rss.add_argument("--libs", nargs="+", default=["recast", "nexrad-crate"],
                       help="recast, nexrad-crate, recast-b")
    p_rss.add_argument("--bin-b", help="directory of the recast-b decode_bench")
    p_rss_table = sub.add_parser("rss-table")
    p_rss_table.add_argument("results", nargs="+")
    p_ratios = sub.add_parser("ratios")
    p_ratios.add_argument("results", nargs="+")
    p_ratios.add_argument("--bases", default="recast,recast-f32")
    p_ratios.add_argument("--modes", default="pinned,multi")
    args = parser.parse_args()
    if args.cmd == "stage":
        stage(args.out, args.corpus)
    elif args.cmd == "bench":
        bench(args)
    elif args.cmd == "compare":
        compare(args.results, args.a, args.b, args.modes.split(","))
    elif args.cmd == "rss-threads":
        rss_threads(args)
    elif args.cmd == "rss-table":
        rss_table(args.results)
    elif args.cmd == "ratios":
        ratios(args.results, args.bases.split(","), args.modes.split(","))
    else:
        table(args.results, args.modes.split(","))


if __name__ == "__main__":
    main()
