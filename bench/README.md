# Solar vs Java vs C# vs C vs Go — allocation & GC throughput / latency

Head-to-head ports of `examples/allocs3.solar`, `examples/threads_list2.solar`,
`examples/splay.solar`, and `examples/allocs5.solar`
to Java (`bench/java/`, five JVM collectors), C# (`bench/csharp/`, .NET workstation
and server GC), C (`bench/c/`, manual `malloc`/`free`), and Go (`bench/go/`, its
concurrent GC). The Solar sources use nullable reference fields (`&?Node`,
`null#[Node]` for the empty case); the Java ports map a `Node`/`null` reference
onto them, the C# ports a `Node?`/`null` reference, the C ports a `Node*`/`NULL`
pointer, and the Go ports a `*Node`/`nil` pointer — so a single nullable field
models both the empty case and Solar's `&` indirection.

`splay` is a port of the V8/Dart splay-tree benchmark
([newspeaklanguage/benchmarks `Splay.java`](https://github.com/newspeaklanguage/benchmarks/blob/master/Splay.java)),
made self-contained with a deterministic RNG and a checksum. The Java/C/Go/C#
ports key the tree on `java.util.Random.nextDouble()` doubles exactly as the
original; Solar has no float literals, so its port keys on the 53-bit integer
mantissa that `nextDouble` divides by 2⁵³ — the mantissa orders identically to
the double, so **all five ports execute bit-identical tree operations and print
the same checksum** (`size=8000 checksum=17673159485837241130` at the default
2000 iterations), which doubles as a cross-language correctness check.

> **Measurement conditions.** All numbers below come from one **interleaved**
> session on a 24-core / 93 GB machine (Intel Core Ultra 9 275HX), produced by
> `bench/bench.py` (3 rounds; load average 7.7 at the start — residue of this
> session's own builds and smoke runs — decaying to 6.5/7.0 at the end, all of
> it the session's own; heavy *external* host load appeared only after the
> matrix finished). Interleaved means each
> round runs every language once before the next round begins, so
> background-load drift is spread evenly across contenders instead of
> penalizing whichever ran last; only one process runs at a time. The STW
> collectors' threaded numbers are **load-sensitive** (Solar stops all mutators at
> each phase; Go/G1/Parallel/.NET have STW phases too), so their `threads`
> worst-case pauses are noisy — the latency table reports the **median over the 3
> rounds** of each run's max/p50, and the p50 column is the more stable signal.
> The latency table samples **each individual STW pause** (not summed per cycle),
> so `max` is the worst single application stall and `p50` the median single
> stall. Load moves these numbers a lot: earlier sessions of this table
> measured Solar's `threads` at 1.66 s / 4.6 ms worst pause (near-idle),
> 2.22 s / 14 ms (load 4.6 → 11.8), and ~8 s / 86 ms (heavy load ~9–29).
> Prefer an idle box. Java uses `-Xmx8g`; Go and .NET use
> their defaults (the .NET binaries select workstation vs server GC at run time
> via `DOTNET_gcServer`); C is native. JDK is 21.0.11, Go 1.24.4, .NET
> 10.0.301 (`net10.0`).
>
> Two Solar-side changes since the previous session's tables:
>
> * **GC-trigger rework**: the collector now waits for **1 GiB of claimed
>   memory beyond the last traced live** (`MIN_SIZE_UNTIL_GC` in
>   `solar-system/src/gc.rs`) before firing a cycle. Benchmarks whose total
>   allocation stays under that floor now run **zero GC cycles**: Solar
>   finishes both `allocs3` (~800 MB, all live) and `splay` (~1 GB total churn)
>   without a single collection — their Solar latency cells read "—" (no pause
>   ever happened), `allocs3` becomes a pure allocator-throughput run, and
>   `splay`'s RSS is deferred-reclamation footprint (1102 MB where the previous
>   session, which collected, measured 179 MB).
> * Solar's arena has no `MADV_HUGEPAGE` hint (removed after a previous session
>   found THP `defrag=defer+madvise` page-faulting through synchronous
>   compaction on a fragmented host); it always runs on base 4 KiB pages.
>   Additionally, this session ran in a sandboxed shell with **per-process THP
>   disabled** (`THP_enabled: 0` in `/proc/self/status`, system policy
>   `madvise`), so no contender used hugepages.

