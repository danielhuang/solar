# Benchmarks

All results measured October 2, 2026 at Solar commit `dcce65e0`.
All Solar programs use release codegen and the separately linked release runtime.
Build and reproduction commands are in [guide.md](guide.md).

## Environment and method

| Component | Version / setting |
| --- | --- |
| CPU | Intel Core Ultra 9 275HX, 24 cores |
| Memory | 93 GiB |
| Linux | 7.2.6-1-cachyos |
| Intel P-state EPP | `balance_performance` |
| Clang / external LLVM | 23.1.2 |
| GCC / G++ | 14.2.0 |
| Rust | 1.100.0-nightly (2026-09-11), LLVM 23.1.1 |
| Julia | 1.13.0 |
| Go | 1.24.4 |
| Node.js | 20.19.2 |
| Allocation/GC Java | OpenJDK 21.0.11 |
| Sieve Java | OpenJDK 25.0.3 |
| .NET SDK / runtime | 10.0.301 / 10.0.9 |

The allocation tables use the second complete allocation rerun on October 2.
Both allocation runs used the same benchmark binaries and `balance_performance`
EPP. Sieve, loops, HashMap, and binary trees were rebuilt and measured afterward
on the same kernel and EPP setting. This is a shared machine; benchmarks ran
sequentially without CPU pinning, but background activity was not controlled.
The allocation/GC matrix load averages were 1.76, 1.71, 1.73 (before) and
5.16, 7.24, 7.00 (after). The C allocator matrix started at 5.16, 7.24, 7.00
and ended at 11.75, 11.15, 9.18. The remaining benchmark groups started at 2.70, 2.10, 2.66
and ended at 3.59, 2.54, 2.77. Treat close rankings cautiously.

Tables report independent minima of wall time and per-run peak RSS across
rounds; those minima can come from different runs. Memory values are MiB.
Process timings include startup and JIT compilation where applicable, with no
warmup. Failed processes are rejected by the harness.

## Allocation and GC

Three interleaved rounds for twelve runtime/collector configurations, with
separate throughput and traced runs. `allocs3` retains a 100-million-node chain;
`threads_list2` has sixteen workers replace 100,000-node lists;
`splay` mutates an 8,000-node tree with allocated payload graphs; `allocs5`
combines the retained chain and threaded churn.

Julia uses the existing equivalent ports: one worker thread for `allocs3`
and `splay`, seventeen for threaded workloads, and default GC thread counts.
The threaded Julia ports share a heap and exit after the first worker finishes.
Node.js workers use independent V8 isolates, so their pauses can overlap.

### Throughput and peak memory

| runtime | Allocs3 wall | Allocs3 RSS | ThreadsList2 wall | ThreadsList2 RSS | Splay wall | Splay RSS | Allocs5 wall | Allocs5 RSS |
|---|---|---|---|---|---|---|---|---|
| Solar | 0.38 s | 758 MiB | 1.14 s | 4808 MiB | 3.25 s | 1616 MiB | 2.76 s | 15169 MiB |
| C (malloc/free) | 1.69 s | 3039 MiB | 2.36 s | 99 MiB | 7.83 s | 48 MiB | 5.16 s | 3151 MiB |
| Go | 1.91 s | 813 MiB | 9.21 s | 62 MiB | 5.70 s | 91 MiB | 21.69 s | 5029 MiB |
| JS (Node/V8) | 6.64 s | 3205 MiB | 2.14 s | 753 MiB | 9.99 s | 347 MiB | 10.24 s | 3903 MiB |
| Julia | 3.98 s | 1761 MiB | 6.47 s | 320 MiB | 11.71 s | 659 MiB | 29.07 s | 3126 MiB |
| Java G1 | 2.95 s | 1943 MiB | 1.43 s | 2993 MiB | 4.17 s | 4311 MiB | 5.38 s | 5662 MiB |
| Java Parallel | 3.26 s | 2339 MiB | 1.43 s | 2769 MiB | 3.30 s | 2779 MiB | 5.35 s | 3458 MiB |
| Java ZGC gen | 1.56 s | 2342 MiB | 2.83 s | 3807 MiB | 7.28 s | 7468 MiB | 14.55 s | 8433 MiB |
| Java ZGC non-gen | 1.54 s | 2982 MiB | 3.06 s | 7942 MiB | 5.28 s | 5195 MiB | 23.28 s | 14961 MiB |
| Java Shenandoah | 0.82 s | 1566 MiB | 1.60 s | 7092 MiB | 3.42 s | 2163 MiB | 14.00 s | 8225 MiB |
| C# Workstation | 4.92 s | 2330 MiB | 45.42 s | 981 MiB | 48.40 s | 251 MiB | 55.88 s | 16777 MiB |
| C# Server | 2.75 s | 2338 MiB | 8.45 s | 381 MiB | 10.44 s | 660 MiB | 6.25 s | 4696 MiB |

