use std::alloc::Layout;
use std::sync::atomic::Ordering;

use crate::gc::{
    ALLOC_FLUSH_CHUNK, ALLOCATED_SINCE_GC, BigAllocLocal, ENABLE_ALLOC_PRINTS, MY_SLOT,
    SOL_CONCURRENT_MARKING, ThreadAllocState, note_claimed, with_signal_deferred,
};
use crate::heap;

pub type MarkFn = unsafe extern "C" fn(*mut u8, *mut u8, u64);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_alloc(size: usize, align: usize, mark_fn: MarkFn) -> *mut u8 {
    // Per-thread slot (stable across GC).
    let slot_ptr = MY_SLOT.get();
    assert!(
        !slot_ptr.is_null(),
        "sol_alloc called on unregistered thread"
    );
    let slot = unsafe { &*slot_ptr };

    // Per-thread alloc state via raw pointer so that no &mut to it exists
    // across a GC suspension (the GC thread mutates it at STW pauses). The GC
    // trigger itself is not checked here: it lives in `heap::claim_run` /
    // `big_allocate` (via `gc::note_claimed`), off the per-allocation path.
    let alloc_ptr = slot.alloc.get();

    if ENABLE_ALLOC_PRINTS.get() {
        eprintln!("allocating new object: {size} bytes (align={align})");
    }

    // Update per-thread structures inside a GC critical section so the signal
    // handler defers rather than interrupting mid-update; if a stop arrived
    // meanwhile, `with_signal_deferred` self-suspends cleanly afterwards.
    let addr = unsafe {
        with_signal_deferred(slot, || match heap::size_class(size, align) {
            Some(class) => arena_allocate(&mut *alloc_ptr, class, size, mark_fn),
            None => big_allocate(&mut *alloc_ptr, size, align, mark_fn),
        })
    };

    unsafe {
        account_alloc(&mut *alloc_ptr, size);
    }

    addr
}

/// Allocate `size` bytes (rounded up to a power-of-2 size class) from the
/// arena. Returns a correctly-aligned pointer to **uninitialized** memory; the
/// caller (codegen) zeroes it with an explicit `memset` that LLVM can elide.
unsafe fn arena_allocate(
    state: &mut ThreadAllocState,
    class: usize,
    size: usize,
    mark_fn: MarkFn,
) -> *mut u8 {
    // Find a free slot in the current claim; claim a fresh run when exhausted.
    let slot = 'find: loop {
        let cs = &mut state.classes[class];
        while cs.cur < cs.end {
            let s = cs.cur as usize;
            cs.cur += 1;
            if !unsafe { heap::is_allocated(class, s) } {
                break 'find s;
            }
        }
        let (s, e) = heap::claim_run(class);
        cs.cur = s;
        cs.end = e;
    };

    let rbase = heap::region_base(class);
    let addr = heap::slot_addr(rbase, slot, class);

    // No zeroing here: codegen emits an explicit `memset(p, 0, size)` after every
    // `sol_alloc` call, which LLVM dead-store-eliminates wherever the caller fully
    // overwrites the object before it escapes, and keeps for any field left
    // unwritten (so the GC never traces an uninitialized pointer field). Recycled
    // slots therefore arrive non-zero; the caller's memset zeroes them.

    // Write metadata before publishing the allocated bit so any GC scan that
    // sees the slot as allocated also sees valid metadata.
    if class >= heap::META_MIN_CLASS {
        let m = unsafe { &mut *heap::meta_entry(class, slot) };
        m.mark_fn = mark_fn as usize;
        m.size = size as u64;
    }
    unsafe { heap::set_allocated(class, slot) };

    // Allocate black: an object born during concurrent marking is marked live
    // immediately, so the stop-the-world sweep at the end of the cycle never
    // reclaims it. Its (zeroed) fields are filled by barriered stores, so its
    // outgoing pointers are still shaded.
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { heap::set_marked(class, slot) };
    }

    addr as *mut u8
}

/// Record `bytes` of allocation against the trigger counter and, in batches,
/// the global back-pressure counter (`ALLOCATED_SINCE_GC`). Batching keeps the
/// global atomic off the per-allocation hot path.
#[inline]
fn account_alloc(state: &mut ThreadAllocState, bytes: usize) {
    state.total_allocations += 1;
    state.unflushed_alloc += bytes;
    if state.unflushed_alloc >= ALLOC_FLUSH_CHUNK {
        ALLOCATED_SINCE_GC.fetch_add(state.unflushed_alloc, Ordering::Relaxed);
        state.unflushed_alloc = 0;
    }
}

/// Allocate a >1 GiB object via the system allocator and record it in the
/// thread-local big-alloc list (merged into the global registry at the next
/// STW). Returns zeroed memory.
unsafe fn big_allocate(
    state: &mut ThreadAllocState,
    size: usize,
    align: usize,
    mark_fn: MarkFn,
) -> *mut u8 {
    // Big allocations never go through `claim_run`, so feed the claim-based GC
    // trigger directly — otherwise a big-object-only workload would never
    // request a cycle.
    note_claimed(size);
    let layout = Layout::from_size_align(size.max(1), align.max(1)).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "big allocation of {size} bytes failed");
    state.big_allocs.push(BigAllocLocal {
        base: ptr as usize,
        size,
        align,
        mark_fn: mark_fn as usize,
    });
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_memcpy(dst: *mut u8, src: *const u8, size: usize) {
    // A plain copy with no GC side effects: keeping the body free of global
    // reads/writes is what lets the optimizer prove sol_memcpy doesn't capture
    // or escape its arguments, so freshly-allocated objects initialized through
    // it can still be SROA'd / elided. Codegen emits sol_memcpy ONLY for
    // pointer-free bytes (GC-pointer words are copied with typed `uint8_t*`
    // member stores, which the write-barrier pass instruments precisely), so
    // the `solar-lower-gc-alloc` pass tags its lowered `llvm.memcpy` with
    // `!solar.nobarrier` and plain-data copies (e.g. `[Uint8]` contents) carry
    // no barrier at all.
    unsafe { std::ptr::copy_nonoverlapping(src, dst, size) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_slice_range(
    base: *const u8,
    start: u64,
    end: u64,
    len: u64,
    elem_size: u64,
) -> *const u8 {
    assert!(start <= end, "slice start ({start}) > end ({end})");
    assert!(end <= len, "slice end ({end}) > length ({len})");
    let offset = start.checked_mul(elem_size).expect("slice offset overflow");
    unsafe { base.add(offset as usize) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_slice_index(
    base: *const u8,
    index: u64,
    len: u64,
    elem_size: u64,
) -> *const u8 {
    assert!(
        index < len,
        "index out of bounds: index is {index} but length is {len}"
    );
    let offset = index.checked_mul(elem_size).expect("index overflow");
    unsafe { base.add(offset as usize) }
}

/// Null check for dereferencing a nullable reference (`&?T`). Panics if the
/// pointer is null; otherwise returns it unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_null_check(ptr: *const u8) -> *const u8 {
    assert!(!ptr.is_null(), "null reference dereference");
    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn sol_assert_array_len(actual: u64, expected: u64) {
    assert!(
        actual == expected,
        "array destructure: expected {expected} elements, got {actual}"
    );
}
