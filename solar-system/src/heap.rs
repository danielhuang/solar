//! Address-based size-class heap.
//!
//! Allocations up to 1 GiB are rounded up to a power-of-2 size class. Each
//! size class owns a 1 TiB sub-region of one big `mmap` reservation, so the
//! size class of a pointer is just `(p - ARENA_BASE) / 1TiB` — `O(1)`, no
//! tree. Two side bitmaps (1 bit per slot per region) track which slots are
//! allocated and which were marked in the current GC cycle; a side metadata
//! table (16 B per slot, only for classes with slot size >= 128 B) stores the
//! `mark_fn` and the user-requested size for precise marking. Slots in classes
//! < 128 B carry no metadata and are conservatively scanned during marking.
//!
//! Allocations larger than 1 GiB bypass the arena entirely (see `mem::big_*`
//! and `gc`'s `BIG_ALLOCS`).
//!
//! All the reservations are `MAP_NORESERVE` and demand-paged, so the resident
//! footprint is proportional to the live heap, not to the ~28 TiB / ~290 GiB
//! of virtual address space reserved.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::mem::MarkFn;

/// Smallest size class: `1 << MIN_LOG` = 8 bytes.
pub const MIN_LOG: u32 = 3;
/// Largest arena size class: `1 << MAX_LOG` = 1 GiB.
pub const MAX_LOG: u32 = 30;
pub const NUM_CLASSES: usize = (MAX_LOG - MIN_LOG + 1) as usize; // 28
/// Each size class gets a `1 << REGION_LOG` = 1 TiB region.
pub const REGION_LOG: u32 = 40;
pub const REGION_SIZE: usize = 1usize << REGION_LOG;
pub const ARENA_SIZE: usize = NUM_CLASSES * REGION_SIZE; // ~28 TiB
pub const PAGE_LOG: u32 = 12;
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
pub const fn slot_size_log(class: usize) -> u32 {
    class as u32 + MIN_LOG
}
#[inline]
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
pub const fn bitmap_class_offset(class: usize) -> usize {
    (1usize << BITS_TOP) - (1usize << (BITS_TOP - class as u32))
}
/// Total bytes to reserve for one bitmap (a slight over-reserve).
pub const BITMAP_TOTAL: usize = 1usize << BITS_TOP; // 32 GiB

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
pub const fn meta_class_offset(class: usize) -> usize {
    debug_assert!(class >= META_MIN_CLASS);
    (1usize << META_TOP) - (1usize << (META_TOP + META_MIN_CLASS as u32 - class as u32))
}
/// Total bytes to reserve for the metadata table (a slight over-reserve).
pub const META_TOTAL: usize = 1usize << META_TOP; // 256 GiB

// ---------------------------------------------------------------------------
// Global state (set once by `init`, then effectively const).
// ---------------------------------------------------------------------------

static ARENA_BASE: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BITS: AtomicUsize = AtomicUsize::new(0);
static MARK_BITS: AtomicUsize = AtomicUsize::new(0);
static META_BASE: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U64: AtomicU64 = AtomicU64::new(0);
/// Next slot index to hand out for each class. `claim_run` `fetch_add`s it.
/// Reset to 0 by `reset_frontier` after a sweep frees most of a class.
static NEXT_SLOT: [AtomicU64; NUM_CLASSES] = [ZERO_U64; NUM_CLASSES];
/// High-water mark (in slots) for each class — the furthest slot ever handed
/// out. Never decreases. Sweep walks `[0, HWM)`; `lookup_arena` short-circuits
/// past it.
static HWM: [AtomicU64; NUM_CLASSES] = [ZERO_U64; NUM_CLASSES];

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
    if ARENA_BASE.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let arena = mmap_reserve(ARENA_SIZE, "arena");
        let alloc_bits = mmap_reserve(BITMAP_TOTAL, "alloc bitmap");
        let mark_bits = mmap_reserve(BITMAP_TOTAL, "mark bitmap");
        let meta = mmap_reserve(META_TOTAL, "metadata table");
        ALLOC_BITS.store(alloc_bits, Ordering::Relaxed);
        MARK_BITS.store(mark_bits, Ordering::Relaxed);
        META_BASE.store(meta, Ordering::Relaxed);
        // Published last; `arena_base() != 0` gates everything else.
        ARENA_BASE.store(arena, Ordering::Relaxed);
    }
}

#[inline]
pub fn arena_base() -> usize {
    ARENA_BASE.load(Ordering::Relaxed)
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
pub fn region_base(class: usize) -> usize {
    arena_base() + (class << REGION_LOG)
}
#[inline]
pub fn slot_index(p: usize, region_base: usize, class: usize) -> usize {
    (p - region_base) >> slot_size_log(class)
}
#[inline]
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
    (ALLOC_BITS.load(Ordering::Relaxed) + bitmap_class_offset(class)) as *mut AtomicU64
}
#[inline]
fn mark_class_base(class: usize) -> *mut AtomicU64 {
    (MARK_BITS.load(Ordering::Relaxed) + bitmap_class_offset(class)) as *mut AtomicU64
}

#[inline]
pub unsafe fn is_allocated(class: usize, slot: usize) -> bool {
    let w = unsafe { &*alloc_class_base(class).add(slot >> 6) };
    w.load(Ordering::Relaxed) & bit_mask(slot) != 0
}
#[inline]
pub unsafe fn set_allocated(class: usize, slot: usize) {
    // Non-atomic read-modify-write: the only thread that writes this word until
    // the next stop-the-world (sweep) is the one that claimed `slot`'s run, and
    // claims are bitmap-word-aligned (see `claim_slots`), so no other thread
    // touches this word concurrently. Avoids a `LOCK OR` on the alloc hot path.
    let w = unsafe { &*alloc_class_base(class).add(slot >> 6) };
    w.store(
        w.load(Ordering::Relaxed) | bit_mask(slot),
        Ordering::Relaxed,
    );
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
pub unsafe fn meta_entry(class: usize, slot: usize) -> *mut MetaEntry {
    let base = META_BASE.load(Ordering::Relaxed) + meta_class_offset(class);
    unsafe { (base as *mut MetaEntry).add(slot) }
}

// ---------------------------------------------------------------------------
// Allocation frontier.
// ---------------------------------------------------------------------------

/// Claim a fresh run of slots for `class`. Returns `[start, end)` slot indices.
/// The run may contain survivors from a previous cycle (after a frontier
/// reset) — the caller must skip slots whose allocated bit is set.
#[inline]
pub fn claim_run(class: usize) -> (u64, u64) {
    let n = claim_slots(class) as u64;
    let s = NEXT_SLOT[class].fetch_add(n, Ordering::Relaxed);
    let e = s + n;
    HWM[class].fetch_max(e, Ordering::Relaxed);
    (s, e)
}
#[inline]
pub fn hwm(class: usize) -> u64 {
    HWM[class].load(Ordering::Relaxed)
}
#[inline]
pub fn reset_frontier(class: usize) {
    NEXT_SLOT[class].store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Lookup (used by the GC's conservative scan).
// ---------------------------------------------------------------------------

pub enum MarkKind {
    Precise {
        mark_fn: MarkFn,
        size: u64,
    },
    /// Conservatively scan `[base, base + slot_size)`.
    Conservative {
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
/// concurrent calls (they're partitioned by the sweep driver) and that all
/// mutators are stopped.
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
