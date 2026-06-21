#!/usr/bin/env python3
"""Benchmark harness: times the Solar and Rust HashMap benchmarks per phase.

Each phase is run as its own process (phase index fed on stdin) so wall time and
peak RSS are isolated per datatype. Reports best-of-N wall time (min) and peak
RSS (max RSS reported by the kernel for that child). Verifies the per-phase
checksums match between the two implementations.
"""
import os
import sys
import time

REPS = 7
PHASES = [(0, "u64"), (1, "u32"), (2, "point"), (3, "mixed")]

SOLAR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "target", "hashmap"))
RUST = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "rust", "target", "release", "hashmap-bench")
)


def measure(binary, phase):
    """Run `binary` for one phase; return (wall_seconds, peak_rss_kib, stdout)."""
    r, w = os.pipe()
    os.write(w, str(phase).encode())
    os.close(w)
    out_r, out_w = os.pipe()
    t0 = time.perf_counter()
    pid = os.posix_spawn(
        binary,
        [binary],
        os.environ,
        file_actions=[
            (os.POSIX_SPAWN_DUP2, r, 0),
            (os.POSIX_SPAWN_DUP2, out_w, 1),
        ],
    )
    os.close(r)
    os.close(out_w)
    chunks = []
    while True:
        b = os.read(out_r, 4096)
        if not b:
            break
        chunks.append(b)
    _, status, ru = os.wait4(pid, 0)
    t1 = time.perf_counter()
    os.close(out_r)
    assert status == 0, f"{binary} phase {phase} exited with {status}"
    return t1 - t0, ru.ru_maxrss, b"".join(chunks).decode().strip()


def bench(binary):
    """Return {label: (best_wall, peak_rss, checksum)} for every phase."""
    results = {}
    for idx, label in PHASES:
        best = None
        rss = 0
        checksum = None
        for _ in range(REPS):
            wall, peak, out = measure(binary, idx)
            best = wall if best is None else min(best, wall)
            rss = max(rss, peak)
            checksum = out.split(":")[-1].strip()
        results[label] = (best, rss, checksum)
    return results


def main():
    for b in (SOLAR, RUST):
        if not os.path.exists(b):
            sys.exit(f"missing binary: {b} (run bench/run.sh first)")

    solar = bench(SOLAR)
    rust = bench(RUST)

    lines = []
    lines.append(f"Best of {REPS} runs; 1,000,000 keys per phase (insert + hit + miss).")
    lines.append("")
    lines.append("| phase | Solar (ms) | Rust (ms) | Solar/Rust | Solar RSS (MB) | Rust RSS (MB) | checksum match |")
    lines.append("|-------|-----------:|----------:|-----------:|---------------:|--------------:|:--------------:|")
    s_total = r_total = 0.0
    for _, label in PHASES:
        sw, srss, sck = solar[label]
        rw, rrss, rck = rust[label]
        s_total += sw
        r_total += rw
        ratio = sw / rw if rw else float("nan")
        match = "yes" if sck == rck else f"NO ({sck} vs {rck})"
        lines.append(
            f"| {label} | {sw*1000:.1f} | {rw*1000:.1f} | {ratio:.2f}x | "
            f"{srss/1024:.1f} | {rrss/1024:.1f} | {match} |"
        )
    ratio_total = s_total / r_total if r_total else float("nan")
    lines.append(
        f"| **total** | **{s_total*1000:.1f}** | **{r_total*1000:.1f}** | "
        f"**{ratio_total:.2f}x** | | | |"
    )
    table = "\n".join(lines)
    print(table)
    return table


if __name__ == "__main__":
    main()
