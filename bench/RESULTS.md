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

(The "beating C on both" above is specifically vs the C port's **glibc**
`malloc`/`free`. Swapping in a modern allocator changes the picture — see the
next section: jemalloc and mimalloc beat Solar's throughput on both benchmarks.)

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
| *Solar (ref)*    | *1.08 s*     | *785 MB*    | *1.94 s*     | *1183 MB*   |

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
   ~2× slower than Solar" result is a **glibc** result: against mimalloc the same
   benchmark is ~2× *faster* than Solar (0.56 s vs 1.08 s).

2. **mimalloc beats Solar on both benchmarks' throughput.** mimalloc is fastest
   everywhere (0.56 s / 0.85 s); jemalloc is close (0.73 s / 1.20 s). Both beat
   Solar's 1.08 s / 1.94 s. Solar's GC throughput lead held against glibc and Go,
   but a state-of-the-art thread-caching allocator that reuses freed memory is
   faster here — and on `threads` it does so at **~55 MB vs Solar's 1183 MB**,
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
throughput, and on the retained `allocs3` chain an RSS (785 MB) on par with the
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
| u64       | 202.2 | 103.4 | 1.96x | 78.5  | 52.9 | yes |
| u32       | 205.2 | 101.1 | 2.03x | 78.7  | 52.9 | yes |
| point     | 399.1 | 139.6 | 2.86x | 191.5 | 76.8 | yes |
| mixed     | 379.4 | 144.9 | 2.62x | 195.4 | 76.8 | yes |
| **total** | **1185.8** | **489.0** | **2.43x** | | | |

(This started at 6.99x total / 175 MB on `u64`. A series of changes brought it to
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

1. **~7× slower, ~3× more memory.** The gap is the cost of Solar's
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
