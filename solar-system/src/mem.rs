use std::alloc::Layout;
use std::sync::atomic::Ordering;

use crate::gc::{
    BigAllocLocal, ENABLE_ALLOC_PRINTS, SOL_CONCURRENT_MARKING, ThreadAllocState, ThreadClassState,
    note_claimed, with_thread_slot,
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
pub unsafe extern "C" fn sol_alloc_impl(size: usize, align: usize, mark_fn: MarkFn) -> *mut u8 {
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
        with_thread_slot(|slot| {
            let state = &mut *slot.alloc.get();
            let addr = if CLASS == -1 {
                match heap::size_class(size, align) {
                    Some(class) => arena_allocate::<-1>(state, class, size, mark_fn),
                    None => big_allocate(state, size, align, mark_fn),
                }
            } else {
                arena_allocate::<CLASS>(state, CLASS as usize, size, mark_fn)
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
    (sol_alloc_class_0_impl, 0),
    (sol_alloc_class_1_impl, 1),
    (sol_alloc_class_2_impl, 2),
    (sol_alloc_class_3_impl, 3),
    (sol_alloc_class_4_impl, 4),
    (sol_alloc_class_5_impl, 5),
    (sol_alloc_class_6_impl, 6),
    (sol_alloc_class_7_impl, 7),
    (sol_alloc_class_8_impl, 8),
    (sol_alloc_class_9_impl, 9),
    (sol_alloc_class_10_impl, 10),
    (sol_alloc_class_11_impl, 11),
    (sol_alloc_class_12_impl, 12),
    (sol_alloc_class_13_impl, 13),
    (sol_alloc_class_14_impl, 14),
    (sol_alloc_class_15_impl, 15),
    (sol_alloc_class_16_impl, 16),
    (sol_alloc_class_17_impl, 17),
    (sol_alloc_class_18_impl, 18),
    (sol_alloc_class_19_impl, 19),
    (sol_alloc_class_20_impl, 20),
    (sol_alloc_class_21_impl, 21),
    (sol_alloc_class_22_impl, 22),
    (sol_alloc_class_23_impl, 23),
    (sol_alloc_class_24_impl, 24),
    (sol_alloc_class_25_impl, 25),
    (sol_alloc_class_26_impl, 26),
    (sol_alloc_class_27_impl, 27),
);

/// Allocate `size` bytes (rounded up to a power-of-2 size class) from the
/// arena. Returns a correctly-aligned pointer to **uninitialized** memory; the
/// caller (codegen) zeroes it with an explicit `memset` that LLVM can elide.
/// `CLASS == -1` uses the runtime class; fixed classes stay specialized through
/// the cold refill, without requiring that refill to inline into the fast path.
unsafe fn arena_allocate<const CLASS: isize>(
    state: &mut ThreadAllocState,
    class: usize,
    size: usize,
    mark_fn: MarkFn,
) -> *mut u8 {
    let class = if CLASS == -1 { class } else { CLASS as usize };
    let cs = &mut state.classes[class];
    if cs.cache_index == 64 {
        unsafe { refill_cache::<CLASS>(cs, class) };
    }
    let addr = cs.cache[cs.cache_index];
    cs.cache_index += 1;
    let rbase = heap::region_base(class);
    let slot = heap::slot_index(addr, rbase, class);

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

/// Scan only on cache exhaustion. Claims contain whole bitmap words and are
/// private to this mutator until GC resets both the claim and its cache.
#[cold]
unsafe fn refill_cache<const CLASS: isize>(cs: &mut ThreadClassState, class: usize) {
    let class = if CLASS == -1 { class } else { CLASS as usize };
    loop {
        if cs.cur == cs.end {
            (cs.cur, cs.end) = heap::claim_run(class);
        }
        let word_slot = cs.cur as usize;
        cs.cur += 64;
        let allocated = unsafe { heap::alloc_word_load(class, word_slot >> 6) };
        if allocated == u64::MAX {
            continue;
        }
        let base = heap::slot_addr(heap::region_base(class), word_slot, class);
        fill_cache::<CLASS>(cs, base, class, !allocated);
        return;
    }
}

const CACHE_CHUNK_BITS: usize = 4;
const CACHE_CHUNK_MASKS: usize = 1 << CACHE_CHUNK_BITS;

/// Write a nibble's free slots, with both its class and bitmap known at compile time.
/// The caller must provide an index in 4..=64, below previously packed entries.
#[inline(always)]
unsafe fn fill_chunk<const CLASS: isize, const MASK: u8>(
    cache: &mut [usize; 64],
    base: usize,
    class: usize,
    mut index: usize,
) -> usize {
    let class = if CLASS == -1 { class } else { CLASS as usize };
    for bit in (0..CACHE_CHUNK_BITS).rev() {
        if MASK & (1 << bit) != 0 {
            index -= 1;
            // SAFETY: the caller provides room for four entries, and the
            // compile-time mask selects at most four decrements/stores.
            unsafe {
                *cache.get_unchecked_mut(index) = base + (bit << heap::slot_size_log(class));
            }
        }
    }
    index
}

fn fill_cache<const CLASS: isize>(cs: &mut ThreadClassState, base: usize, class: usize, free: u64) {
    let class = if CLASS == -1 { class } else { CLASS as usize };
    if free == u64::MAX {
        // Fresh words need no bit scanning; this regular fill can vectorize.
        for (bit, address) in cs.cache.iter_mut().enumerate().rev() {
            *address = base + (bit << heap::slot_size_log(class));
        }
        cs.cache_index = 0;
        return;
    }
    let mut index = 64;
    for chunk in (0..64 / CACHE_CHUNK_BITS).rev() {
        let bit = chunk * CACHE_CHUNK_BITS;
        let mask = ((free >> bit) & (CACHE_CHUNK_MASKS as u64 - 1)) as u8;
        let chunk_base = base + (bit << heap::slot_size_log(class));
        // SAFETY: before chunk k, at most 4*k slots have been packed, so
        // 64 - 4*k <= index <= 64 and there is room for this entire chunk.
        index = unsafe {
            match mask {
                0 => fill_chunk::<CLASS, 0>(&mut cs.cache, chunk_base, class, index),
                1 => fill_chunk::<CLASS, 1>(&mut cs.cache, chunk_base, class, index),
                2 => fill_chunk::<CLASS, 2>(&mut cs.cache, chunk_base, class, index),
                3 => fill_chunk::<CLASS, 3>(&mut cs.cache, chunk_base, class, index),
                4 => fill_chunk::<CLASS, 4>(&mut cs.cache, chunk_base, class, index),
                5 => fill_chunk::<CLASS, 5>(&mut cs.cache, chunk_base, class, index),
                6 => fill_chunk::<CLASS, 6>(&mut cs.cache, chunk_base, class, index),
                7 => fill_chunk::<CLASS, 7>(&mut cs.cache, chunk_base, class, index),
                8 => fill_chunk::<CLASS, 8>(&mut cs.cache, chunk_base, class, index),
                9 => fill_chunk::<CLASS, 9>(&mut cs.cache, chunk_base, class, index),
                10 => fill_chunk::<CLASS, 10>(&mut cs.cache, chunk_base, class, index),
                11 => fill_chunk::<CLASS, 11>(&mut cs.cache, chunk_base, class, index),
                12 => fill_chunk::<CLASS, 12>(&mut cs.cache, chunk_base, class, index),
                13 => fill_chunk::<CLASS, 13>(&mut cs.cache, chunk_base, class, index),
                14 => fill_chunk::<CLASS, 14>(&mut cs.cache, chunk_base, class, index),
                15 => fill_chunk::<CLASS, 15>(&mut cs.cache, chunk_base, class, index),
                _ => unreachable!(),
            }
        };
    }
    cs.cache_index = index;
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
pub unsafe extern "C-unwind" fn sol_slice_range_slow(
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
pub unsafe extern "C-unwind" fn sol_slice_index_slow(
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
pub unsafe extern "C-unwind" fn sol_null_check_slow(ptr: *const u8) -> *const u8 {
    if ptr.is_null() {
        crate::panic::throw_str("null dereference");
    }
    ptr
}

/// Array length check backing both array destructuring and the `[T]` → `[T; N]`
/// coercion (`ArraySizeCoerce`).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn sol_assert_array_len_slow(actual: u64, expected: u64) {
    if actual != expected {
        crate::panic::throw_message(format_args!(
            "array length mismatch: expected {expected} elements, got {actual}"
        ));
    }
}

/// Throws the canonical runtime error for assigning a different-length slice
/// into an existing unsized array value.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn sol_assert_unsized_assignment_len_slow(target: u64, value: u64) -> ! {
    crate::panic::throw_message(format_args!(
        "unsized assignment: length mismatch ({target} vs {value})"
    ));
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn cached_addresses_cover_only_free_slots_in_ascending_order() {
        let mut state = ThreadAllocState::new();
        for (class, cs) in state.classes.iter_mut().enumerate() {
            let base = 128 * heap::slot_size(class);
            let chunk_masks = (0..64 / CACHE_CHUNK_BITS).flat_map(|chunk| {
                (0..CACHE_CHUNK_MASKS as u64).flat_map(move |mask| {
                    let free = mask << (chunk * CACHE_CHUNK_BITS);
                    [free, !free]
                })
            });
            for free in [u64::MAX, 0, 1, 1 << 63, 0xaaaa_5555_8000_0001]
                .into_iter()
                .chain(chunk_masks)
            {
                fill_cache::<-1>(cs, base, class, free);
                let expected: Vec<_> = (0..64)
                    .filter(|bit| free & (1 << bit) != 0)
                    .map(|bit| base + bit * heap::slot_size(class))
                    .collect();
                assert_eq!(&cs.cache[cs.cache_index..], expected);
            }
        }
    }

    #[test]
    fn resetting_claims_discards_partially_consumed_caches() {
        let mut state = ThreadAllocState::new();
        for (class, cs) in state.classes.iter_mut().enumerate() {
            cs.cur = 64;
            cs.end = 128;
            fill_cache::<-1>(cs, 128 * heap::slot_size(class), class, u64::MAX);
            cs.cache_index += 7;
        }
        state.reset_claims();
        for cs in &state.classes {
            assert_eq!((cs.cur, cs.end, cs.cache_index), (0, 0, 64));
        }
    }
}
