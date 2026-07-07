#!/usr/bin/env python3
"""Run the sieve ports (a non-allocation compute benchmark) interleaved.

For each (contender) per round: wall-clock, peak RSS (wait4 rusage), and an
output check (every port must print 5761455, the prime count below 10^8).
Median over ROUNDS interleaved rounds. Unlike bench.py there is nothing for a
collector to do here, so each runtime appears once with its default GC.

Prereqs:
  Solar  target/sieve      (cargo run --release --bin compile -- examples/sieve.solar target/sieve)
  C      bench/c/sieve      (make -C bench/c)
  Go     bench/go/sieve     (go build)
  Java   bench/java/Sieve.class   (javac)
  C#     bench/csharp/sieve/bin/Release/net10.0/sieve   (dotnet build -c Release)
"""
import os, statistics, sys, tempfile, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOTNET_ROOT = str(Path.home() / ".dotnet")
EXPECTED = b"5761455"
ROUNDS = int(os.environ.get("ROUNDS", "5"))

CONTENDERS = [
    ("Solar", [str(ROOT / "target/sieve")], {}),
    ("C",     [str(ROOT / "bench/c/sieve")], {}),
    ("Go",    [str(ROOT / "bench/go/sieve")], {}),
    ("Java",  ["/usr/bin/env", "java", "-cp", str(ROOT / "bench/java"), "Sieve"], {}),
    ("C#",    [str(ROOT / "bench/csharp/sieve/bin/Release/net10.0/sieve")],
              {"DOTNET_ROOT": DOTNET_ROOT}),
]


def run_one(argv, extra_env):
    """Return (wall_seconds, peak_rss_kib); assert the printed prime count."""
    env = {**os.environ, **extra_env}
    with tempfile.TemporaryFile() as out:
        t0 = time.perf_counter()
        pid = os.posix_spawn(argv[0], argv, env,
                             file_actions=[(os.POSIX_SPAWN_DUP2, out.fileno(), 1)])
        _, status, ru = os.wait4(pid, 0)
        wall = time.perf_counter() - t0
        out.seek(0)
        got = out.read().strip()
    assert status == 0, f"{argv} exited status={status}"
    assert got == EXPECTED, f"{argv} printed {got!r}, expected {EXPECTED!r}"
    return wall, ru.ru_maxrss  # ru_maxrss is KiB on Linux


def main():
    results = {lbl: {"wall": [], "rss": []} for lbl, *_ in CONTENDERS}
    for r in range(ROUNDS):
        for lbl, argv, env in CONTENDERS:
            wall, rss = run_one(argv, env)
            results[lbl]["wall"].append(wall)
            results[lbl]["rss"].append(rss)
            print(f"  round {r+1} {lbl:6s} {wall:6.2f}s  rss={rss/1024:6.0f} MB",
                  flush=True)

    print(f"\n## sieve (median of {ROUNDS} rounds, interleaved)\n")
    print("| runtime | wall | peak RSS |")
    print("|---------|-----:|---------:|")
    for lbl, *_ in CONTENDERS:
        w = statistics.median(results[lbl]["wall"])
        m = statistics.median(results[lbl]["rss"]) / 1024
        print(f"| {lbl:7s} | {w:.2f} s | {m:.0f} MB |")


if __name__ == "__main__":
    main()
