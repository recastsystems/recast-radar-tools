"""Interleaved study of run-time variants of one decode_bench build.

    variants.py --bin DECODE_BENCH --variants "a=K:V;K:V|b=K:V" --files F... \
        [--threads 0,12] [--rounds 3] [--iters 10] [--no-rss] [--no-time] --out LOG.jsonl
    variants.py --summarize LOG.jsonl [...]

For builds whose behaviour an environment variable selects (the Level II
pool experiments of docs/perf/data/perf-p1/pool-experiments.patch read
RECAST_EXP_*). Every round runs, for every file and thread count, every
variant (its environment added to the process's), in an order that rotates
and reverses between rounds: one process that decodes once (`rss`: its peak
RSS, `run.py`'s run_one) and one that runs a warmup and --iters timed
decodes (`time`). --summarize prints, per file, thread count and variant,
the range of the peak RSS, the median of the round medians with their
range, the fastest sample and the median one-decode time. Stdlib only.
"""

import argparse
import json
import statistics
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as xr  # noqa: E402


def parse_variants(spec):
    variants = []
    for item in spec.split("|"):
        name, _, envs = item.partition("=")
        env = {}
        for pair in filter(None, envs.split(";")):
            key, _, value = pair.partition(":")
            env[key] = value
        variants.append((name, env))
    return variants


def study(args):
    variants = parse_variants(args.variants)
    threads = [int(t) for t in args.threads.split(",")]
    out = open(args.out, "a")
    for round_index in range(args.rounds):
        files = args.files if round_index % 2 == 0 else list(reversed(args.files))
        for path in files:
            for n in threads:
                rotation = round_index % len(variants)
                order = variants[rotation:] + variants[:rotation]
                if round_index % 2:
                    order = list(reversed(order))
                for name, env in order:
                    base = [args.bin, "--format", "l2", path, "--from-path", "--wait-stdin"]
                    if n:
                        base += ["--threads", str(n)]
                    rows = []
                    if not args.no_rss:
                        result = xr.run_one(base + ["--iters", "1", "--warmup", "0"], env, None, 600)
                        result.update(kind="rss")
                        result.pop("samples_ms", None)
                        rows.append(result)
                    if not args.no_time:
                        result = xr.run_one(base + ["--iters", str(args.iters), "--warmup", "1"], env,
                                            None, 600)
                        result.update(kind="time")
                        rows.append(result)
                    for result in rows:
                        result.update(variant=name, file=Path(path).name, threads_asked=n,
                                      round=round_index, time=time.strftime("%H:%M:%S"))
                        out.write(json.dumps(result) + "\n")
                        out.flush()
                        print(f"r{round_index} {Path(path).name[:24]:24} t={n:<3} {name:10} "
                              f"{result['kind']:4} median={result.get('median_ms')} "
                              f"rss={result.get('peak_rss_kb')} {(result.get('error') or '')[:80]}",
                              flush=True)
    out.close()


def summarize(paths):
    rows = [json.loads(line) for p in paths for line in xr_open(p) if line.strip()]
    peak, one, medians, fastest = (defaultdict(list) for _ in range(4))
    for r in rows:
        if r.get("error"):
            continue
        key = (r["file"], r["threads_asked"], r["variant"])
        if r["kind"] == "rss":
            peak[key].append(r["peak_rss_kb"] / 1024)
            one[key].append(r["median_ms"])
        else:
            medians[key].append(r["median_ms"])
            fastest[key].append(r["min_ms"])
    keys = sorted(set(peak) | set(medians), key=lambda k: (k[0], k[1], k[2]))
    print("| file | threads | variant | peak RSS MiB | MoRM ms | round medians range | fastest ms | "
          "one decode ms (median) |")
    print("|---|---:|---|---|---:|---|---:|---:|")

    def span(values):
        return f"{min(values):.1f}-{max(values):.1f}" if values else ""

    def med(values):
        return f"{statistics.median(values):.1f}" if values else ""

    for key in keys:
        print(f"| {key[0]} | {key[1] or 'default'} | {key[2]} | {span(peak.get(key, []))} | "
              f"{med(medians.get(key, []))} | {span(medians.get(key, []))} | "
              f"{f'{min(fastest[key]):.1f}' if fastest.get(key) else ''} | {med(one.get(key, []))} |")


def xr_open(path):
    import gzip

    return gzip.open(path, "rt") if str(path).endswith(".gz") else open(path)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--summarize", nargs="+")
    parser.add_argument("--bin")
    parser.add_argument("--variants")
    parser.add_argument("--files", nargs="+")
    parser.add_argument("--threads", default="0")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--iters", type=int, default=10)
    parser.add_argument("--out")
    parser.add_argument("--no-rss", action="store_true")
    parser.add_argument("--no-time", action="store_true")
    args = parser.parse_args()
    if args.summarize:
        summarize(args.summarize)
    else:
        if not (args.bin and args.variants and args.files and args.out):
            parser.error("--bin, --variants, --files and --out are required for a study")
        study(args)


if __name__ == "__main__":
    main()
