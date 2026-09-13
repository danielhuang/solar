# Benchmarks

Measured September 13, 2026 at Solar commit `94ddb81b`.
All Solar programs use release codegen and the separately linked release runtime.
Build and reproduction commands are in [guide.md](guide.md).

## Environment and method

| Component | Version / setting |
| --- | --- |
| CPU | Intel Core Ultra 9 275HX, 24 cores |
| Memory | 93 GiB |
| Linux | 7.1.6-arch1-1 |
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

The previous August results used EPP `power`, so changes from those results
cannot be attributed to compiler or runtime changes alone. This is a shared
machine; benchmarks ran sequentially, but background activity was not controlled.
The allocation matrix started with load averages 3.03, 3.15, 5.10 and ended
at 15.70, 14.88, 13.99. Treat close rankings cautiously given this load drift.

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
| Solar | 0.54 s | 763 MiB | 1.07 s | 2000 MiB | 4.65 s | 1301 MiB | 2.22 s | 3209 MiB |
| C (malloc/free) | 1.67 s | 3034 MiB | 2.59 s | 99 MiB | 14.47 s | 48 MiB | 6.77 s | 3151 MiB |
| Go | 2.08 s | 820 MiB | 9.94 s | 61 MiB | 5.85 s | 91 MiB | 18.67 s | 6306 MiB |
| JS (Node/V8) | 6.57 s | 3204 MiB | 2.19 s | 752 MiB | 12.34 s | 346 MiB | 10.72 s | 3901 MiB |
| Julia | 3.98 s | 1760 MiB | 6.54 s | 320 MiB | 16.99 s | 640 MiB | 31.14 s | 3115 MiB |
| Java G1 | 2.93 s | 1943 MiB | 1.95 s | 3534 MiB | 7.23 s | 5093 MiB | 5.87 s | 5661 MiB |
| Java Parallel | 3.17 s | 2339 MiB | 2.06 s | 2764 MiB | 4.88 s | 2779 MiB | 6.49 s | 3596 MiB |
| Java ZGC gen | 1.57 s | 2348 MiB | 4.18 s | 3666 MiB | 10.84 s | 7522 MiB | 17.83 s | 8437 MiB |
| Java ZGC non-gen | 1.53 s | 2921 MiB | 4.40 s | 8456 MiB | 7.16 s | 5303 MiB | 30.21 s | 16945 MiB |
| Java Shenandoah | 0.79 s | 1567 MiB | 2.93 s | 6949 MiB | 5.03 s | 2163 MiB | 17.19 s | 8243 MiB |
| C# Workstation | 4.41 s | 2339 MiB | 65.32 s | 1545 MiB | 76.28 s | 348 MiB | 79.14 s | 9750 MiB |
| C# Server | 2.71 s | 2331 MiB | 16.19 s | 367 MiB | 15.34 s | 693 MiB | 8.40 s | 4668 MiB |

### GC pause latency

| runtime | Allocs3 max | Allocs3 p50 | ThreadsList2 max | ThreadsList2 p50 | Splay max | Splay p50 | Allocs5 max | Allocs5 p50 |
|---|---|---|---|---|---|---|---|---|
| Solar | — | — | 4.01 | 0.18 | 0.63 | 0.02 | 2.46 | 0.22 |
| C (malloc/free) | none | none | none | none | none | none | none | none |
| Go | 0.02 | 0.01 | 2.80 | 0.03 | 0.78 | 0.02 | 0.63 | 0.03 |
| JS (Node/V8) | 1035.07 | 11.59 | 17.27 | 0.62 | 19.66 | 4.89 | 1312.06 | 1.26 |
| Julia | 1103.51 | 224.32 | 15.97 | 4.14 | 168.52 | 104.36 | 2022.69 | 90.25 |
| Java G1 | 473.31 | 224.26 | 11.40 | 4.96 | 138.51 | 32.16 | 543.19 | 122.95 |
| Java Parallel | 1301.83 | 710.57 | 5.78 | 4.55 | 9.19 | 7.21 | 1573.84 | 6.55 |
| Java ZGC gen | 0.02 | 0.02 | 0.08 | 0.03 | 0.04 | 0.02 | 0.07 | 0.03 |
| Java ZGC non-gen | 0.01 | 0.01 | 0.06 | 0.02 | 0.03 | 0.02 | 0.04 | 0.02 |
| Java Shenandoah | — | — | 0.61 | 0.04 | 0.37 | 0.04 | 864.93 | 0.05 |
| C# Workstation | 41.15 | 17.37 | 49.94 | 21.04 | 148.60 | 48.36 | 87.96 | 21.26 |
| C# Server | 272.10 | 26.75 | 49.49 | 12.82 | 69.38 | 31.34 | 588.11 | 21.03 |

### Time represented by GC-pause samples

| runtime | Allocs3 | ThreadsList2 | Splay | Allocs5 |
|---|---|---|---|---|
| Solar | 0.0% | 3.4% | 0.1% | 1.8% |
| C (malloc/free) | 0% | 0% | 0% | 0% |
| Go | 0.0% | 2.3% | 0.2% | 0.0% |
| JS (Node/V8) | 90.0% | 127.2% | 55.4% | 104.4% |
| Julia | 75.1% | 69.6% | 48.9% | 86.3% |
| Java G1 | 82.8% | 4.0% | 18.8% | 53.7% |
| Java Parallel | 81.9% | 3.2% | 1.2% | 54.0% |
| Java ZGC gen | 0.0% | 0.1% | 0.0% | 0.0% |
| Java ZGC non-gen | 0.0% | 0.0% | 0.0% | 0.0% |
| Java Shenandoah | 0.0% | 0.1% | 0.0% | 6.8% |
| C# Workstation | 49.8% | 85.1% | 69.7% | 79.0% |
| C# Server | 50.4% | 81.9% | 44.9% | 45.0% |

