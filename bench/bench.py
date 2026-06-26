#!/usr/bin/env python3
"""Allocation / GC benchmark harness for Solar vs C vs Go vs the JVM collectors.

Runs two benchmarks (allocs3, threads_list2), each ported to every runtime, and
reports throughput (median wall-clock + peak RSS) and GC-pause latency (max/p50
STW stall). Contenders are **interleaved**: every round runs each language once
before the next round begins, so background-load drift over the session is
spread evenly across languages rather than penalizing whichever ran last.

Prereqs (see README.md "How to reproduce"):
  Solar  target/{allocs3,threads_list2}        (cargo ... --bin compile)
  C      bench/c/{allocs3,threads_list2}        (make -C bench/c)
  Go     bench/go/{allocs3,threads_list2}       (go build)
  Java   bench/java/*.class                     (javac)

Usage:
  bench/bench.py                 # both throughput and latency, 3 rounds
  bench/bench.py --rounds 5      # more rounds
  bench/bench.py --only throughput
  bench/bench.py --only latency
  bench/bench.py --markdown      # also emit README.md-style tables
"""
from __future__ import annotations

import argparse
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
JAVA_DIR = ROOT / "bench" / "java"
JAVA_OPTS = ["-Xmx8g"]

# Each benchmark: (solar/c/go binary stem, Java class).
BENCHMARKS = [("allocs3", "Allocs3"), ("threads_list2", "ThreadsList2")]


# --------------------------------------------------------------------------- #
# Contenders
# --------------------------------------------------------------------------- #
def contenders(stem: str, cls: str):
    """Return [(label, argv, env, latency_kind), ...] for one benchmark."""
    java = ["java", *JAVA_OPTS]
    return [
        ("Solar",            [str(ROOT / "target" / stem)],          {}, "solar"),
        ("C (malloc/free)",  [str(ROOT / "bench/c" / stem)],         {}, "none"),
        ("Go",               [str(ROOT / "bench/go" / stem)],        {}, "go"),
        ("Java G1",          [*java, "-XX:+UseG1GC", cls],           {}, "java"),
        ("Java Parallel",    [*java, "-XX:+UseParallelGC", cls],     {}, "java"),
        ("Java ZGC gen",     [*java, "-XX:+UseZGC", "-XX:+ZGenerational", cls], {}, "java"),
        ("Java ZGC non-gen", [*java, "-XX:+UseZGC", cls],            {}, "java"),
        ("Java Shenandoah",  [*java, "-XX:+UseShenandoahGC", cls],   {}, "java"),
    ]


# --------------------------------------------------------------------------- #
# Throughput: wall-clock + peak RSS
# --------------------------------------------------------------------------- #
def peak_rss_kb(pid: int) -> int:
    """Read VmHWM (kernel-tracked peak RSS, monotonic) from /proc; 0 if gone."""
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError, ValueError):
        pass
    return 0