### GC pause latency

| runtime | Allocs3 max | Allocs3 p50 | ThreadsList2 max | ThreadsList2 p50 | Splay max | Splay p50 | Allocs5 max | Allocs5 p50 |
|---|---|---|---|---|---|---|---|---|
| Solar | — | — | 1.09 | 0.31 | 34.83 | 0.04 | 2.82 | 0.37 |
| C (malloc/free) | none | none | none | none | none | none | none | none |
| Go | 0.19 | 0.02 | 1.90 | 0.03 | 0.98 | 0.02 | 0.52 | 0.03 |
| JS (Node/V8) | 1004.24 | 11.52 | 10.49 | 0.56 | 17.80 | 3.49 | 1081.22 | 0.97 |
| Julia | 1151.59 | 219.32 | 15.89 | 3.78 | 104.47 | 74.97 | 1743.64 | 86.20 |
| Java G1 | 464.71 | 225.58 | 7.50 | 4.02 | 105.95 | 25.11 | 493.00 | 111.22 |
| Java Parallel | 1324.66 | 725.21 | 6.72 | 4.02 | 6.41 | 4.75 | 1363.12 | 5.02 |
| Java ZGC gen | 0.02 | 0.02 | 0.11 | 0.02 | 0.03 | 0.01 | 0.07 | 0.02 |
| Java ZGC non-gen | 0.01 | 0.01 | 0.04 | 0.02 | 0.02 | 0.01 | 0.05 | 0.02 |
| Java Shenandoah | — | — | 0.37 | 0.03 | 0.31 | 0.03 | 704.07 | 0.03 |
| C# Workstation | 42.55 | 17.17 | 40.96 | 14.99 | 107.55 | 27.89 | 45.21 | 16.19 |
| C# Server | 258.39 | 25.40 | 23.20 | 8.86 | 49.84 | 20.09 | 580.06 | 8.70 |

### Time represented by GC-pause samples

| runtime | Allocs3 | ThreadsList2 | Splay | Allocs5 |
|---|---|---|---|---|
| Solar | 0.0% | 1.1% | 1.8% | 0.2% |
| C (malloc/free) | 0% | 0% | 0% | 0% |
| Go | 0.0% | 1.5% | 0.4% | 0.0% |
| JS (Node/V8) | 91.0% | 122.1% | 56.0% | 103.0% |
| Julia | 75.7% | 70.8% | 49.4% | 87.8% |
| Java G1 | 83.0% | 4.5% | 26.3% | 56.2% |
| Java Parallel | 81.4% | 3.6% | 1.2% | 58.6% |
| Java ZGC gen | 0.0% | 0.1% | 0.0% | 0.0% |
| Java ZGC non-gen | 0.0% | 0.0% | 0.0% | 0.0% |
| Java Shenandoah | 0.0% | 0.1% | 0.0% | 6.2% |
| C# Workstation | 51.0% | 85.7% | 66.8% | 79.8% |
| C# Server | 46.9% | 76.9% | 41.3% | 40.7% |

Pause values are milliseconds: each cell is the minimum across rounds of
that run's maximum or median individual pause. A dash means no sample was
recorded. Julia samples come from `GC.enable_logging(true)`. Solar reports
each stop-the-world phase separately; Go reports its two stop-the-world
phases; Java reports safepoints; .NET reports suspend/restart windows; Node.js
reports per-isolate pauses. Node.js percentages can exceed 100% because its
isolates pause independently.

| Workload | Solar wall | Julia wall | Julia / Solar |
| --- | ---: | ---: | ---: |
| `allocs3` | 0.38 s | 3.98 s | 10.57× |
| `threads_list2` | 1.14 s | 6.47 s | 5.70× |
| `splay` | 3.25 s | 11.71 s | 3.60× |
| `allocs5` | 2.76 s | 29.07 s | 10.52× |

