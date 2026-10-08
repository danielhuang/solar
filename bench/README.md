# Benchmarks

All Solar programs use release codegen and the separately linked release runtime.
Build and reproduction commands are in [guide.md](guide.md).

## Environment and method

| Component | Version / setting |
| --- | --- |
| CPU | Intel Core Ultra 9 275HX, 24 cores |
| Memory | 93 GiB |
| Linux | 7.2.6-1-cachyos |
| Clang / external LLVM | 23.1.2 |
| GCC / G++ | 14.2.0 |
| Rust | 1.100.0-nightly, LLVM 23.1.1 |
| Julia | 1.13.0 |
| Go | 1.24.4 |
| Node.js | 20.19.2 |
| Allocation/GC Java | OpenJDK 21.0.11 |
| Sieve Java | OpenJDK 25.0.3 |
| .NET SDK / runtime | 10.0.301 / 10.0.9 |

All six benchmark groups were rebuilt and run sequentially in one complete
suite run. This is a shared machine; benchmarks ran without
CPU pinning, and background activity was not controlled. The suite's load
averages were 2.38, 2.23, 1.93 before and 7.83, 14.90, 14.06 after.
Treat close rankings cautiously.

Tables report independent minima of wall time and per-run peak RSS across
rounds; those minima can come from different runs. Memory values are MiB.
Process timings include startup and JIT compilation where applicable, with no
warmup. Failed processes are rejected by the harness.

## Allocation and GC

Three interleaved rounds for twelve runtime/collector configurations, with
separate throughput and traced runs. `allocs3` retains a 100-million-node chain;
`threads_list2` has 24 workers replace 100,000-node lists;
`splay` mutates an 8,000-node tree with allocated payload graphs; `allocs5`
combines the retained chain and threaded churn, keeping the chain live during
churn.

Julia uses the existing equivalent ports: one worker thread for `allocs3`
and `splay`, `--threads=auto,1` for threaded workloads (24 worker threads
plus one interactive thread), and default GC thread counts.
The threaded Julia ports share a heap and exit after the first worker finishes.
Node.js workers use independent V8 isolates, so their pauses can overlap.

### Throughput and peak memory

| runtime | Allocs3 wall | Allocs3 RSS | ThreadsList2 wall | ThreadsList2 RSS | Splay wall | Splay RSS | Allocs5 wall | Allocs5 RSS |
|---|---|---|---|---|---|---|---|---|
| Solar | 0.28 s | 750 MiB | 1.36 s | 2820 MiB | 3.65 s | 1820 MiB | 3.08 s | 23843 MiB |
| C (malloc/free) | 1.70 s | 3045 MiB | 3.00 s | 148 MiB | 7.44 s | 48 MiB | 4.90 s | 3200 MiB |
| Go | 2.01 s | 820 MiB | 11.38 s | 83 MiB | 5.47 s | 92 MiB | 74.76 s | 2439 MiB |
| JS (Node/V8) | 6.66 s | 3205 MiB | 2.75 s | 1101 MiB | 9.53 s | 346 MiB | 9.37 s | 4243 MiB |
| Julia | 3.92 s | 1762 MiB | 11.41 s | 344 MiB | 11.26 s | 649 MiB | 37.76 s | 3202 MiB |
| Java G1 | 2.98 s | 1943 MiB | 1.94 s | 2767 MiB | 4.27 s | 4953 MiB | 5.26 s | 5676 MiB |
| Java Parallel | 3.24 s | 2340 MiB | 1.91 s | 2774 MiB | 3.26 s | 2779 MiB | 5.22 s | 3859 MiB |
| Java ZGC gen | 1.59 s | 2349 MiB | 3.61 s | 3542 MiB | 7.47 s | 7328 MiB | 13.91 s | 8449 MiB |
| Java ZGC non-gen | 1.59 s | 3018 MiB | 4.29 s | 11935 MiB | 4.98 s | 5523 MiB | 33.16 s | 18667 MiB |
| Java Shenandoah | 0.82 s | 1562 MiB | 2.87 s | 6765 MiB | 3.14 s | 2165 MiB | 13.50 s | 8240 MiB |
| C# Workstation | 4.47 s | 2337 MiB | 73.01 s | 10078 MiB | 37.50 s | 211 MiB | 80.17 s | 20529 MiB |
| C# Server | 2.79 s | 2338 MiB | 13.58 s | 438 MiB | 9.20 s | 655 MiB | 6.89 s | 5140 MiB |

