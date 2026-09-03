use std::alloc::Layout;
use std::sync::atomic::Ordering;

use crate::gc::{
    BigAllocLocal, ENABLE_ALLOC_PRINTS, SOL_CONCURRENT_MARKING, ThreadAllocState, note_claimed,
    with_signal_deferred,
};
use crate::heap;

/// Function used by the collector to trace an allocation.
pub type MarkFn = unsafe extern "C" fn(*mut u8, *mut u8, u64);

/// Prevents the optimizer from treating a Solar value as statically known.
#[unsafe(no_mangle)]
pub extern "C" fn sol_black_box_ref(value: *mut u8) {
    let _ = std::hint::black_box(value);
}

/// Keeps a GC reference materialized until this call returns.
///
/// The collector conservatively scans registers captured by its suspension
/// signal as well as the stack. Keeping this function out of line forces the
/// reference through the native calling convention, while the side-effecting
/// assembly operand forces LLVM to materialize it in a register at the fence.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn sol_gc_keepalive(value: *mut u8) {
    // SAFETY: the empty assembly has no machine-level effects. Its input
    // operand is the effect: LLVM must make `value` available in a register,
    // which the collector's suspension signal captures and scans.
    unsafe {
        std::arch::asm!(
            "/* {value} */",
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}

/// Allocates uninitialized GC-managed memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_alloc(size: usize, align: usize, mark_fn: MarkFn) -> *mut u8 {
    unsafe { alloc_in_class::<-1>(size, align, mark_fn) }
}

#[inline(always)]
unsafe fn alloc_in_class<const CLASS: isize>(
    size: usize,
    align: usize,
    mark_fn: MarkFn,
) -> *mut u8 {
    debug_assert!(CLASS == -1 || heap::size_class(size, align) == Some(CLASS as usize));
    if ENABLE_ALLOC_PRINTS.get() {
        eprintln!("allocating new object: {size} bytes (align={align})");
    }

    unsafe {
        with_signal_deferred(|slot| {
            let state = &mut *slot.alloc.get();
            let addr = if CLASS == -1 {
                match heap::size_class(size, align) {
                    Some(class) => arena_allocate(state, class, size, mark_fn),
                    None => big_allocate(state, size, align, mark_fn),
                }
            } else {
                arena_allocate(state, CLASS as usize, size, mark_fn)
            };
            account_alloc(state);
            addr
        })
    }
}

macro_rules! class_allocators {
    ($(($name:ident, $class:literal)),* $(,)?) => {$(
        #[doc = "Compiler-only allocation entry point for a fixed arena class."]
        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[inline(never)]
        pub unsafe extern "C" fn $name(
            size: usize,
            align: usize,
            mark_fn: MarkFn,
        ) -> *mut u8 {
            unsafe { alloc_in_class::<$class>(size, align, mark_fn) }
        }
    )*};
}

class_allocators!(
    (sol_alloc_class_0, 0),
    (sol_alloc_class_1, 1),
    (sol_alloc_class_2, 2),
    (sol_alloc_class_3, 3),
    (sol_alloc_class_4, 4),
    (sol_alloc_class_5, 5),
    (sol_alloc_class_6, 6),
    (sol_alloc_class_7, 7),
    (sol_alloc_class_8, 8),
    (sol_alloc_class_9, 9),
    (sol_alloc_class_10, 10),
    (sol_alloc_class_11, 11),
    (sol_alloc_class_12, 12),
    (sol_alloc_class_13, 13),
    (sol_alloc_class_14, 14),
    (sol_alloc_class_15, 15),
    (sol_alloc_class_16, 16),
    (sol_alloc_class_17, 17),
    (sol_alloc_class_18, 18),
    (sol_alloc_class_19, 19),
    (sol_alloc_class_20, 20),
    (sol_alloc_class_21, 21),
    (sol_alloc_class_22, 22),
    (sol_alloc_class_23, 23),
    (sol_alloc_class_24, 24),
    (sol_alloc_class_25, 25),
    (sol_alloc_class_26, 26),
    (sol_alloc_class_27, 27),
);

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

/// Offsets a reference address by `offset` objects of `unit_size` bytes.
///
/// GC-San additionally requires a managed source and result to belong to the
/// same allocation. Addresses outside the managed heap have no runtime
/// allocation metadata and retain unchecked pointer-arithmetic semantics.
#[unsafe(no_mangle)]
pub extern "C" fn sol_offset_ref(address: *mut u8, offset: i64, unit_size: usize) -> *mut u8 {
    let byte_offset = offset.wrapping_mul(unit_size as i64) as isize;
    let result = address.wrapping_offset(byte_offset);

    if crate::gc::GC_SAN.get() {
        gc_san_assert_same_allocation(address as usize, result as usize);
    }

    result
}

fn gc_san_assert_same_allocation(source: usize, destination: usize) {
    if heap::classify(source).is_some() {
        let source_allocation = unsafe { heap::lookup_arena(source) };
        let Some((source_class, source_slot, _, _)) = source_allocation else {
            panic!("GC-San: offset_ref source is not in a live allocation at {source:#x}");
        };
        let destination_allocation = unsafe { heap::lookup_arena(destination) };
        let same_allocation = destination_allocation
            .is_some_and(|(class, slot, _, _)| class == source_class && slot == source_slot);
        assert!(
            same_allocation,
            "GC-San: offset_ref result at {destination:#x} is outside its source allocation at {source:#x}"
        );
        return;
    }

    if let Some(same_allocation) = unsafe { crate::gc::same_big_allocation(source, destination) } {
        assert!(
            same_allocation,
            "GC-San: offset_ref result at {destination:#x} is outside its source allocation at {source:#x}"
        );
    }
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
