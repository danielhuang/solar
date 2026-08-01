# Benchmark guide

This guide contains the build, run, and measurement instructions for the
results in [README.md](README.md). Run commands from the repository root unless
a command changes directory explicitly.

## Contents

- [Requirements](#requirements)
- [Recorded environment](#recorded-environment)
- [Source layout](#source-layout)
- [Build the complete suite](#build-the-complete-suite)
- [Run the allocation and GC matrix](#run-the-allocation-and-gc-matrix)
- [Run the C allocator matrix](#run-the-c-allocator-matrix)
- [Run sieve](#run-sieve)
- [Run HashMap](#run-hashmap)
- [Run binary trees](#run-binary-trees)
- [Measurement definitions](#measurement-definitions)

## Requirements

The repository toolchain requirements in the root `AGENTS.md` apply. The full
benchmark suite additionally needs:

- C and C++ compilers with `-O3` and `-march=native` support;
- Go;
- Node.js;
- JDK 21, including `javac`;
- .NET 10;
- Python 3;
- GNU `/usr/bin/time`;
- jemalloc, gperftools tcmalloc-minimal, and mimalloc shared libraries.

The harnesses currently expect these allocator paths:

```text
/usr/lib/x86_64-linux-gnu/libjemalloc.so.2
/usr/lib/x86_64-linux-gnu/libtcmalloc_minimal.so.4
/usr/lib/x86_64-linux-gnu/libmimalloc.so.3
```

`bench/bench.py` pins Java to
`/usr/lib/jvm/java-21-openjdk-amd64/bin/java` when that path exists. Compile
the Java ports with the matching JDK 21 `javac`; newer class files cannot run
on JDK 21.

The .NET projects target `net10.0`. The harnesses set `DOTNET_ROOT` to
`$HOME/.dotnet`, matching an installation made by `dotnet-install.sh`. Adjust
the harnesses if the runtime is installed elsewhere.

Run benchmarks on an otherwise idle machine when comparing small differences.
The full allocation/GC matrix can exceed 19 GiB RSS in one process. The no-free
bump allocator exceeded 21 GiB in an individual run during the July 31, 2026
session.

## Recorded environment

The July 31, 2026 results in `README.md` used:

| Component | Version |
| --- | --- |
| Operating system | Linux 7.1.3 |
| CPU | Intel Core Ultra 9 275HX, 24 cores |
| Memory | 93 GiB |
| Clang/LLVM | 22.1.6 |
| GCC/G++ | 14.2.0 |
| Rust | 1.98.0-nightly (2026-06-11) |
| Go | 1.24.4 |
| Allocation/GC Java | OpenJDK 21.0.11 |
| Sieve Java | OpenJDK 25.0.3 |
| .NET | 10.0.301 |
| Node.js | 20.19.2 |

## Source layout

```text
bench/
  bench.py                 allocation/GC throughput and pause harness
  c/                       C ports, allocator harness, and bump allocator
  csharp/                  .NET ports and GC-pause event listener
  go/                      Go ports
  java/                    Java ports
  js/                      Node.js ports
  rust/                    Rust HashMap reference
  sieve_matrix.py          sieve harness
  run.py                   HashMap harness
  run.sh                   HashMap build-and-run shortcut
  binarytrees_arena.cpp    threaded C++ arena implementation
  binaryTrees_vanilla.c    single-threaded C malloc/free implementation
```

The Solar programs are under `examples/`.

## Build the complete suite

Build the Solar runtime and compile every Solar benchmark through release
codegen:

```bash
cargo build --release -p solar-system

for stem in allocs3 threads_list2 splay allocs5 sieve hashmap binarytrees binarytrees_st; do
  cargo run --release --quiet --bin compile -- \
    "examples/$stem.solar" "target/$stem"
done
```

Build the C ports and the preloadable bump allocator:

```bash
make -C bench/c
clang -O3 -fPIC -ftls-model=initial-exec -shared \
  -o bench/c/libbump.so bench/c/bump.c
```

Build the Go ports:

```bash
(
  cd bench/go
  for stem in allocs3 threads_list2 splay allocs5 sieve; do
    go build -o "$stem" "$stem.go"
  done
)
```

Build the Java ports with JDK 21:

```bash
/usr/lib/jvm/java-21-openjdk-amd64/bin/javac \
  bench/java/Allocs3.java \
  bench/java/ThreadsList2.java \
  bench/java/Splay.java \
  bench/java/Allocs5.java \
  bench/java/Sieve.java
```

Build the .NET ports. Replace the `dotnet` path if necessary:

```bash
for project in allocs3 threads_list2 splay allocs5 sieve; do
  "$HOME/.dotnet/dotnet" build "bench/csharp/$project" \
    -c Release --nologo
done
```

Build the Rust HashMap reference:

```bash
cargo build --release --manifest-path bench/rust/Cargo.toml
```

Build the binary-trees C and C++ references:

```bash
mkdir -p target/bench
g++ -O3 -march=native -std=c++17 \
  bench/binarytrees_arena.cpp -o target/bench/bt_arena -lpthread
gcc -O3 -march=native \
  bench/binaryTrees_vanilla.c -o target/bench/bt_vanilla -lm
```

## Run the allocation and GC matrix

Run all four workloads, all eleven runtime/collector configurations, both
throughput and traced-latency modes, and three interleaved rounds:

```bash
python3 bench/bench.py --markdown
```

Useful narrower runs:

```bash
python3 bench/bench.py --only throughput
python3 bench/bench.py --only latency
python3 bench/bench.py --rounds 5 --only latency
```

The Java configurations are G1, Parallel, generational ZGC,
non-generational ZGC, and Shenandoah. The .NET configurations select
workstation and server GC at process startup. Node.js runs with an 8 GiB old
space limit per isolate.

## Run the C allocator matrix

Run every C workload under glibc, jemalloc, tcmalloc-minimal, mimalloc, and the
no-free bump allocator:

```bash
ROUNDS=3 python3 bench/c/alloc_matrix.py
```

The harness raises its own OOM-kill priority before the high-memory bump runs.
It swaps allocators with `LD_PRELOAD`; the benchmark executables do not change.
The Solar baseline shown in `README.md` comes from `bench/bench.py`; Solar is
not executed by `alloc_matrix.py` or interleaved with the C allocator rows.

## Run sieve

Run five interleaved rounds of Solar, C, Go, Java, and C#:

```bash
ROUNDS=5 python3 bench/sieve_matrix.py
```

The harness requires every process to print `5761455` and fails if an output
or exit status is incorrect.

## Run HashMap

If the Solar and Rust binaries are already built, run the measurement harness
directly:

```bash
python3 bench/run.py
```

To rebuild the Solar runtime, Solar benchmark, and Rust reference before
running:

```bash
bench/run.sh
```

The harness runs each key-type phase in a separate process seven times, checks
the Solar and Rust checksums, and reports the best wall time and largest peak
RSS.

## Run binary trees

The Solar programs use depth 21 internally. Pass `21` to the C and C++
references:

```bash
target/binarytrees
target/binarytrees_st
target/bench/bt_arena 21
target/bench/bt_vanilla 21
```

Measure three rounds with GNU `time`:

```bash
for round in 1 2 3; do
  /usr/bin/time -f 'Solar threaded: wall=%e user=%U sys=%S rss_kib=%M' \
    target/binarytrees >/dev/null
  /usr/bin/time -f 'Solar single: wall=%e user=%U sys=%S rss_kib=%M' \
    target/binarytrees_st >/dev/null
  /usr/bin/time -f 'C++ arena: wall=%e user=%U sys=%S rss_kib=%M' \
    target/bench/bt_arena 21 >/dev/null
  /usr/bin/time -f 'C malloc: wall=%e user=%U sys=%S rss_kib=%M' \
    target/bench/bt_vanilla 21 >/dev/null
done
```

Verify equivalent output. The vanilla C program has one extra leading line:

```bash
target/binarytrees > /tmp/bt-solar-threaded
target/bench/bt_arena 21 > /tmp/bt-cpp-arena
diff -u /tmp/bt-solar-threaded /tmp/bt-cpp-arena

target/binarytrees_st > /tmp/bt-solar-single
target/bench/bt_vanilla 21 | tail -n +2 > /tmp/bt-c-malloc
diff -u /tmp/bt-solar-single /tmp/bt-c-malloc
```

## Measurement definitions

### Allocation and GC

- Throughput wall time is the median of the requested rounds.
- Peak RSS is the largest `/proc/<pid>/status` `VmHWM` sample across rounds.
- Solar pause samples are its three individual stop-the-world phases.
- Go samples are sweep-termination and mark-termination pauses from
  `GODEBUG=gctrace=1`.
- Java samples are individual safepoints from `-Xlog:safepoint`.
- .NET samples are runtime
  `GCSuspendEEBegin`-to-`GCRestartEEEnd` windows captured by
  `bench/csharp/GcPause.cs`.
- Node.js samples are main-JavaScript-thread pauses from `--trace-gc`. Each
  worker has an independent isolate, so summing isolate pauses can exceed
  process wall time.
- The pause maximum and p50 are calculated per run, then the median of those
  per-run values is reported.
- The reported stop-the-world percentage is the median across rounds of
  `sum(pause samples) / traced wall time`.

### Other groups

- The C allocator and sieve harnesses report the median wall time and median
  `wait4` peak RSS across interleaved rounds.
- The HashMap harness reports the best wall time and largest `wait4` peak RSS
  across seven runs of each isolated phase.
- The binary-trees table in `README.md` uses the median wall time, median
  `user + system` CPU time, and median peak RSS from three rounds.
