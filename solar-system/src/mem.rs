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
    unsafe { alloc_in_class::<-1, 1>(size, align, mark_fn)[0] }
}

/// C-compatible carrier for an array returned by a batch allocator.
#[repr(C)]
pub struct AllocBatch<const BATCH: usize> {
    /// Distinct addresses of uninitialized GC-managed objects.
    pub addresses: [*mut u8; BATCH],
}

// Reuse the array implementation for both dynamic and fixed size classes.
// Each fixed class expands inside its own scope; export_name supplies the ABI.
macro_rules! batch_allocator {
    ($name:ident, $batch:literal, $class:expr, $prefix:expr) => {
        #[doc = concat!("Allocates ", stringify!($batch), " distinct, uninitialized objects with the same layout.")]
        ///
        /// # Safety
        /// The layout and mark function must satisfy `sol_alloc_impl`'s
        /// requirements. Initialize all returned objects before a GC safepoint.
        #[unsafe(export_name = concat!($prefix, stringify!($batch)))]
        #[inline(never)]
        pub unsafe extern "C" fn $name(
            size: usize,
            align: usize,
            mark_fn: MarkFn,
        ) -> AllocBatch<$batch> {
            AllocBatch {
                addresses: unsafe { alloc_in_class::<$class, $batch>(size, align, mark_fn) },
            }
        }
    };
}

macro_rules! batch_allocators {
    ($class:expr, $prefix:expr) => {
        batch_allocator!(sol_alloc_batch1, 1, $class, $prefix);
        batch_allocator!(sol_alloc_batch2, 2, $class, $prefix);
        batch_allocator!(sol_alloc_batch3, 3, $class, $prefix);
        batch_allocator!(sol_alloc_batch4, 4, $class, $prefix);
        batch_allocator!(sol_alloc_batch5, 5, $class, $prefix);
        batch_allocator!(sol_alloc_batch6, 6, $class, $prefix);
        batch_allocator!(sol_alloc_batch7, 7, $class, $prefix);
        batch_allocator!(sol_alloc_batch8, 8, $class, $prefix);
    };
}

batch_allocators!(-1, "sol_alloc_batch");

macro_rules! batch_view_allocator {
    ($name:ident, $batch:literal, $class:expr) => {
        /// Compiler-only allocation view over thread-local address storage.
        ///
        /// # Safety
        /// Read every returned address before another allocation or safepoint,
        /// and initialize all objects before a safepoint. The layout and mark
        /// function must satisfy the fixed-class allocator's requirements.
        #[doc(hidden)]
        #[unsafe(export_name = concat!("sol_alloc_class_", stringify!($class), "_batch", stringify!($batch), "_view"))]
        #[inline(never)]
        pub unsafe extern "C" fn $name(
            size: usize,
            align: usize,
            mark_fn: MarkFn,
        ) -> *const usize {
            unsafe { alloc_view_in_class::<$class, $batch>(size, align, mark_fn) }
        }
    };
}

#[inline(always)]
unsafe fn alloc_view_in_class<const CLASS: isize, const BATCH: usize>(
    size: usize,
    align: usize,
    mark_fn: MarkFn,
) -> *const usize {
    const { assert!(CLASS >= 0 && BATCH >= 3 && BATCH <= 8) };
    debug_assert_eq!(heap::size_class(size, align), Some(CLASS as usize));
    if ENABLE_ALLOC_PRINTS.get() {
        for _ in 0..BATCH {
            eprintln!("allocating new object: {size} bytes (align={align})");
        }
    }
    unsafe {
        with_thread_slot(
            #[inline(always)]
            |slot| {
                let state = &mut *slot.alloc.get();
                let cs = &mut state.classes[CLASS as usize];
                if cs.cache_index == 64 {
                    refill_cache::<CLASS>(cs, CLASS as usize);
                    std::hint::assert_unchecked(cs.cache_index < 64);
                }
                let start = cs.cache_index;
                let view = if start <= 64 - BATCH {
                    allocate_cached_view::<CLASS, BATCH>(state, size, mark_fn)
                } else {
                    let addresses =
                        arena_allocate::<CLASS, BATCH>(state, CLASS as usize, size, mark_fn);
                    for (out, address) in state.batch_addresses.iter_mut().zip(addresses) {
                        *out = address as usize;
                    }
                    state.batch_addresses.as_ptr()
                };
                state.total_allocations += BATCH;
                view
            },
        )
    }
}

