# Solar vs Java vs C vs Go — allocation & GC throughput / latency

Head-to-head ports of `examples/allocs3.solar` and `examples/threads_list2.solar`
to Java (`bench/java/`, five JVM collectors), C (`bench/c/`, manual
`malloc`/`free`), and Go (`bench/go/`, its concurrent GC). The Solar sources use
a nullable reference field `next: &?Node` (`null#[Node]` for the empty case); the
Java port maps a `Node`/`null` reference onto it, the C port a `Node*`/`NULL`
pointer, and the Go port a `*Node`/`nil` pointer — so a single nullable field
models both the empty case and Solar's `&` indirection.

> **Measurement conditions.** All numbers below come from one **interleaved**
> session on a 24-core / 93 GB machine, produced by `bench/bench.py` (3 rounds).
> Interleaved means each round runs every language once before the next round
> begins, so background-load drift is spread evenly across contenders instead of
> penalizing whichever ran last; only one process runs at a time. Load average
> climbed ~6 → 21 across this session (that EWMA is the *trailing* average of the
> prior 16-thread runs), and the STW collectors' threaded numbers are
> **load-sensitive** (Solar stops all mutators at each phase; Go/G1/Parallel have
> STW phases too), so their `threads` worst-case pauses are noisy — the latency
> table reports the **median over the 3 rounds** of each run's max/p50, and the
> p50 column is the more stable signal. An earlier `threads` run under heavy load
> ~9–29 measured Solar at ~8 s with an 86 ms worst pause. Prefer an idle box.
> Java/Go use `-Xmx8g` / Go defaults; C is native.

## Directory layout

```
bench/
  java/      Allocs3.java, ThreadsList2.java   (javac before running)
  c/         allocs3.c, threads_list2.c, Makefile   (make before running)
  go/        allocs3.go, threads_list2.go, go.mod   (go build before running)
  bench.py   interleaved harness — throughput (median wall + peak RSS) and
             GC-pause latency (Solar pause1+pause2; Java safepoint; Go gctrace)
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

# Go (1.24)
(cd bench/go && go build -o allocs3 allocs3.go && go build -o threads_list2 threads_list2.go)

# Java (JDK 21)
javac bench/java/Allocs3.java bench/java/ThreadsList2.java

# Full interleaved matrix (Solar + C + Go + 5 JVM collectors x 2 benchmarks):
bench/bench.py                 # throughput + latency, 3 rounds, interleaved
bench/bench.py --markdown      # also print these RESULTS.md tables
bench/bench.py --rounds 5 --only latency   # e.g. more rounds, latency only
```

The five JVM collectors are `-XX:+UseG1GC` (default), `-XX:+UseParallelGC`
(throughput STW), `-XX:+UseZGC -XX:+ZGenerational` (concurrent, generational),
`-XX:+UseZGC` alone (legacy single-generation, deprecated in JDK 21), and
`-XX:+UseShenandoahGC` (concurrent, non-generational).

Latency is measured as actual application stall: Solar = `pause1 + pause2` (its
two STW phases, from `SOLAR_PRINT_GC_STATS=1`); Java = `At safepoint` per
safepoint (from `-Xlog:safepoint`); Go = STW sweep-termination + mark-termination
per cycle (the first+third clock terms of `GODEBUG=gctrace=1`); C = none (no
collector — reclamation is inline `free`).

## What each benchmark stresses

- **allocs3** — single thread, 100M allocations, building one chain that is
  **never freed**. In Solar each node is a single 8-byte `&?Node` cell, so the
  live chain is ~800 MB; C allocates the same 8-byte node but glibc rounds it to
  a 32-byte minimum chunk (~3 GB resident); Go's 8-byte-pointer node sits in a
  small size class (~800 MB); the Java port's nodes carry object headers. A
  growing-live-set / mark-throughput test — nothing is garbage, so a copying
  collector keeps re-copying an ever-larger live set, while C never frees (pure
  `malloc` throughput, no reclamation).
