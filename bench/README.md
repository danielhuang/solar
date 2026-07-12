# Solar vs Java vs C# vs C vs Go vs JavaScript — allocation & GC throughput / latency

Head-to-head ports of `examples/allocs3.solar`, `examples/threads_list2.solar`,
`examples/splay.solar`, and `examples/allocs5.solar`
to Java (`bench/java/`, five JVM collectors), C# (`bench/csharp/`, .NET workstation
and server GC), C (`bench/c/`, manual `malloc`/`free`), Go (`bench/go/`, its
concurrent GC), and JavaScript (`bench/js/`, Node.js on V8's generational
Scavenge + Mark-Compact GC). The Solar sources use nullable reference fields (`&?Node`,
`null#[Node]` for the empty case); the Java ports map a `Node`/`null` reference
onto them, the C# ports a `Node?`/`null` reference, the C ports a `Node*`/`NULL`
pointer, the Go ports a `*Node`/`nil` pointer, and the JS ports a plain object
reference/`null` — so a single nullable field
models both the empty case and Solar's `&` indirection.

> **JS port caveat.** JavaScript has no shared-heap threads: each
> `worker_threads` Worker is its own V8 *isolate* with its own heap and its own
> GC, and object references cannot cross isolates. In the two threaded
> benchmarks the shared `root` therefore becomes a per-worker variable (same
> allocation and garbage timing, no cross-thread visibility; the done-flag is a
> `SharedArrayBuffer`), so 16 independent collectors each see 1/16th of the
> churn instead of one collector seeing all of it — and in `allocs5` the
> retained chain lives in the *main* isolate while the churn runs in the worker
> isolates, so **no V8 collector ever traces the chain concurrently with the
> churn**, which is exactly the combined stress allocs5 exists to measure in
> the shared-heap runtimes. The JS pause samples are per-isolate stalls (one
> JS thread stops, the other 16 keep running), not process-wide stops like
> every other contender's — both its latency columns and its STW fraction are
> flattered accordingly on the threaded benchmarks. Node runs with
> `--max-old-space-size=8192` (the `-Xmx8g` equivalent, applied per isolate).

`splay` is a port of the V8/Dart splay-tree benchmark
([newspeaklanguage/benchmarks `Splay.java`](https://github.com/newspeaklanguage/benchmarks/blob/master/Splay.java)),
made self-contained with a deterministic RNG and a checksum. The Java/C/Go/C#/JS
ports key the tree on `java.util.Random.nextDouble()` doubles exactly as the
original (the JS port reimplements the 48-bit LCG in exact double arithmetic);
Solar has no float literals, so its port keys on the 53-bit integer
mantissa that `nextDouble` divides by 2⁵³ — the mantissa orders identically to
the double, so **all six ports execute bit-identical tree operations and print
the same checksum** (`size=8000 checksum=17940596815358358787` at the current
parameters: 5 outer runs × 5000 exercise iterations), which doubles as a
cross-language correctness check — each port also asserts that all 5 outer
runs produce the identical checksum.

> **Measurement conditions.** All numbers below come from one **interleaved**
> session on a 24-core / 93 GB machine (Intel Core Ultra 9 275HX), produced by
> `bench/bench.py --markdown` on July 10, 2026 (3 rounds; load average 2.6 at
> the start rising to ~9 by the end, all of it the session's own runs — only
> one benchmark process runs at a time). This session re-measured **all four
> columns together** — including `splay` at its resized parameters, which the
> previous revision had measured in a separate, externally-loaded session —
> and adds the JavaScript contender. Interleaved means each
> round runs every language once before the next round begins, so
> background-load drift is spread evenly across contenders instead of
> penalizing whichever ran last. The STW
> collectors' threaded numbers are **load-sensitive** (Solar stops all mutators at
> each phase; Go/G1/Parallel/.NET have STW phases too), so their `threads`
> worst-case pauses are noisy — the latency table reports the **median over the 3
> rounds** of each run's max/p50, and the p50 column is the more stable signal.
> The latency table samples **each individual STW pause** (not summed per cycle),
> so `max` is the worst single application stall and `p50` the median single
> stall. Load moves these numbers a lot: earlier sessions
> measured Solar's `threads` at 1.66 s / 4.6 ms worst pause (near-idle),
> 2.22 s / 14 ms (load 4.6 → 11.8), and ~8 s / 86 ms (heavy load ~9–29).
> Prefer an idle box. Java uses `-Xmx8g` on JDK 21.0.11 — **pinned**: the
> system JDK is now 25, where non-generational ZGC was removed (JDK 24), so
> `bench.py` runs `/usr/lib/jvm/java-21-openjdk-amd64` to keep the
> five-collector matrix. Go 1.24.4 and .NET
> 10.0.301 (`net10.0`) use their defaults (the .NET binaries select workstation
> vs server GC at run time via `DOTNET_gcServer`); JavaScript is Node.js
> 20.19.2 (V8 11.3) with `--max-old-space-size=8192` per isolate; C is native
> glibc.
>
> Solar-side context that still shapes these numbers:
>
> * **GC trigger floor**: the collector waits for **1 GiB of claimed
>   memory beyond the last traced live** (`MIN_SIZE_UNTIL_GC` in
>   `solar-system/src/gc.rs`) before firing a cycle. A benchmark whose total
>   allocation stays under that floor runs **zero GC cycles**: Solar
>   finishes `allocs3` (~800 MB, all live) without a single collection — its
>   latency cells read "none" and it becomes a pure allocator-throughput run.
>   The floor is also visible as footprint on mid-sized heaps (see the splay
>   RSS commentary below).
> * `splay` runs at the resized parameters introduced in the previous
>   revision (~12.5× the original work: `kRuns` 5000 plus **5 outer runs**
>   each rebuilding the tree from scratch; checksum `17940596815358358787`,
>   which all six ports must print and each asserts across its 5 outer runs).
> * Solar's arena has no `MADV_HUGEPAGE` hint (removed after a previous session
>   found THP `defrag=defer+madvise` page-faulting through synchronous
>   compaction on a fragmented host); it always runs on base 4 KiB pages.
>   Additionally, this session ran in a sandboxed shell with **per-process THP
>   disabled** (`THP_enabled: 0` in `/proc/self/status`, system policy
>   `madvise`), so no contender used hugepages.

## Directory layout

```
bench/
  java/      Allocs3.java, ThreadsList2.java, Splay.java, Allocs5.java, Sieve.java   (javac before running)
  csharp/    allocs3/, threads_list2/, splay/, allocs5/, sieve/, GcPause.cs   (dotnet build -c Release)
  c/         allocs3.c, threads_list2.c, splay.c, allocs5.c, sieve.c, Makefile   (make before running)
  go/        allocs3.go, threads_list2.go, splay.go, allocs5.go, sieve.go, go.mod   (go build before running)
  js/        allocs3.js, threads_list2.js, splay.js, allocs5.js   (nothing to build; needs node)
  bench.py   interleaved harness — throughput (median wall + peak RSS),
             GC-pause latency (Solar per-pause pause1/2/3; Java safepoint; Go gctrace;
             C# in-process GCSuspend→GCRestart EventListener; JS V8 --trace-gc),
             and the STW-fraction table (Σpause/wall of each traced run)
  sieve_matrix.py  interleaved harness for the non-allocation sieve section
  README.md  this file
```

## How to reproduce

```bash
# Solar (native, release)
cargo build --release -p solar-system
cargo run --release --bin compile -- examples/allocs3.solar       target/allocs3
cargo run --release --bin compile -- examples/threads_list2.solar target/threads_list2
cargo run --release --bin compile -- examples/splay.solar         target/splay
cargo run --release --bin compile -- examples/allocs5.solar       target/allocs5

# C (manual malloc/free)
make -C bench/c

# Go (1.24)
(cd bench/go && for b in allocs3 threads_list2 splay allocs5; do go build -o $b $b.go; done)

# Java (JDK 21)
javac bench/java/Allocs3.java bench/java/ThreadsList2.java bench/java/Splay.java bench/java/Allocs5.java

# C# (.NET 10; install once via dotnet-install.sh if not present)
dotnet build bench/csharp/allocs3       -c Release
dotnet build bench/csharp/threads_list2 -c Release
dotnet build bench/csharp/splay         -c Release
dotnet build bench/csharp/allocs5       -c Release
# the apphost binaries find the runtime via DOTNET_ROOT when .NET lives under
# ~/.dotnet (a non-system-registered location):
export DOTNET_ROOT="$HOME/.dotnet"

# JavaScript (Node.js): nothing to build — bench.py runs bench/js/*.js with
# `node --max-old-space-size=8192` directly.

# Full interleaved matrix (Solar + C + Go + JS + 5 JVM collectors + 2 .NET GCs x 4 benchmarks):
bench/bench.py                 # throughput + latency, 3 rounds, interleaved
bench/bench.py --markdown      # also print these README.md tables
bench/bench.py --rounds 5 --only latency   # e.g. more rounds, latency only
```

