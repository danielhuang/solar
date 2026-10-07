//! Address-partitioned size-class heap.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::init_cell::InitCell;

use crate::mem::MarkFn;

/// Smallest size class: `1 << MIN_LOG` = 8 bytes.
pub const MIN_LOG: u32 = 3;
/// Largest arena size class: `1 << MAX_LOG` = 1 GiB.
pub const MAX_LOG: u32 = 30;
/// Number of arena size classes.
pub const NUM_CLASSES: usize = (MAX_LOG - MIN_LOG + 1) as usize; // 28
/// Each size class gets a `1 << REGION_LOG` = 1 TiB region.
pub const REGION_LOG: u32 = 40;
/// Virtual address-space size assigned to each class.
pub const REGION_SIZE: usize = 1usize << REGION_LOG;
/// Total virtual address-space reservation for the arena.
pub const ARENA_SIZE: usize = NUM_CLASSES * REGION_SIZE; // ~28 TiB
/// Base-two logarithm of the system page size.
pub const PAGE_LOG: u32 = 12;
/// System page size in bytes.
pub const PAGE_SIZE: usize = 1usize << PAGE_LOG;

/// Slots of this size or larger get a metadata-table entry (precise marking);
/// smaller slots are conservatively scanned.
pub const META_THRESHOLD: usize = 128;
/// First size class with `slot_size >= META_THRESHOLD` (= class 4 → 128 B).
pub const META_MIN_CLASS: usize = (META_THRESHOLD.trailing_zeros() - MIN_LOG) as usize;

/// Largest request served from the arena. Anything bigger uses the big-object
/// path.
pub const MAX_ARENA_ALLOC: usize = 1usize << MAX_LOG;

#[inline]
/// Returns the base-two logarithm of a class's slot size.
pub const fn slot_size_log(class: usize) -> u32 {
    class as u32 + MIN_LOG
}
#[inline]
/// Returns a class's slot size in bytes.
pub const fn slot_size(class: usize) -> usize {
    1usize << slot_size_log(class)
}
/// Slots per 1 TiB region for `class`.
#[inline]
pub const fn slots_per_region(class: usize) -> usize {
    REGION_SIZE >> slot_size_log(class)
}
/// Bytes handed out per `claim_run`. Sized well above a page so the contended
/// `NEXT_SLOT[class]` fetch_add is amortized across many allocations: at one
/// page (~128 slots for a small object) that shared frontier counter was a real
/// multi-thread bottleneck — 16 mutators hammering one cache line every ~128
/// allocs. Must be a power of two so `CLAIM_BYTES / slot_size` is itself a power
/// of two, keeping every claim a whole number of 64-slot bitmap words.
const CLAIM_BYTES: usize = 256 * 1024;
const _: () = assert!(CLAIM_BYTES.is_power_of_two() && CLAIM_BYTES >= PAGE_SIZE);

/// Number of slots handed out per `claim_run` — `CLAIM_BYTES` worth, but always
/// rounded up to a whole 64-slot bitmap word. Since `NEXT_SLOT` only ever moves
/// by multiples of this (or resets to 0), every claim's slot range is
/// bitmap-word-aligned, so two threads' claims never share an alloc-bitmap word
/// — which lets `set_allocated` skip the atomic `fetch_or`.
#[inline]
pub const fn claim_slots(class: usize) -> usize {
    let ssz = slot_size(class);
    let per_claim = if ssz >= CLAIM_BYTES {
        1
    } else {
        CLAIM_BYTES / ssz
    };
    if per_claim < 64 { 64 } else { per_claim }
}

// Bitmap layout: `bitmap_class_offset(c)` bytes of region-0..c-1 precede
// class c's slice. Class c occupies `slots_per_region(c) / 8` bytes
// = `1 << (REGION_LOG - MIN_LOG - 3 - c)`.
const BITS_TOP: u32 = REGION_LOG - MIN_LOG - 3 + 1; // 35
#[inline]
/// Returns a class's byte offset within a bitmap.
pub const fn bitmap_class_offset(class: usize) -> usize {
    (1usize << BITS_TOP) - (1usize << (BITS_TOP - class as u32))
}
/// Total bytes to reserve for one bitmap (a slight over-reserve).
pub const BITMAP_TOTAL: usize = 1usize << BITS_TOP; // 32 GiB

