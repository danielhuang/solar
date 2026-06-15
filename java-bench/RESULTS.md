# Solar vs Java — allocation & GC throughput / latency

Java ports of `examples/allocs3.solar` and `examples/threads_list2.solar`. The
Solar sources use a nullable reference field `next: &?Node` (`null#[Node]` for
the empty case); the Java port maps a `Node`/`null` reference straight onto it,
so one nullable `Node` field models both the empty case and Solar's `&`
indirection (which Java references already provide).

> **Measurement conditions.** Numbers below were taken in one session on a
> 24-core machine under *light* load (load average ~2–9 during the run). Solar's
> threaded throughput and pause times are **load-sensitive** — it stops all
> mutators at each STW phase, so under heavy CPU contention they inflate sharply
> (an earlier run of `threads` under load ~9–29 measured Solar at ~8 s with an
> 86 ms worst pause, vs ~1.5 s / 7.6 ms here). Always `uptime` first and measure
> on an idle box.

## How to reproduce

```bash
# Solar (native, release)
cargo build --release -p solar-system
cargo run --release --bin compile -- examples/allocs3.solar       target/allocs3
cargo run --release --bin compile -- examples/threads_list2.solar target/threads_list2
SOLAR_PRINT_GC_STATS=1 ./target/allocs3
SOLAR_PRINT_GC_STATS=1 ./target/threads_list2

# Java (JDK 21)
cd java-bench && javac Allocs3.java ThreadsList2.java
java -Xmx8g -XX:+UseG1GC        Allocs3        # default
java -Xmx8g -XX:+UseParallelGC  Allocs3        # throughput STW
java -Xmx8g -XX:+UseZGC -XX:+ZGenerational Allocs3   # concurrent, generational
java -Xmx8g -XX:+UseZGC                    Allocs3   # concurrent, single-generation (legacy)
# …same four for ThreadsList2
```

Latency is measured as actual application stall: Solar = `pause1 + pause2`
(its two STW phases); Java = `At safepoint` from `-Xlog:safepoint`. All Java
runs use `-Xmx8g` on a 24-core machine.

## What each benchmark stresses

- **allocs3** — single thread, 100M allocations, building one chain that is
  **never freed**. In Solar each node is a single 8-byte `&?Node` cell, so the
  live chain is ~800 MB (the Java port's nodes carry object headers, so its live
  set is larger). A growing-live-set / mark-throughput test; nothing is garbage,
  so a copying collector keeps re-copying an ever-larger live set.
- **threads_list2** — 16 threads each build a fresh 100k-node list 1000×,
  publishing the head to a shared `root`; the previous list becomes garbage
  immediately. A concurrent **high-garbage-rate** test. (Java workers are daemon
  threads so the JVM exits when the first finishes, matching Solar, which
  abandons the other threads when `main` returns.)

## Throughput — wall-clock, median of 3 (lower is better)

| benchmark   | Solar    | Java G1 | Java Parallel | ZGC gen | ZGC non-gen |
|-------------|---------:|--------:|--------------:|--------:|------------:|
| allocs3     | **~1.0 s** | ~3.0 s  | ~3.3 s      | ~1.6 s  | ~1.7 s      |
| threads     | **~1.55 s**| ~1.9 s  | ~1.8 s      | ~3.7 s  | ~4.1 s      |

## GC pause latency — STW stall per cycle (ms)

| benchmark / metric | Solar | Java G1 | Java Parallel | ZGC gen | ZGC non-gen |
|--------------------|------:|--------:|--------------:|--------:|------------:|
| allocs3  max       | 0.79  | 520.7   | 1371.4        | 0.019   | 0.012       |
| allocs3  p50       | 0.27  | 241.0   | 731.7         | 0.019   | 0.012       |
| threads  max       | 7.59  | 8.75    | 6.38          | 0.114   | 0.083       |
| threads  p50       | 2.45  | 4.83    | 4.52          | 0.037   | 0.024       |

(Non-generational ZGC is `-XX:+UseZGC` alone — the legacy single-generation
mode, deprecated in JDK 21. It is the leanest collector on the monotonic-growth
allocs3, where the young/old split is pure overhead; on threads it ties the
generational variant because the live set is tiny either way.)

## Fraction of wall-clock spent in STW GC

| benchmark | Solar | Java G1 | Java Parallel | Java ZGC |
|-----------|------:|--------:|--------------:|---------:|
| allocs3   | ~0.3% | ~85%    | ~83%          | ~0%      |
| threads   | ~8.7% | ~4%     | ~3%           | ~0.1%    |

(For Solar and ZGC the marking work is concurrent/off the critical path: Solar's
concurrent mark sums to ~0.34 s on allocs3 and ~0.46 s on threads but does not
stop the mutator.)

## Takeaways

1. **Monotonic growth (allocs3) is where Solar's design pays off most.** G1 and
   Parallel spend ~83–85% of wall-clock *stopped*, repeatedly evacuating a live
   set that only grows — Parallel's worst single pause is **1.37 s**. Solar's
   non-moving concurrent mark-sweep never copies, so STW stays ≤ 0.8 ms (~0.3%
   of wall). Solar (~1.0 s) is the fastest collector here — ~1.6× faster than
   ZGC (~1.6 s) and ~3× faster than G1/Parallel. The periodic GC trigger added
   in the `gc-trigger-opt` merge accounts for most of this: it cut allocs3 from
   ~1.6 s to ~1.0 s by collecting proactively (concurrent mark overlapping
   allocation) instead of only via the blocking back-pressure stall.

2. **High concurrent garbage (threads): now a Solar win, but load-sensitive.**
   On a lightly loaded machine Solar (~1.55 s) edges out G1 (~1.9 s) and
   Parallel (~1.8 s), and is ~2.5× faster than ZGC (~3.7–4.1 s, which trails on
   this allocate-and-discard workload). The catch is contention: Solar stops all
   16 mutators at each STW phase, so when the GC threads must compete with 16
   busy mutators for 24 cores its pauses and `stall_for_gc` back-pressure inflate
   fast — the same benchmark under heavy load measured ~8 s with an 86 ms worst
   pause. The JVM's collectors degrade far more gracefully under load.

3. **Latency: small when uncontended, still no match for ZGC.** Solar's STW
   pauses are tiny on allocs3 (max 0.79 ms) and modest on threads (max 7.6 ms,
   comparable to G1/Parallel's 6–9 ms), but both are ~100× the sub-0.1 ms ZGC
   delivers everywhere. ZGC remains the latency king; Solar trades a few-ms tail
   for higher throughput.

**Net:** on these two workloads, measured head-to-head on a lightly loaded box,
Solar's non-moving concurrent mark-sweep is competitive with or faster than
Java's best collectors on *throughput* — clearly ahead on the growing-live-set
allocs3 — while avoiding the catastrophic full-heap pauses of G1/Parallel. It
does **not** yet deliver ZGC-class pause times: its remaining STW work (mutator
stop + root rescan + sweep) leaves single-digit-millisecond tails under many
threads where ZGC stays sub-100µs, and that STW cost is what makes Solar's
threaded numbers degrade under machine load.
