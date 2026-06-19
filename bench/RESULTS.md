# Solar vs Java vs C — allocation & GC throughput / latency

Head-to-head ports of `examples/allocs3.solar` and `examples/threads_list2.solar`
to Java (`bench/java/`, five JVM collectors) and C (`bench/c/`, manual
`malloc`/`free`). The Solar sources use a nullable reference field
`next: &?Node` (`null#[Node]` for the empty case); the Java port maps a
`Node`/`null` reference onto it and the C port a `Node*`/`NULL` pointer, so a
single nullable field models both the empty case and Solar's `&` indirection.

> **Measurement conditions.** All numbers below were taken in one session on a
> 24-core / 93 GB machine. Runs are **sequential** (one process at a time), so
> the only contention per run is the benchmark itself plus light background load
> (`uptime` load average drifted 3 → 12 over the session, but that EWMA is the
> *trailing* average of the prior 16-thread runs — no two benchmarks ran
> concurrently). Solar's threaded throughput and pause times are nonetheless
> **load-sensitive** because it stops all mutators at each STW phase; under heavy
> concurrent CPU contention they inflate sharply (an earlier run of `threads`
> under load ~9–29 measured Solar at ~8 s with an 86 ms worst pause). Always
> `uptime` first and measure on an idle box. Java/C use `-Xmx8g` / native.

## Directory layout

```
bench/
  java/      Allocs3.java, ThreadsList2.java   (javac before running)
  c/         allocs3.c, threads_list2.c, Makefile   (make before running)
  run.sh     throughput harness (3 runs, median wall-clock + peak RSS)
  latency.sh GC-pause harness  (Solar pause1+pause2; Java "At safepoint")
  RESULTS.md this file
```

## How to reproduce

```bash
# Solar (native, release)
cargo build --release -p solar-system
cargo run --release --bin compile -- examples/allocs3.solar       target/allocs3
cargo run --release --bin compile -- examples/threads_list2.solar target/threads_list2

# C (manual malloc/free)
make -C bench/c

# Java (JDK 21)
javac bench/java/Allocs3.java bench/java/ThreadsList2.java

# Full matrices (Solar + C + 5 JVM collectors x 2 benchmarks):
bash bench/run.sh        # throughput
bash bench/latency.sh    # GC pause latency
```

The five JVM collectors are `-XX:+UseG1GC` (default), `-XX:+UseParallelGC`
(throughput STW), `-XX:+UseZGC -XX:+ZGenerational` (concurrent, generational),
`-XX:+UseZGC` alone (legacy single-generation, deprecated in JDK 21), and
`-XX:+UseShenandoahGC` (concurrent, non-generational).

Latency is measured as actual application stall: Solar = `pause1 + pause2` (its
two STW phases, from `SOLAR_PRINT_GC_STATS=1`); Java = `At safepoint` per
safepoint (from `-Xlog:safepoint`); C = none (no collector — reclamation is
inline `free`).

## What each benchmark stresses

- **allocs3** — single thread, 100M allocations, building one chain that is
  **never freed**. In Solar each node is a single 8-byte `&?Node` cell, so the
  live chain is ~800 MB; the C port allocates the same 8-byte node but glibc
  rounds it to a 32-byte minimum chunk (~3 GB resident); the Java port's nodes
  carry object headers. A growing-live-set / mark-throughput test — nothing is
  garbage, so a copying collector keeps re-copying an ever-larger live set, while
  C never frees (pure `malloc` throughput, no reclamation).
- **threads_list2** — 16 threads each build a fresh 100k-node list 1000×,
  publishing the head to a shared `root`; the previous list becomes garbage
  immediately (1.6 billion total allocations). A concurrent
  **high-garbage-rate** test. The C port has no collector, so each thread
  **manually `free`s** the list it built the previous iteration — the
  reclamation Solar/Java do in the GC is paid inline by `free`. (Java workers are
  daemon threads and the C/Solar `main` returns on first-worker-done, so the
  process exits when the first finishes, abandoning the other 15.)

## Throughput — wall-clock, median of 3 (lower is better)

| benchmark | Solar      | C (malloc/free) | Java G1 | Java Parallel | ZGC gen | ZGC non-gen | Shenandoah |
|-----------|-----------:|----------------:|--------:|--------------:|--------:|------------:|-----------:|
| allocs3   | 1.05 s     | 2.10 s          | 3.33 s  | 3.70 s        | 2.00 s  | 2.01 s      | **0.97 s** |
| threads   | **1.80 s** | 3.49 s          | 2.28 s  | 1.97 s        | 4.42 s  | 4.53 s      | 2.30 s     |

## Peak resident memory — max RSS, MB (lower is better)

| benchmark | Solar | C (malloc/free) | Java G1 | Java Parallel | ZGC gen | ZGC non-gen | Shenandoah |
|-----------|------:|----------------:|--------:|--------------:|--------:|------------:|-----------:|
| allocs3   | 783   | 3051            | 1943    | 2340          | 2349    | 3024        | 1567       |
| threads   | 891   | **99**          | 3543    | 2775          | 4406    | 8390        | 7244       |

