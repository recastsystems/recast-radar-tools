"""Interleaved A/B wall-clock comparison of two decode_bench builds.

    ab.py --a BIN_A --b BIN_B --format l2 [--threads 1] [--rounds 5]
          [--iters 5] [--cpu 20] [--high-priority] [--args "--physical"]
          [--bench-stage raster_ref] FILE...

Every round runs, for every file, one process of A and one of B, the order
alternating between rounds (A B, then B A), so drift in machine load
affects both builds alike. With --threads 1 each process is pinned to
--cpu (taskset on Linux; SetProcessAffinityMask on Windows, after the
process started and before it reads its go line); --high-priority also
raises a Windows process to HIGH_PRIORITY_CLASS so normal-priority work
on the same core does not preempt it. Each process runs one warmup and
--iters timed decodes and reports their median. The summary gives, per
file, both builds' medians of the round medians and the median over
rounds of the per-round ratio B/A with its range. With --bench-stage the two
binaries are recast-radar-bench builds (decode plus rasters) and the stage's
mean is compared; the pixel checksum is the output check. Stdlib only.

    ab.py --summarize LOG.jsonl [...]

prints the summary again from the --out log(s), with two more columns: the
ratio of the two builds' fastest samples over all rounds (the least
load-sensitive wall-clock figure: preemption only ever adds time) and the
range of the host's 1-minute load average over the rows (Linux).
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
from pathlib import Path


def run(binary, fmt, path, iters, threads, cpu, high_priority, extra, bench_stage):
    if bench_stage:
        # recast-radar-bench: decode + rasters, one warmup iteration built in;
        # affinity is set right after the start, inside that warmup.
        argv = [binary, str(path), "--iters", str(iters), "--json"]
    else:
        argv = [binary, "--format", fmt, str(path), "--iters", str(iters), "--warmup", "1",
                "--wait-stdin"] + extra
        if threads:
            argv += ["--threads", str(threads)]
    if os.name != "nt" and cpu is not None:
        argv = ["taskset", "-c", str(cpu)] + argv
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE)
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        handle = wintypes.HANDLE(int(proc._handle))  # noqa: SLF001
        if cpu is not None:
            ctypes.windll.kernel32.SetProcessAffinityMask(handle, ctypes.c_size_t(1 << cpu))
        if high_priority:
            ctypes.windll.kernel32.SetPriorityClass(handle, 0x80)
    out, err = proc.communicate(b"" if bench_stage else b"go\n")
    if proc.returncode != 0:
        raise SystemExit(f"{binary} {path}: {err.decode(errors='replace')}")
    line = next(l for l in out.decode().splitlines() if l.startswith("{"))
    result = json.loads(line)
    if bench_stage:
        stage = result["total"] if bench_stage == "total" else result["stages"][bench_stage]
        result["median_ms"] = stage["mean_ms"]
        result["hash"] = result["checksum"]
    return result


def summarize(paths):
    """The summary of logged rounds, plus the fastest-sample ratio and the
    load range."""
    rows = []
    for path in paths:
        opener = __import__("gzip").open if str(path).endswith(".gz") else open
        with opener(path, "rt") as lines:
            rows += [json.loads(line) for line in lines if line.strip()]
    files = list(dict.fromkeys(row["file"] for row in rows))
    print("| file | A MoRM ms | B MoRM ms | B/A per round (median, min-max) | B/A fastest samples "
          "| rounds | load | same output |")
    print("|---|---:|---:|---|---:|---:|---|---|")
    all_ratios, all_fastest = [], []
    for name in files:
        mine = [row for row in rows if row["file"] == name]
        per_round = {}
        for row in mine:
            per_round.setdefault(row["round"], {})[row["build"]] = row
        pairs = [(r["a"], r["b"]) for r in per_round.values() if "a" in r and "b" in r]
        ratios = [b["median_ms"] / a["median_ms"] for a, b in pairs]
        fastest = (min(min(b["samples_ms"]) for _, b in pairs)
                   / min(min(a["samples_ms"]) for a, _ in pairs))
        all_ratios.append(statistics.median(ratios))
        all_fastest.append(fastest)
        loads = [row["loadavg1"] for row in mine if row.get("loadavg1") is not None]
        load = f"{min(loads):.1f}-{max(loads):.1f}" if loads else "n/a"
        same = len({row["hash"] for row in mine}) == 1
        print(f"| {name} | {statistics.median(a['median_ms'] for a, _ in pairs):.1f} | "
              f"{statistics.median(b['median_ms'] for _, b in pairs):.1f} | "
              f"{statistics.median(ratios):.3f} ({min(ratios):.3f}-{max(ratios):.3f}) | "
              f"{fastest:.3f} | {len(pairs)} | {load} | {'yes' if same else 'NO'} |")
    print(f"\nmedian of the per-file ratios: {statistics.median(all_ratios):.3f}; "
          f"geometric mean: {statistics.geometric_mean(all_ratios):.3f}; "
          f"fastest samples: median {statistics.median(all_fastest):.3f}, "
          f"geometric mean {statistics.geometric_mean(all_fastest):.3f}")


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--summarize":
        summarize(sys.argv[2:])
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--a", required=True)
    parser.add_argument("--b", required=True)
    parser.add_argument("--format", default="l2")
    parser.add_argument("--threads", type=int, default=1)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--iters", type=int, default=5)
    parser.add_argument("--cpu", type=int, default=20)
    parser.add_argument("--high-priority", action="store_true")
    parser.add_argument("--out")
    parser.add_argument("--args", default="", help="extra decode_bench arguments")
    parser.add_argument("--bench-stage", choices=["decode", "raster_ref", "raster_vel", "total"],
                        help="the binaries are recast-radar-bench; compare this stage's mean")
    parser.add_argument("files", nargs="+")
    args = parser.parse_args()
    cpu = args.cpu if args.threads == 1 else None
    results = {f: {"a": [], "b": [], "hash": set()} for f in args.files}
    log = open(args.out, "a") if args.out else None
    for round_index in range(args.rounds):
        order = [("a", args.a), ("b", args.b)]
        if round_index % 2:
            order.reverse()
        files = args.files if round_index % 2 == 0 else list(reversed(args.files))
        for path in files:
            for label, binary in order:
                result = run(binary, args.format, path, args.iters, args.threads, cpu,
                             args.high_priority, args.args.split(), args.bench_stage)
                results[path][label].append(result["median_ms"])
                results[path]["hash"].add(result["hash"])
                if log:
                    # The host's 1-minute load average as the process ended
                    # (Linux), so a quiet-host claim can be checked row by row.
                    load = os.getloadavg()[0] if hasattr(os, "getloadavg") else None
                    log.write(json.dumps({"round": round_index, "build": label,
                                          "file": Path(path).name, "loadavg1": load,
                                          **result}) + "\n")
                    log.flush()
        print(f"round {round_index} done", file=sys.stderr, flush=True)
    print("| file | A MoRM ms | B MoRM ms | B/A per round (median, min-max) | same output |")
    print("|---|---:|---:|---|---|")
    all_ratios = []
    for path in args.files:
        a, b = results[path]["a"], results[path]["b"]
        ratios = [y / x for x, y in zip(a, b)]
        all_ratios.append(statistics.median(ratios))
        print(f"| {Path(path).name} | {statistics.median(a):.1f} | {statistics.median(b):.1f} | "
              f"{statistics.median(ratios):.3f} ({min(ratios):.3f}-{max(ratios):.3f}) | "
              f"{'yes' if len(results[path]['hash']) == 1 else 'NO'} |")
    print(f"\nmedian of the per-file ratios: {statistics.median(all_ratios):.3f}; "
          f"geometric mean: {statistics.geometric_mean(all_ratios):.3f}")


if __name__ == "__main__":
    main()