def run_throughput(argv, env) -> tuple[float, int]:
    """Run once; return (wall_seconds, peak_rss_kb)."""
    full_env = {**os.environ, **env}
    start = time.perf_counter()
    proc = subprocess.Popen(
        argv, cwd=JAVA_DIR, env=full_env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    hwm = 0
    while proc.poll() is None:
        hwm = max(hwm, peak_rss_kb(proc.pid))
        time.sleep(0.01)
    hwm = max(hwm, peak_rss_kb(proc.pid))  # last chance before reaping
    proc.wait()
    return time.perf_counter() - start, hwm


# --------------------------------------------------------------------------- #
# Latency: STW pause samples (ms) per runtime
# --------------------------------------------------------------------------- #
_UNIT = {"µs": 1e-3, "ms": 1.0, "s": 1e3}
_SOLAR_RE = re.compile(r"pause([12]) ([0-9.]+)(µs|ms|s)")
_GO_RE = re.compile(r"([0-9.]+)\+[0-9.]+\+([0-9.]+) ms clock")
_JAVA_RE = re.compile(r"At safepoint: (\d+) ns")


def capture(argv, env) -> str:
    full_env = {**os.environ, **env}
    p = subprocess.run(
        argv, cwd=JAVA_DIR, env=full_env,
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
    )
    return p.stderr


def pause_samples(argv, kind: str) -> list[float]:
    """Run once with the runtime's GC trace enabled; return STW pauses in ms."""
    if kind == "none":
        return []
    if kind == "solar":
        # Solar prints stats to stdout; capture both streams.
        env = {"SOLAR_PRINT_GC_STATS": "1"}
        p = subprocess.run(argv, cwd=JAVA_DIR, env={**os.environ, **env},
                           capture_output=True, text=True)
        text = p.stdout + p.stderr
        out = []
        for line in text.splitlines():
            parts = {n: float(v) * _UNIT[u] for n, v, u in _SOLAR_RE.findall(line)}
            if "1" in parts and "2" in parts:  # one GC cycle -> STW = pause1+pause2
                out.append(parts["1"] + parts["2"])
        return out
    if kind == "go":
        text = capture(argv, {"GODEBUG": "gctrace=1"})
        # clock triple: STW sweep-term + concurrent mark + STW mark-term
        return [float(a) + float(b) for a, b in _GO_RE.findall(text)]
    if kind == "java":
        # JVM flags must precede the main class; -Xlog defaults to stdout.
        java_argv = argv[:-1] + ["-Xlog:safepoint", argv[-1]]
        p = subprocess.run(java_argv, cwd=JAVA_DIR, capture_output=True, text=True)
        text = p.stdout + p.stderr
        return [int(ns) / 1e6 for ns in _JAVA_RE.findall(text)]
    raise ValueError(kind)


# --------------------------------------------------------------------------- #
# Driver
# --------------------------------------------------------------------------- #
def loadavg() -> str:
    return f"load average: {', '.join(f'{x:.2f}' for x in os.getloadavg())}"


def fmt(x, nd=2):
    return "—" if x is None else f"{x:.{nd}f}"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--only", choices=["throughput", "latency"], default=None)
    ap.add_argument("--markdown", action="store_true",
                    help="also print README.md-style transposed tables")
    args = ap.parse_args()

    do_tp = args.only in (None, "throughput")
    do_lat = args.only in (None, "latency")
    rounds = args.rounds

    print(f"### {loadavg()} (before)\n")
    # results[bench][label] = dict of metrics
    results: dict[str, dict[str, dict]] = {}

    for stem, cls in BENCHMARKS:
        print(f"==== {cls} ====")
        conts = contenders(stem, cls)
        walls = {lbl: [] for lbl, *_ in conts}
        rss = {lbl: [] for lbl, *_ in conts}
        lat_max = {lbl: [] for lbl, *_ in conts}   # per-run max
        lat_p50 = {lbl: [] for lbl, *_ in conts}   # per-run p50

        for r in range(1, rounds + 1):
            print(f"  round {r}/{rounds}:", end="", flush=True)
            for lbl, argv, env, kind in conts:
                if do_tp:
                    w, m = run_throughput(argv, env)
                    walls[lbl].append(w)
                    rss[lbl].append(m)
                if do_lat and kind != "none":
                    s = pause_samples(argv, kind)
                    if s:
                        lat_max[lbl].append(max(s))
                        lat_p50[lbl].append(statistics.median(s))
                print(" .", end="", flush=True)
            print()

        results[cls] = {}
        for lbl, *_ in conts:
            results[cls][lbl] = {
                "wall": statistics.median(walls[lbl]) if walls[lbl] else None,
                "rss_mb": max(rss[lbl]) // 1024 if rss[lbl] else None,
                # median across rounds of each run's max / p50 (robust to outliers)
                "lat_max": statistics.median(lat_max[lbl]) if lat_max[lbl] else None,
                "lat_p50": statistics.median(lat_p50[lbl]) if lat_p50[lbl] else None,
            }

        # Console summary for this benchmark
        for lbl, *_ in conts:
            m = results[cls][lbl]
            row = f"  {lbl:<18}"
            if do_tp:
                row += f" wall={fmt(m['wall']):>7}s rss={str(m['rss_mb']):>5}MB"
            if do_lat:
                row += f"  pause max={fmt(m['lat_max']):>8} p50={fmt(m['lat_p50']):>6} ms"
            print(row)
        print()

    print(f"### {loadavg()} (after)")

    if args.markdown:
        print_markdown(results, do_tp, do_lat)


def print_markdown(results, do_tp, do_lat):
    labels = list(next(iter(results.values())).keys())
    benches = list(results.keys())
    print("\n----- markdown -----\n")
    if do_tp:
        print("## Throughput & peak memory (lower is better)\n")
        hdr = "| runtime | " + " | ".join(
            f"{b} wall | {b} RSS" for b in benches) + " |"
        print(hdr)
        print("|" + "---|" * (1 + 2 * len(benches)))
        for lbl in labels:
            cells = []
            for b in benches:
                m = results[b][lbl]
                cells.append(f"{fmt(m['wall'])} s")
                cells.append("—" if m["rss_mb"] is None else f"{m['rss_mb']} MB")
            print(f"| {lbl} | " + " | ".join(cells) + " |")
        print()
    if do_lat:
        print("## GC pause latency — STW stall (ms, median of rounds)\n")
        print("| runtime | " + " | ".join(
            f"{b} max | {b} p50" for b in benches) + " |")
        print("|" + "---|" * (1 + 2 * len(benches)))
        for lbl in labels:
            cells = []
            for b in benches:
                m = results[b][lbl]
                if lbl.startswith("C "):
                    cells += ["none", "none"]
                else:
                    cells += [fmt(m["lat_max"]), fmt(m["lat_p50"])]
            print(f"| {lbl} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    sys.exit(main())