The five JVM collectors are `-XX:+UseG1GC` (default), `-XX:+UseParallelGC`
(throughput STW), `-XX:+UseZGC -XX:+ZGenerational` (concurrent, generational),
`-XX:+UseZGC` alone (legacy single-generation, deprecated in JDK 21), and
`-XX:+UseShenandoahGC` (concurrent, non-generational).

The two .NET GC flavors are **workstation** (`DOTNET_gcServer=0`, the console-app
default: one heap, one GC thread, background-concurrent gen2) and **server**
(`DOTNET_gcServer=1`: a per-core heap with a dedicated GC thread each, also
background-concurrent gen2). Both keep gen0/gen1 (ephemeral) collections
blocking; only the gen2 sweep is concurrent. The same two binaries serve both
flavors — the flag is read at run time.

Latency is measured as actual application stall, one sample per individual STW
pause: Solar = each of `pause1`/`pause2`/`pause3` (its three STW phases — the
arena sweep runs concurrently, outside the pauses, from `SOLAR_PRINT_GC_STATS=1`);
Java = `At safepoint` per safepoint (from `-Xlog:safepoint`); Go = STW
sweep-termination and mark-termination, each its own sample (the first+third clock
terms of `GODEBUG=gctrace=1`); C# = each
`GCSuspendEEBegin`→`GCRestartEEEnd` window (the EE-suspension span around every
collection, read in-process by an `EventListener` on the runtime GC EventSource —
`bench/csharp/GcPause.cs`, enabled with `BENCH_GC_TRACE=1`); JS = the
main-JS-thread pause of each `--trace-gc` line (`X / Y ms` = pause /
background time, one line per collection per isolate — main + each worker;
remember these stop one isolate, not the process); C = none (no
collector — reclamation is inline `free`).

## What each benchmark stresses

- **allocs3** — single thread, 100M allocations, building one chain that is
  **never freed**. In Solar each node is a single 8-byte `&?Node` cell, so the
  live chain is ~800 MB; C allocates the same 8-byte node but glibc rounds it to
  a 32-byte minimum chunk (~3 GB resident); Go's 8-byte-pointer node sits in a
  small size class (~800 MB); the Java and .NET ports' nodes carry object headers
  (and .NET's compacting GC keeps extra headroom live, ~2.3 GB). A
  growing-live-set / mark-throughput test — nothing is garbage, so a copying
  collector keeps re-copying an ever-larger live set, while C never frees (pure
  `malloc` throughput, no reclamation).
- **threads_list2** — 16 threads each build a fresh 100k-node list 1000×,
  publishing the head to a shared `root`; the previous list becomes garbage
  immediately (1.6 billion total allocations). A concurrent
  **high-garbage-rate** test. Solar, Java, .NET, and Go let the GC reclaim the
  discarded lists; the C port has no collector, so each thread **manually `free`s**
  the list it built the previous iteration. (Java workers are daemon threads, the
  C# workers are background threads, the JS main thread `process.exit`s on the
  first worker's done-flag, and the C/Go/Solar `main` returns on
  first-worker-done, so the process exits when the first finishes, abandoning the
  other 15.)
- **splay** — single thread; an 8000-node splay tree is continually mutated
  (**5 outer runs**, each building a fresh tree from scratch — setup +
  5000 rounds × 80 insert-then-remove modifications — with a re-seeded RNG,
  so the whole previous ~35 MB tree becomes garbage at each outer-run
  boundary), each inserted node
  carrying a freshly allocated depth-5 payload object graph (~63 objects + a
  10-element array), an equal amount becoming garbage every modification.
  Unlike the two list benchmarks, the live set is **mid-sized and stable**
  (~35 MB) while both the allocation rate and — because splaying restructures
  the tree on every operation — the **heap-pointer mutation rate** are high:
  every rotation rewires `left`/`right` fields of long-lived nodes, so a
  concurrent collector's write barrier and remark are on the critical path.
  The C port has no collector and instead **manually frees** each removed
  node's payload graph inline (and the whole tree at each outer-run boundary). This benchmark caught three real Solar bugs: a
  GC pacing feedback loop (trigger paced against float-inflated live), a
  write-barrier hole for pointer stores that LLVM had retyped as `i64`
  (missed marks → premature frees), and nondeterministic thin/fat reference
  layout from HashMap-ordered struct lowering.
- **allocs5** — the combination of the previous two: phase 1 builds the allocs3
  retained chain (100M 8-byte `Chain` cells, ~800 MB live, never freed), then
  phase 2 runs the threads_list2 churn (16 threads × 1000 × 100k-node lists,
  1.6 billion discarded allocations) **while the chain stays live**. Every GC
  cycle must now trace the large retained set concurrently with the high
  allocation rate — a **mark-throughput-under-churn** test. Collectors that
  re-mark (or re-copy) the whole live set per cycle pay for it on every one of
  the churn phase's frequent cycles; the C port pays nothing for the retained
  chain after building it (no collector ever scans it), so it degenerates to
  allocs3-then-threads_list2 run back-to-back with a ~3 GB resident footprint.

## Throughput & peak memory (lower is better)

Wall-clock is the median of 3 runs; RSS is peak resident set.

| runtime          | allocs3 wall | threads wall | splay wall | allocs5 wall | allocs3 RSS | threads RSS | splay RSS | allocs5 RSS |
|------------------|-------------:|-------------:|-----------:|-------------:|------------:|------------:|----------:|------------:|
| Solar            | **0.63 s**   | **1.48 s**   | 4.16 s     | **2.41 s**   | **774 MB**  | 2549 MB     | 3527 MB   | 4202 MB     |
| C (malloc/free)  | 1.52 s       | 3.40 s       | 6.69 s     | 5.32 s       | 3048 MB     | 99 MB       | **48 MB** | **3151 MB** |
| Go               | 2.07 s       | 10.35 s      | 5.53 s     | 20.70 s      | 827 MB      | **75 MB**   | 93 MB     | 7732 MB     |
| JS (Node/V8)     | 6.25 s       | 3.33 s       | 9.64 s     | 11.62 s      | 3205 MB     | 753 MB      | 348 MB    | 3905 MB²    |
| Java G1          | 2.81 s       | 1.78 s       | 4.76 s     | 5.96 s       | 1943 MB     | 3534 MB     | 5274 MB   | 5667 MB     |
| Java Parallel    | 3.04 s       | 1.81 s       | **3.17 s** | 5.71 s       | 2340 MB     | 2771 MB     | 2780 MB   | 3605 MB     |
| Java ZGC gen     | 1.41 s       | 3.42 s       | 7.14 s     | 14.97 s      | 2348 MB     | 4318 MB     | 7760 MB   | 8455 MB     |
| Java ZGC non-gen | 1.40 s       | 3.80 s       | 4.94 s     | 28.03 s      | 2941 MB     | 8569 MB     | 5971 MB¹  | 17792 MB¹   |
| Java Shenandoah  | 0.75 s       | 2.23 s       | 3.34 s     | 14.80 s      | 1567 MB     | 7258 MB     | 2176 MB   | 8487 MB     |
| C# Workstation   | 4.65 s       | 50.47 s      | 43.05 s    | 65.22 s      | 2340 MB     | 2579 MB     | 234 MB    | 17308 MB    |
| C# Server        | 2.63 s       | 12.31 s      | 11.54 s    | 9.45 s       | 2355 MB     | 415 MB      | 5765 MB   | 5923 MB     |

¹ ZGC's multi-mapping (the same physical page mapped at several virtual
addresses) inflates kernel-reported RSS; non-generational ZGC counts each page
up to 3×, so its real physical footprint here is roughly a third of the 17.8 GB
shown (still the largest of the field). The generational-ZGC and threads
figures carry the same caveat to a lesser degree.
² JS RSS is summed differently *by construction*: the retained chain and the
16 churn heaps are separate V8 isolates inside one process (see the port
caveat), so no single collector ever handles the combined load the number
suggests.

