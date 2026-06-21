#!/usr/bin/env python3
"""Run the C benchmarks under several malloc implementations.

For each (benchmark, allocator) pair: median wall-clock and peak RSS over N
interleaved rounds (every combo runs once per round, so background drift is
spread evenly). Allocators are swapped in via LD_PRELOAD; the binaries are
unchanged from `make`.
"""
import os, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
LIB = "/usr/lib/x86_64-linux-gnu"

ALLOCATORS = [
    ("glibc",    ""),  # baseline: no preload
    ("jemalloc", f"{LIB}/libjemalloc.so.2"),
    ("tcmalloc", f"{LIB}/libtcmalloc_minimal.so.4"),
    ("mimalloc", f"{LIB}/libmimalloc.so.3"),
    ("bump",     f"{HERE}/libbump.so"),
]
BENCHES = ["allocs3", "threads_list2"]
ROUNDS = int(os.environ.get("ROUNDS", "3"))

# (bench, alloc) -> {"wall": [...], "rss": [...]}
results = {(b, a): {"wall": [], "rss": []} for b in BENCHES for a, _ in ALLOCATORS}


def run_one(bench, preload):
    """Return (wall_seconds, peak_rss_kib) via os.wait4's per-process rusage."""
    env = dict(os.environ)
    if preload:
        env["LD_PRELOAD"] = preload
    else:
        env.pop("LD_PRELOAD", None)
    devnull = os.open(os.devnull, os.O_WRONLY)
    path = os.path.join(HERE, bench)
    t0 = time.perf_counter()
    pid = os.posix_spawn(path, [path], env,
                         file_actions=[(os.POSIX_SPAWN_DUP2, devnull, 1)])
    _, status, ru = os.wait4(pid, 0)
    wall = time.perf_counter() - t0
    os.close(devnull)
    if status != 0:
        print(f"  !! {bench} ({preload or 'glibc'}) exited status={status}",
              file=sys.stderr)
    return wall, ru.ru_maxrss  # ru_maxrss is KiB on Linux


def main():
    for r in range(ROUNDS):
        for bench in BENCHES:
            for alloc, preload in ALLOCATORS:
                wall, rss = run_one(bench, preload)
                results[(bench, alloc)]["wall"].append(wall)
                if rss is not None:
                    results[(bench, alloc)]["rss"].append(rss)
                print(f"  round {r+1} {bench:14s} {alloc:9s} "
                      f"{wall:6.2f}s  rss={rss/1024:7.0f} MB", flush=True)

    print("\n## C benchmark: allocator comparison "
          f"(median of {ROUNDS} rounds, interleaved)\n")
    print("| allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS |")
    print("|-----------|-------------:|------------:|-------------:|------------:|")
    for alloc, _ in ALLOCATORS:
        cells = []
        for bench in BENCHES:
            w = results[(bench, alloc)]["wall"]
            m = results[(bench, alloc)]["rss"]
            wall = statistics.median(w)
            rss = statistics.median(m) / 1024 if m else float("nan")
            cells.append((wall, rss))
        (aw, ar), (tw, tr) = cells
        print(f"| {alloc:9s} | {aw:9.2f} s | {ar:7.0f} MB | "
              f"{tw:9.2f} s | {tr:7.0f} MB |")


if __name__ == "__main__":
    main()
