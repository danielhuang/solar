# LLVM GC passes

`SolarWriteBarriers.cpp` builds as an LLVM pass plugin from the repository
`build.rs`. Build it against the same LLVM version used by `opt`.

## Release order

1. `solar-lower-gc-alloc`
2. LLVM `-O3`
3. `solar-write-barriers`
4. final LTO link

`solar-lower-gc-alloc` replaces generated `sol_alloc(size, align, mark_fn)`
calls with recognized `aligned_alloc` calls carrying `!solar.alloc` metadata.
LLVM attributes on `sol_alloc` enable most allocation optimization, but are not
equivalent to a recognized allocator for every LLVM 22 transform. The pass also
lowers pointer-free, overlap-safe `sol_memcpy` calls to
`llvm.memmove !solar.nobarrier`.

Preserve these invariants:

- Allocation metadata contains the alignment and mark function.
- Allocation calls use `nomerge`; losing metadata must fail compilation rather
  than silently allocate outside the GC.
- Mark functions referenced only by metadata remain in `llvm.compiler.used`
  until allocations are restored.
- Solar copies may overlap, so lowering must use memmove rather than memcpy.

`solar-write-barriers` restores surviving allocations to `sol_alloc`, then:

- instruments non-stack/global scalar pointer stores precisely;
- uses the bulk barrier for stores wider than one pointer;
- instruments untagged memcpy/memmove operations;
- skips `!solar.nobarrier` transfers.

Barriers run after `-O3` so they do not inhibit allocation elimination. The
inserted calls must inherit valid debug locations.

Debug codegen does not run these passes and disables collection.