(`allocs3` is a *retained* chain, so RSS reflects allocator overhead per live
node: Solar's 8-byte cell and Go's 8-byte size class win; C pays glibc's 32-byte
minimum chunk; the JVM and .NET object headers land them in the 1.5–3 GB band,
and V8's per-object maps land JS at the top with C (~32 bytes/node, 3.2 GB).
`threads` is *discarded* garbage, so RSS reflects reclamation aggression: Go's
pacing keeps it leanest at 75 MB, C frees inline to 99 MB, with **.NET server GC
third-leanest at 415 MB** — while the JVM collectors let garbage accumulate
toward `-Xmx8g` (ZGC non-gen fills most of the cap) and .NET workstation GC,
choking on 16-thread contention, bloats to ~2.6 GB. JS's 753 MB is 16 small
isolate heaps whose scavengers each reclaim their own churn promptly. Solar
sits at 2549 MB — the
**concurrent sweep** allocates above the high-water mark during the sweep window
and defers hole reuse to the next pause, so its peak footprint grows with sweep
duration and machine load.
`splay`'s live set is only ~35 MB, so RSS is pure collector headroom: C stays
at 48 MB (inline frees), Go at 93 MB (tight pacing), C# workstation at 234 MB
and JS at 348 MB (both collect eagerly and pay for it in wall-clock), while
the `-Xmx8g` JVMs let churn accumulate (G1 5.3 GB, ZGC 6.0–7.8 GB;
Shenandoah/Parallel 2.2–2.8 GB) and C# server balloons to 5.8 GB. Solar's
3527 MB is the 1 GiB trigger floor made visible — it accumulates ~1 GiB of
churn between cycles by design, plus sweep-window float that grows with load
(the previous, quieter session measured 1704 MB on the same workload).
`allocs5` RSS = the ~0.8–3 GB retained chain (per the runtime's node overhead)
*plus* however much churn garbage the collector lets accumulate while it is
busy tracing that chain — which is exactly what it stresses: C stays at its
chain-only 3151 MB, Solar holds 4.2 GB (chain + sweep-window float), the
generational STW collectors sit at 3.6–5.9 GB, and the concurrent collectors
that fall behind balloon: Go 7.7 GB (its pacer overshoots against the marking
backlog), ZGC gen 8.5 GB, Shenandoah 8.5 GB, ZGC non-gen ~18 GB reported /
~6 GB physical¹, and C# workstation 17 GB. JS's 3.9 GB² is mostly the chain
isolate.)

## GC pause latency — worst / median single STW pause (ms, median of 3 runs)

One sample per individual STW pause (Solar's three phases, Java's safepoints,
Go's two STW terms — not summed per cycle), so `max` is the worst single
application stall and `p50` the median single stall.

| runtime          | allocs3 max | allocs3 p50 | threads max² | threads p50 | splay max⁴ | splay p50 | allocs5 max | allocs5 p50 |
|------------------|------------:|------------:|-------------:|------------:|-----------:|----------:|------------:|------------:|
| Solar            | none¹       | none¹       | 1.70         | 0.13        | 32.91⁴     | 0.02      | 14.45²      | 0.19        |
| C (malloc/free)  | none        | none        | none         | none        | none       | none      | none        | none        |
| Go               | 0.14        | 0.01        | 3.30         | 0.03        | 0.65       | 0.01      | 4.70        | 0.03        |
| JS (Node/V8)     | 1031.03⁵    | 10.77       | 15.69⁵       | 1.03        | 16.55      | 3.38      | 1124.75⁵    | 1.34        |
| Java G1          | 460.86      | 224.73      | 8.82         | 4.53        | 118.03     | 28.42     | 513.49      | 116.22      |
| Java Parallel    | 1271.40     | 692.32      | 5.50         | 4.35        | 6.16       | 4.92      | 1397.50     | 6.21        |
| Java ZGC gen     | 0.02        | 0.02        | 0.10         | 0.03        | 0.03       | 0.01      | 0.08        | 0.03        |
| Java ZGC non-gen | 0.01        | 0.01        | 0.07         | 0.02        | 0.02       | 0.01      | 0.05        | 0.02        |
| Java Shenandoah  | none¹       | none¹       | 0.35         | 0.04        | 0.73       | 0.03      | 761.08³     | 0.02        |
| C# Workstation   | 38.62       | 16.97       | 41.34        | 16.48       | 113.78     | 26.44     | 72.30       | 18.20       |
| C# Server        | 308.52      | 25.85       | 40.31        | 12.52       | 73.20      | 20.81     | 676.33      | 33.17       |

¹ Completed the benchmark **without a single collection**: Shenandoah's
`-Xmx8g` headroom means `allocs3` never forces a mark pause before the VM
exits, and Solar's 1 GiB trigger floor (`MIN_SIZE_UNTIL_GC`) means its
collector never fires on it — zero cycles, zero pauses (see the
measurement-conditions note). At splay's original size both also ran it
pause-free; the resized splay collects on every runtime.
² The `threads`/`allocs5` worst-case pauses for the STW collectors (Solar, Go,
G1, Parallel, both .NET flavors) are noisy under load: a single scheduling
spike during a 16-mutator-stop handshake produces multi-millisecond maxes
(previous sessions measured Solar's `threads` max anywhere from 1.5 to 14 ms,
and its `allocs5` max at 2.2 ms vs this session's 14.45 ms under rising load).
The medians over rounds are shown; the **p50 row is far more stable** — on a
per-pause basis Solar's p50 single stall is **0.13–0.19 ms** on the threaded
benchmarks (its three phases are each small in the median; the big one, the
pause-2 remark, is only one of three). ZGC stays sub-millisecond throughout.
**.NET's pauses are neither**: every
ephemeral (gen0/gen1) collection is blocking, and at this allocation rate there
are thousands of them, so even at p50 the stalls are tens of milliseconds (C#
server's 309 ms `allocs3` max is one blocking compacting gen2). On `allocs5`
the retained chain is the discriminator: Solar's worst single stall stays in
the low milliseconds (pauses scan only roots — stacks and registers — never the
heap, so the 100M-node chain never enters a pause), ZGC stays at 0.05–0.08 ms,
while G1 stalls 116 ms *at the median* (its pauses evacuate chain regions),
Parallel spikes to a 1.40 s full-GC max, and C# server to 676 ms.
³ Shenandoah's `allocs5` max is a **degenerated collection**: the 16-thread
allocation rate outruns its concurrent mark of the 800 MB chain, and the cycle
falls back to stop-the-world — its p50 stays 0.02 ms, but the failure mode
costs 761 ms when it hits.
⁴ Solar's `splay` max is a **pause-2 remark tail**: a few cycles per run reach
the remark with a large gray backlog (the single splaying mutator's write
barrier re-shades tree nodes faster than that cycle's concurrent mark drained
them) and pay a 15–120 ms STW drain-to-fixpoint — though the p50 stays
0.02 ms and most cycles' remarks are ~20 µs. A real tail in the current design
(the remark's drain is unbounded), and the one place the resized splay hurts
Solar's profile.
⁵ JS pauses are **per-isolate** (one JS thread stops; the other 16 keep
running), so on the threaded benchmarks they are not process-wide stalls like
every other row's. The 1.0–1.1 s maxes on `allocs3`/`allocs5` are real,
though: full-heap Mark-Compacts of the growing 100M-node chain in the one
isolate that owns it, exactly the copying-collector failure mode G1/Parallel
show — V8 just pays it later and bigger.

## Fraction of wall-clock spent in STW GC

| runtime          | allocs3 | threads | splay | allocs5 |
|------------------|--------:|--------:|------:|--------:|
| Solar            | 0%      | 1.4%    | 1.8%  | 1.8%    |
| C (malloc/free)  | 0%      | 0%      | 0%    | 0%      |
| Go               | ~0%     | 1.8%    | 0.3%  | ~0%     |
| JS (Node/V8)     | 92%     | 140%¹   | 55%   | 105%¹   |
| Java G1          | 85%     | 4.3%    | 27%   | 53%     |
| Java Parallel    | 83%     | 3.5%    | 1.2%  | 57%     |
| Java ZGC gen     | ~0%     | 0.1%    | ~0%   | ~0%     |
| Java ZGC non-gen | ~0%     | ~0%     | ~0%   | ~0%     |
| Java Shenandoah  | 0%      | 0.1%    | 0.1%  | 5.9%    |
| C# Workstation   | 50%     | 86%     | 67%   | 82%     |
| C# Server        | 54%     | 80%     | 48%   | 60%     |

¹ JS sums per-isolate pauses across 17 independently-collecting isolates, so
it can exceed 100% of wall-clock — it is aggregate thread-stall time, not a
process-wide stop fraction like the other rows.

(Each cell is summed STW pause time over the traced run's wall-clock, median
of the 3 traced rounds — `bench.py` now computes this alongside the latency
table from the same traces (`SOLAR_PRINT_GC_STATS`, `-Xlog:safepoint`,
`gctrace`, the C# EventListener, `--trace-gc`). Treat them as one-or-two
significant figures — the tracing itself and background load inflate the
denominators a little. The exact 0%s are the zero-collection runs (footnote ¹
of the latency table). For Solar, ZGC, Shenandoah, and Go the
marking work is concurrent / off the critical path, so STW fraction is small.
**Go's GC cost does not show up here**:
it is paid as concurrent *mark-assist* throttling of allocating goroutines,
which is what tanks its `threads` and `allocs5` throughput while keeping
pauses sub-millisecond. C does no marking; its
reclamation cost is inline `free`, not a pause — see takeaway 7. **.NET sits at
the opposite extreme from Go**: only its gen2 sweep is concurrent, so the constant
blocking gen0/gen1 collections at this allocation rate put it in the G1/Parallel
"mostly stopped" camp on every benchmark — summed across
thousands of short blocking pauses rather than a few long ones. **JS joins
that camp**: V8's scavenges and mark-compacts are all main-thread-blocking
(only some marking/sweeping is incremental/background), so each isolate spends
half-to-most of its time stopped on every benchmark here. `allocs5` drags
G1/Parallel back up to ~55% (their pauses move or scan the retained chain) and
puts Shenandoah at ~6% (degenerated cycles, footnote ³); Solar's 1.8% is the
sum of many small 16-thread stop handshakes, not a few big stalls.)

## Takeaways

1. **Monotonic growth (allocs3) splits the field cleanly.** The non-moving /
   concurrent collectors win and the copying / compacting collectors lose.
   **Solar is fastest outright (0.63 s)** — with the 1 GiB trigger floor the
   ~800 MB chain never even starts a collection, so this is pure allocator
   throughput — followed by Shenandoah (0.75 s, which also never collects it),
   ZGC (1.40–1.41 s), C (1.52 s), and Go (2.07 s).
   The compactors trail: C# server (2.63 s), G1 (2.81 s), and Parallel (3.04 s)
   spend ~50–85% of wall-clock **stopped** moving the growing chain (Parallel's
   worst single pause is **1.27 s**, C# server's **0.31 s** for one blocking gen2
   compaction), C# workstation manages 4.65 s — and **JS is the slowest of the
   field at 6.25 s**, spending ~92% of the run stopped in ever-larger
   Mark-Compacts of the growing chain (worst single stall **1.03 s**, p50
   11 ms) while V8's per-object maps inflate the chain to 3.2 GB, C-chunk
   territory. Notably **C `malloc` (1.52 s)
   is ~2.4× slower than Solar** here, and glibc's 32-byte minimum chunk inflates
   the chain to ~3 GB vs Solar's 774 MB and Go's 827 MB.

2. **High concurrent garbage (threads): Solar wins on throughput; Go and .NET
   collapse.** Solar (1.48 s) leads, ahead of G1 (1.78 s), Parallel (1.81 s),
   and Shenandoah (2.23 s), then JS (3.33 s), C (3.40 s), and ZGC (3.4–3.8 s),
   **~7×
   faster than Go
   (10.35 s)** and **~34× faster than C# workstation (50.47 s)**. Two runtimes
   collapse for different reasons. Go's concurrent GC cannot keep pace with 16
   goroutines churning 1.6 billion short-lived nodes: mutators are conscripted
   into mark-assist and throttled, so throughput craters even though its pauses
   stay tiny. **.NET workstation GC is far worse** — its *single* GC heap
   serializes 16 allocating threads, so it spends ~86% of wall stopped in
   back-to-back ephemeral collections. **Server GC fixes most of that** (12.3 s,
   a per-core heap each) and reclaims promptly enough to post the third-leanest
   footprint of the field (**415 MB**, behind only Go's 75 MB and C's 99 MB),
   but it is
   still slower than Go. **JS's respectable 3.33 s / 753 MB comes with an
   asterisk**: the isolate model shards the benchmark into 16 independent
   single-threaded churn heaps (see the port caveat), each of which V8's
   generational scavenger handles well — it never faces the one-collector,
   16-mutator contention that breaks .NET workstation. The catch for Solar is
   contention: it stops all 16
   mutators at each STW phase, so under load its pauses and `stall_for_gc`
   back-pressure inflate (this session measured a 1.70 ms worst
   pause; earlier sessions under load measured 2.22 s wall / 14 ms and
   ~8 s / 86 ms).

3. **Pointer churn (splay, resized): Solar is in the leaders' pack, and
   the remark tail is its price.** At the original size the 1 GiB trigger
   floor made Solar run splay collection-free — zero pauses, but a
   bump-allocator working set (1102 MB) that cost it the win it held before
   the floor existed (0.72 s vs Parallel's 0.43 s; pre-floor Solar measured
   0.38 s *with* the collector running). The resized splay (~12.5× work, 5
   fresh-tree outer runs) pushes every runtime past its trigger: Solar
   collects again and places third at 4.16 s, behind
   Parallel (3.17 s) and Shenandoah (3.34 s), ahead of G1 (4.76 s), ZGC
   non-gen (4.94 s), Go (5.53 s), C (6.69 s — its serial pointer-chasing
   `free` of every payload
   graph loses to concurrent reclamation), and ZGC gen (7.14 s). JS runs
   V8's own home benchmark at 9.64 s with 3.4 ms *median* stalls and ~55% of
   wall stopped; the .NET GCs trail badly (11.5 s
   server, 43.1 s workstation, with 21–26 ms median stalls). Splay is the
   write-barrier stress test — every splay rewires long-lived pointers
   mid-mark, and all six ports assert the same checksum across all 5 outer
   runs — and it exposes Solar's one latency weak spot: a few remarks
   per run reach pause 2 with a big gray backlog and stall 15–120 ms
   (footnote ⁴), even though the p50 pause stays 0.02 ms.

4. **Retained live set + churn (allocs5) is the discriminator this suite was
   missing — Solar wins it outright and by the largest margin.** Solar
   (2.41 s) is ~2.2× faster than C (5.32 s) and the next GC runtime
   (Parallel/G1 at 5.71/5.96 s). Every collector
   now has to deal with the ~800 MB chain *while* 16 threads churn 1.6 billion
   short-lived nodes: (a) **Solar's pauses don't see the chain at all** — its
   STW phases scan only roots (stacks + registers) and the marking of the
   100M-node chain runs concurrently on the worker pool, so allocs5 costs it
   almost exactly the sum of its allocs3 + threads walls (0.63 + 1.48 ≈ 2.41 s
   measured) and its pause profile stays root-only (p50 0.19 ms; max
   14.45 ms, a stop-handshake scheduling tail under this session's load —
   footnote ²). (b) **The fully-concurrent collectors collapse**: each cycle
   must re-trace the whole chain while allocation keeps pace-triggering more
   cycles — Go goes from 10.4 s (threads) to **20.7 s** with RSS exploding from
   75 MB to 7.7 GB as its pacer falls behind; ZGC gen lands at 15.0 s, ZGC
   non-gen at 28.0 s; and **Shenandoah (14.8 s) degenerates outright**,
   repeatedly cancelling concurrent cycles on allocation failure and falling
   back to stop-the-world marks (761 ms median-of-round max) — the
   exact failure mode Solar's design avoids by keeping mutators un-throttled
   and pauses root-only. (c) **The generational STW collectors do what
   generations are for**: G1/Parallel young collections reclaim the churn
   without re-copying the old chain, so their walls (5.7–6.0 s) are just their
   parts summed — but the chain still leaks into their pauses (G1's p50 jumps
   to **116 ms**; Parallel's max is a 1.40 s full GC). (d) C (5.32 s) also
   pays nothing ongoing for the retained chain — but it paid up front in
   glibc's 32-byte chunks (3.1 GB resident) and still does all reclamation
   inline. (e) C# server, oddly, runs allocs5 (9.45 s) *faster* than threads
   alone (12.3 s) — the 2.3 GB chain forces the heap and its ephemeral budgets
   up early, cutting its blocking-collection fraction (80% → 60%);
   workstation stays pathological (65.2 s, 17 GB). (f) **JS (11.6 s) never
   actually faces the combined stress** — the chain and the churn live in
   disjoint isolates (port caveat) — and still runs 4.8× slower than Solar,
   with 1.1 s worst stalls whenever the chain isolate mark-compacts.

5. **Latency: ZGC rules; Solar matches it in the median but shows two
   distinct tails; Shenandoah's concurrency has a cliff; .NET and JS are not
   in the race.** Per individual pause Solar's stalls are small in the median
   on every benchmark (p50 0.02–0.19 ms; allocs3 literally zero pauses) and
   its 16-thread stop handshake leaves a low-millisecond, load-sensitive tail
   (threads max 1.70 ms; allocs5 max 14.45 ms this session under rising load,
   2.2 ms in the previous quieter one) — and the resized splay exposes the
   second, bigger tail: the occasional 15–120 ms pause-2 remark drain
   (footnote ⁴), which puts its splay max (33 ms median-of-rounds) above Go's
   0.65 ms.
   ZGC stays sub-0.1 ms everywhere — paying for that purity with a
   6–12× throughput loss vs Solar on allocs5 and ~1.2–1.7× on splay.
   Shenandoah is sub-0.1 ms until the load exceeds what its concurrent cycle
   can absorb, then **degenerates to 761 ms stalls** (footnote ³). **.NET
   sits at the other end with G1/Parallel**: its always-blocking ephemeral GCs
   give p50 stalls of ~13–33 ms on every benchmark, and blocking gen2
   compactions spike C# server to 309 ms (allocs3), 676 ms (allocs5), and
   73 ms (splay). **JS's medians are 1–11 ms** (per-isolate, footnote ⁵) and
   its maxes are the field's worst after Parallel: 1.0–1.1 s full-heap
   Mark-Compacts on the two chain benchmarks.

6. **Go is the latency/throughput inverse of the STW collectors.** It keeps
   pauses tiny by doing all reclamation concurrently — but on a high allocation
   rate that concurrency is *paid by the mutators* via mark-assist, so it posts
   the leanest `threads` footprint of the field (75 MB) and near-best latency
   yet a **very
   poor `threads` throughput** (10.35 s — though .NET holds the throughput
   floor), and adding the retained chain doubles that (20.7 s
   allocs5) while its footprint discipline breaks down entirely (7.7 GB). On
   the single-threaded benchmarks, where one thread can't outrun the
   GC, Go is competitive (2.07 s allocs3; 5.53 s on the resized splay, still
   at a 93 MB footprint) with sub-millisecond pauses throughout.

7. **C is fastest only when there is nothing to reclaim — and on these
   workloads there always is.** On allocs3 (never frees) C is still ~2.4× slower
   than Solar on `malloc` call overhead alone. On threads (3.40 s), allocs5
   (5.32 s), and the resized splay (6.69 s) C must `free` inline on the
   mutator — the
   reclamation Solar/Java/Go/.NET/V8 do concurrently or in bulk, serialized into
   the hot path; on splay that means walking and freeing every removed node's
   63-object payload graph plus the whole tree at each outer-run boundary,
   which loses to Solar (4.16 s), Parallel, Shenandoah, G1, ZGC non-gen, and
   Go. C's "GC pause" is `none` yet its wall-clock is
   mid-pack; its unambiguous win is footprint (99 MB threads / 48 MB splay).

8. **JS/V8 is built for a different shape of program, and these benchmarks
   show exactly which.** V8's generational scavenger is excellent when
   garbage dies young in a modest heap — `threads` (3.33 s / 753 MB, helped
   by the isolate model sharding the churn 16 ways) beats C, Go, ZGC, and
   both .NET flavors. But everything here that involves a *large or
   long-lived* heap hits its blocking Mark-Compact: allocs3 is the field's
   slowest (6.25 s, 92% stopped, 1 s single stalls on a growing live set the
   collector keeps re-marking), splay — V8's own benchmark, at 12.5× the
   original size — runs 2.3× slower than Solar with 3.4 ms median stalls,
   and allocs5 posts 11.6 s with 1.1 s stalls *despite* never facing the
   combined chain+churn stress (port caveat). Its STW fractions (55–140% of
   wall in aggregate thread-stall) put it in the .NET "mostly stopped" camp
   on every benchmark, hidden only by the isolate sharding.

**Net:** across these four workloads, on this box, Solar's non-moving
concurrent mark-sweep is **the outright throughput leader on three of four
benchmarks** — allocs3, threads, and (by the widest margin, ~2.2×) the
combined-stress allocs5 — beating C, Go, JS, and both .NET GCs on all four
(including the resized splay, where it places third behind Parallel and
Shenandoah). Its pause profile sits in the ZGC class in the
median (~0.02–0.2 ms) with two distinct tails: the low-millisecond (of order
1–15 ms, load-sensitive) 16-thread stop handshake, and the splay-exposed
15–120 ms pause-2 remark
drain — the current design's one unbounded pause, and the clearest next
target. allocs5 is the separator: it shows what happens when a large
live set and a high allocation rate arrive together — fully-concurrent
collectors (Go, ZGC, Shenandoah) fall behind or degenerate, blocking
generational collectors (G1, Parallel, .NET) protect throughput but leak the
live set into 100 ms–1.4 s pauses, and Solar's root-only pauses + concurrent
parallel mark keep *both* its throughput additive and its stalls in the low
milliseconds.
The 1 GiB trigger floor remains visible as footprint: Solar carries
~1–3 GB of accumulated churn on mid-sized heaps (splay 3527 MB against a
~35 MB live set, up from 1704 MB in a quieter session — sweep-window float
grows with load) — a floor keyed to (or capped by) working-set growth would
reclaim sooner there.

(The "beating C everywhere" above is specifically vs the C ports' **glibc**
`malloc`/`free`. Swapping in a modern allocator changes the picture — see the
next section: mimalloc and jemalloc beat Solar's throughput on the list
benchmarks and tcmalloc/jemalloc on splay, but every one of them except
jemalloc has a benchmark where it collapses.)

---

# C allocator comparison: glibc vs jemalloc vs tcmalloc vs mimalloc vs bump

The C ports above use glibc `malloc`/`free`. The same four binaries, unchanged,
run here under four other allocators swapped in by `LD_PRELOAD` — to separate
"what the C *language* costs" from "what glibc's allocator costs", and to put a
hard floor under the numbers with a no-op-`free` bump allocator.

The contenders: **glibc** (baseline, no preload), **jemalloc** 5.3,
**tcmalloc** (gperftools `tcmalloc_minimal` 2.16), **mimalloc** 3.0, and
**bump** (`bench/c/bump.c`, built to `libbump.so`) — a per-thread arena
allocator where each thread bump-points a thread-local cursor with no atomics
and `free` is a no-op. The bump allocator is the manual-memory analogue of
Solar's `SOLAR_DISABLE_GC=1` mode: pure allocation throughput, zero reclamation.

## How to reproduce

```bash
# Debian/Ubuntu: the three shared allocators
sudo apt-get install -y libjemalloc2 libmimalloc3 libtcmalloc-minimal4t64
# the bump allocator (initial-exec TLS so the per-thread cursor isn't a
# __tls_get_addr call in the preloaded lib)
clang -O3 -fPIC -ftls-model=initial-exec -shared -o bench/c/libbump.so bench/c/bump.c
make -C bench/c                                  # the four benchmark binaries
ROUNDS=3 python3 bench/c/alloc_matrix.py         # interleaved matrix, the table below
```

## Throughput & peak memory (median of 3 interleaved rounds, lower is better)

Re-measured July 7, 2026 with all four benchmarks (splay at its resized
parameters). Same box as the GC tables; an unrelated host job held ~70 GB of
RAM throughout, and round 3 was visibly slower than rounds 1–2 for every
allocator (e.g. glibc splay 6.2 / 6.2 / 9.8 s), so the medians mostly reflect
the calmer rounds — treat small gaps as noise.

| allocator        | allocs3 wall | allocs3 RSS | threads wall | threads RSS | splay wall | splay RSS | allocs5 wall | allocs5 RSS |
|------------------|-------------:|------------:|-------------:|------------:|-----------:|----------:|-------------:|------------:|
| glibc            | 1.80 s       | 3053 MB     | 2.54 s       | **97 MB**   | 6.25 s     | 48 MB     | 4.38 s       | 3149 MB     |
| jemalloc         | 0.62 s       | 794 MB      | 1.02 s       | 55 MB       | 3.03 s     | 134 MB    | 1.64 s       | 845 MB      |
| tcmalloc (min)   | 0.59 s       | 775 MB      | 44.74 s²     | 51 MB       | **2.39 s** | 47 MB     | 47.72 s²     | 819 MB      |
| mimalloc         | **0.46 s**   | 766 MB      | **0.71 s**   | 55 MB       | 12.40 s³   | **43 MB** | **1.16 s**   | **810 MB**  |
| bump (no-op free)| 0.47 s       | **764 MB**  | 2.80 s       | 22329 MB¹   | 6.70 s     | 9962 MB¹  | 3.05 s       | 20732 MB¹   |
| *Solar (ref)*    | *0.63 s*     | *774 MB*    | *1.48 s*     | *2549 MB*   | *4.16 s*   | *3527 MB* | *2.41 s*     | *4202 MB*   |

*Solar (ref)* is from the throughput table further up (the July 10 session) —
**different sessions**,
shown only for scale, not measured interleaved with these. ¹ bump never frees,
so the churn benchmarks' RSS is the high-water of everything ever allocated:
~22 GB on `threads`/`allocs5` (~1.6 B nodes, how far past the first-finished
worker the other 15 get varies with scheduling) and ~10 GB on `splay`. ² not a
typo and not noise: tcmalloc_minimal is reproducibly ~45–58 s on `threads` and
`allocs5` across all rounds at the near-leanest RSS — CPU/lock-bound, not
swapping (see takeaway 4; ~90 s in the previous, more loaded session).
³ reproducible across rounds (11.9 / 12.4 / 20.2 s): mimalloc is the *slowest*
allocator on splay while posting the *leanest* RSS (43 MB) — see takeaway 5.

## Takeaways

1. **glibc is the slow one; the C *language* is not.** On `allocs3` (pure
   `malloc`, never frees) jemalloc/tcmalloc/mimalloc are **~3–4× faster than
   glibc** (0.46–0.62 s vs 1.80 s) and pack the 8-byte node at 8 bytes (~766 MB)
   instead of glibc's 32-byte minimum chunk (3 GB). So the GC section's "C
   malloc is ~2.4× slower than Solar" result is a **glibc** result: against
   mimalloc the same
   benchmark is ~1.4× *faster* than Solar (0.46 s vs 0.63 s).

2. **No single modern allocator wins everywhere — jemalloc is the only one
   that never collapses.** mimalloc is fastest on three benchmarks (0.46 /
   0.71 / 1.16 s) but **worst-in-field on splay** (12.4 s³); tcmalloc is
   fastest on splay (2.39 s) but **collapses on the two 16-thread churn
   benchmarks** (45–48 s²); glibc avoids pathologies but is 2–4× off the pace
   everywhere. jemalloc (0.62 / 1.02 / 3.03 / 1.64 s) is never first and
   never worse than ~2nd–3rd — the only C allocator whose ranking survives
   all four workloads. Solar shares that profile (no collapse on any of the
   four), sitting just behind jemalloc on threads/allocs5/splay and beating
   glibc everywhere.

3. **mimalloc and jemalloc beat Solar's throughput where they're healthy.**
   mimalloc's 0.71 s `threads` / 1.16 s `allocs5` and jemalloc's 1.02 /
   1.64 s beat Solar's 1.48 / 2.41 s — a state-of-the-art thread-caching
   allocator that reuses freed memory immediately is faster than collect-later,
   and does it at **~55 MB / ~845 MB vs Solar's 2549 / 4202 MB**. On splay
   only tcmalloc (2.39 s) and jemalloc (3.03 s) beat Solar (4.16 s);
   mimalloc (12.4 s) and bump
   (6.70 s) lose to it.

4. **tcmalloc_minimal collapses on the concurrent-churn workloads.** ~45–58 s
   on `threads` and `allocs5` (vs mimalloc's 0.71 / 1.16 s), reproducibly, at
   near-leanest RSS. The
   gperftools `_minimal` build aggressively returns freed pages to the OS; under
   16 threads churning 1.6 B short-lived nodes that becomes a storm of
   page-release / re-fault syscalls on the mutators. The retained chain
   changes nothing (allocs5 ≈ threads for it) — the pathology is the churn.

5. **mimalloc collapses on splay — the mirror image of tcmalloc.** 12.4 s
   median (vs tcmalloc's 2.39 s) at the leanest RSS of the field (43 MB, below
   even glibc). Same failure shape as tcmalloc's, on a different trigger:
   splay's payload graphs die at widely mixed ages (80 modifications per
   round, whole 35 MB trees at outer-run boundaries), and mimalloc's eager
   page-purging returns those pages to the OS just before the next round
   re-faults them. Together with takeaway 4 this is the study's core lesson:
   **every eager-reclaiming allocator has a workload-shaped pathology**;
   which one you hit depends on your object lifetimes.

6. **The bump floor: fastest only when nothing is freed.** With a no-op `free`,
   `allocs3` hits the theoretical minimum (0.47 s / 764 MB — a dead heat with
   mimalloc, since `allocs3` frees nothing anyway). But on every churn
   benchmark the bump is **slower than mimalloc** (threads 2.80 vs 0.71 s,
   allocs5 3.05 vs 1.16 s, splay 6.70 vs jemalloc's 3.03 s) even
   though allocation itself is just a pointer add: never freeing inflates the
   working set to 10–22 GB, so the mutators eat page-fault and cache-miss cost
   that reuse-based allocators avoid. This is exactly the effect Solar's HashMap
   takeaway #2 reports for `SOLAR_DISABLE_GC=1` — past a point, *not* reclaiming
   is a throughput loss, not a win. Allocation is cheap; memory traffic is not.
   (Note Solar's GC *beats* the no-op-free bump on both 16-thread benchmarks —
   1.48 vs 2.80 s and 2.41 vs 3.05 s — concurrent reclamation pays for itself
   in working-set size alone.)

**Net:** the right read of the GC section's "Solar beats C" is "Solar beats
**glibc**." Against modern allocators the picture is workload-shaped: mimalloc
takes three throughput crowns but collapses on splay, tcmalloc takes splay but
collapses on concurrent churn, and jemalloc — the only allocator with no
pathology — beats Solar modestly everywhere it's measured cleanly. Solar is
the other no-pathology contender, sitting between jemalloc and glibc on
throughput while carrying its GC float in RSS (2–4 GB on the churn
benchmarks vs jemalloc's 55–845 MB). Solar's distinct advantage is elsewhere —
automatic reclamation with no manual `free` and no use-after-free (the C
`threads`/`allocs5` ports have
to carefully free only their own per-thread lists to avoid racing), and on the
retained `allocs3` chain an RSS (778 MB) on par with the
best 8-byte-packing allocators.

---

# sieve — a non-allocation compute benchmark

Everything above stresses the allocator and collector. `sieve` is the control:
a port of [`examples/sieve.solar`](../examples/sieve.solar) — a Sieve of
Eratosthenes over 10⁸ — to the same four languages (`bench/c/sieve.c`,
`bench/go/sieve.go`, `bench/java/Sieve.java`, `bench/csharp/sieve/`). It
allocates **one 100 MB byte array up front and nothing after**: two nested
scan/mark loops perform ~3.4×10⁸ array stores, so after setup the allocator
and collector are idle in every runtime (Solar runs zero GC cycles — the
array is below the trigger floor and nothing ever dies). What's left is raw
generated-code quality: array indexing, bounds checks (Solar, Go, Java, and
C# check every store; C doesn't), loop optimization, and for the JVM/.NET the
JIT warmup, all sitting on top of a memory-bandwidth-bound access pattern.

All five ports run the identical single-pass algorithm (count `n` while
marking multiples from `2n` upward — no `n²` start, matching the Solar
source) and must print the same prime count, **5761455**, which the harness
asserts on every run.

## How to reproduce

```bash
cargo run --release --bin compile -- examples/sieve.solar target/sieve
make -C bench/c
(cd bench/go && go build -o sieve sieve.go)
javac bench/java/Sieve.java
dotnet build bench/csharp/sieve -c Release

ROUNDS=5 python3 bench/sieve_matrix.py    # interleaved, prints the table below
```

Each runtime appears once with its default GC — there is nothing for a
collector to do, so the collector-flavor matrix would measure noise.

## Results (median of 5 interleaved rounds, lower is better)

Measured July 7, 2026, same session/conditions as the allocator table (the
unrelated host job was still running; every contender drifted ~1.1 → 1.6 s
uniformly across the five rounds as it ramped, which the interleaving spreads
evenly — the medians land on the middle round).

| runtime | wall | peak RSS |
|---------|-----:|---------:|
| Solar   | **1.33 s** | 98 MB |
| C       | 1.35 s | 96 MB |
| Go      | 1.34 s | 97 MB |
| Java    | 1.42 s | 139 MB |
| C#      | 1.46 s | 131 MB |

## Takeaways

1. **It's a five-way tie, and that is the result.** Solar, C, and Go are
   within 2% of each other; Java and C# trail by only ~7–10% (JVM/.NET
   startup + JIT warmup on a ~1.3 s run accounts for most of that). On a
   memory-bandwidth-bound loop, language and runtime stop mattering: the
   marking stores miss cache no matter who compiled them.

2. **Solar's bounds checks cost nothing here.** Every Solar array store is
   bounds-checked, yet it matches unchecked C to the noise floor — the checks
   either get hoisted/elided by LLVM (the loop bounds prove the index
   in-range) or hide entirely behind the cache misses. The "bounds + null
   checks" tax the HashMap study measures on a cache-resident probe loop does
   not generalize to streaming workloads.

3. **RSS is the array plus the runtime.** Solar/C/Go sit at the raw 96–98 MB
   of the sieve array; Java and .NET add ~35–40 MB of VM. Solar's GC float —
   its footprint tax on the churn benchmarks — is absent because nothing is
   ever collected: with no garbage, Solar's footprint is C's.

---

# HashMap: Solar vs Rust + foldhash

Compares Solar's standard-library `hashbrown::HashMap` (a memory-safe SwissTable
port with the foldhash hasher and the concurrent GC) against Rust's
`std::collections::HashMap` using the same
[foldhash](https://crates.io/crates/foldhash) hasher.

Both use the **language's default hashing**:

- **Solar** — a single blanket `hash#[T]` / `operator_eq#[T]` in the stdlib,
  implemented with compile-time reflection. Struct keys get hashing and equality
  for free from their `pub` fields; no per-type boilerplate.
- **Rust** — `#[derive(Hash, PartialEq, Eq)]` on the key structs.

Sources: [`examples/hashmap.solar`](../examples/hashmap.solar) and
[`bench/rust/`](rust/) (a standalone crate, not part of the Solar workspace).

## Workload

Four key datatypes, each run in its own process:

| phase | key type | hashing path |
|-------|----------|--------------|
| `u64`   | `Uint64` | concrete primitive `hash` overload (`write_u64`) |
| `u32`   | `Uint32` | concrete primitive `hash` overload (`write_u32`) |
| `point` | `struct { x: Int64, y: Int64 }` | reflective `hash` over fields |
| `mixed` | `struct { a: Uint64, b: Uint32, c: Bool }` | reflective `hash` over fields |

Each phase, for **N = 1,000,000** keys drawn from a shared splitmix64 stream,
inserts `N` entries, performs `N` lookups that hit, then `N` lookups that miss,
and prints a checksum (`Σ looked-up values + len + miss count`, all wrapping).
The checksum is **hasher-independent** — it depends only on the key set and
lookup outcomes — so it is identical across the two implementations and serves as
a cross-implementation correctness check (the harness verifies it; all four
match).

## How to reproduce

```bash
bench/run.sh        # builds both (release) and prints the table below
```

`run.sh` builds the Solar runtime, compiles `examples/hashmap.solar` via
`--bin compile` (LLVM LTO + GC passes), builds the Rust crate (`lto = true`), and
runs `bench/run.py`. The harness launches each phase as its own process (phase
index on stdin) and reports the best wall-clock of 7 runs plus peak RSS
(`wait4` `ru_maxrss`).

- CPU: Intel Core Ultra 9 275HX (24 threads); rustc 1.98.0-nightly, LLVM 22.1.6
- foldhash 0.1.5 (`fast::FixedState`)

## Throughput & peak memory (best of 7, lower is better)

1,000,000 keys per phase (insert + hit + miss).

| phase | Solar (ms) | Rust (ms) | Solar/Rust | Solar RSS (MB) | Rust RSS (MB) | checksum match |
|-------|-----------:|----------:|-----------:|---------------:|--------------:|:--------------:|
| u64       | 162.7 | 93.2  | 1.74x | 71.2  | 52.9 | yes |
| u32       | 169.3 | 104.8 | 1.62x | 71.3  | 52.8 | yes |
| point     | 371.0 | 144.3 | 2.57x | 161.3 | 76.8 | yes |
| mixed     | 349.9 | 148.4 | 2.36x | 161.3 | 76.8 | yes |
| **total** | **1052.8** | **490.6** | **2.15x** | | | |

(Remeasured after the splay-exposed fixes plus the precise-barrier rework: the
GC-pacing fix — the trigger now paces against traced live instead of the
float-inflated total — trims Solar's RSS (78 → 71 MB on `u64`), and restoring a
*precise* write barrier (pointer-typed codegen; no barrier on plain integer
stores) cut Solar's wall from 1144 → 1053 ms (`u64` −15%). This started at
6.99x total / 175 MB on `u64`. A series of changes brought it to
2.43x: the `(inline)` hint on foldhash's `write_num`; an `ir_opt` escape analysis
that places non-escaping locals/params on the C stack instead of GC-heap-boxing
them (`NodeKind::Let { noescape }` + `param_noescape`, run to a fixpoint with a
transitive rule so HashMap keys flow `get → find`/`key_hash` to the stack);
binding a non-place `match` scrutinee to a `Let` so `match call()` stacks its
result; and route-checking field-rooted refs (`e.key&`) so the resize loop's
per-entry copy stays on the stack. The `u64`/`u32` phases are now essentially
allocation-free per operation (only the table's backing arrays remain), within
~2x of Rust. `point`/`mixed` still allocate per lookup — the reflective `hash#[T]`
desugars `for.reflect_fields f in self` to `let tmp = self; …tmp@.field…`, and
that copy of `self` breaks the non-escape chain for struct keys, so `key_hash`'s
parameter isn't provably non-escaping there. Fixing it needs the reflective
desugar to deref the object in place (or copy-aliasing in the analysis).)

## Takeaways

1. **~2× slower, ~1.3–2× more memory** (down from ~7× when this study began).
   The remaining gap is the cost of Solar's
   memory-safety guarantees over `std`'s hand-tuned, `unsafe`-heavy table: every
   slot access is bounds-checked, the table is two GC allocations (`ctrl` +
   `slots`) rather than one `unsafe` block, and each insert touches the GC
   (allocation accounting, born-black marking, write barriers). The struct phases
   cost more on both sides (larger keys, more hashing) and roughly double Solar's
   RSS (16-byte `Entry` slots plus control bytes).

2. **The GC is not the bottleneck.** Re-running with `SOLAR_DISABLE_GC=1` (bump
   allocator, never frees) is *slower* — e.g. `u64` 938 → 1262 ms, `point`
   1008 → 1389 ms — because unreclaimed garbage from table resizes blows up the
   working set. The concurrent collector keeps the live set cache-resident.

3. **Reflection has no runtime cost.** The blanket `hash` / `operator_eq` are
   resolved and fully unrolled at compile time into the same field-by-field code a
   hand-written impl would emit, so the reflective struct phases are competitive
   with the primitive phases on both sides — the per-phase deltas track key size
   and hashed bytes, not hashing strategy.

## Where the gap comes from (profiled)

`perf record` flat profiles of a single phase (Solar `target/hashmap`, Rust
release), plus `objdump`/`SOLAR_PRINT_GC_STATS`. Three causes, in order of
impact:

1. **Allocation of value-type temporaries dominates.** Solar's codegen
   lowers every IR `Let` to a `sol_alloc`, so each *intermediate* value — the
   per-lookup foldhash `Hasher` (a 48-byte `[Uint64; 6]` seed array + scalars),
   each `Group`/`BitMask` produced inside the probe, the `Option` result, and the
   boxed `self&`/`key&` method arguments — is heap-allocated. The `u64` phase does
   **27.9 M allocations for 3 M operations (~9 per op)**; Rust does on the order of
   a *dozen* (only the backing-array resizes — its hasher state lives on the stack
   and the probe runs in registers). In the Solar profile this shows up as
   `sol_alloc` **17–19 % self**, plus kernel page traffic from heap growth
   (`get_page_from_freelist` + `__rmqueue_pcplist` + `_raw_spin_lock` ≈ 5 %) and
   the concurrent collector's CPU (~30 cycles/phase, mostly off the critical path).

   *Why doesn't LLVM elide them?* The `solar-lower-gc-alloc` pass rewrites each
   `sol_alloc` to a recognized `calloc` precisely so LLVM's allocator-aware
   SROA/DSE *can* drop non-escaping ones — and for simple code it works
   completely: `examples/loop2fn5.solar` (closures, HOFs, per-iteration arrays in
   a 10⁹-iteration loop) optimizes down to **2 static allocations in the whole
   module and 0 surviving heap boxes in its hot loop**. So LLVM is capable; the
   hashmap hits two specific walls.

   - **Loop-carried per-iteration boxes (the dominant survivor).** Solar's
     codegen allocates a fresh `sol_alloc` for each loop-body `let` temporary
     *every iteration*, and threads it via a pointer-phi across the back-edge. In
     `find` the probe loop's index buffer becomes `%31 = phi ptr [%93 (back-edge
     calloc), %17 (preheader calloc)]` — a `calloc(1, 8)` per probe step that is
     **written once and never read** (a dead allocation), yet LLVM won't remove
     it: its DSE/allocation-elision is conservative about a loop-carried pointer
     phi that merges *distinct* heap allocations. loop2fn5 never hits this because
     its per-iteration allocations are created-and-killed within one fully-inlined
     iteration (no loop-carried pointer phi), so SROA/DSE delete them. This is why
     aggressive inlining only recovers ~24 % (below): the bulk aren't an inlining
     problem at all.

   - **An inline-cost near-miss for the `Hasher`.** The per-lookup foldhash
     `Hasher` `calloc(1, 80)` escapes into `write_num`, which has inline cost
     **exactly 250 = the default `-O3` threshold** (LLVM inlines only when
     cost *<* threshold, so it misses by one unit); `find→get` is genuinely big
     (cost ~985). Re-running the post-lowering bitcode with
     `opt -O3 -inline-threshold=10000` inlines these and drops runtime allocations
     **27.9 M → 21.1 M**, `u64` **852 → 667 ms (−22 %)**, `point`
     **1013 → 769 ms (−24 %)**, identical checksums — but ~20 M (the loop-carried
     boxes above) remain.

   Both walls are now fixed, codegen-side:

   - *Wall 1* — codegen hoists a `let`'s allocation to the function entry block
     when the variable's **address is never taken** (so reusing one box across
     iterations can't alias; captured/`&`-taken loop-locals are excluded and stay
     per-iteration — see `tests/runtime/hoist_capture.solar`). That replaces the
     loop-carried phi of distinct `calloc`s with a single non-escaping allocation,
     which `opt -O3` then promotes to stack/SSA exactly like `found`. The probe's
     per-iteration `Group` box (`let g`) disappears.
   - *Wall 2* — the `(inline)` attribute (`method(inline) write_num` in
     `foldhash.solar`) emits `static inline` → LLVM `inlinehint`, raising that
     function past its cost-250 so the whole `key_hash` chain inlines and the
     per-lookup `Hasher` `calloc(80)` is promoted away.

   Together they drop the `u64` phase **27.9 M → 19.7 M** allocations and
   **867 → 651 ms**, and the whole benchmark from **6.99x → 5.09x** vs Rust
   (table above), identical checksums. What remains of the gap is the scalar SWAR
   probe (Wall 2 of the SIMD section) and the residual method-boundary boxing.

2. **No SIMD in the probe (the suspected cause — confirmed).** Rust's hashbrown
   scans **16 control bytes per step with one `pcmpeqb` + `pmovmskb`** (the SSE2
   SwissTable group scan: `objdump` shows 24×`pcmpeqb`, 32×`pmovmskb`, 18×`movdqu`
   inlined into `main`). Solar has **no vector instructions** — `group::load` is a
   scalar 8-byte SWAR word (`GROUP_WIDTH = 8`, *half* Rust's group width) processed
   with `imul`/`shr`/`and` + a `tzcnt`. So Solar does ~2× the probe iterations and
   far more ALU work per iteration. `find` is the single hottest function at
   **36–37 %** of Solar's time; in Rust the probe is inlined into `main` and never
   appears as its own symbol.

3. **Bounds + null checks.** Each `ctrl`/`slots` access in `find` is
   bounds-checked and `slots` is a `&?` nullable deref (the cold
   `panic_*`/`expect_failed` call sites visible in `find`'s disassembly are these
   guards' failure branches); Rust's table uses unchecked `unsafe` indexing. Minor
   next to (1) and (2), but it adds a compare+branch to every slot touch.

Not the cause: hashing (`write_num` is only 3–4 %, and foldhash is bit-exact
across both), and the GC pauses (concurrent; see takeaway 2). Both sides pay
similar first-touch page-fault cost for fresh backing memory. **The ~7× is
mostly (1) temporary boxing and (2) the scalar-vs-SSE probe; a SIMD `Group` and
stack/register temporaries would close most of it.**

---

# binary-trees — Solar vs C/C++ (arena & vanilla malloc)

A separate study, using the Computer Language Benchmarks Game
[`binary-trees`](https://benchmarksgame-team.pages.debian.net/benchmarksgame/)
program. It allocates an enormous number of tiny 2-pointer tree nodes, so it is
almost entirely an allocator/reclaimer stress test. Solar uses its concurrent
GC with no manual freeing; the two C/C++ baselines sit at opposite ends of the
allocation spectrum:

- an **arena** version (best case — bump-pointer alloc, bulk free), and
- a **vanilla** version (realistic case — general-purpose per-node
  `malloc`/`free`).

## Files

| File | What it is | Source |
| --- | --- | --- |
| [`../examples/binarytrees.solar`](../examples/binarytrees.solar) | Solar, multi-threaded (one worker thread per depth) | this repo |
| [`../examples/binarytrees_st.solar`](../examples/binarytrees_st.solar) | Solar, single-threaded (1:1 with the vanilla C) | this repo |
| [`binarytrees_arena.cpp`](binarytrees_arena.cpp) | C++ arena (`std::pmr::monotonic_buffer_resource`) | [benchmarksgame `binarytrees-gpp-7`](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-gpp-7.html) — adapted (see header) |
| [`binaryTrees_vanilla.c`](binaryTrees_vanilla.c) | Vanilla C (`malloc`/`free`, single-threaded) | [bau-lang `binaryTrees.c`](https://raw.githubusercontent.com/thomasmueller/bau-lang/refs/heads/main/src/test/resources/org/bau/benchmarks/c/binaryTrees.c) — verbatim |

> **C++ adaptation note.** Upstream `binarytrees-gpp-7` uses
> `boost::counting_iterator` and TBB-backed parallel STL (`std::execution::par`),
> neither available here. The parallel `for_each` over depths is replaced with
> one `std::thread` per depth, each with its own `monotonic_buffer_resource` —
> structurally identical to the Solar multi-threaded port. The arena allocation
> that defines the entry's performance is unchanged.

All four produce byte-identical tree output (the vanilla C additionally prints a
leading `C` line).

## How to reproduce

```sh
# Solar (release runtime + compile)
cargo build --release -p solar-system
cargo run --bin compile -- examples/binarytrees.solar    target/binarytrees
cargo run --bin compile -- examples/binarytrees_st.solar target/binarytrees_st

# baselines
g++ -O3 -march=native -std=c++17 bench/binarytrees_arena.cpp -o /tmp/bt_arena   -lpthread
gcc -O3 -march=native            bench/binaryTrees_vanilla.c -o /tmp/bt_vanilla -lm

# run (N = 21)
target/binarytrees;  target/binarytrees_st;  /tmp/bt_arena 21;  /tmp/bt_vanilla 21
```

## Results

- **Workload:** `N = 21` (max depth 21, stretch 22)
- **Machine:** Intel Core Ultra 9 275HX, 24 logical cores, Linux
- **Toolchain:** gcc/g++ 14.2.0 `-O3 -march=native`; Solar release (LTO)
- 3 runs each; representative numbers below.

| Variant | Threads | Wall | CPU (user+sys) | Max RSS |
| --- | --- | ---: | ---: | ---: |
| C++ arena (`pmr` bump, threaded) | 9 | **0.42 s** | 1.6 s | 134 MB |
| Solar, multi-threaded | 9 | **1.8 s** | ~22 s | ~1.0 GB |
| Vanilla C (`malloc`/`free`) | 1 | **8.9 s** | 8.7 s | 264 MB |
| Solar, single-threaded | 1 (+ GC threads) | **5.7 s** | ~48 s | ~0.3 GB |

### Head-to-head (same threading model)

| Comparison | Wall | Memory |
| --- | --- | --- |
| Solar threaded **vs** C++ arena threaded | Solar **~4× slower** | Solar ~7× |
| Solar single-thread **vs** vanilla C `malloc`/`free` | Solar **~1.5× faster** | Solar ~1.2–1.5× |

## Takeaways

1. **Against a hand-rolled bump arena (C++), Solar's GC is uncompetitive
   (~4× slower, ~7× memory).** The arena is the ideal allocator for this shape:
   allocation is a pointer increment and the whole tree is freed at once with no
   per-node bookkeeping. Solar instead allocates each node as an individually
   traced, GC-managed object and keeps far more headroom live between cycles.

2. **Against general-purpose `malloc`/`free` (vanilla C), Solar wins on
   wall-clock (~1.5×) — but by spending CPU.** The vanilla C uses ~8.7 CPU-s on
   one core; single-threaded Solar uses ~48 CPU-s across many cores and finishes
   in 5.7 s wall. The C bottleneck is its recursive per-node `free()`, which is
   serial and unavoidable. Solar never frees individually — allocation is a
   born-black bump, and reclamation runs in bulk on background mark/sweep threads
   that overlap the mutator, moving "free is expensive and serial" off the
   critical path at the cost of total CPU and some memory.

3. **Per-core efficiency still favors C** in both comparisons. Solar's edge over
   vanilla C comes entirely from parallelizing reclamation across the 24-core
   machine; on a single core the vanilla C would win.
