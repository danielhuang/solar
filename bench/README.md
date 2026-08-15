# Benchmarks

## Table of contents

- [Recorded environment](#recorded-environment)
- [Allocation and GC](#allocation-and-gc)
- [C allocator comparison](#c-allocator-comparison)
- [Sieve](#sieve)
- [HashMap](#hashmap)
- [Binary trees](#binary-trees)
- [Build and run guide](guide.md)

The results below were measured on an Intel Core Ultra 9 275HX with 24 cores
and 93 GiB of RAM, running Linux 7.1.6. See the [benchmark guide](guide.md) for
the source layout, prerequisites, build commands, measurement definitions, and
commands used to reproduce each group.

## Recorded environment

The August 15, 2026 results used:

| Component | Version |
| --- | --- |
| Operating system | Linux 7.1.6 |
| CPU | Intel Core Ultra 9 275HX, 24 cores |
| Intel P-state EPP | `power` |
| Memory | 93 GiB |
| Clang/LLVM | 22.1.6 |
| GCC/G++ | 14.2.0 |
| Rust | 1.98.0-nightly (2026-06-11) |
| Go | 1.24.4 |
| Allocation/GC Java | OpenJDK 21.0.11 |
| Sieve Java | OpenJDK 25.0.3 |
| .NET | 10.0.301 |
| Node.js | 20.19.2 |

## Allocation and GC

This group compares Solar with C, Go, Node.js, five JDK 21 collectors, and two
.NET 10 GC modes across four equivalent allocation workloads. `allocs3` retains
a 100-million-node chain; `threads_list2` has 16 workers repeatedly replace
100,000-node lists; `splay` mutates an 8,000-node tree whose nodes own allocated
payload graphs; and `allocs5` combines the retained chain with the threaded
list churn. The Node.js threaded ports use independent V8 isolates rather than
a shared heap, so their pause samples are per-isolate rather than process-wide.

### Results

`bench.py` ran three interleaved rounds. Each value is the minimum across the
rounds, including per-run peak RSS and pause summaries. The run began with load
average 4.77 and ended at 9.06.

#### Throughput and peak memory

Lower is better.

| Runtime | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Solar | **1.06 s** | **769 MB** | **1.27 s** | 2,681 MB | 7.29 s | 1,361 MB | **2.95 s** | 3,710 MB |
| C (`malloc`/`free`) | 2.97 s | 3,042 MB | 3.69 s | 99 MB | 25.83 s | **48 MB** | 7.50 s | **3,151 MB** |
| Go | 3.87 s | 822 MB | 17.90 s | **61 MB** | 17.40 s | 88 MB | 21.74 s | 7,302 MB |
| JavaScript (Node/V8) | 12.32 s | 3,204 MB | 3.20 s | 739 MB | 27.00 s | 356 MB | 18.19 s | 3,890 MB |
| Java G1 | 5.63 s | 1,943 MB | 2.11 s | 3,432 MB | 9.32 s | 4,226 MB | 9.51 s | 5,662 MB |
| Java Parallel | 5.91 s | 2,340 MB | 2.02 s | 2,770 MB | **7.16 s** | 2,778 MB | 9.05 s | 3,459 MB |
| Java ZGC, generational | 2.68 s | 2,347 MB | 4.16 s | 3,147 MB | 16.43 s | 7,441 MB | 28.01 s | 8,428 MB |
| Java ZGC, non-generational | 2.60 s | 2,916 MB | 5.32 s | 8,750 MB | 11.07 s | 5,279 MB | 43.81 s | 13,635 MB |
| Java Shenandoah | 1.43 s | 1,567 MB | 3.19 s | 7,066 MB | 7.45 s | 2,169 MB | 23.98 s | 8,382 MB |
| C# Workstation | 8.24 s | 2,335 MB | 121.29 s | 5,495 MB | 109.86 s | 385 MB | 145.37 s | 15,532 MB |
| C# Server | 5.14 s | 2,338 MB | 23.92 s | 362 MB | 28.49 s | 6,282 MB | 11.30 s | 4,723 MB |

#### Stop-the-world pause latency

Values are milliseconds. Each cell is the minimum across rounds of that run's
worst or median individual pause. `none` means no pause sample was produced;
C performs reclamation inline and has no GC pauses.

| Runtime | allocs3 max | allocs3 p50 | threads max | threads p50 | splay max | splay p50 | allocs5 max | allocs5 p50 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Solar | none | none | 2.51 | 0.17 | 0.86 | 0.05 | 1.64 | 0.21 |
| C (`malloc`/`free`) | none | none | none | none | none | none | none | none |
| Go | 0.05 | 0.03 | 4.90 | 0.07 | 0.34 | 0.05 | 1.10 | 0.05 |
| JavaScript (Node/V8) | 1,907.62 | 21.96 | 25.59 | 1.05 | 42.28 | 9.53 | 1,980.66 | 1.12 |
| Java G1 | 917.63 | 441.28 | 17.43 | 7.87 | 106.76 | 35.05 | 942.19 | 214.71 |
| Java Parallel | 2,432.89 | 1,321.32 | 10.99 | 7.06 | 13.66 | 10.21 | 2,544.10 | 8.69 |
| Java ZGC, generational | 0.04 | 0.04 | 0.10 | 0.05 | 0.09 | 0.05 | 0.09 | 0.04 |
| Java ZGC, non-generational | 0.02 | 0.02 | 0.08 | 0.04 | 0.05 | 0.03 | 0.06 | 0.04 |
| Java Shenandoah | 0.00 | 0.00 | 0.70 | 0.05 | 1.01 | 0.06 | 1,399.36 | 0.10 |
| C# Workstation | 83.92 | 32.04 | 85.62 | 40.40 | 235.50 | 75.07 | 114.21 | 41.32 |
| C# Server | 464.20 | 51.33 | 57.82 | 27.75 | 101.09 | 34.74 | 545.63 | 21.16 |

#### Time represented by stop-the-world samples

Each value is the minimum of `sum(pause samples) / traced wall time`. Node.js
values can exceed 100% because the numerator sums overlapping pauses from
independent worker isolates.

| Runtime | allocs3 | threads | splay | allocs5 |
| --- | ---: | ---: | ---: | ---: |
| Solar | 0.0% | 2.1% | 0.1% | 0.9% |
| C (`malloc`/`free`) | 0% | 0% | 0% | 0% |
| Go | 0.0% | 2.0% | 0.2% | 0.0% |
| JavaScript (Node/V8) | 90.7% | 148.8% | 55.3% | 96.5% |
| Java G1 | 82.7% | 6.0% | 16.1% | 60.6% |
| Java Parallel | 81.6% | 4.8% | 1.2% | 65.7% |
| Java ZGC, generational | 0.0% | 0.1% | 0.0% | 0.0% |
| Java ZGC, non-generational | 0.0% | 0.0% | 0.0% | 0.0% |
| Java Shenandoah | 0.0% | 0.1% | 0.1% | 21.5% |
| C# Workstation | 54.8% | 85.9% | 71.9% | 81.9% |
| C# Server | 48.2% | 82.0% | 38.4% | 41.6% |

### Conclusions

1. Solar had the lowest wall time on `allocs3`, `threads_list2`, and `allocs5`.
   Java Parallel had the lowest wall time on `splay`; Solar ranked second.
2. Solar was 2.80×, 2.91×, 3.54×, and 2.54× faster than the glibc C ports on
   `allocs3`, `threads_list2`, `splay`, and `allocs5`, respectively.
3. No runtime had the lowest RSS on every workload. Solar had the lowest RSS on
   `allocs3`, Go on `threads_list2`, and C on `splay` and `allocs5`.
4. Both ZGC modes kept their measured worst pauses at or below 0.10 ms. Solar's
   measured p50 pauses stayed at or below 0.21 ms, and its worst measured pause
   was 2.51 ms on `threads_list2`.

## C allocator comparison

This group uses Solar as the baseline and runs the unchanged C allocation
workloads with glibc, jemalloc, tcmalloc, mimalloc, and a per-thread bump
allocator selected through `LD_PRELOAD`. The bump allocator never frees, so it
measures allocation without reclamation while retaining every touched
allocation.

### Results

The C allocator rows report the minimum wall time and per-run peak RSS from
three interleaved rounds. The Solar baseline comes from the allocation/GC
matrix immediately above and was not interleaved with the five C allocators.
Lower is better.

| Runtime / allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **Solar baseline** | 1.06 s | 769 MB | 1.27 s | 2,681 MB | **7.29 s** | 1,361 MB | 2.95 s | 3,710 MB |
| glibc | 3.92 s | 3,053 MB | 4.03 s | 97 MB | 27.89 s | 48 MB | 7.79 s | 3,149 MB |
| jemalloc | 1.28 s | 794 MB | 1.41 s | 56 MB | 9.35 s | 112 MB | 2.66 s | 845 MB |
| tcmalloc | 1.19 s | 775 MB | 108.98 s | **49 MB** | 7.91 s | 47 MB | 115.54 s | 818 MB |
| mimalloc | **0.95 s** | 766 MB | **1.21 s** | 54 MB | 74.42 s | **44 MB** | **2.19 s** | **808 MB** |
| bump, no free | 0.96 s | **764 MB** | 4.08 s | 19,390 MB | 19.48 s | 9,962 MB | 4.74 s | 18,402 MB |

### Conclusions

1. Among the C allocators, mimalloc had the lowest wall time on `allocs3`,
   `threads_list2`, and `allocs5`; tcmalloc had the lowest wall time on
   `splay`.
2. Jemalloc ranked fourth on `allocs3` and second on the other three workloads;
   it was the only C allocator whose wall time stayed at or below 9.35 s in all
   four. Tcmalloc took 108.98 s and 115.54 s on the two threaded churn
   workloads, while mimalloc took 74.42 s on `splay`.
3. The no-free bump allocator was within 0.01 s of the fastest `allocs3` result,
   where allocations remain live in every implementation. Its peak RSS reached
   19,390 MB on `threads_list2`, 9,962 MB on `splay`, and 18,402 MB on
   `allocs5`.
4. Glibc used about four times the memory of the other reclaiming allocators on
   the retained `allocs3` chain: 3,053 MB versus 766–794 MB.
5. Against the displayed Solar baseline, mimalloc and the bump allocator were
   faster on `allocs3`; only mimalloc was faster on `threads_list2`; and
   jemalloc and mimalloc were faster on `allocs5`. Solar had the lowest
   displayed wall time on `splay`, 0.62 s below tcmalloc. Solar used more memory
   than every reclaiming C allocator on `threads_list2`, `splay`, and `allocs5`.

## Sieve

This group compares equivalent Sieve of Eratosthenes implementations over
100 million entries in Solar, C, Go, Java, and C#. Each implementation
allocates one byte array, performs the same marking algorithm, and must print
the prime count `5761455`.

### Results

The table reports the minimum of five interleaved rounds. Every output check
passed. Lower is better.

| Runtime | Wall | Peak RSS |
| --- | ---: | ---: |
| Solar | **2.13 s** | 98 MB |
| C | 2.16 s | **97 MB** |
| Go | 2.15 s | **97 MB** |
| Java | 2.28 s | 140 MB |
| C# | 2.27 s | 131 MB |

### Conclusions

1. Solar had the lowest wall time at 2.13 s, 0.02 s ahead of Go and 0.03 s
   ahead of C. The range was 2.13–2.28 s, a 7.0% spread relative to Solar.
2. C and Go had the lowest peak RSS at 97 MB. Solar used 98 MB, C# used
   131 MB, and Java used 140 MB.

## HashMap

This group compares Solar's standard-library SwissTable-style `HashMap` with
Rust's `std::collections::HashMap`; both use foldhash. Each phase inserts one
million keys, performs one million successful lookups and one million misses,
and verifies a cross-implementation checksum. Primitive `u64` and `u32` keys
are measured separately from `point` and `mixed` struct keys.

### Results

Both implementations ran as seven independent processes per phase. The table
reports the minimum wall time and per-run peak RSS. Lower is better.

| Phase | Solar | Rust | Solar/Rust | Solar RSS | Rust RSS | Checksum |
| --- | ---: | ---: | ---: | ---: | ---: | :---: |
| `u64` | 735.8 ms | 255.5 ms | 2.88× | 178.3 MB | 52.6 MB | match |
| `u32` | 808.2 ms | 248.1 ms | 3.26× | 178.5 MB | 52.5 MB | match |
| `point` | 922.2 ms | 277.9 ms | 3.32× | 210.0 MB | 76.6 MB | match |
| `mixed` | 905.6 ms | 297.4 ms | 3.05× | 209.8 MB | 76.6 MB | match |
| **Total** | **3,371.7 ms** | **1,078.8 ms** | **3.13×** |  |  |  |

With `SOLAR_PRINT_GC_STATS=1`, the four diagnostic runs reported 17,972,257 to
17,981,768 allocations. Reported memory use was approximately 172.4 MB for the
primitive phases and 221.7 MB for the struct phases.

### Conclusions

1. Rust was faster in every phase. Solar's summed best times were 3.13× Rust's,
   with per-phase ratios from 2.88× to 3.32×.
2. Solar's peak RSS was 3.39–3.40× Rust's for primitive keys and 2.74× for
   struct keys.
3. Solar performed approximately 18 million allocations per phase for three
   million map operations.
4. All four checksums matched, so both implementations produced the same lookup
   results for every measured key type.

## Binary trees

This group runs the depth-21 binary-trees allocation workload in four forms:
threaded Solar, single-mutator Solar, threaded C++ using per-worker monotonic
arenas, and single-threaded C using per-node `malloc`/`free`. The threaded
implementations use one worker for each tested depth. All four implementations
produced identical normalized output.

### Results

Each variant ran once per round for three rounds. The table reports minimum
wall time, minimum total CPU time (`user + system`), and minimum per-run peak
RSS. Lower is better.

| Variant | Execution | Wall | CPU | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| C++ monotonic arena | 9 workers | **1.05 s** | **3.47 s** | **131 MB** |
| Solar, threaded | 9 workers plus GC threads | 2.42 s | 13.27 s | 1,778 MB |
| Solar, single | 1 mutator plus GC threads | **8.49 s** | **9.82 s** | 1,226 MB |
| C `malloc`/`free` | 1 thread | 26.39 s | 26.29 s | **257 MB** |

### Conclusions

1. In the threaded comparison, Solar took 2.30× the wall time, 3.82× the CPU
   time, and 13.57× the peak RSS of the C++ monotonic-arena implementation.
2. In the single-mutator comparison, Solar completed 3.11× faster and used
   2.68× less total CPU time than the C `malloc`/`free` implementation.
3. Single-mutator Solar used 4.77× the peak RSS of the C implementation.
