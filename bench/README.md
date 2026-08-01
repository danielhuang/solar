# Benchmarks

## Table of contents

- [Allocation and GC](#allocation-and-gc)
- [C allocator comparison](#c-allocator-comparison)
- [Sieve](#sieve)
- [HashMap](#hashmap)
- [Binary trees](#binary-trees)
- [Build and run guide](guide.md)

The results below were measured on an Intel Core Ultra 9 275HX with 24 cores
and 93 GiB of RAM, running Linux 7.1.3. See the [benchmark guide](guide.md) for
the source layout, prerequisites, build commands, measurement definitions, and
commands used to reproduce each group.

## Allocation and GC

This group compares Solar with C, Go, Node.js, five JDK 21 collectors, and two
.NET 10 GC modes across four equivalent allocation workloads. `allocs3` retains
a 100-million-node chain; `threads_list2` has 16 workers repeatedly replace
100,000-node lists; `splay` mutates an 8,000-node tree whose nodes own allocated
payload graphs; and `allocs5` combines the retained chain with the threaded
list churn. The Node.js threaded ports use independent V8 isolates rather than
a shared heap, so their pause samples are per-isolate rather than process-wide.

### Results

`bench.py` ran three interleaved rounds. Wall time is the median and peak RSS is
the largest observed value. The run began with load average 12.14 and ended at
8.41.

#### Throughput and peak memory

Lower is better.

| Runtime | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Solar | **0.91 s** | **778 MB** | **1.98 s** | 2,498 MB | 4.76 s | 1,639 MB | **2.41 s** | 3,403 MB |
| C (`malloc`/`free`) | 2.49 s | 3,052 MB | 5.09 s | 99 MB | 8.49 s | **48 MB** | 5.81 s | **3,151 MB** |
| Go | 2.39 s | 824 MB | 11.35 s | **87 MB** | 5.69 s | 95 MB | 21.34 s | 7,341 MB |
| JavaScript (Node/V8) | 10.58 s | 3,205 MB | 3.84 s | 759 MB | 11.38 s | 358 MB | 11.99 s | 3,906 MB |
| Java G1 | 3.58 s | 1,943 MB | 2.79 s | 3,552 MB | 5.67 s | 5,245 MB | 5.72 s | 5,666 MB |
| Java Parallel | 3.97 s | 2,340 MB | 2.47 s | 2,775 MB | **3.66 s** | 2,779 MB | 6.12 s | 3,616 MB |
| Java ZGC, generational | 2.34 s | 2,350 MB | 5.43 s | 4,488 MB | 8.23 s | 7,684 MB | 17.18 s | 8,450 MB |
| Java ZGC, non-generational | 2.39 s | 3,151 MB | 5.76 s | 9,847 MB | 5.75 s | 5,443 MB | 29.36 s | 17,250 MB |
| Java Shenandoah | 1.17 s | 1,567 MB | 3.52 s | 7,372 MB | 3.95 s | 2,173 MB | 14.66 s | 8,248 MB |
| C# Workstation | 7.05 s | 2,341 MB | 75.99 s | 10,142 MB | 53.60 s | 443 MB | 71.03 s | 19,588 MB |
| C# Server | 4.24 s | 2,347 MB | 22.81 s | 450 MB | 12.74 s | 3,874 MB | 7.91 s | 4,777 MB |

#### Stop-the-world pause latency

Values are milliseconds. Each cell is the median across rounds of that run's
worst or median individual pause. `none` means no pause sample was produced;
C performs reclamation inline and has no GC pauses.

| Runtime | allocs3 max | allocs3 p50 | threads max | threads p50 | splay max | splay p50 | allocs5 max | allocs5 p50 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Solar | none | none | 3.46 | 0.24 | 36.86 | 0.03 | 2.98 | 0.17 |
| C (`malloc`/`free`) | none | none | none | none | none | none | none | none |
| Go | 0.12 | 0.03 | 18.00 | 0.05 | 0.77 | 0.02 | 0.97 | 0.03 |
| JavaScript (Node/V8) | 1,228.82 | 19.04 | 62.60 | 1.65 | 18.09 | 3.86 | 1,110.52 | 1.35 |
| Java G1 | 568.06 | 267.70 | 11.97 | 6.22 | 125.59 | 31.08 | 511.47 | 118.83 |
| Java Parallel | 1,528.74 | 847.48 | 11.00 | 5.45 | 6.66 | 5.72 | 1,435.25 | 5.95 |
| Java ZGC, generational | 0.03 | 0.03 | 0.40 | 0.05 | 0.04 | 0.02 | 0.07 | 0.03 |
| Java ZGC, non-generational | 0.02 | 0.02 | 0.08 | 0.03 | 0.03 | 0.01 | 0.05 | 0.02 |
| Java Shenandoah | 0.00 | 0.00 | 1.01 | 0.06 | 0.44 | 0.04 | 759.28 | 0.03 |
| C# Workstation | 84.41 | 28.45 | 75.72 | 24.45 | 116.28 | 30.99 | 69.92 | 20.62 |
| C# Server | 529.31 | 48.75 | 70.68 | 28.08 | 72.41 | 27.40 | 619.55 | 20.82 |

#### Time represented by stop-the-world samples

Each value is the median of `sum(pause samples) / traced wall time`. Node.js
values can exceed 100% because the numerator sums overlapping pauses from
independent worker isolates.

| Runtime | allocs3 | threads | splay | allocs5 |
| --- | ---: | ---: | ---: | ---: |
| Solar | 0.0% | 1.9% | 2.2% | 1.3% |
| C (`malloc`/`free`) | 0% | 0% | 0% | 0% |
| Go | 0.0% | 4.3% | 0.3% | 0.0% |
| JavaScript (Node/V8) | 88.6% | 171.3% | 55.9% | 106.0% |
| Java G1 | 77.9% | 4.0% | 25.7% | 52.1% |
| Java Parallel | 77.0% | 3.3% | 1.2% | 57.2% |
| Java ZGC, generational | 0.0% | 0.1% | 0.0% | 0.0% |
| Java ZGC, non-generational | 0.0% | 0.0% | 0.0% | 0.0% |
| Java Shenandoah | 0.0% | 0.1% | 0.0% | 5.8% |
| C# Workstation | 54.9% | 86.2% | 67.0% | 81.8% |
| C# Server | 54.1% | 86.1% | 47.3% | 47.8% |

### Conclusions

1. Solar had the lowest wall time on `allocs3`, `threads_list2`, and `allocs5`.
   Java Parallel had the lowest wall time on `splay`; Solar ranked third behind
   Java Parallel and Shenandoah.
2. Solar was 2.74×, 2.57×, 1.78×, and 2.41× faster than the glibc C ports on
   `allocs3`, `threads_list2`, `splay`, and `allocs5`, respectively.
3. No runtime had the lowest RSS on every workload. Solar had the lowest RSS on
   `allocs3`, Go on `threads_list2`, and C on `splay` and `allocs5`.
4. Both ZGC modes kept their measured worst pauses at or below 0.40 ms. Solar's
   measured p50 pauses stayed at or below 0.24 ms, and its worst measured pause
   was 36.86 ms on `splay`.

## C allocator comparison

This group uses Solar as the baseline and runs the unchanged C allocation
workloads with glibc, jemalloc, tcmalloc, mimalloc, and a per-thread bump
allocator selected through `LD_PRELOAD`. The bump allocator never frees, so it
measures allocation without reclamation while retaining every touched
allocation.

### Results

The C allocator rows report median wall time and median peak RSS from three
interleaved rounds. The Solar baseline comes from the three-round allocation/GC
matrix above, which ran immediately before the allocator matrix on the same
machine. Solar was not interleaved with the five C allocators. Lower is better.

| Runtime / allocator | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **Solar baseline** | 0.91 s | 778 MB | 1.98 s | 2,498 MB | **4.76 s** | 1,639 MB | 2.41 s | 3,403 MB |
| glibc | 2.13 s | 3,053 MB | 3.45 s | 97 MB | 13.18 s | 48 MB | 5.51 s | 3,149 MB |
| jemalloc | 0.68 s | 794 MB | 1.04 s | 55 MB | 5.13 s | 133 MB | 1.74 s | 845 MB |
| tcmalloc | 0.68 s | 775 MB | 63.95 s | **52 MB** | 4.80 s | 47 MB | 64.85 s | 819 MB |
| mimalloc | **0.52 s** | 766 MB | **0.76 s** | 54 MB | 32.09 s | **43 MB** | **1.31 s** | **811 MB** |
| bump, no free | 0.54 s | **764 MB** | 2.95 s | 19,089 MB | 10.83 s | 9,962 MB | 3.21 s | 19,565 MB |

### Conclusions

1. Among the C allocators, mimalloc had the lowest wall time on `allocs3`,
   `threads_list2`, and `allocs5`; tcmalloc had the lowest wall time on
   `splay`.
2. Jemalloc ranked third on `allocs3` and second on the other three workloads;
   it was the only allocator whose wall time stayed at or below 5.13 s in all
   four. Tcmalloc took 63.95 s and 64.85 s on the two threaded churn workloads,
   while mimalloc took 32.09 s on `splay`.
3. The no-free bump allocator was within 0.02 s of the fastest `allocs3` result,
   where allocations remain live in every implementation. Its peak RSS reached
   19,089 MB on `threads_list2`, 9,962 MB on `splay`, and 19,565 MB on
   `allocs5`.
4. Glibc used about four times the memory of the other reclaiming allocators on
   the retained `allocs3` chain: 3,053 MB versus 766–794 MB.
5. Against the displayed Solar baseline, every allocator except glibc was
   faster on `allocs3`; jemalloc and mimalloc were faster on `threads_list2`
   and `allocs5`. Solar had the lowest displayed wall time on `splay`, 0.04 s
   below tcmalloc, and used more memory than every reclaiming C allocator on
   `threads_list2`, `splay`, and `allocs5`.

## Sieve

This group compares equivalent Sieve of Eratosthenes implementations over
100 million entries in Solar, C, Go, Java, and C#. Each implementation
allocates one byte array, performs the same marking algorithm, and must print
the prime count `5761455`.

### Results

The table reports medians from five interleaved rounds. Every output check
passed. Lower is better.

| Runtime | Wall | Peak RSS |
| --- | ---: | ---: |
| Solar | **1.65 s** | 98 MB |
| C | 1.67 s | **97 MB** |
| Go | 1.68 s | **97 MB** |
| Java | 1.69 s | 141 MB |
| C# | 1.76 s | 131 MB |

### Conclusions

1. Solar had the lowest median wall time at 1.65 s. The complete wall-time
   range was 1.65–1.76 s, a 6.7% spread relative to Solar.
2. C and Go had the lowest peak RSS at 97 MB. Solar used 98 MB, C# used
   131 MB, and Java used 141 MB.

## HashMap

This group compares Solar's standard-library SwissTable-style `HashMap` with
Rust's `std::collections::HashMap`; both use foldhash. Each phase inserts one
million keys, performs one million successful lookups and one million misses,
and verifies a cross-implementation checksum. Primitive `u64` and `u32` keys
are measured separately from `point` and `mixed` struct keys.

### Results

The results below report the best of seven independent process runs per phase;
RSS is the largest peak reported for those runs. Lower is better.

| Phase | Solar | Rust | Solar/Rust | Solar RSS | Rust RSS | Checksum |
| --- | ---: | ---: | ---: | ---: | ---: | :---: |
| `u64` | 138.8 ms | 116.6 ms | 1.19× | 68.3 MB | 52.8 MB | match |
| `u32` | 150.1 ms | 97.8 ms | 1.54× | 68.2 MB | 53.0 MB | match |
| `point` | 219.9 ms | 131.8 ms | 1.67× | 99.9 MB | 76.8 MB | match |
| `mixed` | 202.0 ms | 137.8 ms | 1.47× | 99.8 MB | 76.9 MB | match |
| **Total** | **710.8 ms** | **484.0 ms** | **1.47×** |  |  |  |

With `SOLAR_PRINT_GC_STATS=1`, every phase performed 43 allocations. Reported
live memory was 56,623,184 bytes for each primitive phase and 106,954,832 bytes
for each struct phase.

### Conclusions

1. Rust was faster in every phase. Solar's summed best times were 1.47× Rust's,
   with per-phase ratios from 1.19× to 1.67×.
2. Solar's peak RSS was 1.29× Rust's for primitive keys and 1.30× for struct
   keys.
3. Solar's 43 allocations per phase do not scale with the three million map
   operations in each phase.
4. All four checksums matched, so both implementations produced the same lookup
   results for every measured key type.

## Binary trees

This group runs the depth-21 binary-trees allocation workload in four forms:
threaded Solar, single-mutator Solar, threaded C++ using per-worker monotonic
arenas, and single-threaded C using per-node `malloc`/`free`. The threaded
implementations use one worker for each tested depth. All four implementations
produced identical normalized output.

### Results

Each variant ran once per round for three rounds. The table reports median wall
time, median total CPU time (`user + system`), and median peak RSS. Lower is
better.

| Variant | Execution | Wall | CPU | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| C++ monotonic arena | 9 workers | **0.63 s** | **1.88 s** | **131 MB** |
| Solar, threaded | 9 workers plus GC threads | 1.65 s | 9.23 s | 1,638 MB |
| Solar, single | 1 mutator plus GC threads | **5.13 s** | **5.62 s** | 1,183 MB |
| C `malloc`/`free` | 1 thread | 13.39 s | 13.34 s | **257 MB** |

### Conclusions

1. In the threaded comparison, Solar took 2.62× the wall time, 4.91× the CPU
   time, and 12.50× the peak RSS of the C++ monotonic-arena implementation.
2. In the single-mutator comparison, Solar completed 2.61× faster and used
   2.37× less total CPU time than the C `malloc`/`free` implementation.
3. Single-mutator Solar used 4.60× the peak RSS of the C implementation.