## Directory layout

```
bench/
  java/      Allocs3.java, ThreadsList2.java, Splay.java, Allocs5.java   (javac before running)
  csharp/    allocs3/, threads_list2/, splay/, allocs5/, GcPause.cs   (dotnet build -c Release)
  c/         allocs3.c, threads_list2.c, splay.c, allocs5.c, Makefile   (make before running)
  go/        allocs3.go, threads_list2.go, splay.go, allocs5.go, go.mod   (go build before running)
  bench.py   interleaved harness — throughput (median wall + peak RSS) and
             GC-pause latency (Solar per-pause pause1/2/3; Java safepoint; Go gctrace;
             C# in-process GCSuspend→GCRestart EventListener)
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

# Full interleaved matrix (Solar + C + Go + 5 JVM collectors + 2 .NET GCs x 4 benchmarks):
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
`bench/csharp/GcPause.cs`, enabled with `BENCH_GC_TRACE=1`); C = none (no
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
  C# workers are background threads, and the C/Go/Solar `main` returns on
  first-worker-done, so the process exits when the first finishes, abandoning the
  other 15.)
- **splay** — single thread; an 8000-node splay tree is continually mutated
  (2000 rounds × 80 insert-then-remove modifications), each inserted node
  carrying a freshly allocated depth-5 payload object graph (~63 objects + a
  10-element array), an equal amount becoming garbage every modification.
  Unlike the two list benchmarks, the live set is **mid-sized and stable**
  (~35 MB) while both the allocation rate and — because splaying restructures
  the tree on every operation — the **heap-pointer mutation rate** are high:
  every rotation rewires `left`/`right` fields of long-lived nodes, so a
  concurrent collector's write barrier and remark are on the critical path.
  The C port has no collector and instead **manually frees** each removed
  node's payload graph inline. This benchmark caught three real Solar bugs: a
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
| Solar            | **0.87 s**   | **1.66 s**   | 0.72 s     | **2.16 s**   | **778 MB**  | 2096 MB     | 1102 MB   | 4011 MB     |
| C (malloc/free)  | 2.23 s       | 3.42 s       | 0.63 s     | 5.37 s       | 3052 MB     | 99 MB       | **48 MB** | **3151 MB** |
| Go               | 2.40 s       | 10.52 s      | 0.54 s     | 23.40 s      | 824 MB      | **62 MB**   | 90 MB     | 7757 MB     |
| Java G1          | 3.43 s       | 2.25 s       | 0.58 s     | 5.51 s       | 1944 MB     | 3528 MB     | 1079 MB   | 5668 MB     |
| Java Parallel    | 3.65 s       | 2.11 s       | **0.43 s** | 5.50 s       | 2340 MB     | 2771 MB     | 505 MB    | 3620 MB     |
| Java ZGC gen     | 2.14 s       | 4.27 s       | 1.24 s     | 16.30 s      | 2351 MB     | 4383 MB     | 916 MB    | 8451 MB     |
| Java ZGC non-gen | 2.05 s       | 4.77 s       | 1.24 s     | 29.28 s      | 3206 MB     | 8953 MB     | 1214 MB   | 19407 MB¹   |
| Java Shenandoah  | 1.09 s       | 2.55 s       | 0.64 s     | 13.56 s      | 1567 MB     | 7238 MB     | 932 MB    | 8241 MB     |
| C# Workstation   | 6.27 s       | 55.30 s      | 4.53 s     | 59.38 s      | 2341 MB     | 2558 MB     | 295 MB    | 15139 MB    |
| C# Server        | 3.96 s       | 14.31 s      | 1.42 s     | 7.16 s       | 2355 MB     | 433 MB      | 795 MB    | 6071 MB     |

¹ ZGC's multi-mapping (the same physical page mapped at several virtual
addresses) inflates kernel-reported RSS; non-generational ZGC counts each page
up to 3×, so its real physical footprint here is roughly a third of the 19.4 GB
shown (still the largest of the field). The generational-ZGC and threads
figures carry the same caveat to a lesser degree.

(`allocs3` is a *retained* chain, so RSS reflects allocator overhead per live
node: Solar's 8-byte cell and Go's 8-byte size class win; C pays glibc's 32-byte
minimum chunk; the JVM and .NET object headers land them in the 1.5–3 GB band.
`threads` is *discarded* garbage, so RSS reflects reclamation aggression: Go's
pacing keeps it leanest at 62 MB, C frees inline to 99 MB, with **.NET server GC
third-leanest at 433 MB** — while the JVM collectors let garbage accumulate
toward `-Xmx8g` (ZGC non-gen fills most of the cap) and .NET workstation GC,
choking on 16-thread contention, bloats to ~2.6 GB. Solar sits at 2096 MB — the
**concurrent sweep** allocates above the high-water mark during the sweep window
and defers hole reuse to the next pause, so its peak footprint grows with sweep
duration and machine load.
`splay`'s live set is only ~35 MB, so RSS is pure collector headroom: C 48 MB,
Go 90 MB, the JVMs 0.5–1.2 GB — and Solar's 1102 MB is this session's
**zero-collection** artifact: total churn stays under the new 1 GiB trigger
floor, so nothing is ever reclaimed and the whole churn history stays resident.
`allocs5` RSS = the ~0.8–3 GB retained chain (per the runtime's node overhead)
*plus* however much churn garbage the collector lets accumulate while it is
busy tracing that chain — which is exactly what it stresses: C stays at its
chain-only 3151 MB, Solar holds 4 GB (chain + sweep-window float), the
generational STW collectors sit at 3.6–5.7 GB, and the concurrent collectors
that fall behind balloon: Go 7.8 GB (its pacer overshoots against the marking
backlog), Shenandoah 8.2 GB, ZGC gen 8.5 GB, ZGC non-gen ~19 GB reported /
~6.5 GB physical¹, and C# workstation 15 GB.)

## GC pause latency — worst / median single STW pause (ms, median of 3 runs)

One sample per individual STW pause (Solar's three phases, Java's safepoints,
Go's two STW terms — not summed per cycle), so `max` is the worst single
application stall and `p50` the median single stall.

| runtime          | allocs3 max | allocs3 p50 | threads max² | threads p50 | splay max | splay p50 | allocs5 max | allocs5 p50 |
|------------------|------------:|------------:|-------------:|------------:|----------:|----------:|------------:|------------:|
| Solar            | none¹       | none¹       | 1.49         | 0.18        | none¹     | none¹     | 2.21        | 0.19        |
| C (malloc/free)  | none        | none        | none         | none        | none      | none      | none        | none        |
| Go               | 0.19        | 0.02        | 2.60         | 0.03        | 0.44      | 0.02      | 0.68        | 0.02        |
| Java G1          | 529.30      | 244.15      | 10.26        | 4.82        | 27.19     | 12.31     | 492.69      | 114.14      |
| Java Parallel    | 1477.05     | 783.98      | 7.10         | 4.75        | 6.69      | 6.16      | 1375.55     | 5.64        |
| Java ZGC gen     | 0.03        | 0.03        | 0.09         | 0.03        | 0.03      | 0.02      | 0.08        | 0.03        |
| Java ZGC non-gen | 0.02        | 0.02        | 0.06         | 0.03        | 0.02      | 0.01      | 0.06        | 0.02        |
| Java Shenandoah  | none¹       | none¹       | 0.52         | 0.06        | none¹     | none¹     | 712.44³     | 0.02        |
| C# Workstation   | 72.17       | 26.26       | 53.37        | 17.92       | 98.94     | 31.93     | 51.77       | 16.99       |
| C# Server        | 447.42      | 46.97       | 63.10        | 13.99       | 65.90     | 19.19     | 553.84      | 19.94       |

¹ Completed the benchmark **without a single collection**: Shenandoah's
`-Xmx8g` headroom means neither `allocs3` nor `splay` forces a mark pause
before the VM exits, and Solar's 1 GiB trigger floor (`MIN_SIZE_UNTIL_GC`)
means its collector never fires on either — zero cycles, zero pauses (see the
measurement-conditions note; splay pays for it in RSS).
² The `threads` worst-case pauses for the STW collectors (Solar, Go, G1,
Parallel, both .NET flavors) are noisy under load, so a single scheduling spike
during a mutator-stop handshake produces multi-millisecond maxes (previous
sessions measured Solar at 4.6–14 ms here; this session's 1.49 ms max also
reflects the trigger rework running fewer, better-paced cycles). The medians
over rounds are shown; the **p50 row is far more stable** — and on a per-pause
basis Solar's p50 single stall is **0.18 ms** (its three phases are each small
in the median; the big one, the pause-2 remark, is only one of three). ZGC
stays sub-millisecond throughout. **.NET's pauses are neither**: every
ephemeral (gen0/gen1) collection is blocking, and at this allocation rate there
are thousands of them, so even at p50 the stalls are tens of milliseconds (C#
server's 447 ms `allocs3` max is one blocking compacting gen2). On `allocs5`
the retained chain is the discriminator: Solar's worst single stall stays at
**2.21 ms** (pauses scan only roots — stacks and registers — never the heap, so
the 100M-node chain never enters a pause), ZGC stays at 0.06–0.08 ms, while G1
stalls 114 ms *at the median* (its pauses evacuate chain regions), Parallel
spikes to a 1.38 s full-GC max, and C# server to 554 ms.
³ Shenandoah's `allocs5` max is a **degenerated collection**: the 16-thread
allocation rate outruns its concurrent mark of the 800 MB chain, and the cycle
falls back to stop-the-world — its p50 stays 0.02 ms, but the failure mode
costs 712 ms when it hits.

## Fraction of wall-clock spent in STW GC

| runtime          | allocs3 | threads | splay | allocs5 |
|------------------|--------:|--------:|------:|--------:|
| Solar            | 0%      | ~13%    | 0%    | ~7%     |
| C (malloc/free)  | 0%      | 0%      | 0%    | 0%      |
| Go               | ~0%     | ~3%     | ~0.2% | ~0%     |
| Java G1          | ~75%    | ~10%    | ~24%  | ~59%    |
| Java Parallel    | ~73%    | ~10%    | ~6%   | ~62%    |
| Java ZGC         | ~0%     | ~0.05%  | ~0%   | ~0.04%  |
| Java Shenandoah  | 0%      | ~0.3%   | 0%    | ~17%    |
| C# Workstation   | ~54%    | ~82%    | ~67%  | ~77%    |
| C# Server        | ~44%    | ~79%    | ~43%  | ~54%    |

(Each cell is summed STW pause time over wall-clock from **one traced run per
contender** (`SOLAR_PRINT_GC_STATS`, `-Xlog:safepoint`, `gctrace`, the C#
EventListener) taken after the interleaved matrix, so treat them as one
significant figure — the tracing itself and background load inflate the
denominators a little. The exact 0%s are the zero-collection runs (footnote ¹
above). For Solar, ZGC, Shenandoah, and Go the
marking work is concurrent / off the critical path, so STW fraction is small.
**Go's GC cost does not show up here**:
it is paid as concurrent *mark-assist* throttling of allocating goroutines,
which is what tanks its `threads` and `allocs5` throughput while keeping
pauses sub-millisecond. C does no marking; its
reclamation cost is inline `free`, not a pause — see takeaway 7. **.NET sits at
the opposite extreme from Go**: only its gen2 sweep is concurrent, so the constant
blocking gen0/gen1 collections at this allocation rate put it in the G1/Parallel
"mostly stopped" camp on every benchmark — summed across
thousands of short blocking pauses rather than a few long ones. `allocs5` drags
G1/Parallel back up to ~60% (their pauses move or scan the retained chain) and
puts Shenandoah at ~17% (degenerated cycles, footnote ³); Solar's ~7% is the
sum of many small 16-thread stop handshakes (66 pauses in the traced run —
median ~0.2 ms, the tail inflated by scheduling spikes under the traced run's
background load), not a few big stalls.)

## Takeaways

1. **Monotonic growth (allocs3) splits the field cleanly.** The non-moving /
   concurrent collectors win and the copying / compacting collectors lose.
   **Solar is fastest outright (0.87 s)** — with the 1 GiB trigger floor the
   ~800 MB chain never even starts a collection, so this is pure allocator
   throughput — followed by Shenandoah (1.09 s, which also never collects it),
   ZGC (2.05–2.14 s), C (2.23 s), and Go (2.40 s).
   The compactors trail: G1 (3.43 s), Parallel (3.65 s), and C# server (3.96 s)
   spend ~44–75% of wall-clock **stopped** moving the growing chain (Parallel's
   worst single pause is **1.48 s**, C# server's **0.45 s** for one blocking gen2
   compaction), and **C# workstation is the slowest at 6.27 s** — its single GC
   heap can't keep up even single-threaded. Notably **C `malloc` (2.23 s)
   is ~2.6× slower than Solar** here, and glibc's 32-byte minimum chunk inflates
   the chain to ~3 GB vs Solar's 778 MB and Go's 824 MB.

2. **High concurrent garbage (threads): Solar wins on throughput; Go and .NET
   collapse.** Solar (1.66 s) leads, ahead of Parallel (2.11 s), G1 (2.25 s),
   and Shenandoah (2.55 s), then C (3.42 s) and ZGC (4.3–4.8 s), **~6.3×
   faster than Go
   (10.52 s)** and **~33× faster than C# workstation (55.30 s)**. Two runtimes
   collapse for different reasons. Go's concurrent GC cannot keep pace with 16
   goroutines churning 1.6 billion short-lived nodes: mutators are conscripted
   into mark-assist and throttled, so throughput craters even though its pauses
   stay tiny. **.NET workstation GC is far worse** — its *single* GC heap
   serializes 16 allocating threads, so it spends ~82% of wall stopped in
   back-to-back ephemeral collections. **Server GC fixes most of that** (14.3 s,
   a per-core heap each) and reclaims promptly enough to post the third-leanest
   footprint of the field (**433 MB**, behind only Go's 62 MB and C's 99 MB),
   but it is
   still slower than Go. The catch for Solar is contention: it stops all 16
   mutators at each STW phase, so under load its pauses and `stall_for_gc`
   back-pressure inflate (this session measured a 1.49 ms worst
   pause; earlier sessions under load measured 2.22 s wall / 14 ms and
   ~8 s / 86 ms).

3. **Pointer churn (splay): the zero-collection trade-off shows its cost.**
   Parallel is fastest this session (0.43 s), then Go (0.54 s), G1 (0.58 s),
   C (0.63 s), Shenandoah (0.64 s), and Solar at 0.72 s. The previous session
   — before the 1 GiB trigger floor — Solar won this benchmark outright
   (0.38 s at 179 MB RSS) *with* the collector running. Now splay's ~1 GB of
   total churn stays under the floor, Solar never collects, and the run
   behaves like the bump allocator in the study below: RSS grows to 1102 MB
   and the ballooning working set eats page-fault and cache-miss cost that
   reclaim-and-reuse would have avoided (the same "not reclaiming is a
   throughput loss" effect as the bump allocator's `threads` entry and the
   HashMap study's `SOLAR_DISABLE_GC` row). The flip side is zero pauses on a
   benchmark whose pointer-mutation rate makes other collectors stall: the
   blocking collectors pay 6–27 ms stalls (Parallel/G1) and .NET 66–99 ms.
   Splay remains the write-barrier stress test — when a cycle *does* run
   during splaying (as in previous sessions and the debug/ASAN test suite),
   every rotation rewires long-lived pointers mid-mark; all five ports print
   the same checksum.

4. **Retained live set + churn (allocs5) is the discriminator this suite was
   missing — Solar wins it outright and by the largest margin.** Solar
   (2.16 s) is ~2.5× faster than the next GC runtime (Parallel/G1 at 5.50/5.51 s)
   and C (5.37 s), with a worst single stall of **2.21 ms**. Every collector
   now has to deal with the ~800 MB chain *while* 16 threads churn 1.6 billion
   short-lived nodes: (a) **Solar's pauses don't see the chain at all** — its
   STW phases scan only roots (stacks + registers) and the marking of the
   100M-node chain runs concurrently on the worker pool, so allocs5 costs it
   almost exactly the sum of its allocs3 + threads walls (0.87 + 1.66 ≈ 2.16 s
   measured) and its pause profile is unchanged from threads (max 2.21 ms /
   p50 0.19 ms). (b) **The fully-concurrent collectors collapse**: each cycle
   must re-trace the whole chain while allocation keeps pace-triggering more
   cycles — Go goes from 10.5 s (threads) to **23.4 s** with RSS exploding from
   62 MB to 7.8 GB as its pacer falls behind; ZGC gen lands at 16.3 s, ZGC
   non-gen at 29.3 s; and **Shenandoah (13.6 s) degenerates outright**,
   repeatedly cancelling concurrent cycles on allocation failure and falling
   back to 0.5–1.5 s stop-the-world marks (712 ms median-of-round max) — the
   exact failure mode Solar's design avoids by keeping mutators un-throttled
   and pauses root-only. (c) **The generational STW collectors do what
   generations are for**: G1/Parallel young collections reclaim the churn
   without re-copying the old chain, so their walls (5.5 s) are just their
   parts summed — but the chain still leaks into their pauses (G1's p50 jumps
   to **114 ms**; Parallel's max is a 1.38 s full GC). (d) C (5.37 s) also
   pays nothing ongoing for the retained chain — but it paid up front in
   glibc's 32-byte chunks (3.1 GB resident) and still does all reclamation
   inline. (e) C# server, oddly, runs allocs5 (7.16 s) *faster* than threads
   alone (14.3 s) — the 2.3 GB chain forces the heap and its ephemeral budgets
   up early, roughly halving its blocking-collection fraction (79% → 54%);
   workstation stays pathological (59.4 s, 15 GB).

5. **Latency: ZGC rules; Solar matches it in the median and stays
   single-digit-ms in the worst case; Shenandoah's concurrency has a cliff;
   .NET is not in the race.** Per individual pause Solar's stalls are small
   (threads max 1.49 ms / p50 0.18 ms; allocs5 max 2.21 ms / p50 0.19 ms;
   allocs3 and splay literally zero pauses this session) — it beats Go
   pause-for-pause on every benchmark (Go's threads max 2.60 ms, allocs5
   0.68 ms), and only its 16-thread stop handshake leaves a low-millisecond
   tail. ZGC stays sub-0.1 ms everywhere — including allocs5, where it pays
   for that purity with a 7.5–13.5× throughput loss vs Solar. Shenandoah is
   sub-0.1 ms until the load exceeds what its concurrent cycle can absorb,
   then **degenerates to 712 ms stalls** (footnote ³). **.NET
   sits at the other end with G1/Parallel**: its always-blocking ephemeral GCs
   give p50 stalls of ~14–32 ms on every benchmark, and blocking gen2
   compactions spike C# server to 447 ms (allocs3) and 554 ms (allocs5).

6. **Go is the latency/throughput inverse of the STW collectors.** It keeps
   pauses tiny by doing all reclamation concurrently — but on a high allocation
   rate that concurrency is *paid by the mutators* via mark-assist, so it posts
   the leanest `threads` footprint of the field (62 MB) and near-best latency
   yet a **very
   poor `threads` throughput** (10.52 s — though .NET holds the throughput
   floor), and adding the retained chain more than doubles that (23.4 s
   allocs5) while its footprint discipline breaks down entirely (7.8 GB). On
   the single-threaded benchmarks, where one thread can't outrun the
   GC, Go is competitive (2.40 s allocs3, 0.54 s splay) with sub-0.1 ms median
   pauses.

7. **C is fastest only when there is nothing to reclaim — and on these
   workloads there always is.** On allocs3 (never frees) C is still ~2.6× slower
   than Solar on `malloc` call overhead alone. On threads (3.42 s) and
   allocs5 (5.37 s) C must `free` inline on the mutator — the reclamation
   Solar/Java/Go/.NET do concurrently or in bulk, serialized into the hot path
   (splay, 0.63 s, is its best showing this session, edging out Solar's
   zero-collection run). C's "GC pause" is `none` yet its wall-clock is
   mid-pack; its unambiguous win is footprint (99 MB threads / 48 MB splay).

**Net:** across these four workloads, on this box, Solar's non-moving
concurrent mark-sweep is **the outright throughput leader on three of four
benchmarks** — allocs3, threads, and (by the widest margin, ~2.5×) the new
combined-stress allocs5 — beating C, Go, and both .NET GCs everywhere except
splay, where the new 1 GiB trigger floor turns Solar into a
never-reclaiming allocator and costs it the win it held last session (0.72 s
vs Parallel's 0.43 s). Its pause profile sits in the ZGC class in the median
(~0.2 ms) with a low-single-digit-millisecond worst case from the 16-thread
stop handshake. allocs5 is the separator: it shows what happens when a large
live set and a high allocation rate arrive together — fully-concurrent
collectors (Go, ZGC, Shenandoah) fall behind or degenerate, blocking
generational collectors (G1, Parallel, .NET) protect throughput but leak the
live set into 100 ms–1.4 s pauses, and Solar's root-only pauses + concurrent
parallel mark keep *both* its throughput additive and its stalls at 2 ms. The
splay regression is the flip side of the same trigger tuning and looks like a
tunable, not a design limit: a floor keyed to (or capped by) working-set
growth would restore reclamation on mid-sized heaps.

(The "beating C everywhere" above is specifically vs the C ports' **glibc**
`malloc`/`free`. Swapping in a modern allocator changes the picture — see the
next section: jemalloc and mimalloc beat Solar's throughput on the two list
benchmarks. The allocator study's numbers predate the trigger rework and this
session's machine conditions.)

---

# C allocator comparison: glibc vs jemalloc vs tcmalloc vs mimalloc vs bump

The C ports above use glibc `malloc`/`free`. The same two binaries, unchanged,
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
make -C bench/c                                  # the two benchmark binaries
ROUNDS=3 python3 bench/c/alloc_matrix.py         # interleaved matrix, the table below
```

