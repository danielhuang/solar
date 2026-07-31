use std::alloc::Layout;
use std::sync::atomic::Ordering;

use crate::gc::{
    BigAllocLocal, ENABLE_ALLOC_PRINTS, MY_SLOT, SOL_CONCURRENT_MARKING, ThreadAllocState,
    note_claimed, with_signal_deferred,
};
use crate::heap;

/// Function used by the collector to trace an allocation.
pub type MarkFn = unsafe extern "C" fn(*mut u8, *mut u8, u64);

/// Allocates uninitialized GC-managed memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_alloc(size: usize, align: usize, mark_fn: MarkFn) -> *mut u8 {
    let slot_ptr = MY_SLOT.get();
    assert!(
        !slot_ptr.is_null(),
        "sol_alloc called on unregistered thread"
    );
    let slot = unsafe { &*slot_ptr };

    // Avoid holding a mutable reference across a GC suspension.
    let alloc_ptr = slot.alloc.get();

    if ENABLE_ALLOC_PRINTS.get() {
        eprintln!("allocating new object: {size} bytes (align={align})");
    }

    let addr = unsafe {
        with_signal_deferred(slot, || match heap::size_class(size, align) {
            Some(class) => arena_allocate(&mut *alloc_ptr, class, size, mark_fn),
            None => big_allocate(&mut *alloc_ptr, size, align, mark_fn),
        })
    };

    unsafe {
        account_alloc(&mut *alloc_ptr);
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
    // Scan one allocation-bitmap word per recycled run segment.
    let slot = 'find: loop {
        let cs = &mut state.classes[class];
        while cs.cur < cs.end {
            let cur = cs.cur;
            let w = unsafe { heap::alloc_word_load(class, (cur >> 6) as usize) };
            if w & (1 << (cur & 63)) == 0 {
                cs.cur = cur + 1;
                break 'find cur as usize;
            }
            let free = !w & (u64::MAX << (cur & 63));
            if free != 0 {
                let s = (cur & !63) + free.trailing_zeros() as u64;
                cs.cur = s + 1;
                break 'find s as usize;
            }
            cs.cur = (cur | 63) + 1;
        }
        let (s, e) = heap::claim_run(class);
        cs.cur = s;
        cs.end = e;
    };

    let rbase = heap::region_base(class);
    let addr = heap::slot_addr(rbase, slot, class);

    // Publish metadata before the allocation bit.
    if class >= heap::META_MIN_CLASS {
        let m = unsafe { &mut *heap::meta_entry(class, slot) };
        m.mark_fn = mark_fn as usize;
        m.size = size as u64;
    }
    unsafe { heap::set_allocated(class, slot) };

    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { heap::set_marked(class, slot) };
    }

    addr as *mut u8
}

/// Record `bytes` of allocation against the trigger counter and, in batches,
/// the global back-pressure counter (`ALLOCATED_SINCE_GC`). Batching keeps the
/// global atomic off the per-allocation hot path.
#[inline]
fn account_alloc(state: &mut ThreadAllocState) {
    state.total_allocations += 1;
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

/// Copies possibly-overlapping pointer-free bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_memcpy(dst: *mut u8, src: *const u8, size: usize) {
    unsafe { std::ptr::copy(src, dst, size) };
}

/// Checks a slice range and returns its starting address.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_slice_range(
    base: *const u8,
    start: u64,
    end: u64,
    len: u64,
    elem_size: u64,
) -> *const u8 {
    if start > end {
        crate::panic::throw_message(format_args!("slice start ({start}) > end ({end})"));
    }
    if end > len {
        crate::panic::throw_message(format_args!("slice end ({end}) > length ({len})"));
    }
    let offset = start.checked_mul(elem_size).expect("slice offset overflow");
    unsafe { base.add(offset as usize) }
}

/// Checks a slice index and returns the element address.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_slice_index(
    base: *const u8,
    index: u64,
    len: u64,
    elem_size: u64,
) -> *const u8 {
    if index >= len {
        crate::panic::throw_message(format_args!(
            "index out of bounds: index is {index} but length is {len}"
        ));
    }
    let offset = index.checked_mul(elem_size).expect("index overflow");
    unsafe { base.add(offset as usize) }
}

/// Null check for dereferencing a nullable reference (`&?T`). Throws a Solar
/// exception if the pointer is null; otherwise returns it unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_null_check(ptr: *const u8) -> *const u8 {
    if ptr.is_null() {
        crate::panic::throw_str("null dereference");
    }
    ptr
}

/// Array length check backing both array destructuring and the `[T]` → `[T; N]`
/// coercion (`ArraySizeCoerce`).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn sol_assert_array_len(actual: u64, expected: u64) {
    if actual != expected {
        crate::panic::throw_message(format_args!(
            "array length mismatch: expected {expected} elements, got {actual}"
        ));
    }
}
