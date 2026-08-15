# LLVM GC passes

`SolarWriteBarriers.cpp` builds as an LLVM pass plugin from the repository
`build.rs`. Build it against the same LLVM version used by `opt`.

## Release order

1. `solar-specialize-gc-alloc`
2. LLVM `-O3`
3. `solar-write-barriers`
4. final LTO link

`solar-specialize-gc-alloc` redirects constant request size/alignment pairs to
a const-generic runtime entry point for the resulting arena class. The entry
point retains the allocator ABI and `noinline`, while LLVM folds class-dependent
address calculations. The pass also lowers pointer-free `sol_memcpy` calls to
`llvm.memmove !solar.nobarrier`.

Preserve these invariants:

- Dynamic-size allocations continue to call the generic `sol_alloc`.
- Fixed-class allocators must not be inlined into generated functions: the
  write barrier pass distinguishes runtime allocator stores by function ownership.
- Solar copies may overlap, so lowering must use memmove rather than memcpy.

`solar-write-barriers`:

- instruments non-stack/global scalar pointer stores precisely;
- uses the bulk barrier for stores wider than one pointer;
- instruments untagged memcpy/memmove operations;
- skips `!solar.nobarrier` transfers.

Barriers run after `-O3` so they do not inhibit allocation elimination. The
inserted calls must inherit valid debug locations.

Debug codegen does not run these passes and disables collection.