/// Metadata stored for a precisely traced slot.
#[repr(C)]
pub struct MetaEntry {
    /// `MarkFn` reinterpreted as `usize`. Valid whenever the slot's allocated
    /// bit is set.
    pub mark_fn: usize,
    /// User-requested size (the slot size may be larger). Needed by `mark_fn`
    /// for arrays/slices.
    pub size: u64,
}
const META_ENTRY_LOG: u32 = 4; // log2(size_of::<MetaEntry>())
const _: () = assert!(1usize << META_ENTRY_LOG == size_of::<MetaEntry>());

// Metadata layout (classes META_MIN_CLASS..NUM_CLASSES): class c occupies
// `size_of::<MetaEntry>() * slots_per_region(c)`
// = `1 << (META_ENTRY_LOG + REGION_LOG - MIN_LOG - c)` bytes.
const META_TOP: u32 = META_ENTRY_LOG + REGION_LOG - MIN_LOG - META_MIN_CLASS as u32 + 1; // 38
#[inline]
/// Returns a class's byte offset within the metadata table.
pub const fn meta_class_offset(class: usize) -> usize {
    debug_assert!(class >= META_MIN_CLASS);
    (1usize << META_TOP) - (1usize << (META_TOP + META_MIN_CLASS as u32 - class as u32))
}
/// Total bytes to reserve for the metadata table (a slight over-reserve).
pub const META_TOTAL: usize = 1usize << META_TOP; // 256 GiB

// ---------------------------------------------------------------------------
// Global state (set once by `init`, then effectively const).
// ---------------------------------------------------------------------------

static ARENA_BASE: InitCell<usize> = InitCell::new(0);
static ALLOC_BITS: InitCell<usize> = InitCell::new(0);
static MARK_BITS: InitCell<usize> = InitCell::new(0);
static META_BASE: InitCell<usize> = InitCell::new(0);
/// Next slot index to hand out for each class. `claim_run` `fetch_add`s it.
/// Reset to 0 by `reset_frontier` after a sweep frees most of a class.
static NEXT_SLOT: [AtomicU64; NUM_CLASSES] = [const { AtomicU64::new(0) }; NUM_CLASSES];
/// High-water mark (in slots) for each class — the furthest slot ever handed
/// out. Never decreases. Sweep walks `[0, HWM)`; `lookup_arena` short-circuits
/// past it.
static HWM: [AtomicU64; NUM_CLASSES] = [const { AtomicU64::new(0) }; NUM_CLASSES];

/// Completed sweep regions, consumed in publication order so new arrivals do
/// not interrupt a partially claimed region. Claims are split under the mutex.
/// The hint avoids locking when there is no published region; a stale false hint
/// merely sends an allocator to the fresh frontier.
struct SweptRuns {
    available: AtomicBool,
    ranges: Mutex<VecDeque<Range<u64>>>,
}

impl SweptRuns {
    const fn new() -> Self {
        Self {
            available: AtomicBool::new(false),
            ranges: Mutex::new(VecDeque::new()),
        }
    }

    fn publish(&self, range: Range<u64>) {
        let mut ranges = self.ranges.lock().unwrap();
        ranges.push_back(range);
        self.available.store(true, Ordering::Relaxed);
    }

    fn claim(&self, slots: u64) -> Option<u64> {
        if !self.available.load(Ordering::Relaxed) {
            return None;
        }
        let mut ranges = self.ranges.lock().unwrap();
        let range = ranges.front_mut()?;
        let start = range.start;
        let end = start + slots;
        assert!(end <= range.end);
        range.start = end;
        if range.is_empty() {
            ranges.pop_front();
            if ranges.is_empty() {
                self.available.store(false, Ordering::Relaxed);
            }
        }
        Some(start)
    }

    fn clear(&self) {
        self.ranges.lock().unwrap().clear();
        self.available.store(false, Ordering::Relaxed);
    }
}

static SWEPT_RUNS: [SweptRuns; NUM_CLASSES] = [const { SweptRuns::new() }; NUM_CLASSES];