### GC pause latency

| runtime | Allocs3 max | Allocs3 p50 | ThreadsList2 max | ThreadsList2 p50 | Splay max | Splay p50 | Allocs5 max | Allocs5 p50 |
|---|---|---|---|---|---|---|---|---|
| Solar | — | — | 5.51 | 1.13 | 37.49 | 0.04 | 4.99 | 1.66 |
| C (malloc/free) | none | none | none | none | none | none | none | none |
| Go | 0.08 | 0.02 | 1.90 | 0.05 | 0.86 | 0.02 | 0.54 | 0.02 |
| JS (Node/V8) | 1029.06 | 11.55 | 141.79 | 1.00 | 13.80 | 3.33 | 1056.44 | 0.95 |
| Julia | 1124.48 | 225.24 | 12.52 | 5.06 | 116.89 | 68.18 | 1715.49 | 99.93 |
| Java G1 | 487.86 | 230.19 | 9.70 | 4.95 | 110.48 | 25.74 | 488.47 | 8.93 |
| Java Parallel | 1331.81 | 723.21 | 7.56 | 5.03 | 5.88 | 4.95 | 1328.25 | 5.92 |
| Java ZGC gen | 0.01 | 0.01 | 0.11 | 0.02 | 0.03 | 0.02 | 0.19 | 0.02 |
| Java ZGC non-gen | 0.01 | 0.01 | 0.06 | 0.02 | 0.03 | 0.01 | 0.05 | 0.01 |
| Java Shenandoah | — | — | 0.96 | 0.05 | 0.65 | 0.03 | 1231.97 | 0.03 |
| C# Workstation | 44.32 | 17.11 | 43.09 | 11.34 | 88.23 | 23.77 | 43.07 | 11.60 |
| C# Server | 247.66 | 25.65 | 34.06 | 10.09 | 48.27 | 15.07 | 657.66 | 10.40 |

### Time represented by GC-pause samples

| runtime | Allocs3 | ThreadsList2 | Splay | Allocs5 |
|---|---|---|---|---|
| Solar | 0.0% | 8.0% | 2.4% | 0.4% |
| C (malloc/free) | 0% | 0% | 0% | 0% |
| Go | 0.0% | 3.4% | 0.4% | 0.0% |
| JS (Node/V8) | 90.4% | 293.4% | 55.6% | 119.1% |
| Julia | 75.7% | 74.1% | 49.4% | 89.9% |
| Java G1 | 82.4% | 5.3% | 23.0% | 54.6% |
| Java Parallel | 81.3% | 5.1% | 1.1% | 59.4% |
| Java ZGC gen | 0.0% | 0.1% | 0.0% | 0.0% |
| Java ZGC non-gen | 0.0% | 0.0% | 0.0% | 0.0% |
| Java Shenandoah | 0.0% | 0.2% | 0.1% | 32.8% |
| C# Workstation | 54.5% | 82.0% | 66.0% | 75.6% |
| C# Server | 48.2% | 76.3% | 38.3% | 46.1% |

Pause values are milliseconds: each cell is the minimum across rounds of
that run's maximum or median individual pause. A dash means no sample was
recorded. Julia samples come from `GC.enable_logging(true)`. Solar reports
each stop-the-world phase separately; Go reports its two stop-the-world
phases; Java reports safepoints; .NET reports suspend/restart windows; Node.js
reports per-isolate pauses. Node.js percentages can exceed 100% because its
isolates pause independently.

| Workload | Solar wall | Julia wall | Julia / Solar |
| --- | ---: | ---: | ---: |
| `allocs3` | 0.28 s | 3.92 s | 13.81× |
| `threads_list2` | 1.36 s | 11.41 s | 8.38× |
| `splay` | 3.65 s | 11.26 s | 3.09× |
| `allocs5` | 3.08 s | 37.76 s | 12.26× |

