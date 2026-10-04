#!/usr/bin/env python3
"""Run the built benchmark suite and save a report, logs, and raw measurements.

Uses the round counts and workloads documented in guide.md. Groups run
sequentially, including after a failed group; any failure makes the command
fail after writing the report. Build the binaries with bash bench/build.sh.
"""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
GROUPS = {
    "allocation": ("Allocation and GC", "bench/bench.py",
                   ["--markdown", "--json", "allocation.json"]),
    "allocators": ("C allocator comparison", "bench/c/alloc_matrix.py", []),
    "sieve": ("Sieve", "bench/sieve_matrix.py", []),
    "loops": ("Loop optimization", "bench/control.py",
              ["loops", "--json", "loops.json"]),
    "hashmap": ("HashMap", "bench/run.py", []),
    "binarytrees": ("Binary trees", "bench/control.py",
                    ["binarytrees", "--json", "binarytrees.json"]),
}


def environment():
    """Record machine details and tool versions without dumping environment secrets."""
    commands = [
        ["git", "rev-parse", "HEAD"], ["uname", "-a"], ["cat", "/etc/os-release"],
        ["lscpu"], ["free", "-h"],
        ["rustc", "--version"], ["clang", "--version"], ["llvm-config", "--version"],
        ["gcc", "--version"], ["go", "version"], ["node", "--version"],
        ["julia", "--version"], ["java", "-version"], ["javac", "-version"],
        [str(Path.home() / ".dotnet/dotnet"), "--info"],
        ["python3", "--version"],
        ["dpkg-query", "-W", "libjemalloc2", "libtcmalloc-minimal4t64", "libmimalloc3"],
    ]
    sections = []
    for command in commands:
        try:
            result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, text=True, timeout=30)
            output = result.stdout
        except (OSError, subprocess.TimeoutExpired) as error:
            output = str(error)
        sections.append(f"$ {shlex.join(command)}\n{output}\n")
    return "\n".join(sections)


def run_group(command, log_path):
    """Stream a group's output to its log and the CI console, returning its exit code."""
    with log_path.open("w") as log:
        try:
            with subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, text=True) as process:
                for line in process.stdout:
                    log.write(line)
                    log.flush()
                    print(line, end="", flush=True)
                return process.wait()
        except OSError as error:
            log.write(f"Unable to run benchmark: {error}\n")
            return 1


def write_report(output, metadata, results):
    """Save completed group statuses and their existing Markdown result tables."""
    (output / "results.json").write_text(
        json.dumps({**metadata, "groups": results}, indent=2) + "\n")
    outcome = "in progress / incomplete"
    if "finished_at" in metadata:
        outcome = "FAILED" if any(row["exit_code"] != 0 for row in results) else "passed"
    lines = ["# Benchmark report", "", f"Started: {metadata['started_at']}", "",
             f"Outcome: **{outcome}**", "",
             f"Commit: `{metadata['commit']}`", "",
             f"Groups requested: {', '.join(metadata['requested_groups'])}", "",
             "[Machine and tool versions](environment.txt) · [Run status](results.json)", "",
             "Release codegen; groups run sequentially. Allocation/GC, C allocators, "
             "loops and binary trees use three rounds, sieve five, and HashMap seven "
             "per phase. Tables report independent minima across rounds, including "
             "process startup and JIT time. See bench/guide.md for metric definitions.", "",
             "| Group | Status | Harness elapsed (s) |", "| --- | --- | ---: |"]
    for result in results:
        status = "passed" if result["exit_code"] == 0 else f"FAILED ({result['exit_code']})"
        lines.append(f"| {result['title']} | {status} | {result['elapsed_seconds']:.1f} |")
    for result in results:
        lines += ["", f"## {result['title']}", "",
                  f"[Full output]({result['log']})", "",
                  f"Command: `{shlex.join(result['command'])}`", ""]
        if result["exit_code"] != 0:
            lines += ["**Failed: results below may be incomplete. See the full output.**", ""]
        for raw in result["measurements"]:
            lines += [f"[Raw measurements]({raw})", ""]
        for line in (output / result["log"]).read_text().splitlines():
            if line.startswith("## "):
                lines += ["", "#" + line, ""]
            elif line.startswith("|"):
                lines.append(line)
    (output / "report.md").write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "target/benchmark-results")
    parser.add_argument("--groups", nargs="+", choices=GROUPS, default=list(GROUPS),
                        help="run only selected groups (default: the complete suite)")
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    metadata = {
        "started_at": datetime.now(timezone.utc).isoformat(),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "build_url": os.environ.get("CIRCLE_BUILD_URL"),
        "requested_groups": args.groups,
        "load_before": os.getloadavg(),
    }
    (output / "environment.txt").write_text(environment())
    results = []
    write_report(output, metadata, results)
    for group in args.groups:
        title, script, extra = GROUPS[group]
        for arg in extra:
            if arg.endswith(".json"):
                (output / arg).unlink(missing_ok=True)
        command = [sys.executable, "-u", script]
        command += [str(output / arg) if arg.endswith(".json") else arg for arg in extra]
        print(f"Running {title}: {shlex.join(command)}", flush=True)
        start = time.perf_counter()
        code = run_group(command, output / f"{group}.log")
        results.append({"group": group, "title": title, "command": command,
                        "exit_code": code, "elapsed_seconds": time.perf_counter() - start,
                        "log": f"{group}.log",
                        "measurements": [arg for arg in extra
                                         if arg.endswith(".json") and (output / arg).exists()]})
        write_report(output, metadata, results)
    metadata["finished_at"] = datetime.now(timezone.utc).isoformat()
    metadata["load_after"] = os.getloadavg()
    write_report(output, metadata, results)
    return int(any(result["exit_code"] != 0 for result in results))


if __name__ == "__main__":
    sys.exit(main())