Pause values are milliseconds: each cell is the minimum across rounds of
that run's maximum or median individual pause. A dash means no sample was
recorded. Julia samples come from `GC.enable_logging(true)`. Solar reports
each stop-the-world phase separately; Go reports its two stop-the-world
phases; Java reports safepoints; .NET reports suspend/restart windows; Node.js
reports per-isolate pauses. Node.js percentages can exceed 100% because its
isolates pause independently.

| Workload | Solar wall | Julia wall | Julia / Solar |
| --- | ---: | ---: | ---: |
| `allocs3` | 0.54 s | 3.98 s | 7.41× |
| `threads_list2` | 1.07 s | 6.54 s | 6.11× |
| `splay` | 4.65 s | 16.99 s | 3.65× |
| `allocs5` | 2.22 s | 31.14 s | 14.04× |

## C allocator comparison

Three interleaved rounds of unchanged C binaries with glibc, jemalloc,
tcmalloc-minimal, mimalloc, and a no-free bump allocator selected through
`LD_PRELOAD`. C ports use Clang 23.1.2 with `-O3 -march=native`.
The bump allocator retains every touched allocation.

| allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
|-----------|-------------:|------------:|-------------:|------------:|-------------:|------------:|-------------:|------------:|
| Solar (separate run) | 0.54 s | 763 MiB | 1.07 s | 2000 MiB | 4.65 s | 1301 MiB | 2.22 s | 3209 MiB |
| glibc     |      2.52 s |    3053 MiB |      4.62 s |      98 MiB |     18.45 s |      48 MiB |      7.10 s |    3149 MiB |
| jemalloc  |      0.77 s |     793 MiB |      1.19 s |      55 MiB |      7.09 s |     108 MiB |      1.97 s |     845 MiB |
| tcmalloc  |      0.75 s |     775 MiB |     72.21 s |      50 MiB |      6.56 s |      47 MiB |     60.78 s |     819 MiB |
| mimalloc  |      0.58 s |     766 MiB |      0.88 s |      55 MiB |     45.16 s |      43 MiB |      1.35 s |     808 MiB |
| bump      |      0.57 s |     764 MiB |      2.87 s |   14902 MiB |     13.88 s |    9962 MiB |      4.04 s |   19566 MiB |

Solar's allocation results above were measured separately and were not interleaved with this allocator matrix.

## Sieve

Five interleaved rounds over 100 million entries. Every run must print
`5761455`; all output checks passed.

| runtime | wall | peak RSS |
|---------|-----:|---------:|
| Solar   | 1.82 s | 98 MiB |
| C       | 1.71 s | 96 MiB |
| Go      | 1.79 s | 97 MiB |
| Java    | 1.90 s | 140 MiB |
| C#      | 1.79 s | 130 MiB |

## Loop optimization

Three interleaved rounds of `loop2.solar`, `loop2fn5.solar`, and the C
reference. Every process runs the outer loop to one billion and prints when
`i % 10000 == 0`; every run's 100,000 output lines were checked.
The C reference uses Clang 23.1.2 with `-O3 -march=native`.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar loop2 | 0.452 | 0.430 | 10.5 |
| Solar loop2fn5 | 0.543 | 0.510 | 10.2 |
| C loop2 | 0.362 | 0.340 | 1.2 |

`loop2fn5` took about 20% longer than `loop2` in this run; the expected
release-performance equivalence was not observed.

## HashMap

Seven independent processes per key type and runtime; each inserts one million
keys, performs one million hits and one million misses. Solar uses its
standard HashMap, Rust uses std::collections::HashMap, and both use foldhash.
All reported checksums match. Total is the sum of each phase's minimum.

| phase | Solar (ms) | Rust (ms) | Solar/Rust | Solar RSS (MiB) | Rust RSS (MiB) | checksum match |
|-------|-----------:|----------:|-----------:|---------------:|--------------:|:--------------:|
| u64 | 164.1 | 137.4 | 1.19x | 70.5 | 52.6 | yes |
| u32 | 161.6 | 121.0 | 1.34x | 68.0 | 52.6 | yes |
| point | 237.7 | 164.6 | 1.44x | 99.4 | 76.4 | yes |
| mixed | 247.4 | 184.6 | 1.34x | 99.2 | 76.4 | yes |
| **total** | **810.7** | **607.5** | **1.33x** | | | |

## Binary trees

Depth 21, three interleaved rounds. All normalized outputs match.
Threaded Solar and C++ use nine workload workers; Solar also has GC threads.
The C++ reference uses per-worker monotonic arenas, while the single-threaded
C reference uses per-node malloc/free. Both references use GCC/G++ 14.2.0
with `-O3 -march=native`. CPU time is user plus system time.

| Runtime | Wall (s) | CPU (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| Solar threaded | 1.645 | 8.740 | 1614.4 |
| Solar single | 5.554 | 5.960 | 1174.2 |
| C++ arena | 0.696 | 1.950 | 131.0 |
| C malloc/free | 17.461 | 17.390 | 257.0 |