/// Publishes a completed, claim-aligned sweep region for concurrent reuse.
/// The sweeper must never touch its bitmap words again this cycle, and the
/// normal allocation frontier must remain above every published region.
pub(crate) unsafe fn publish_swept_range(class: usize, word_start: usize, word_end: usize) {
    let start = word_start as u64 * 64;
    let end = word_end as u64 * 64;
    let claim = claim_slots(class) as u64;
    assert!(start < end && start.is_multiple_of(claim) && end.is_multiple_of(claim));
    SWEPT_RUNS[class].publish(start..end);
}

/// Discards unused sweep regions before moving any allocation frontier back.
/// Requires stopped mutators and joined sweep workers; cached claims must also
/// be abandoned before allocation resumes.
pub(crate) fn clear_swept_ranges() {
    for runs in &SWEPT_RUNS {
        runs.clear();
    }
}

unsafe fn mmap_reserve(size: usize, what: &str) -> usize {
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    assert!(
        p != libc::MAP_FAILED,
        "solar heap: mmap of {size} bytes for {what} failed (errno {})",
        std::io::Error::last_os_error()
    );
    p as usize
}

/// Reserve the arena, bitmaps and metadata table. Idempotent; call once from
/// `sol_start` before any Solar code runs.
pub fn init() {
    if ARENA_BASE.get() != 0 {
        return;
    }
    unsafe {
        let arena = mmap_reserve(ARENA_SIZE, "arena");
        let alloc_bits = mmap_reserve(BITMAP_TOTAL, "alloc bitmap");
        let mark_bits = mmap_reserve(BITMAP_TOTAL, "mark bitmap");
        let meta = mmap_reserve(META_TOTAL, "metadata table");
        // Large small-object bitmaps can share page-table fault locks across
        // many mutators. Allow huge backing after the first 2 MiB of each
        // conservative class, preserving small-heap demand-paging granularity.
        // This is optional advice; allocation and GC do not depend on it.
        const HUGE_PAGE: usize = 2 * 1024 * 1024;
        for class in 0..META_MIN_CLASS {
            let base = alloc_bits + bitmap_class_offset(class);
            let start = (base + HUGE_PAGE).next_multiple_of(HUGE_PAGE);
            let end = (base + slots_per_region(class) / 8) & !(HUGE_PAGE - 1);
            if start < end {
                let _ = libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_HUGEPAGE);
            }
        }
        // SAFETY: `init` runs once from `sol_start`, before any thread that
        // reads these cells is spawned.
        ALLOC_BITS.set(alloc_bits);
        MARK_BITS.set(mark_bits);
        META_BASE.set(meta);
        ARENA_BASE.set(arena);
    }
}

#[inline]
/// Returns the arena's base address.
pub fn arena_base() -> usize {
    ARENA_BASE.get()
}

// ---------------------------------------------------------------------------
// Address math.
// ---------------------------------------------------------------------------

/// Size class for a request, or `None` if it must use the big-object path.
#[inline]
pub fn size_class(size: usize, align: usize) -> Option<usize> {
    let need = size.max(align).max(slot_size(0));
    if need > MAX_ARENA_ALLOC {
        return None;
    }
    Some((need.next_power_of_two().trailing_zeros() - MIN_LOG) as usize)
}

/// `(class, region_base)` if `p` is inside the arena, else `None`.
#[inline]
pub fn classify(p: usize) -> Option<(usize, usize)> {
    let base = arena_base();
    let d = p.wrapping_sub(base);
    if d >= ARENA_SIZE {
        return None;
    }
    let class = d >> REGION_LOG;
    Some((class, base + (class << REGION_LOG)))
}

#[inline]
/// Returns the base address of a size-class region.
pub fn region_base(class: usize) -> usize {
    arena_base() + (class << REGION_LOG)
}
#[inline]
/// Returns the slot containing an address.
pub fn slot_index(p: usize, region_base: usize, class: usize) -> usize {
    (p - region_base) >> slot_size_log(class)
}
#[inline]
/// Returns a slot's base address.
pub fn slot_addr(region_base: usize, slot: usize, class: usize) -> usize {
    region_base + (slot << slot_size_log(class))
}

// ---------------------------------------------------------------------------
// Bitmap / metadata accessors.
// ---------------------------------------------------------------------------

#[inline]
fn bit_mask(slot: usize) -> u64 {
    1u64 << (slot & 63)
}
#[inline]
fn alloc_class_base(class: usize) -> *mut AtomicU64 {
    (ALLOC_BITS.get() + bitmap_class_offset(class)) as *mut AtomicU64
}
#[inline]
fn mark_class_base(class: usize) -> *mut AtomicU64 {
    (MARK_BITS.get() + bitmap_class_offset(class)) as *mut AtomicU64
}

