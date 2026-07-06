# Solar vs Java vs C# vs C vs Go — allocation & GC throughput / latency

Head-to-head ports of `examples/allocs3.solar`, `examples/threads_list2.solar`,
and `examples/splay.solar`
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
> session on a near-idle 24-core / 93 GB machine (Intel Core Ultra 9 275HX),
> produced by `bench/bench.py` (3 rounds; load average 1.7 at the start — the
> 9.0 at the end is the session's own accumulation). Interleaved means each
> round runs every language once before the next round begins, so
> background-load drift is spread evenly across contenders instead of
> penalizing whichever ran last; only one process runs at a time. The STW
> collectors' threaded numbers are **load-sensitive** (Solar stops all mutators at
> each phase; Go/G1/Parallel/.NET have STW phases too), so their `threads`
> worst-case pauses are noisy — the latency table reports the **median over the 3
> rounds** of each run's max/p50, and the p50 column is the more stable signal.
> The latency table samples **each individual STW pause** (not summed per cycle),
> so `max` is the worst single application stall and `p50` the median single
> stall. Load moves these numbers a lot: the previous session of this table
> (load 4.6 → 11.8) measured Solar's `threads` at 2.22 s with a 14 ms worst
> pause, and an earlier heavy-load run (~9–29) at ~8 s / 86 ms. Prefer an idle
> box. Java uses `-Xmx8g`; Go and .NET use
> their defaults (the .NET binaries select workstation vs server GC at run time
> via `DOTNET_gcServer`); C is native. .NET is 10.0.301 (`net10.0`).
> This session was measured after **removing the arena's `MADV_HUGEPAGE` hint**
> (`solar-system/src/heap.rs`): under THP `defrag=defer+madvise` (a common
> default) hinted regions page-fault through *synchronous* direct reclaim and
> compaction, and on a fragmented host (42 GB swapped out) that tripled Solar's
> `threads` wall (3.4 s, of which 15 s of CPU was kernel compaction — 99% of
> the attempts failing). Solar now always runs on base 4 KiB pages. The
> earlier sessions' Solar numbers were taken with the hint active on an
> unfragmented host, where it was harmless-to-helpful.

## Directory layout

