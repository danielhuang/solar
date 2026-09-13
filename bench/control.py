#!/usr/bin/env python3
"""Measure loop optimization and binary trees in three interleaved rounds.

Validate every output, and retain wall time, CPU time and GNU time peak RSS.
Run from any directory; binaries must be built using guide.md.
"""
import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GROUPS = {
    "loops": [
        ("Solar loop2", ["target/loop2"]),
        ("Solar loop2fn5", ["target/loop2fn5"]),
        ("C loop2", ["bench/c/loop2"]),
    ],
    "binarytrees": [
        ("Solar threaded", ["target/binarytrees"]),
        ("Solar single", ["target/binarytrees_st"]),
        ("C++ arena", ["target/bench/bt_arena", "21"]),
        ("C malloc/free", ["target/bench/bt_vanilla", "21"]),
    ],
}


def measure(argv):
    """Return process timing, per-process peak RSS, and captured output."""
    argv = [str(ROOT / argv[0]), *argv[1:]]
    with tempfile.TemporaryFile() as output, tempfile.NamedTemporaryFile() as timing:
        start = time.perf_counter()
        subprocess.run(
            ["/usr/bin/time", "-f", "%U %S %M", "-o", timing.name, *argv],
            stdout=output, check=True,
        )
        elapsed = time.perf_counter() - start
        user, system, rss = timing.read().decode().split()
        output.seek(0)
        text = output.read().decode()
    return dict(wall=elapsed, cpu=float(user) + float(system),
                rss_kib=int(rss)), text


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("group", choices=GROUPS)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args()
    assert args.rounds > 0
    rows = []
    expected = None
    for round_index in range(1, args.rounds + 1):
        for label, argv in GROUPS[args.group]:
            metrics, output = measure(argv)
            if args.group == "loops":
                assert output == "".join(f"{i}\n" for i in range(0, 1_000_000_000, 10000))
            else:
                lines = output.splitlines()
                if label == "C malloc/free":
                    lines = lines[1:]
                normalized = [" ".join(line.split()) for line in lines]
                if expected is None:
                    expected = normalized
                assert normalized == expected, (label, normalized, expected)
            row = dict(runtime=label, round=round_index, **metrics)
            rows.append(row)
            args.json.write_text(json.dumps(rows, indent=2) + "\n")
            print(row, flush=True)
    print("| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |")
    print("| --- | ---: | ---: | ---: |")
    for label, _ in GROUPS[args.group]:
        samples = [row for row in rows if row["runtime"] == label]
        best = {key: min(row[key] for row in samples) for key in ("wall", "cpu", "rss_kib")}
        print(f"| {label} | {best['wall']:.3f} | {best['cpu']:.3f} | {best['rss_kib']/1024:.1f} |")


if __name__ == "__main__":
    main()