#[inline]
/// Returns whether a slot is allocated.
pub unsafe fn is_allocated(class: usize, slot: usize) -> bool {
    let w = unsafe { &*alloc_class_base(class).add(slot >> 6) };
    w.load(Ordering::Relaxed) & bit_mask(slot) != 0
}

/// Asserts that each arena slot intersecting `[addr, addr + size)` is live.
/// Addresses outside the arena are ignored.
pub fn assert_allocated_range(addr: usize, size: usize) {
    let arena = arena_base();
    if arena == 0 || size == 0 {
        return;
    }
    let arena_end = arena + ARENA_SIZE;
    let access_end = addr.saturating_add(size);
    let mut p = addr.max(arena);
    let end = access_end.min(arena_end);
    while p < end {
        let (class, rbase) = classify(p).unwrap();
        let slot = slot_index(p, rbase, class);
        assert!(
            unsafe { is_allocated(class, slot) },
            "GC-San: access to swept arena allocation at {p:#x} (class {class}, slot {slot})"
        );
        let slot_end = slot_addr(rbase, slot, class) + slot_size(class);
        p = slot_end.min(end);
    }
}
/// Load a whole alloc-bitmap word. The allocator's find-slot scan loads the
/// word covering its cursor once and tests all 64 slots from it, instead of
/// reloading the word per slot via `is_allocated`.
#[inline]
pub unsafe fn alloc_word_load(class: usize, word: usize) -> u64 {
    unsafe { &*alloc_class_base(class).add(word) }.load(Ordering::Relaxed)
}
#[inline]
/// Marks a slot as allocated.
pub unsafe fn set_allocated(class: usize, slot: usize) {
    unsafe { alloc_word_or(class, slot >> 6, bit_mask(slot)) };
}

/// Publishes allocation bits in a bitmap word owned by the current mutator.
#[inline]
pub unsafe fn alloc_word_or(class: usize, word: usize, bits: u64) {
    // Non-atomic read-modify-write: the only thread that writes this word until
    // the next stop-the-world (sweep) is the one that claimed this word's run, and
    // claims are bitmap-word-aligned (see `claim_slots`), so no other thread
    // touches this word concurrently. Avoids a `LOCK OR` on the alloc hot path.
    let w = unsafe { &*alloc_class_base(class).add(word) };
    w.store(w.load(Ordering::Relaxed) | bits, Ordering::Relaxed);
}
/// Releases unconsumed reservations in a word still exclusively owned by the
/// allocator. No returned allocation or older survivor may appear in `bits`.
#[inline]
pub(crate) unsafe fn release_reserved_bits(class: usize, word: usize, bits: u64) {
    let w = unsafe { &*alloc_class_base(class).add(word) };
    w.store(w.load(Ordering::Relaxed) & !bits, Ordering::Relaxed);
}
/// Publishes every remaining free slot through `last` in an owned bitmap word,
/// returning exactly the newly allocated bits. Initialize metadata for all
/// these slots before calling. `last` must be in 0..64.
#[inline]
pub unsafe fn alloc_word_through(class: usize, word: usize, last: usize) -> u64 {
    let prefix = u64::MAX >> (63 - last);
    let w = unsafe { &*alloc_class_base(class).add(word) };
    let allocated = w.load(Ordering::Relaxed);
    w.store(allocated | prefix, Ordering::Relaxed);
    prefix & !allocated
}
/// Load a whole mark-bitmap word. Used by the batched marker to answer
/// "newly marked?" when it rolls over to a new word; a plain (non-atomic)
/// load is enough — see `mark_slot_batched` in `gc`.
#[inline]
pub unsafe fn mark_word_load(class: usize, word: usize) -> u64 {
    unsafe { &*mark_class_base(class).add(word) }.load(Ordering::Relaxed)
}
/// Atomically OR `bits` into one mark-bitmap word. The marker accumulates
/// bits for a word locally and flushes them here in one RMW, so a chain of
/// 64 consecutive slots costs one `fetch_or` instead of 64.
#[inline]
pub unsafe fn mark_word_or(class: usize, word: usize, bits: u64) {
    unsafe { &*mark_class_base(class).add(word) }.fetch_or(bits, Ordering::Relaxed);
}
/// Atomically set the mark bit for a single slot. Used for "allocate black":
/// an object born during concurrent marking is marked live immediately so the
/// stop-the-world sweep at the end of the cycle does not reclaim it. Atomic
/// because the concurrent marker may be flushing other bits in the same word.
#[inline]
pub unsafe fn set_marked(class: usize, slot: usize) {
    unsafe { &*mark_class_base(class).add(slot >> 6) }.fetch_or(bit_mask(slot), Ordering::Relaxed);
}