- **threads_list2** — 16 threads each build a fresh 100k-node list 1000×,
  publishing the head to a shared `root`; the previous list becomes garbage
  immediately (1.6 billion total allocations). A concurrent
  **high-garbage-rate** test. Solar, Java, and Go let the GC reclaim the
  discarded lists concurrently; the C port has no collector, so each thread
  **manually `free`s** the list it built the previous iteration. (Java workers
  are daemon threads, and the C/Go/Solar `main` returns on first-worker-done, so
  the process exits when the first finishes, abandoning the other 15.)

## Throughput & peak memory (lower is better)

Wall-clock is the median of 3 runs; RSS is peak resident set.

| runtime          | allocs3 wall | threads wall | allocs3 RSS | threads RSS |
|------------------|-------------:|-------------:|------------:|------------:|
| Solar            | **1.08 s**   | **1.94 s**   | **785 MB**  | 1183 MB     |
| C (malloc/free)  | 2.20 s       | 3.97 s       | 3049 MB     | 99 MB       |
| Go               | 2.25 s       | 10.92 s      | 826 MB      | **71 MB**   |
| Java G1          | 3.39 s       | 2.32 s       | 1943 MB     | 3546 MB     |
| Java Parallel    | 3.67 s       | 2.03 s       | 2340 MB     | 2775 MB     |
| Java ZGC gen     | 2.14 s       | 4.38 s       | 2350 MB     | 4117 MB     |
| Java ZGC non-gen | 2.20 s       | 5.16 s       | 3183 MB     | 9422 MB     |
| Java Shenandoah  | **1.08 s**   | 2.77 s       | 1568 MB     | 7259 MB     |

(`allocs3` is a *retained* chain, so RSS reflects allocator overhead per live
node: Solar's 8-byte cell and Go's 8-byte size class win; C pays glibc's 32-byte
minimum chunk. `threads` is *discarded* garbage, so RSS reflects reclamation
aggression: Go's pacing keeps it leanest at 71 MB and C frees inline to 99 MB,
while the JVM collectors let garbage accumulate toward `-Xmx8g` — ZGC non-gen's
multi-mapped heap even pushes RSS past the 8 GB cap.)

## GC pause latency — STW stall per cycle (ms, median of 3 runs)

| runtime          | allocs3 max | allocs3 p50 | threads max² | threads p50 |
|------------------|------------:|------------:|-------------:|------------:|
| Solar            | 2.02        | 1.28        | 12.78        | 2.60        |
| C (malloc/free)  | none        | none        | none         | none        |
| Go               | 0.08        | 0.05        | 8.31         | 0.07        |
| Java G1          | 519.9       | 246.2       | 10.73        | 4.81        |
| Java Parallel    | 1407.4      | 765.6       | 6.78         | 4.76        |
| Java ZGC gen     | 0.03        | 0.03        | 0.19         | 0.04        |
| Java ZGC non-gen | 0.03        | 0.03        | 0.14         | 0.03        |
| Java Shenandoah  | none¹       | none¹       | 0.61         | 0.06        |

¹ Shenandoah completed `allocs3` (two concurrent cycles) **without any
STW-bearing safepoint** — with `-Xmx8g` the ~1.5 GB live set never forced a mark
pause before the VM exited.
² The `threads` worst-case pauses for the STW collectors (Solar, Go, G1,
Parallel) are noisy under load — under the high load of this session (avg → 21)
Solar's per-round max ranged ~6–13, Go's ~8–10, Parallel's ~7–10. The medians
are shown; the p50 row is far more stable. ZGC and Shenandoah stay
sub-millisecond throughout.

## Fraction of wall-clock spent in STW GC

| runtime          | allocs3 | threads |
|------------------|--------:|--------:|
| Solar            | ~0.3%   | ~9%     |
| C (malloc/free)  | 0%      | 0%      |
| Go               | ~0%     | ~0%     |
| Java G1          | ~83%    | ~4%     |
| Java Parallel    | ~85%    | ~3%     |
| Java ZGC         | ~0%     | ~0.1%   |
| Java Shenandoah  | ~0%     | ~0.1%   |

(For Solar, ZGC, Shenandoah, and Go the marking work is concurrent / off the
critical path, so STW fraction is small. **Go's GC cost does not show up here**:
it is paid as concurrent *mark-assist* throttling of allocating goroutines — 3%
of GC CPU on allocs3 but 18% on threads — which is what tanks its `threads`
throughput while keeping pauses sub-millisecond. C does no marking; its
reclamation cost is inline `free`, not a pause — see takeaway 5.)