## Throughput & peak memory (median of 3 interleaved rounds, lower is better)

| allocator        | allocs3 wall | allocs3 RSS | threads wall | threads RSS |
|------------------|-------------:|------------:|-------------:|------------:|
| glibc            | 2.35 s       | 3053 MB     | 4.28 s       | **97 MB**   |
| jemalloc         | 0.73 s       | 794 MB      | 1.20 s       | 55 MB       |
| tcmalloc (min)   | 0.71 s       | 775 MB      | 90.62 s²     | **53 MB**   |
| mimalloc         | **0.56 s**   | 766 MB      | **0.85 s**   | 54 MB       |
| bump (no-op free)| 0.57 s       | **764 MB**  | 2.83 s       | 15030 MB¹   |
| *Solar (ref)*    | *0.87 s*     | *778 MB*    | *1.66 s*     | *2096 MB*   |

*Solar (ref)* is from the throughput table further up — a **different session**,
shown only for scale, not measured interleaved with these. ¹ bump never frees,
so `threads` RSS is the high-water of all ~1.6 B allocations (≈15 GB). ² not a
typo and not noise: tcmalloc_minimal is reproducibly ~90 s on `threads` across
all three rounds at 53 MB RSS — CPU/lock-bound, not swapping (see takeaway 4).