/// Consume a batch that fits in the current cache, without copying addresses.
#[inline(always)]
unsafe fn allocate_cached_view<const CLASS: isize, const BATCH: usize>(
    state: &mut ThreadAllocState,
    size: usize,
    mark_fn: MarkFn,
) -> *const usize {
    let class = CLASS as usize;
    let cs = &mut state.classes[class];
    let start = cs.cache_index;
    debug_assert!(start <= 64 - BATCH);
    if class < heap::META_MIN_CLASS && cs.prepared {
        cs.cache_index += BATCH;
        return unsafe { cs.cache.as_ptr().add(start) };
    }
    let rbase = heap::region_base(class);
    let first = heap::slot_index(cs.cache[start], rbase, class);
    let last = heap::slot_index(cs.cache[start + BATCH - 1], rbase, class);
    if class >= heap::META_MIN_CLASS {
        for &address in &cs.cache[start..start + BATCH] {
            let slot = heap::slot_index(address, rbase, class);
            let metadata = unsafe { &mut *heap::meta_entry(class, slot) };
            metadata.mark_fn = mark_fn as usize;
            metadata.size = size as u64;
        }
    }
    cs.cache_index += BATCH;
    // The cache contains every free slot in ascending order. The next batch
    // consumes all remaining free bits through its last address. Older live
    // slots in that prefix retain their existing marks.
    let bits = unsafe { heap::alloc_word_through(class, first >> 6, last & 63) };
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { heap::mark_word_or(class, first >> 6, bits) };
    }
    unsafe { cs.cache.as_ptr().add(start) }
}

#[inline(always)]
unsafe fn alloc_in_class<const CLASS: isize, const BATCH: usize>(
    size: usize,
    align: usize,
    mark_fn: MarkFn,
) -> [*mut u8; BATCH] {
    const { assert!(BATCH > 0 && BATCH <= 64) };
    debug_assert!(CLASS == -1 || heap::size_class(size, align) == Some(CLASS as usize));
    if ENABLE_ALLOC_PRINTS.get() {
        for _ in 0..BATCH {
            eprintln!("allocating new object: {size} bytes (align={align})");
        }
    }

    unsafe {
        with_thread_slot(
            #[inline(always)]
            |slot| {
                let state = &mut *slot.alloc.get();
                let addresses = if CLASS == -1 {
                    match heap::size_class(size, align) {
                        Some(class) => arena_allocate::<-1, BATCH>(state, class, size, mark_fn),
                        None => std::array::from_fn(|_| big_allocate(state, size, align, mark_fn)),
                    }
                } else {
                    arena_allocate::<CLASS, BATCH>(state, CLASS as usize, size, mark_fn)
                };
                state.total_allocations += BATCH;
                addresses
            },
        )
    }
}

