# Native runtime

`solar-system` is linked into native Solar programs. `CompileOptions::enable_gc`
selects the concurrent collector; builds without it disable collection and
bump-allocate. This choice is independent of LLVM optimization and LTO.

## Collector invariants

The collector cycle is:

```text
STW root scan -> concurrent mark -> STW remark -> concurrent arena sweep
-> short STW frontier reset
```

- New allocations are born black during concurrent marking.
- Heap pointer stores use the Dijkstra insertion barrier.
- Stack and static destinations are rescanned at remark and do not need write
  barriers.
- Arena sweeping and mutator allocation must operate on disjoint bitmap words.
- GC-San clears dead allocation bits but never moves an arena allocation
  frontier backward, keeping swept addresses permanently poisoned.
- A mutator may only call `sol_alloc` after thread registration.
- Generated descriptors for thread-local static cells are attached during
  thread registration and scanned with that thread's stack roots.
- Thread-local static cells have process lifetime: their descriptors move to a
  retired root list when the owning thread exits, preserving escaped references.
- Runtime code that constructs GC objects manually must initialize pointer
  storage before the object can be traced, pass the correct mark function, and
  preserve roots across further allocations.
- Do not unwind out of a signal-deferred or GC critical section.

## File descriptors

`FileDesc` values are traced handles into the fd arena. Unreachable registered
descriptors are closed during sweep. `sol_file_close` replaces a descriptor
with the dead fd instead of releasing its number, preventing stale handles from
aliasing a later open. Standard descriptors are not registered and therefore
are never auto-closed.

Sockets use the same descriptor arena and lifecycle.

## Exceptions

User-facing runtime failures throw catchable Solar exceptions. Throwing
functions crossing C frames must use `extern "C-unwind"`. Uncaught throws abort
at the throw site.

Exception text is part of the cross-backend contract. Keep native, AST
interpreter, and IR interpreter messages byte-identical.

## Process data

`sol_args` and `sol_env` copy nested byte slices into GC-managed memory. Their
outer pointer arrays must be zeroed before any nested allocation can trigger a
collection. Interpreters return empty process data.

## Diagnostics

These variables affect compiled binaries:

- `SOLAR_PRINT_GC_STATS=1`
- `SOLAR_PRINT_ALLOCS=1`
- `SOLAR_DISABLE_GC=1`

Build the release runtime with:

```bash
cargo build --release -p solar-system
```