## C allocator comparison

Three interleaved rounds of unchanged C binaries with glibc, jemalloc,
tcmalloc-minimal, mimalloc, and a no-free bump allocator selected through
`LD_PRELOAD`. C ports use Clang 23.1.2 with `-O3 -march=native`.
The bump allocator retains every touched allocation.

| allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
|-----------|-------------:|------------:|-------------:|------------:|-------------:|------------:|-------------:|------------:|
| Solar (separate run) | 0.28 s | 750 MiB | 1.36 s | 2820 MiB | 3.65 s | 1820 MiB | 3.08 s | 23843 MiB |
| glibc | 1.84 s | 3053 MiB | 4.18 s | 146 MiB | 7.23 s | 48 MiB | 5.87 s | 3198 MiB |
| jemalloc | 0.61 s | 794 MiB | 1.20 s | 81 MiB | 3.08 s | 126 MiB | 1.82 s | 871 MiB |
| tcmalloc | 0.58 s | 775 MiB | 89.96 s | 77 MiB | 2.67 s | 47 MiB | 101.61 s | 844 MiB |
| mimalloc | 0.44 s | 766 MiB | 0.90 s | 81 MiB | 14.27 s | 43 MiB | 1.37 s | 828 MiB |
| bump | 0.46 s | 764 MiB | 3.19 s | 26594 MiB | 7.43 s | 9962 MiB | 3.53 s | 28152 MiB |

Solar's allocation results above were measured separately and were not interleaved with this allocator matrix.

## Sieve

Five interleaved rounds over 100 million entries. Every run must print
`5761455`; all output checks passed.

| runtime | wall | peak RSS |
|---------|-----:|---------:|
| Solar   | 1.33 s | 98 MiB |
| C       | 1.16 s | 96 MiB |
| Go      | 1.15 s | 97 MiB |
| Java    | 1.18 s | 140 MiB |
| C#      | 1.19 s | 130 MiB |

## Loop optimization

Three interleaved rounds of `loop2.solar`, `loop2fn5.solar`, and the C
reference. Every process runs the outer loop to one billion and prints when
`i % 10000 == 0`; every run's 100,000 output lines were checked.
The C reference uses Clang 23.1.2 with `-O3 -march=native`.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar loop2 | 0.428 | 0.410 | 11.3 |
| Solar loop2fn5 | 0.436 | 0.420 | 12.6 |
| C loop2 | 0.322 | 0.310 | 1.2 |

The minimum wall time for `loop2fn5` was about 2.0% longer than for `loop2`.

## HashMap

Seven independent processes per key type and runtime; each inserts one million
keys, performs one million hits and one million misses. Solar uses its
standard HashMap, Rust uses std::collections::HashMap, and both use foldhash.
All reported checksums match. Total is the sum of each phase's minimum.

| phase | Solar (ms) | Rust (ms) | Solar/Rust | Solar RSS (MiB) | Rust RSS (MiB) | checksum match |
|-------|-----------:|----------:|-----------:|---------------:|--------------:|:--------------:|
| u64 | 96.7 | 74.5 | 1.30x | 67.9 | 52.6 | yes |
| u32 | 93.9 | 72.6 | 1.29x | 68.3 | 52.5 | yes |
| point | 140.7 | 96.8 | 1.45x | 99.8 | 76.6 | yes |
| mixed | 145.9 | 103.1 | 1.42x | 99.8 | 76.6 | yes |
| **total** | **477.2** | **347.0** | **1.38x** | | | |

## Binary trees

Depth 21, three interleaved rounds. All normalized outputs match.
Threaded Solar and C++ use nine workload workers; Solar also has GC threads.
The C++ reference uses per-worker monotonic arenas, while the single-threaded
C reference uses per-node malloc/free. Both references use GCC/G++ 14.2.0
with `-O3 -march=native`. CPU time is user plus system time.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar threaded | 1.138 | 7.110 | 1709.0 |
| Solar single | 3.482 | 3.980 | 1179.6 |
| C++ arena | 0.455 | 1.730 | 130.8 |
| C malloc/free | 8.452 | 8.320 | 257.1 |