(`allocs3` is a *retained* chain, so RSS reflects allocator overhead per live
node: Solar's 8-byte cell wins; C pays glibc's 32-byte minimum chunk. `threads`
is *discarded* garbage, so RSS reflects reclamation aggression: C frees inline
and stays at 99 MB; the JVM collectors let garbage accumulate toward `-Xmx8g`.)

## GC pause latency — STW stall per cycle (ms)

| benchmark / metric | Solar | C    | Java G1 | Java Parallel | ZGC gen | ZGC non-gen | Shenandoah |
|--------------------|------:|-----:|--------:|--------------:|--------:|------------:|-----------:|
| allocs3  max       | 2.20  | none | 518.3   | 1406.6        | 0.03    | 0.02        | none¹      |
| allocs3  p50       | 1.44  | none | 130.2   | 790.5         | 0.03    | 0.02        | none¹      |
| threads  max       | 6.29  | none | 11.71   | 6.31          | 0.08    | 0.07        | 0.54       |
| threads  p50       | 2.46  | none | 5.13    | 4.68          | 0.04    | 0.03        | 0.07       |

¹ Shenandoah completed `allocs3` (two concurrent cycles) **without any
STW-bearing safepoint** — with `-Xmx8g` the ~1.5 GB live set never forced a
mark pause before the VM exited. On `threads` its STW pauses are sub-millisecond
(init/final mark only).

## Fraction of wall-clock spent in STW GC

| benchmark | Solar | C  | Java G1 | Java Parallel | ZGC  | Shenandoah |
|-----------|------:|---:|--------:|--------------:|-----:|-----------:|
| allocs3   | ~0.3% | 0% | ~83%    | ~85%          | ~0%  | ~0%        |
| threads   | ~9%   | 0% | ~4%     | ~3%           | ~0.1%| ~0.1%      |

(For Solar, ZGC, and Shenandoah the marking work is concurrent / off the critical
path. C does no marking at all; its reclamation cost is inline `free`, not a
pause — see takeaway 4.)

## Takeaways

1. **Monotonic growth (allocs3) splits the field cleanly.** The non-moving /
   concurrent collectors win and the copying collectors lose. Shenandoah (0.97 s)
   and Solar (1.05 s) are fastest — both avoid evacuating a live set that only
   grows — followed by ZGC (~2.0 s). G1 (3.33 s) and Parallel (3.70 s) spend
   ~83–85% of wall-clock **stopped**, repeatedly copying the growing chain;
   Parallel's worst single pause is **1.4 s**. Solar's STW stays ≤ 2.2 ms
   (~0.3% of wall). Notably **C `malloc` (2.10 s) is 2× slower than Solar** here:
   100M individual `malloc` calls cost more than Solar's allocator, and glibc's
   32-byte minimum chunk inflates the chain to ~3 GB vs Solar's 783 MB.

2. **High concurrent garbage (threads): Solar wins on throughput.** Solar
   (1.80 s) leads, ahead of Parallel (1.97 s), G1 (2.28 s), and Shenandoah
   (2.30 s), and ~2.5× faster than ZGC (~4.4–4.5 s, which trails badly on this
   allocate-and-discard workload). The catch is contention: Solar stops all 16
   mutators at each STW phase, so when the GC threads must compete with 16 busy
   mutators for 24 cores its pauses and `stall_for_gc` back-pressure inflate fast
   (the same benchmark under heavy load measured ~8 s / 86 ms worst pause). The
   JVM collectors degrade more gracefully under load.

3. **Latency: ZGC is still the king; Shenandoah is close.** Solar's STW pauses
   are small (allocs3 max 2.2 ms, threads max 6.3 ms — comparable to
   G1/Parallel's 6–12 ms) but remain ~100× the sub-0.1 ms ZGC and Shenandoah
   deliver. Both concurrent JVM collectors keep pauses sub-millisecond on
   threads; Solar trades a few-ms tail for higher throughput, and that STW cost
   is what makes its threaded numbers degrade under machine load.

4. **C is fastest only when there is nothing to reclaim — and it never is here.**
   On allocs3 (never frees) C is still 2× slower than Solar purely on `malloc`
   call overhead. On threads C is the **slowest** contender (3.49 s): with no
   collector, each thread must walk and `free` its previous 100k-node list inline
   on the mutator, serially in the hot path — exactly the reclamation work Solar
   and the JVM collectors do concurrently off the critical path. C pays it as
   distributed mutator cost rather than a GC pause, which is why its "GC pause" is
   `none` yet its wall-clock is worst. C's one unambiguous win is **memory**:
   freeing aggressively, threads stays at 99 MB while the JVM collectors balloon
   to 3–8 GB and Solar sits at 891 MB.

**Net:** across these two workloads, on a lightly loaded box, Solar's non-moving
concurrent mark-sweep is **competitive with or faster than every contender on
throughput** — beating C on both, leading outright on the allocate-and-discard
threads test, and a hair behind only Shenandoah on the growing-live-set allocs3.
It does **not** match the sub-0.1 ms pause times of ZGC/Shenandoah: its remaining
STW work (mutator stop + root rescan + sweep) leaves single-digit-millisecond
tails that also make its threaded throughput sensitive to machine load. Manual C
`malloc`/`free` is neither the throughput nor the latency winner here — it only
wins on footprint — illustrating that for high-churn allocation a good
concurrent collector beats moving reclamation onto the mutator.