## Takeaways

1. **glibc is the slow one; the C *language* is not.** On `allocs3` (pure
   `malloc`, never frees) jemalloc/tcmalloc/mimalloc are **~3–4× faster than
   glibc** (0.56–0.73 s vs 2.35 s) and pack the 8-byte node at 8 bytes (~766 MB)
   instead of glibc's 32-byte minimum chunk (3 GB). So the GC section's "C
   malloc is ~2.6× slower than Solar" result is a **glibc** result: against
   mimalloc the same
   benchmark is ~1.6× *faster* than Solar (0.56 s vs 0.87 s).

2. **mimalloc beats Solar on both benchmarks' throughput.** mimalloc is fastest
   everywhere (0.56 s / 0.85 s); jemalloc is close (0.73 s / 1.20 s). Both beat
   Solar's 0.87 s / 1.66 s. Solar's GC throughput lead held against glibc and Go,
   but a state-of-the-art thread-caching allocator that reuses freed memory is
   faster here — and on `threads` it does so at **~55 MB vs Solar's 2096 MB**,
   because it reclaims immediately rather than at the next GC cycle.

3. **The bump floor: fastest only when nothing is freed.** With a no-op `free`,
   `allocs3` hits the theoretical minimum (0.57 s / 764 MB — a dead heat with
   mimalloc, since `allocs3` frees nothing anyway). But on the high-churn
   `threads` bench the bump is **slower than mimalloc *and* glibc** (2.83 s) even
   though allocation itself is just a pointer add: never freeing inflates the
   working set to 15 GB, so the mutators eat page-fault and cache-miss cost that
   reuse-based allocators avoid. This is exactly the effect Solar's HashMap
   takeaway #2 reports for `SOLAR_DISABLE_GC=1` — past a point, *not* reclaiming
   is a throughput loss, not a win. Allocation is cheap; memory traffic is not.

4. **tcmalloc_minimal collapses on this workload.** ~90 s on `threads` (vs
   mimalloc's 0.85 s), reproducibly, at the leanest RSS of the field (53 MB). The
   gperftools `_minimal` build aggressively returns freed pages to the OS; under
   16 threads churning 1.6 B short-lived nodes that becomes a storm of
   page-release / re-fault syscalls on the mutators. Lean but pathological here —
   a reminder that allocator behavior is workload-shaped, not a single ranking.

**Net:** the right read of the GC section's "Solar beats C" is "Solar beats
**glibc**." Against modern allocators the throughput crown on these two
benchmarks goes to mimalloc, with jemalloc second; Solar sits between the modern
allocators and glibc. Solar's distinct advantage is elsewhere — automatic
reclamation with no manual `free` and no use-after-free (the C `threads` port has
to carefully free only its own per-thread lists to avoid racing), competitive
throughput, and on the retained `allocs3` chain an RSS (778 MB) on par with the
best 8-byte-packing allocators.

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