```
bench/
  java/      Allocs3.java, ThreadsList2.java, Splay.java   (javac before running)
  csharp/    allocs3/, threads_list2/, splay/, GcPause.cs   (dotnet build -c Release)
  c/         allocs3.c, threads_list2.c, splay.c, Makefile   (make before running)
  go/        allocs3.go, threads_list2.go, splay.go, go.mod   (go build before running)
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

# C (manual malloc/free)
make -C bench/c

# Go (1.24)
(cd bench/go && go build -o allocs3 allocs3.go && go build -o threads_list2 threads_list2.go && go build -o splay splay.go)

# Java (JDK 21)
javac bench/java/Allocs3.java bench/java/ThreadsList2.java bench/java/Splay.java

# C# (.NET 10; install once via dotnet-install.sh if not present)
dotnet build bench/csharp/allocs3       -c Release
dotnet build bench/csharp/threads_list2 -c Release
dotnet build bench/csharp/splay         -c Release
# the apphost binaries find the runtime via DOTNET_ROOT when .NET lives under
# ~/.dotnet (a non-system-registered location):
export DOTNET_ROOT="$HOME/.dotnet"

# Full interleaved matrix (Solar + C + Go + 5 JVM collectors + 2 .NET GCs x 3 benchmarks):
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

## Throughput & peak memory (lower is better)

Wall-clock is the median of 3 runs; RSS is peak resident set.

| runtime          | allocs3 wall | threads wall | splay wall | allocs3 RSS | threads RSS | splay RSS |
|------------------|-------------:|-------------:|-----------:|------------:|------------:|----------:|
| Solar            | 0.99 s       | **1.66 s**   | **0.38 s** | **781 MB**  | 2117 MB     | 179 MB    |
| C (malloc/free)  | 1.63 s       | 3.42 s       | 0.82 s     | 3052 MB     | 99 MB       | **48 MB** |
| Go               | 2.28 s       | 10.67 s      | 0.50 s     | 822 MB      | **83 MB**   | 89 MB     |
| Java G1          | 2.88 s       | 2.04 s       | 0.63 s     | 1943 MB     | 3547 MB     | 1095 MB   |
| Java Parallel    | 3.16 s       | 1.78 s       | 0.44 s     | 2340 MB     | 2772 MB     | 505 MB    |
| Java ZGC gen     | 1.46 s       | 3.80 s       | 1.09 s     | 2349 MB     | 4625 MB     | 924 MB    |
| Java ZGC non-gen | 1.51 s       | 4.18 s       | 1.05 s     | 2980 MB     | 8753 MB     | 1172 MB   |
| Java Shenandoah  | **0.77 s**   | 2.02 s       | 0.69 s     | 1567 MB     | 7250 MB     | 932 MB    |
| C# Workstation   | 5.10 s       | 53.85 s      | 4.50 s     | 2342 MB     | 6937 MB     | 339 MB    |
| C# Server        | 2.76 s       | 13.70 s      | 1.51 s     | 2352 MB     | 432 MB      | 712 MB    |

(`allocs3` is a *retained* chain, so RSS reflects allocator overhead per live
node: Solar's 8-byte cell and Go's 8-byte size class win; C pays glibc's 32-byte
minimum chunk; the JVM and .NET object headers land them in the 1.5–3 GB band.
`threads` is *discarded* garbage, so RSS reflects reclamation aggression: Go's
pacing keeps it leanest at 83 MB, C frees inline to 99 MB, with **.NET server GC
third-leanest at 432 MB** — while the JVM collectors let garbage accumulate
toward `-Xmx8g` (ZGC non-gen fills most of the cap) and .NET workstation GC,
choking on 16-thread contention, bloats to ~7 GB. Solar sits at 2117 MB — the
**concurrent sweep** allocates above the high-water mark during the sweep window
and defers hole reuse to the next pause, so its peak footprint grows with sweep
duration and machine load (the previous, loaded session measured 2502 MB).
`splay`'s live set is only ~35 MB, so RSS is pure collector headroom: C 48 MB,
Go 89 MB, Solar 179 MB, the JVMs 0.5–1.2 GB.)

## GC pause latency — worst / median single STW pause (ms, median of 3 runs)

One sample per individual STW pause (Solar's three phases, Java's safepoints,
Go's two STW terms — not summed per cycle), so `max` is the worst single
application stall and `p50` the median single stall.

| runtime          | allocs3 max | allocs3 p50 | threads max² | threads p50 | splay max | splay p50 |
|------------------|------------:|------------:|-------------:|------------:|----------:|----------:|
| Solar            | 0.03        | 0.01        | 4.61         | 0.23        | 0.20      | 0.02      |
| C (malloc/free)  | none        | none        | none         | none        | none      | none      |
| Go               | 0.06        | 0.01        | 6.90         | 0.03        | 0.67      | 0.02      |
| Java G1          | 477.06      | 230.52      | 9.04         | 5.18        | 24.15     | 11.70     |
| Java Parallel    | 1340.62     | 695.31      | 8.14         | 4.59        | 7.27      | 6.04      |
| Java ZGC gen     | 0.02        | 0.02        | 0.10         | 0.03        | 0.03      | 0.02      |
| Java ZGC non-gen | 0.01        | 0.01        | 0.06         | 0.03        | 0.02      | 0.02      |
| Java Shenandoah  | none¹       | none¹       | 0.92         | 0.03        | none¹     | none¹     |
| C# Workstation   | 60.35       | 19.32       | 60.02        | 17.70       | 141.39    | 32.60     |
| C# Server        | 374.37      | 30.03       | 67.11        | 16.18       | 73.75     | 22.09     |

¹ Shenandoah completed `allocs3` and `splay` **without any STW-bearing
safepoint** — with `-Xmx8g` neither live set forced a mark pause before the VM
exited.
² The `threads` worst-case pauses for the STW collectors (Solar, Go, G1,
Parallel, both .NET flavors) are noisy under load, so a single scheduling spike
during a mutator-stop handshake produces these ~5–9 ms maxes (Solar 4.6, Go
6.9, G1 9.0; the previous, loaded session measured 10–16 ms). The medians over
rounds are shown; the **p50 row is far more
stable** — and on a per-pause basis Solar's p50 single stall is **0.23 ms** (its
three phases are each small in the median; the big one, the pause-2 remark, is
only one of three). ZGC and Shenandoah stay sub-millisecond throughout. **.NET's
pauses are neither**: every ephemeral (gen0/gen1) collection is blocking, and at
this allocation rate there are thousands of them, so even at p50 the stalls are
tens of milliseconds (C# server's 374 ms `allocs3` max is one blocking
compacting gen2). On `splay` — the one benchmark whose *pointer-mutation* rate,
not just allocation rate, is high — Solar's worst single stall is **0.20 ms**,
the best worst-case in the field this session (Go 0.67, ZGC 0.02–0.03), while
the blocking collectors stall 7–24 ms (Parallel/G1) and .NET 74–141 ms.

## Fraction of wall-clock spent in STW GC

| runtime          | allocs3 | threads |
|------------------|--------:|--------:|
| Solar            | ~0.05%  | ~5%     |
| C (malloc/free)  | 0%      | 0%      |
| Go               | ~0%     | ~0%     |
| Java G1          | ~83%    | ~4%     |
| Java Parallel    | ~85%    | ~3%     |
| Java ZGC         | ~0%     | ~0.1%   |
| Java Shenandoah  | ~0%     | ~0.1%   |
| C# Workstation   | ~52%    | ~87%    |
| C# Server        | ~55%    | ~86%    |

(Derived for `allocs3`/`threads` — Solar's directly from `SOLAR_PRINT_GC_STATS`
pause sums (0.6 ms STW over a 1.2 s allocs3 run; 87 ms over a 1.7 s threads
run); on `splay` the same split holds — the
concurrent collectors' summed stalls are well under 1% of its ~0.4–0.7 s wall
(Solar: 2.2 ms of STW across a 0.40 s run, ~0.5%),
while Parallel/G1's 7–24 ms blocking collections and .NET's 22–141 ms stalls are
a visible fraction of a sub-second run. For Solar, ZGC, Shenandoah, and Go the
marking work is concurrent / off the critical path, so STW fraction is small. **Go's GC cost does not show up here**:
it is paid as concurrent *mark-assist* throttling of allocating goroutines — 3%
of GC CPU on allocs3 but 18% on threads — which is what tanks its `threads`
throughput while keeping pauses sub-millisecond. C does no marking; its
reclamation cost is inline `free`, not a pause — see takeaway 5. **.NET sits at
the opposite extreme from Go**: only its gen2 sweep is concurrent, so the constant
blocking gen0/gen1 collections at this allocation rate put it in the G1/Parallel
"mostly stopped" camp — ~52–55% on allocs3 and ~86–87% on threads, summed across
thousands of short blocking pauses rather than a few long ones.)

## Takeaways

1. **Monotonic growth (allocs3) splits the field cleanly.** The non-moving /
   concurrent collectors win and the copying / compacting collectors lose.
   Shenandoah is **fastest (0.77 s) with Solar second (0.99 s)** — both avoid
   evacuating a live
   set that only grows — followed by ZGC (~1.5 s), C (1.63 s), and Go (2.28 s).
   The compactors trail: C# server (2.76 s), G1 (2.88 s) and Parallel (3.16 s)
   spend ~52–85% of wall-clock **stopped** moving the growing chain (Parallel's
   worst single pause is **1.34 s**, C# server's **0.37 s** for one blocking gen2
   compaction), and **C# workstation is the slowest at 5.10 s** — its single GC
   heap can't keep up even single-threaded. Solar's worst single pause stays
   ≤ 0.03 ms and its median pause is **0.01 ms**. Notably **C `malloc` (1.63 s)
   is ~1.6× slower than Solar** here, and glibc's 32-byte minimum chunk inflates
   the chain to ~3 GB vs Solar's 781 MB and Go's 822 MB.

2. **High concurrent garbage (threads): Solar wins on throughput; Go and .NET
   collapse.** Solar (1.66 s) leads, ahead of Parallel (1.78 s), Shenandoah
   (2.02 s), and G1 (2.04 s), then C (3.42 s) and ZGC (3.8–4.2 s), **~6.4×
   faster than Go
   (10.67 s)** and **~32× faster than C# workstation (53.85 s)**. Two runtimes
   collapse for different reasons. Go's concurrent GC cannot keep pace with 16
   goroutines churning 1.6 billion short-lived nodes: mutators are conscripted
   into mark-assist and throttled, so throughput craters even though its pauses
   stay tiny. **.NET workstation GC is far worse** — its *single* GC heap
   serializes 16 allocating threads, so it spends ~87% of wall stopped in
   back-to-back ephemeral collections. **Server GC fixes most of that** (13.7 s,
   a per-core heap each) and reclaims promptly enough to post the third-leanest
   footprint of the field (**432 MB**, behind only Go's 83 MB and C's 99 MB),
   but it is
   still slower than Go. The catch for Solar is contention: it stops all 16
   mutators at each STW phase, so under load its pauses and `stall_for_gc`
   back-pressure inflate (the previous, loaded session measured 2.22 s with a
   14 ms worst pause; an earlier heavy-load session ~8 s / 86 ms).

3. **Pointer churn (splay): Solar is the only runtime that wins both throughput
   AND latency.** Solar is **fastest outright at 0.38 s** — ahead of Parallel
   (0.44 s), Go (0.50 s), G1 (0.63 s), Shenandoah (0.69 s), and **2.2× faster
   than C (0.82 s)**, which must walk and `free` every removed node's 63-object
   payload graph inline on the mutator. The interesting part is *how* each
   winner wins: Parallel nearly matches Solar's wall-clock but stalls the
   application
   **6.0 ms at the median pause** (blocking young-gen collections), while
   Solar's worst single stall is **0.20 ms** (p50 0.02 ms) — the best worst-case
   in the field this session, below even Go (0.67 ms) and in ZGC's class
   (0.02–0.03 ms), at the best throughput in the field. ZGC itself pays for its
   concurrency on
   this small hot heap (1.05–1.09 s, ~2.8× Solar), and C# workstation is
   pathological again (4.50 s with 141 ms stalls). Splay is also the benchmark
   that validates the write barrier under fire: every splay operation rewires
   `left`/`right` pointers of long-lived nodes during concurrent marking, and
   all five ports print the same checksum.

4. **Latency: ZGC and Shenandoah rule; Solar and Go are close; .NET is not.**
   Per individual pause Solar's stalls are small (allocs3 max 0.03 ms / p50
   0.01 ms, splay max 0.20 ms / p50 0.02 ms, threads max 4.6 ms / p50 0.23 ms) —
   on this idle session it matches or beats Go pause-for-pause on every
   benchmark (Go's threads max was 6.9 ms, splay max 0.67 ms), and only its
   16-thread stop handshake leaves a multi-millisecond tail.
   ZGC/Shenandoah stay sub-0.1 ms everywhere. **.NET
   sits at the other end with G1/Parallel**: its always-blocking ephemeral GCs
   give p50 stalls of ~16–33 ms on every benchmark, and a blocking gen2
   compaction spikes C# server's `allocs3` max to 374 ms. Solar trades a
   loaded-box threads tail for the best throughput; .NET pays both a worse tail
   *and* lower throughput here.

5. **Go is the latency/throughput inverse of the STW collectors.** It keeps
   pauses tiny by doing all reclamation concurrently — but on a high allocation
   rate that concurrency is *paid by the mutators* via mark-assist, so it posts
   the leanest `threads` footprint of the field (83 MB) and near-best latency
   yet a **very
   poor `threads` throughput** (10.67 s — though .NET holds the throughput
   floor). On the single-threaded benchmarks, where one thread can't outrun the
   GC, Go is competitive (2.28 s allocs3, 0.50 s splay) with sub-0.1 ms median
   pauses.

6. **C is fastest only when there is nothing to reclaim — and on these
   workloads there always is.** On allocs3 (never frees) C is still ~1.6× slower
   than Solar on `malloc` call overhead alone. On threads (3.42 s) and splay
   (0.82 s) C must `free` inline on the mutator — the reclamation
   Solar/Java/Go/.NET do concurrently or in bulk, serialized into the hot path.
   C's "GC pause" is `none` yet its wall-clock is mid-pack; its unambiguous win
   is footprint (99 MB threads / 48 MB splay).

**Net:** across these three workloads, on this box, Solar's non-moving
concurrent mark-sweep is **the outright throughput leader on two of three
benchmarks and second only to Shenandoah on the third** (0.77 vs 0.99 s on
allocs3) — beating C, Go, and both .NET GCs everywhere,
leading outright on the allocate-and-discard threads test and the
pointer-churning splay test. On this idle session its pause profile sits in
the ZGC/Go class on every benchmark (sub-0.25 ms worst on the single-threaded
ones, ~0.01–0.02 ms median); the remaining gap is the
16-thread stop handshake, whose worst-case stall rises with machine load
(4.6 ms here; 14 ms in the previous, loaded session). The comparison cleanly
separates the strategies: copying /
compacting collectors (G1, Parallel, and .NET's compacting gen2) choke on the
growing live set; a fully-concurrent collector tuned for latency (Go) chokes on
the allocation *rate*; a generational collector whose ephemeral GCs are
blocking (.NET) chokes on *both*, the worst of the field on the high-churn
threads test; and moving reclamation onto the mutator (C) or stopping the world
briefly (Solar) trades latency for the best throughput on high-churn
allocation — with splay showing Solar no longer has to make that trade on
single-threaded code.

(The "beating C everywhere" above is specifically vs the C ports' **glibc**
`malloc`/`free`. Swapping in a modern allocator changes the picture — see the
next section: jemalloc and mimalloc beat Solar's throughput on the two list
benchmarks.)

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
| *Solar (ref)*    | *0.99 s*     | *781 MB*    | *1.66 s*     | *2117 MB*   |

*Solar (ref)* is from the throughput table further up — a **different session**,
shown only for scale, not measured interleaved with these. ¹ bump never frees,
so `threads` RSS is the high-water of all ~1.6 B allocations (≈15 GB). ² not a
typo and not noise: tcmalloc_minimal is reproducibly ~90 s on `threads` across
all three rounds at 53 MB RSS — CPU/lock-bound, not swapping (see takeaway 4).

## Takeaways

1. **glibc is the slow one; the C *language* is not.** On `allocs3` (pure
   `malloc`, never frees) jemalloc/tcmalloc/mimalloc are **~3–4× faster than
   glibc** (0.56–0.73 s vs 2.35 s) and pack the 8-byte node at 8 bytes (~766 MB)
   instead of glibc's 32-byte minimum chunk (3 GB). So the earlier "C malloc is
   ~1.6× slower than Solar" result is a **glibc** result: against mimalloc the
   same
   benchmark is ~1.8× *faster* than Solar (0.56 s vs 0.99 s).

2. **mimalloc beats Solar on both benchmarks' throughput.** mimalloc is fastest
   everywhere (0.56 s / 0.85 s); jemalloc is close (0.73 s / 1.20 s). Both beat
   Solar's 0.99 s / 1.66 s. Solar's GC throughput lead held against glibc and Go,
   but a state-of-the-art thread-caching allocator that reuses freed memory is
   faster here — and on `threads` it does so at **~55 MB vs Solar's 2117 MB**,
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
throughput, and on the retained `allocs3` chain an RSS (781 MB) on par with the
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