## C allocator comparison

Three interleaved rounds of unchanged C binaries with glibc, jemalloc,
tcmalloc-minimal, mimalloc, and a no-free bump allocator selected through
`LD_PRELOAD`. C ports use Clang 23.1.2 with `-O3 -march=native`.
The bump allocator retains every touched allocation.

| allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
|-----------|-------------:|------------:|-------------:|------------:|-------------:|------------:|-------------:|------------:|
| Solar (separate run) | 0.38 s | 758 MiB | 1.14 s | 4808 MiB | 3.25 s | 1616 MiB | 2.76 s | 15169 MiB |
| glibc | 1.87 s | 3053 MiB | 3.28 s | 97 MiB | 8.11 s | 48 MiB | 5.05 s | 3149 MiB |
| jemalloc | 0.60 s | 794 MiB | 1.04 s | 55 MiB | 3.13 s | 133 MiB | 1.67 s | 845 MiB |
| tcmalloc | 0.60 s | 775 MiB | 49.68 s | 50 MiB | 2.74 s | 47 MiB | 51.23 s | 818 MiB |
| mimalloc | 0.47 s | 766 MiB | 0.70 s | 55 MiB | 15.76 s | 43 MiB | 1.16 s | 810 MiB |
| bump | 0.47 s | 764 MiB | 2.25 s | 19937 MiB | 7.73 s | 9962 MiB | 2.70 s | 19819 MiB |

Solar's allocation results above were measured separately and were not interleaved with this allocator matrix.

## Sieve

Five interleaved rounds over 100 million entries. Every run must print
`5761455`; all output checks passed.

| runtime | wall | peak RSS |
|---------|-----:|---------:|
| Solar   | 1.13 s | 98 MiB |
| C       | 0.94 s | 96 MiB |
| Go      | 0.97 s | 97 MiB |
| Java    | 0.99 s | 141 MiB |
| C#      | 0.98 s | 131 MiB |

## Loop optimization

Three interleaved rounds of `loop2.solar`, `loop2fn5.solar`, and the C
reference. Every process runs the outer loop to one billion and prints when
`i % 10000 == 0`; every run's 100,000 output lines were checked.
The C reference uses Clang 23.1.2 with `-O3 -march=native`.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar loop2 | 0.395 | 0.380 | 10.5 |
| Solar loop2fn5 | 0.399 | 0.390 | 12.2 |
| C loop2 | 0.315 | 0.310 | 1.2 |

`loop2fn5` took about 0.9% longer than `loop2` in this run, within the
variation observed across rounds.

## HashMap

Seven independent processes per key type and runtime; each inserts one million
keys, performs one million hits and one million misses. Solar uses its
standard HashMap, Rust uses std::collections::HashMap, and both use foldhash.
All reported checksums match. Total is the sum of each phase's minimum.

| phase | Solar (ms) | Rust (ms) | Solar/Rust | Solar RSS (MiB) | Rust RSS (MiB) | checksum match |
|-------|-----------:|----------:|-----------:|---------------:|--------------:|:--------------:|
| u64 | 87.9 | 68.0 | 1.29x | 68.2 | 52.7 | yes |
| u32 | 93.7 | 69.9 | 1.34x | 68.1 | 52.4 | yes |
| point | 128.2 | 91.5 | 1.40x | 99.7 | 76.6 | yes |
| mixed | 141.1 | 95.9 | 1.47x | 99.8 | 76.6 | yes |
| **total** | **450.9** | **325.2** | **1.39x** | | | |

## Binary trees

Depth 21, three interleaved rounds. All normalized outputs match.
Threaded Solar and C++ use nine workload workers; Solar also has GC threads.
The C++ reference uses per-worker monotonic arenas, while the single-threaded
C reference uses per-node malloc/free. Both references use GCC/G++ 14.2.0
with `-O3 -march=native`. CPU time is user plus system time.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar threaded | 1.036 | 6.330 | 1755.0 |
| Solar single | 3.443 | 3.910 | 1176.4 |
| C++ arena | 0.419 | 1.470 | 131.0 |
| C malloc/free | 7.831 | 7.790 | 257.2 |