macro_rules! class_allocators {
    ($(($name:ident, $class:literal)),* $(,)?) => {$(
        #[doc = "Compiler-only allocation entry point for a fixed arena class."]
        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[inline(never)]
        pub unsafe extern "C" fn $name(
            size: usize, align: usize, mark_fn: MarkFn,
        ) -> *mut u8 {
            unsafe { alloc_in_class::<$class, 1>(size, align, mark_fn)[0] }
        }
        const _: () = {
            batch_allocators!($class, concat!("sol_alloc_class_", stringify!($class), "_batch"));
            batch_view_allocator!(sol_alloc_batch3_view, 3, $class);
            batch_view_allocator!(sol_alloc_batch4_view, 4, $class);
            batch_view_allocator!(sol_alloc_batch5_view, 5, $class);
            batch_view_allocator!(sol_alloc_batch6_view, 6, $class);
            batch_view_allocator!(sol_alloc_batch7_view, 7, $class);
            batch_view_allocator!(sol_alloc_batch8_view, 8, $class);
        };
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

/// Allocate a fixed-size array of addresses. A cache contains free slots from
/// exactly one bitmap word, so a batch that fits can publish all bits at once.
#[inline(always)]
unsafe fn arena_allocate<const CLASS: isize, const BATCH: usize>(
    state: &mut ThreadAllocState,
    class: usize,
    size: usize,
    mark_fn: MarkFn,
) -> [*mut u8; BATCH] {
    let class = if CLASS == -1 { class } else { CLASS as usize };
    let cs = &mut state.classes[class];
    if cs.cache_index == 64 {
        unsafe { refill_cache::<CLASS>(cs, class) };
        // SAFETY: refill skips full words and packs a nonempty set of free
        // slots into the cache, so every normal return leaves an index < 64.
        unsafe { std::hint::assert_unchecked(cs.cache_index < cs.cache.len()) };
    }
    // Consume sparse caches and batches crossing a word without dropping slots.
    if cs.cache_index > 64 - BATCH {
        return std::array::from_fn(|_| unsafe {
            arena_allocate_one::<CLASS>(state, class, size, mark_fn)
        });
    }
    let addresses: [usize; BATCH] = cs.cache[cs.cache_index..cs.cache_index + BATCH]
        .try_into()
        .unwrap();
    cs.cache_index += BATCH;
    if class < heap::META_MIN_CLASS && cs.prepared {
        return addresses.map(|addr| addr as *mut u8);
    }
    let rbase = heap::region_base(class);
    let mut bits = 0;
    for addr in addresses {
        let slot = heap::slot_index(addr, rbase, class);
        if class >= heap::META_MIN_CLASS {
            let m = unsafe { &mut *heap::meta_entry(class, slot) };
            m.mark_fn = mark_fn as usize;
            m.size = size as u64;
        }
        bits |= 1 << (slot & 63);
    }
    let word = heap::slot_index(addresses[0], rbase, class) >> 6;
    unsafe { heap::alloc_word_or(class, word, bits) };
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { heap::mark_word_or(class, word, bits) };
    }
    addresses.map(|addr| addr as *mut u8)
}

/// Allocate `size` bytes (rounded up to a power-of-2 size class) from the
/// arena. Returns a correctly-aligned pointer to **uninitialized** memory; the
/// caller (codegen) zeroes it with an explicit `memset` that LLVM can elide.
/// `CLASS == -1` uses the runtime class; fixed classes stay specialized through
/// the cold refill, without requiring that refill to inline into the fast path.
unsafe fn arena_allocate_one<const CLASS: isize>(
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
    if class < heap::META_MIN_CLASS && cs.prepared {
        return addr as *mut u8;
    }
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
            (cs.cur, cs.end) = heap::claim_run(class, cs.end != 0);
        }
        let word_slot = cs.cur as usize;
        cs.cur += 64;
        let allocated = unsafe { heap::alloc_word_load(class, word_slot >> 6) };
        if allocated == u64::MAX {
            continue;
        }
        let base = heap::slot_addr(heap::region_base(class), word_slot, class);
        cs.prepared = false;
        fill_cache::<CLASS>(cs, base, class, !allocated);
        if class < heap::META_MIN_CLASS
            && !crate::gc::GC_SAN.get()
            && !SOL_CONCURRENT_MARKING.load(Ordering::Relaxed)
        {
            unsafe { prepare_cache(cs, base, class, word_slot >> 6, !allocated) };
        }
        return;
    }
}

/// Zero and publish free conservative slots once, before any address is returned.
unsafe fn prepare_cache(
    cs: &mut ThreadClassState,
    base: usize,
    class: usize,
    word: usize,
    free: u64,
) {
    debug_assert!(class < heap::META_MIN_CLASS && free != 0);
    let size = heap::slot_size(class);
    if free == u64::MAX {
        unsafe { (base as *mut u8).write_bytes(0, 64 * size) };
    } else {
        for &address in &cs.cache[cs.cache_index..] {
            unsafe { (address as *mut u8).write_bytes(0, size) };
        }
    }
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { heap::mark_word_or(class, word, free) };
    }
    unsafe { heap::alloc_word_or(class, word, free) };
    cs.prepared = true;
}

/// Remaining addresses belong to one bitmap word and exclude returned objects.
fn remaining_reservations(cs: &ThreadClassState, class: usize) -> Option<(usize, u64)> {
    if !cs.prepared || cs.cache_index == 64 {
        return None;
    }
    let region = heap::region_base(class);
    let first = heap::slot_index(cs.cache[cs.cache_index], region, class);
    let mut bits = 0;
    for &address in &cs.cache[cs.cache_index..] {
        let slot = heap::slot_index(address, region, class);
        debug_assert_eq!(slot >> 6, first >> 6);
        bits |= 1 << (slot & 63);
    }
    Some((first >> 6, bits))
}

/// Premarks only unconsumed reservations during the first stop-the-world pause.
pub(crate) unsafe fn mark_prepared_cache(cs: &ThreadClassState, class: usize) {
    if let Some((word, bits)) = remaining_reservations(cs, class) {
        unsafe { heap::mark_word_or(class, word, bits) };
    }
}