/// Is the slot containing arena pointer `p` already marked? Used by the write
/// barrier for white-only shading: an already-marked (black/gray) target needs
/// no shading, which keeps the barrier from flooding the gray queue with
/// redundant already-live pointers (e.g. freshly born-black objects). Returns
/// false for pointers outside `[0, hwm)` so the marker still sees them.
#[inline]
pub unsafe fn is_marked_addr(p: usize) -> bool {
    let Some((class, rbase)) = classify(p) else {
        return false;
    };
    let slot = slot_index(p, rbase, class);
    if slot as u64 >= hwm(class) {
        return false;
    }
    let w = unsafe { &*mark_class_base(class).add(slot >> 6) };
    w.load(Ordering::Relaxed) & bit_mask(slot) != 0
}

#[inline]
/// Returns a pointer to a slot's metadata entry.
pub unsafe fn meta_entry(class: usize, slot: usize) -> *mut MetaEntry {
    let base = META_BASE.get() + meta_class_offset(class);
    unsafe { (base as *mut MetaEntry).add(slot) }
}

// ---------------------------------------------------------------------------
// Allocation frontier.
// ---------------------------------------------------------------------------

/// Claim a disjoint run of slots for `class`. Returns `[start, end)` slot indices.
/// The run may contain survivors from a previous cycle (after a frontier
/// reset) — the caller must skip slots whose allocated bit is set.
/// `populate` requests bounded eager backing after the caller has exhausted a
/// previous claim, avoiding extra page work for one-off allocations.
#[inline]
pub fn claim_run(class: usize, populate: bool) -> (u64, u64) {
    let n = claim_slots(class) as u64;
    if let Some(start) = SWEPT_RUNS[class].claim(n) {
        // The queue's mutex acquires the sweeper's completed bitmap writes.
        // This claim is disjoint from both fresh claims and other reused ones.
        crate::gc::note_claimed((n as usize) << slot_size_log(class));
        return (start, start + n);
    }
    let s = NEXT_SLOT[class].fetch_add(n, Ordering::Relaxed);
    let e = s + n;
    let previous_hwm = HWM[class].fetch_max(e, Ordering::Relaxed);
    if populate && e > previous_hwm {
        let first = s.max(previous_hwm) as usize;
        let addr = slot_addr(region_base(class), first, class);
        let bytes = ((e as usize - first) << slot_size_log(class)).min(CLAIM_BYTES);
        // Populate fresh pages in one kernel operation instead of taking a
        // user-mode fault per page. Cap eager backing for large size classes,
        // whose minimum 64-slot claims can span many gigabytes. Unsupported
        // kernels retain demand paging.
        unsafe {
            let _ = libc::madvise(addr as *mut libc::c_void, bytes, libc::MADV_POPULATE_WRITE);
        }
    }
    // The GC trigger lives here rather than in `sol_alloc_impl`: a claim is the
    // rare, amortized event (one per `CLAIM_BYTES` run), so pacing on claimed
    // bytes keeps the per-allocation path free of trigger bookkeeping.
    crate::gc::note_claimed((n as usize) << slot_size_log(class));
    (s, e)
}
#[inline]
/// Returns a class's high-water slot index.
pub fn hwm(class: usize) -> u64 {
    HWM[class].load(Ordering::Relaxed)
}
#[inline]
/// Resets a class's allocation frontier.
pub fn reset_frontier(class: usize) {
    NEXT_SLOT[class].store(0, Ordering::Relaxed);
}
/// Push the allocation frontier up to the current high-water mark, so the next
/// `claim_run` hands out slots strictly above `[0, hwm)`. Used by the concurrent
/// sweep to partition the arena into disjoint bitmap words: the sweeper owns
/// `[0, hwm)` while post-resume allocations claim `[hwm, …)`. Must run while the
/// world is stopped (it's a backward-safe move only because every thread's
/// cached claim is abandoned in the same pause).
#[inline]
pub fn freeze_frontier_to_hwm(class: usize) {
    NEXT_SLOT[class].store(HWM[class].load(Ordering::Relaxed), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Lookup (used by the GC's conservative scan).
// ---------------------------------------------------------------------------

/// Describes how an allocated slot must be traced.
pub enum MarkKind {
    /// Uses a generated mark function.
    Precise {
        /// Mark function.
        mark_fn: MarkFn,
        /// User-requested allocation size.
        size: u64,
    },
    /// Conservatively scan `[base, base + slot_size)`.
    Conservative {
        /// Allocated slot size.
        slot_size: usize,
    },
}

/// Resolve a (possibly interior) pointer to the live arena slot containing it.
/// Returns `(class, slot, slot_base, how-to-mark)`, or `None` for free slots,
/// the untouched tail, or pointers outside the arena.
#[inline]
pub unsafe fn lookup_arena(p: usize) -> Option<(usize, usize, usize, MarkKind)> {
    let (class, rbase) = classify(p)?;
    let slot = slot_index(p, rbase, class);
    if slot as u64 >= hwm(class) {
        return None;
    }
    if !unsafe { is_allocated(class, slot) } {
        return None;
    }
    let base = slot_addr(rbase, slot, class);
    let kind = if class >= META_MIN_CLASS {
        let m = unsafe { &*meta_entry(class, slot) };
        MarkKind::Precise {
            mark_fn: unsafe { std::mem::transmute::<usize, MarkFn>(m.mark_fn) },
            size: m.size,
        }
    } else {
        MarkKind::Conservative {
            slot_size: slot_size(class),
        }
    };
    Some((class, slot, base, kind))
}

// ---------------------------------------------------------------------------
// Sweep.
// ---------------------------------------------------------------------------

/// Sweep one `[word_start, word_end)` word range of `class`'s bitmaps:
/// allocated-but-unmarked slots become free, marked slots stay allocated, and
/// the mark word is cleared for the next cycle. Returns `(live_slots,
/// freed_slots)` in this range. Caller must ensure ranges don't overlap across
/// concurrent calls (they're partitioned by the sweep driver) and that no
/// mutator can allocate into these words until this call completes.
pub unsafe fn sweep_word_range(class: usize, word_start: usize, word_end: usize) -> (u64, u64) {
    let abase = alloc_class_base(class);
    let mbase = mark_class_base(class);
    let mut live = 0u64;
    let mut freed = 0u64;
    for w in word_start..word_end {
        let aw = unsafe { &*abase.add(w) };
        let a = aw.load(Ordering::Relaxed);
        if a == 0 {
            continue;
        }
        let mw = unsafe { &*mbase.add(w) };
        let m = mw.load(Ordering::Relaxed);
        let survivors = a & m;
        live += survivors.count_ones() as u64;
        freed += (a & !m).count_ones() as u64;
        if survivors != a {
            aw.store(survivors, Ordering::Relaxed);
        }
        if m != 0 {
            mw.store(0, Ordering::Relaxed);
        }
    }
    (live, freed)
}

/// Number of bitmap words spanning `[0, hwm(class))`.
#[inline]
pub fn hwm_words(class: usize) -> usize {
    (hwm(class) as usize).div_ceil(64)
}

/// `(live slot count, live slot bytes)` across all classes. Walks the
/// allocation bitmaps; only meaningful when no GC is running (e.g. for stats
/// at process exit).
pub fn live_slots() -> (usize, usize) {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for c in 0..NUM_CLASSES {
        let words = hwm_words(c);
        if words == 0 {
            continue;
        }
        let base = alloc_class_base(c);
        let mut pop = 0u64;
        for w in 0..words {
            pop += unsafe { (*base.add(w)).load(Ordering::Relaxed) }.count_ones() as u64;
        }
        count += pop as usize;
        bytes += pop as usize * slot_size(c);
    }
    (count, bytes)
}

#[cfg(test)]
/// Initializes the shared heap once across runtime unit tests.
pub(crate) fn init_for_tests() {
    static INITIALIZED: std::sync::Once = std::sync::Once::new();
    INITIALIZED.call_once(init);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_regions_give_disjoint_claims_for_every_class() {
        for class in 0..NUM_CLASSES {
            let runs = SweptRuns::new();
            let n = claim_slots(class) as u64;
            for i in 0..8 {
                runs.publish(i * 2 * n..(i + 1) * 2 * n);
            }
            let mut claims = std::thread::scope(|scope| {
                let jobs: Vec<_> = (0..4)
                    .map(|_| {
                        let runs = &runs;
                        scope.spawn(move || {
                            let mut claims = Vec::new();
                            while let Some(claim) = runs.claim(n) {
                                claims.push((claim, claim + n));
                            }
                            claims
                        })
                    })
                    .collect();
                jobs.into_iter()
                    .flat_map(|job| job.join().unwrap())
                    .collect::<Vec<_>>()
            });
            claims.sort_unstable();
            assert_eq!(claims.len(), 8 * 2);
            for (i, (start, end)) in claims.into_iter().enumerate() {
                assert_eq!((start, end), (i as u64 * n, (i as u64 + 1) * n));
            }

            runs.publish(0..2 * n);
            assert_eq!(runs.claim(n), Some(0));
            runs.clear();
            assert_eq!(runs.claim(n), None);
            runs.publish(2 * n..3 * n);
            assert_eq!(runs.claim(n), Some(2 * n));
            assert_eq!(runs.claim(n), None);
        }
    }

    #[test]
    fn published_region_can_be_reused_while_another_region_is_unswept() {
        init_for_tests();
        // Other heap tests use class zero. These two regions are private here.
        let class = 2;
        let n = claim_slots(class) as u64;
        let words = n as usize / 64;
        unsafe {
            alloc_word_or(class, 0, 3);
            mark_word_or(class, 0, 2);
            alloc_word_or(class, words, 4);
        }
        let runs = SweptRuns::new();
        assert_eq!(runs.claim(n), None);
        std::thread::scope(|scope| {
            let (publish, published) = std::sync::mpsc::channel();
            let (allocated, allocation) = std::sync::mpsc::channel();
            let runs = &runs;
            scope.spawn(move || {
                published.recv().unwrap();
                assert_eq!(runs.claim(n), Some(0));
                // Publication exposes the survivor and cleared mark bits.
                assert_eq!(unsafe { alloc_word_load(class, 0) }, 2);
                assert_eq!(unsafe { mark_word_load(class, 0) }, 0);
                unsafe { alloc_word_or(class, 0, 1) };
                allocated.send(()).unwrap();
            });
            assert_eq!(unsafe { sweep_word_range(class, 0, words) }, (1, 1));
            runs.publish(0..n);
            publish.send(()).unwrap();
            allocation.recv().unwrap();
            assert_eq!(unsafe { alloc_word_load(class, words) }, 4);
            assert_eq!(unsafe { sweep_word_range(class, words, 2 * words) }, (0, 1));
            // Finishing the remaining sweep must preserve the reused slot.
            assert_eq!(unsafe { alloc_word_load(class, 0) }, 3);
        });

        // The next cycle retains the replacement but not the old survivor.
        // Retire an unconsumed previous-cycle entry before sweeping again:
        // republishing a region must never leave two ways to claim it.
        runs.publish(n..2 * n);
        runs.clear();
        unsafe { mark_word_or(class, 0, 1) };
        assert_eq!(unsafe { sweep_word_range(class, 0, 2 * words) }, (1, 1));
        assert_eq!(unsafe { alloc_word_load(class, 0) }, 1);
        assert_eq!(unsafe { mark_word_load(class, 0) }, 0);
        runs.publish(0..n);
        assert_eq!(runs.claim(n), Some(0));
        assert_eq!(runs.claim(n), None);
    }

    #[test]
    fn allocation_range_check_rejects_swept_slots() {
        init_for_tests();
        let class = 0;
        let addr = region_base(class);

        unsafe { set_allocated(class, 0) };
        assert_allocated_range(addr, slot_size(class));
        assert_allocated_range(addr + 1, 1);
        assert_allocated_range(1, 8);

        let swept = unsafe { sweep_word_range(class, 0, 1) };
        assert_eq!(swept, (0, 1));
        let panic = std::panic::catch_unwind(|| assert_allocated_range(addr, 1));
        assert!(panic.is_err());
    }
}
