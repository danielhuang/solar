# Native runtime

`solar-system` is linked into native Solar programs. `CompileOptions::enable_gc`
selects the concurrent collector; builds without it disable collection and
bump-allocate. This choice is independent of LLVM optimization and LTO.

Cargo release LTO is disabled. The workspace's `-Clinker-plugin-lto` rustflag
still emits `solar-system` archive members as LLVM bitcode so the optimized
Solar pipeline can merge them with generated code and perform its own
cross-language optimization.

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
- `sol_offset_ref` checks under GC-San that managed source and result addresses
  belong to the same allocation; unmanaged addresses retain unchecked pointer
  arithmetic semantics.
- A mutator may only call `sol_alloc` after thread registration.
- Generated descriptors for thread-local static cells are attached during
  thread registration and scanned with that thread's stack roots.
- Thread-local static cells have process lifetime: their descriptors move to a
  retired root list when the owning thread exits, preserving escaped references.
- Runtime code that constructs GC objects manually must initialize pointer
  storage before the object can be traced, pass the correct mark function, and
  preserve roots across further allocations.
- Do not unwind out of a signal-deferred or GC critical section.

## Finalizers and explicit collection

`Finalizer` holds a reference to a fresh, immutable heap `fn()` record. Its
runtime registration is weak until finalization becomes eligible. After ordinary
remark, snapshot all unreachable registrations before marking any callback
captures, then trace their entire graphs before fd, big-object, or arena sweep.
Queued and running callback records are explicit roots at both root scans.
After sweep, callbacks run on registered mutator threads with normal thread-local
initialization. Remove each registration after its callback returns; resurrection
does not re-register it. Callbacks have no ordering guarantee and do not have to
finish before process exit. Uncaught exceptions follow spawned-thread behavior.

The collector conservatively scans stacks and arena slots smaller than 128
bytes. Stale references, including inactive small-enum payloads, can delay
finalization; type layout alone never registers a callback.

`request_gc` is nonblocking and does nothing when collection is disabled.
`collect_gc` obtains a new request ticket and waits for a cycle servicing that
ticket to finish; it throws when collection is disabled. Completion is published
after sweep and frontier reset, independently of asynchronous callback execution,
so callbacks may themselves collect. Waiting mutators keep GC signals enabled.

## File descriptors

`FileDesc` values are traced handles into the fd arena. Unreachable registered
descriptors are closed during sweep. `sol_file_close` replaces a descriptor
with the dead fd instead of releasing its number, preventing stale handles from
aliasing a later open. Standard descriptors are not registered and therefore
are never auto-closed.

`sol_fd_from_raw` transfers a newly owned raw descriptor into the arena and
registers it for GC closure. `sol_fd_to_raw` only borrows the descriptor number;
it does not remove the handle from GC ownership.

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