/// Releases unused reservations before claim abandonment or thread exit. The
/// caller owns the cache and excludes ownership resets. Marks need not be
/// cleared here: every refill returns at least one allocation before any poll,
/// so sweep still visits this word and clears its marks before any later reuse.
pub(crate) unsafe fn release_prepared_cache(cs: &mut ThreadClassState, class: usize) {
    if let Some((word, bits)) = remaining_reservations(cs, class) {
        unsafe { heap::release_reserved_bits(class, word, bits) };
    }
    cs.prepared = false;
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
    fn preparation_is_deferred_during_concurrent_marking() {
        unsafe extern "C" fn mark(_: *mut u8, _: *mut u8, _: u64) {}
        heap::init_for_tests();
        let class = 3;
        let size = heap::slot_size(class);
        SOL_CONCURRENT_MARKING.store(true, Ordering::Relaxed);
        let mut active = ThreadAllocState::new();
        let address = unsafe { arena_allocate_one::<3>(&mut active, class, size, mark) };
        let slot = heap::slot_index(address as usize, heap::region_base(class), class);
        assert!(!active.classes[class].prepared);
        assert_eq!(
            unsafe { heap::alloc_word_load(class, slot >> 6) },
            1 << (slot & 63)
        );
        assert_eq!(
            unsafe { heap::mark_word_load(class, slot >> 6) },
            1 << (slot & 63)
        );
        SOL_CONCURRENT_MARKING.store(false, Ordering::Relaxed);
        let mut idle = ThreadAllocState::new();
        let address = unsafe { arena_allocate_one::<3>(&mut idle, class, size, mark) };
        let slot = heap::slot_index(address as usize, heap::region_base(class), class);
        assert!(idle.classes[class].prepared);
        assert_eq!(unsafe { heap::alloc_word_load(class, slot >> 6) }, u64::MAX);
        idle.reset_claims();
        assert_eq!(
            unsafe { heap::alloc_word_load(class, slot >> 6) },
            1 << (slot & 63)
        );
    }

    #[test]
    fn prepared_caches_preserve_survivors_and_release_unused_reservations() {
        unsafe extern "C" fn mark(_: *mut u8, _: *mut u8, _: u64) {}
        fn check<const CLASS: isize, const BATCH: usize>(word: &mut usize) {
            let class = CLASS as usize;
            let size = heap::slot_size(class);
            for marking_at_refill in [false, true] {
                for allocated in [0, 1, 1 << 63, 0xaaaa_5555_8000_0001] {
                    let free: Vec<_> = (0..64).filter(|bit| allocated & (1 << bit) == 0).collect();
                    let base = heap::slot_addr(heap::region_base(class), *word * 64, class);
                    unsafe {
                        (base as *mut u8).write_bytes(0xa5, size * 64);
                        heap::alloc_word_or(class, *word, allocated);
                    }
                    let mut state = ThreadAllocState::new();
                    let cs = &mut state.classes[class];
                    cs.cur = ((*word + 1) * 64) as u64;
                    fill_cache::<CLASS>(cs, base, class, !allocated);
                    SOL_CONCURRENT_MARKING.store(marking_at_refill, Ordering::Relaxed);
                    unsafe { prepare_cache(cs, base, class, *word, !allocated) };
                    assert!(cs.prepared);
                    assert_eq!(unsafe { heap::alloc_word_load(class, *word) }, u64::MAX);
                    for bit in 0..64 {
                        let bytes = unsafe {
                            std::slice::from_raw_parts((base + bit * size) as *const u8, size)
                        };
                        let expected = if allocated & (1 << bit) == 0 { 0 } else { 0xa5 };
                        assert!(bytes.iter().all(|&b| b == expected));
                    }
                    let first =
                        unsafe { arena_allocate_one::<CLASS>(&mut state, class, size, mark) };
                    assert_eq!(first as usize, base + free[0] * size);
                    let mut consumed = 1 << free[0];
                    if !marking_at_refill {
                        assert_eq!(unsafe { heap::mark_word_load(class, *word) }, 0);
                        unsafe { mark_prepared_cache(&state.classes[class], class) };
                        assert_eq!(
                            unsafe { heap::mark_word_load(class, *word) },
                            !allocated & !consumed
                        );
                        // The first returned object is an ordinary pre-existing
                        // root, traced separately from unused reservations.
                        unsafe { heap::mark_word_or(class, *word, consumed) };
                        SOL_CONCURRENT_MARKING.store(true, Ordering::Relaxed);
                    }
                    let addresses =
                        unsafe { arena_allocate::<CLASS, BATCH>(&mut state, class, size, mark) };
                    for (address, bit) in addresses.into_iter().zip(&free[1..]) {
                        assert_eq!(address as usize, base + bit * size);
                        consumed |= 1 << bit;
                    }
                    if BATCH >= 3 {
                        let view =
                            unsafe { allocate_cached_view::<CLASS, BATCH>(&mut state, size, mark) };
                        for (&address, bit) in unsafe { std::slice::from_raw_parts(view, BATCH) }
                            .iter()
                            .zip(&free[1 + BATCH..])
                        {
                            assert_eq!(address, base + bit * size);
                            consumed |= 1 << bit;
                        }
                    }
                    assert_eq!(unsafe { heap::alloc_word_load(class, *word) }, u64::MAX);
                    assert_eq!(unsafe { heap::mark_word_load(class, *word) }, !allocated);
                    SOL_CONCURRENT_MARKING.store(false, Ordering::Relaxed);
                    state.reset_claims();
                    assert!(!state.classes[class].prepared);
                    assert_eq!(
                        unsafe { heap::alloc_word_load(class, *word) },
                        allocated | consumed
                    );
                    assert_eq!(
                        unsafe { heap::sweep_word_range(class, *word, *word + 1) },
                        (consumed.count_ones() as u64, allocated.count_ones() as u64)
                    );
                    assert_eq!(unsafe { heap::alloc_word_load(class, *word) }, consumed);
                    assert_eq!(unsafe { heap::mark_word_load(class, *word) }, 0);
                    // A following cycle must not inherit reservation marks.
                    assert_eq!(
                        unsafe { heap::sweep_word_range(class, *word, *word + 1) },
                        (0, consumed.count_ones() as u64)
                    );
                    *word += 1;
                }
            }
        }
        heap::init_for_tests();
        fn class<const CLASS: isize>() {
            let mut word = 4096;
            check::<CLASS, 1>(&mut word);
            check::<CLASS, 2>(&mut word);
            check::<CLASS, 3>(&mut word);
            check::<CLASS, 4>(&mut word);
            check::<CLASS, 5>(&mut word);
            check::<CLASS, 6>(&mut word);
            check::<CLASS, 7>(&mut word);
            check::<CLASS, 8>(&mut word);
        }
        class::<0>();
        class::<1>();
        class::<2>();
        class::<3>();
    }

    #[test]
    fn cached_views_publish_only_consumed_free_slots() {
        unsafe extern "C" fn mark(_: *mut u8, _: *mut u8, _: u64) {}
        fn check<const CLASS: isize, const BATCH: usize>(word: &mut usize) {
            let class = CLASS as usize;
            let size = heap::slot_size(class);
            let mut state = ThreadAllocState::new();
            let mut random = 0x1234_5678_9abc_def0u64;
            for allocated in [0, 1, 1 << 63, 0x5555_aaaa_8000_0001]
                .into_iter()
                .chain((0..256).map(|_| {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    random
                }))
            {
                let free: Vec<_> = (0..64).filter(|bit| allocated & (1 << bit) == 0).collect();
                unsafe { heap::alloc_word_or(class, *word, allocated) };
                let base = heap::slot_addr(heap::region_base(class), *word * 64, class);
                state.classes[class].cur = ((*word + 1) * 64) as u64;
                fill_cache::<CLASS>(&mut state.classes[class], base, class, !allocated);
                let mut consumed = 0;
                for batch in free.as_chunks::<BATCH>().0 {
                    let view =
                        unsafe { allocate_cached_view::<CLASS, BATCH>(&mut state, size, mark) };
                    let addresses = unsafe { std::slice::from_raw_parts(view, BATCH) };
                    for (&address, &bit) in addresses.iter().zip(batch) {
                        assert_eq!(address, base + bit * size);
                        if class >= heap::META_MIN_CLASS {
                            let metadata = unsafe { &*heap::meta_entry(class, *word * 64 + bit) };
                            assert_eq!(metadata.size, size as u64);
                            assert_eq!(metadata.mark_fn, mark as *const () as usize);
                        }
                        consumed |= 1 << bit;
                    }
                    assert_eq!(
                        unsafe { heap::alloc_word_load(class, *word) },
                        allocated | consumed
                    );
                    assert_eq!(unsafe { heap::mark_word_load(class, *word) }, consumed);
                }
                *word += 1;
            }
        }
        heap::init_for_tests();
        SOL_CONCURRENT_MARKING.store(true, Ordering::Relaxed);
        fn check_class<const CLASS: isize>() {
            // Keep clear of the ranges used by the independent sweep tests.
            let mut word = 1024;
            check::<CLASS, 3>(&mut word);
            check::<CLASS, 4>(&mut word);
            check::<CLASS, 5>(&mut word);
            check::<CLASS, 6>(&mut word);
            check::<CLASS, 7>(&mut word);
            check::<CLASS, 8>(&mut word);
        }
        check_class::<0>();
        check_class::<1>();
        check_class::<2>();
        check_class::<3>();
        check_class::<4>();
        SOL_CONCURRENT_MARKING.store(false, Ordering::Relaxed);
    }

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