## Takeaways

1. **Monotonic growth (allocs3) splits the field cleanly.** The non-moving /
   concurrent collectors win and the copying collectors lose. Solar and
   Shenandoah **tie for fastest at 1.08 s** — both avoid evacuating a live set
   that only grows — followed by ZGC (~2.1 s), C (2.20 s), and Go (2.25 s). G1
   (3.39 s) and Parallel (3.67 s) spend ~83–85% of wall-clock **stopped**,
   repeatedly copying the growing chain; Parallel's worst single pause is
   **1.41 s**. Solar's STW stays ≤ 2.0 ms (~0.3% of wall). Notably **C `malloc`
   (2.20 s) is ~2× slower than Solar** here, and glibc's 32-byte minimum chunk
   inflates the chain to ~3 GB vs Solar's 785 MB and Go's 826 MB.

2. **High concurrent garbage (threads): Solar wins on throughput; Go collapses.**
   Solar (1.94 s) leads, ahead of Parallel (2.03 s), G1 (2.32 s), and Shenandoah
   (2.77 s), ~2× faster than ZGC (~4.4–5.2 s), and **~5.6× faster than Go
   (10.92 s)**. Go's default concurrent GC cannot keep pace with 16 goroutines
   churning 1.6 billion short-lived nodes: mutators are conscripted into
   mark-assist and throttled, so throughput craters even though its pauses stay
   tiny. The catch for Solar is contention: it stops all 16 mutators at each STW
   phase, so under heavy load its pauses and `stall_for_gc` back-pressure inflate
   fast (the same benchmark under heavy load measured ~8 s / 86 ms worst pause) —
   visible here as its threaded numbers rising with this session's load.

3. **Latency: ZGC and Shenandoah rule; Go is close.** Solar's STW pauses are
   small (allocs3 max 2.0 ms, threads p50 2.6 ms) but its tail trails the
   concurrent collectors. Go keeps pauses sub-0.1 ms at p50 on both benchmarks
   (a brief STW tail on threads under load), and ZGC/Shenandoah stay sub-0.1 ms
   everywhere — all ~100× tighter than Solar's and G1/Parallel's millisecond
   pauses. Solar trades a few-ms tail for higher throughput.

4. **Go is the latency/throughput inverse of the STW collectors.** It keeps
   pauses tiny by doing all reclamation concurrently — but on a high allocation
   rate that concurrency is *paid by the mutators* via mark-assist, so it posts
   the best `threads` memory footprint (71 MB) and near-best latency yet the
   **worst `threads` throughput**. On the single-threaded allocs3, where one
   thread can't outrun the GC, Go is mid-pack (2.25 s) with sub-0.1 ms pauses.

5. **C is fastest only when there is nothing to reclaim — and here there never
   is.** On allocs3 (never frees) C is still ~2× slower than Solar on `malloc`
   call overhead alone. On threads C must walk and `free` each previous 100k-node
   list inline on the mutator (3.97 s) — the reclamation Solar/Java/Go do
   concurrently, here serialized into the hot path. C's "GC pause" is `none` yet
   its wall-clock is mid-pack; its one unambiguous win is footprint (99 MB).

**Net:** across these two workloads, on this box, Solar's non-moving concurrent
mark-sweep is **competitive with or faster than every contender on throughput** —
beating C and Go on both, leading outright on the allocate-and-discard threads
test, and tying Shenandoah for fastest on the growing-live-set allocs3. It does
**not** match the sub-0.1 ms pause times of ZGC/Shenandoah/Go: its remaining STW
work (mutator stop + root rescan + sweep) leaves single-digit-millisecond tails
that also make its threaded throughput sensitive to machine load. The
comparison cleanly separates the three strategies: copying collectors (G1,
Parallel) choke on the growing live set; a fully-concurrent collector tuned for
latency (Go) chokes on the allocation *rate*; and moving reclamation onto the
mutator (C) or stopping the world briefly (Solar) trades latency for the best
throughput on high-churn allocation.
