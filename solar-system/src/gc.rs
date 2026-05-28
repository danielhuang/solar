use std::arch::asm;
use std::cell::{Cell, UnsafeCell};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use crate::heap::{self, MarkKind};
use crate::mem::MarkFn;

// ---------------------------------------------------------------------------
// Per-thread allocation state.
//
// The size-class arena, bitmaps and metadata table are global (see `heap`);
// the only per-thread allocator state is a claim cursor per size class plus a
// list of big (>1 GiB) allocations not yet published to the global registry.
// ---------------------------------------------------------------------------

/// Cursor over the slots a thread has claimed from one size class. `[cur, end)`
/// is the unconsumed part of the current claim; when `cur == end` the thread
/// claims a fresh run via `heap::claim_run`.
pub struct ThreadClassState {
    pub cur: u64,
    pub end: u64,
}

/// A big allocation made by this thread, not yet merged into `BIG_ALLOCS`.
pub struct BigAllocLocal {
    pub base: usize,
    pub size: usize,
    pub align: usize,
    pub mark_fn: usize,
}

pub struct ThreadAllocState {
    pub classes: [ThreadClassState; heap::NUM_CLASSES],
    pub big_allocs: Vec<BigAllocLocal>,
    pub new_size_since_last_gc: usize,
    pub total_allocations: usize,
}

impl Default for ThreadAllocState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadAllocState {
    pub fn new() -> Self {
        Self {
            classes: std::array::from_fn(|_| ThreadClassState { cur: 0, end: 0 }),
            big_allocs: Vec::new(),
            new_size_since_last_gc: 0,
            total_allocations: 0,
        }
    }
    /// Drop all claim cursors so the next allocation re-claims against the
    /// post-sweep frontier. Called at end of GC.
    pub fn reset_claims(&mut self) {
        for c in &mut self.classes {
            c.cur = 0;
            c.end = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Global atomics.
// ---------------------------------------------------------------------------

/// Total live size (in slot bytes + big-alloc bytes), recomputed by the GC at
/// the end of each cycle. Read by allocating threads for the trigger heuristic.
pub(crate) static TOTAL_LIVE_SIZE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ENABLE_STAT_PRINTS: AtomicBool = AtomicBool::new(false);
pub(crate) static ENABLE_ALLOC_PRINTS: AtomicBool = AtomicBool::new(false);
pub(crate) static DISABLE_GC: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Big allocations (>1 GiB), kept out of the size-class arena.
// ---------------------------------------------------------------------------

pub(crate) struct BigAlloc {
    pub size: usize,
    pub align: usize,
    pub mark_fn: usize,
}

/// All currently-live big allocations, keyed by base address. Populated at STW
/// from threads' `big_allocs` lists (and by `unregister_thread`); swept at the
/// end of each GC. Only the GC thread reads it during STW; `unregister_thread`
/// writes it (holding `GC_LOCK.read()`, so never during a cycle).
pub(crate) static BIG_ALLOCS: Mutex<BTreeMap<usize, BigAlloc>> = Mutex::new(BTreeMap::new());

/// Accumulated `total_allocations` from exited threads (for stats printing).
pub(crate) static ORPHANED_TOTAL_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Thread registry.
// ---------------------------------------------------------------------------

pub(crate) struct ThreadSlot {
    pub(crate) stack_base: *mut usize,
    pub(crate) stack_top: AtomicPtr<usize>,
    pub(crate) saved_regs: [AtomicU64; 6], // rbx, rbp, r12-r15 from ucontext
    pub(crate) alloc: UnsafeCell<ThreadAllocState>,
    /// True while `sol_alloc` is updating per-thread structures. If the GC
    /// signal arrives then, the handler defers (stores `gc_pending_epoch`)
    /// and `sol_alloc` self-suspends after its critical section.
    pub(crate) in_alloc: AtomicBool,
    pub(crate) gc_pending_epoch: AtomicU64,
    /// Set to epoch N when this thread acknowledges GC cycle N and is stopped.
    /// Monotonically increases.
    pub(crate) gc_waiting_epoch: AtomicU64,
}

// ThreadSlot has raw pointers / UnsafeCell but is only accessed safely:
// - the owning thread accesses `alloc` single-threadedly;
// - the GC thread accesses `alloc` only during STW (all mutators stopped).
unsafe impl Send for ThreadSlot {}
unsafe impl Sync for ThreadSlot {}

pub(crate) static THREAD_REGISTRY: LazyLock<RwLock<HashMap<i32, Box<ThreadSlot>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

// ---------------------------------------------------------------------------
// Signal-handler-accessible globals.
// ---------------------------------------------------------------------------

/// Last completed GC generation. During cycle N this is N-1; set to N when the
/// cycle finishes. Threads wait for it to reach their target generation.
static GC_EPOCH: AtomicU64 = AtomicU64::new(0);

thread_local! {
    pub(crate) static MY_SLOT: Cell<*const ThreadSlot> = const { Cell::new(std::ptr::null()) };
}

// ---------------------------------------------------------------------------
// Signal helpers.
// ---------------------------------------------------------------------------

fn gc_signal() -> i32 {
    libc::SIGRTMIN() + 4
}

pub(crate) fn block_gc_signal() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, gc_signal());
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

pub(crate) fn unblock_gc_signal() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, gc_signal());
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

// ---------------------------------------------------------------------------
// Signal handler (async-signal-safe: only atomics, write, futex).
// ---------------------------------------------------------------------------

unsafe extern "C" fn gc_signal_handler(
    _sig: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let wait_epoch = unsafe { (*info).si_value().sival_ptr as u64 };

    let slot = MY_SLOT.get();
    if slot.is_null() {
        return;
    }
    let slot = unsafe { &*slot };

    if slot.in_alloc.load(Ordering::Acquire) {
        slot.gc_pending_epoch.store(wait_epoch, Ordering::Release);
        return;
    }

    // The kernel placed the ucontext (all interrupted registers) on the signal
    // frame, below the interrupted RSP. Pointing `stack_top` at it makes the
    // conservative stack scan cover those registers too — including caller-
    // saved ones like rax holding freshly-allocated pointers.
    slot.stack_top
        .store(context as *mut usize, Ordering::Release);

    let uc = context as *const libc::ucontext_t;
    let gregs = unsafe { &(*uc).uc_mcontext.gregs };
    let reg_indices = [
        libc::REG_RBX,
        libc::REG_RBP,
        libc::REG_R12,
        libc::REG_R13,
        libc::REG_R14,
        libc::REG_R15,
    ];
    for (i, &ri) in reg_indices.iter().enumerate() {
        slot.saved_regs[i].store(gregs[ri as usize] as u64, Ordering::Release);
    }

    unsafe { notify_and_wait_for_gc(slot, wait_epoch) };
}

pub(crate) fn install_signal_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = gc_signal_handler as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(gc_signal(), &sa, std::ptr::null_mut());
    }
}

/// Mark this thread stopped for `wait_epoch`, wake the GC thread, then block
/// until GC_EPOCH reaches that value.
unsafe fn notify_and_wait_for_gc(slot: &ThreadSlot, wait_epoch: u64) {
    slot.gc_waiting_epoch.store(wait_epoch, Ordering::Release);
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            &slot.gc_waiting_epoch as *const AtomicU64,
            libc::FUTEX_WAKE,
            1i32 as i64,
        );
        loop {
            // Read once: the value used for the exit check MUST be the same one
            // passed to FUTEX_WAIT. Reading GC_EPOCH a second time for the
            // `expected` arg opens a lost-wakeup window — between the two reads
            // the GC could bump GC_EPOCH and FUTEX_WAKE, and we'd then park on
            // the already-current value with the wake gone.
            let cur = GC_EPOCH.load(Ordering::Acquire);
            if cur >= wait_epoch {
                break;
            }
            libc::syscall(
                libc::SYS_futex,
                &GC_EPOCH as *const AtomicU64,
                libc::FUTEX_WAIT,
                cur as u32,
                std::ptr::null::<libc::timespec>(),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Cooperative self-suspend (from sol_alloc when gc_pending_epoch is set).
// ---------------------------------------------------------------------------

pub(crate) unsafe fn self_suspend(slot: &ThreadSlot, wait_epoch: u64) {
    unsafe {
        asm!(
            "call {}",
            sym self_suspend_inner,
            in("rdi") slot as *const ThreadSlot,
            in("rsi") wait_epoch,
            clobber_abi("C"),
        );
    }
}

unsafe extern "C" fn self_suspend_inner(slot: *const ThreadSlot, wait_epoch: u64) {
    // Snapshot callee-saved registers before any Rust code can use them as
    // scratch. rbx/rbp can't be explicit asm operands, so dump all six to a
    // memory array via a pointer in rax.
    let mut saved_regs: [u64; 6] = [0; 6];
    unsafe {
        asm!(
            "mov [rax + 0], rbx",
            "mov [rax + 8], rbp",
            "mov [rax + 16], r12",
            "mov [rax + 24], r13",
            "mov [rax + 32], r14",
            "mov [rax + 40], r15",
            in("rax") saved_regs.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
    unsafe {
        let slot = &*slot;
        for (i, &val) in saved_regs.iter().enumerate() {
            slot.saved_regs[i].store(val, Ordering::Release);
        }
        let rsp: *mut usize;
        asm!("mov {}, rsp", out(reg) rsp);
        slot.stack_top.store(rsp, Ordering::Release);
        notify_and_wait_for_gc(slot, wait_epoch);
    }
}

// ---------------------------------------------------------------------------
// GC core.
// ---------------------------------------------------------------------------

pub(crate) unsafe fn run_gc() {
    unsafe {
        asm!("call {}", sym run_gc_inner, clobber_abi("C"));
    }
}

#[allow(clippy::redundant_locals)]
unsafe extern "C" fn run_gc_inner() {
    // Snapshot the caller's callee-saved registers at the very top, before the
    // compiler can use any of them as scratch — they may hold the only live
    // reference to a freshly-allocated object.
    let mut saved_regs: [u64; 6] = [0; 6];
    unsafe {
        asm!(
            "mov [rax + 0], rbx",
            "mov [rax + 8], rbp",
            "mov [rax + 16], r12",
            "mov [rax + 24], r13",
            "mov [rax + 32], r14",
            "mov [rax + 40], r15",
            in("rax") saved_regs.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
    unsafe {
        let my_tid = libc::syscall(libc::SYS_gettid) as i32;

        // --- Phase 1: Stop the world ---
        static GC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
        if GC_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        let _gc_write_guard = crate::thread::GC_LOCK.write();
        let this_epoch = GC_EPOCH.load(Ordering::Acquire) + 1;
        let enable_stat_prints = ENABLE_STAT_PRINTS.load(Ordering::Relaxed);
        let gc_start = std::time::Instant::now();
        if enable_stat_prints {
            eprintln!("running gc");
        }

        let registry = THREAD_REGISTRY.read().unwrap();

        // Signal all other threads, passing the epoch in si_value.
        let pid = libc::getpid();
        let sig = gc_signal();
        for &tid in registry.keys() {
            if tid != my_tid {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                info.si_signo = sig;
                info.si_code = libc::SI_QUEUE;
                #[repr(C)]
                struct SiginfoSetValue {
                    _si_signo: libc::c_int,
                    _si_errno: libc::c_int,
                    _si_code: libc::c_int,
                    _pad1: libc::c_int,
                    _pad2: libc::c_int,
                    si_value: libc::sigval,
                }
                let info_ptr = &mut info as *mut libc::siginfo_t as *mut SiginfoSetValue;
                (*info_ptr).si_value = libc::sigval {
                    sival_ptr: this_epoch as *mut libc::c_void,
                };
                libc::syscall(
                    libc::SYS_rt_tgsigqueueinfo,
                    pid as i64,
                    tid as i64,
                    sig as i64,
                    &info as *const libc::siginfo_t,
                );
            }
        }

        // Wait for all other threads to acknowledge.
        for (&tid, slot) in registry.iter() {
            if tid != my_tid {
                loop {
                    // Read once: the value checked against `this_epoch` must be
                    // the same one passed to FUTEX_WAIT. A second load for the
                    // `expected` arg opens a lost-wakeup window — the thread can
                    // store its epoch and FUTEX_WAKE between the two loads,
                    // leaving us to park on the already-acked value forever.
                    let cur = slot.gc_waiting_epoch.load(Ordering::Acquire);
                    if cur >= this_epoch {
                        break;
                    }
                    libc::syscall(
                        libc::SYS_futex,
                        &slot.gc_waiting_epoch as *const AtomicU64,
                        libc::FUTEX_WAIT,
                        cur as u32,
                        std::ptr::null::<libc::timespec>(),
                    );
                }
            }
        }

        // --- Phase 2: Capture rsp. ---
        let stack_top: *mut usize;
        asm!("mov {}, rsp", out(reg) stack_top);

        // --- Phase 3: Build the big-allocation snapshot. ---
        // Merge each thread's thread-local big_allocs into the global registry,
        // then snapshot it (sorted by base) for the parallel mark workers.
        let big_snapshot: Arc<Vec<BigSnap>> = {
            let mut big = BIG_ALLOCS.lock().unwrap();
            for slot in registry.values() {
                let alloc_state = &mut *slot.alloc.get();
                for b in alloc_state.big_allocs.drain(..) {
                    big.insert(
                        b.base,
                        BigAlloc {
                            size: b.size,
                            align: b.align,
                            mark_fn: b.mark_fn,
                        },
                    );
                }
            }
            Arc::new(
                big.iter()
                    .map(|(&base, a)| BigSnap {
                        base,
                        end: base + a.size,
                        size: a.size,
                        align: a.align,
                        mark_fn: a.mark_fn,
                        marked: AtomicBool::new(false),
                    })
                    .collect(),
            )
        };

        // --- Phase 4: Collect roots (conservative: stacks + saved registers). ---
        let mut roots: Vec<Root> = Vec::new();
        for (&tid, slot) in registry.iter() {
            let base = slot.stack_base;
            let top = if tid == my_tid {
                stack_top
            } else {
                slot.stack_top.load(Ordering::Acquire)
            };
            if !top.is_null() && !base.is_null() && top < base {
                roots.push(Root::StackRange(
                    top.add(1) as *mut u8..base.add(1) as *mut u8,
                ));
            }
            if tid == my_tid {
                for &val in &saved_regs {
                    roots.push(Root::Register(val as *mut u8));
                }
            } else {
                for reg in &slot.saved_regs {
                    roots.push(Root::Register(reg.load(Ordering::Acquire) as *mut u8));
                }
            }
        }

        // --- Phase 5: Parallel mark. ---
        let mark_start = std::time::Instant::now();
        let pool = &*crate::thread_pool::THREAD_POOL;
        let n_workers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1);
        let chunk_size = (roots.len() + n_workers - 1).max(1) / n_workers.max(1);

        for chunk in roots.chunks(chunk_size) {
            let chunk = chunk.to_vec();
            let snap = big_snapshot.clone();
            pool.submit(move || {
                let arena_base = heap::arena_base();
                let big_len = snap.len();
                // Seed the worklist from this chunk's roots, pre-filtering
                // obvious non-pointers so the drain loop only sees candidates.
                let mut worklist: Vec<usize> = Vec::new();
                for root in &chunk {
                    match root {
                        Root::StackRange(range) => {
                            let mut word = range.start as *const usize;
                            assert!(word.is_aligned());
                            let end = range.end as *const usize;
                            while word < end {
                                let v = *word;
                                if plausible(v, arena_base, big_len) {
                                    worklist.push(v);
                                }
                                word = word.add(1);
                            }
                        }
                        Root::Register(p) => {
                            let v = *p as usize;
                            if plausible(v, arena_base, big_len) {
                                worklist.push(v);
                            }
                        }
                    }
                }
                let mut ctx = MarkContext {
                    big_ptr: snap.as_ptr(),
                    big_len,
                    worklist: &mut worklist,
                    accum_class: u32::MAX,
                    accum_word: 0,
                    accum_bits: 0,
                };
                drain(&mut ctx, arena_base);
                // Keep the snapshot alive until all marking through `ctx` is
                // done.
                drop(snap);
            });
        }
        pool.wait_for_all();
        let mark_elapsed = mark_start.elapsed();
        let sweep_start = std::time::Instant::now();

        // --- Phase 6: Sweep. ---
        // 6a. Arena: partition each class's bitmap into word-range jobs.
        let per_class: Arc<Vec<(AtomicU64, AtomicU64)>> = Arc::new(
            (0..heap::NUM_CLASSES)
                .map(|_| (AtomicU64::new(0), AtomicU64::new(0)))
                .collect(),
        );
        const SWEEP_CHUNK_WORDS: usize = 1 << 12;
        for c in 0..heap::NUM_CLASSES {
            let words = heap::hwm_words(c);
            let mut w = 0usize;
            while w < words {
                let end = (w + SWEEP_CHUNK_WORDS).min(words);
                let per_class = per_class.clone();
                let (ws, we) = (w, end);
                pool.submit(move || {
                    let (live, freed) = heap::sweep_word_range(c, ws, we);
                    if live != 0 {
                        per_class[c].0.fetch_add(live, Ordering::Relaxed);
                    }
                    if freed != 0 {
                        per_class[c].1.fetch_add(freed, Ordering::Relaxed);
                    }
                });
                w = end;
            }
        }

        // 6b. Big allocations: sweep on the GC thread (there are few).
        let mut big_live_size = 0usize;
        let mut big_freed_count = 0usize;
        {
            let mut big = BIG_ALLOCS.lock().unwrap();
            big.clear();
            for s in big_snapshot.iter() {
                if s.marked.load(Ordering::Relaxed) {
                    big_live_size += s.size;
                    big.insert(
                        s.base,
                        BigAlloc {
                            size: s.size,
                            align: s.align,
                            mark_fn: s.mark_fn,
                        },
                    );
                } else {
                    big_freed_count += 1;
                    let layout =
                        std::alloc::Layout::from_size_align(s.size.max(1), s.align.max(1)).unwrap();
                    std::alloc::dealloc(s.base as *mut u8, layout);
                }
            }
        }

        // Ensure all arena sweep jobs (6a) have finished before reading
        // `per_class` in Phase 7.
        pool.wait_for_all();

        // --- Phase 7: Post-sweep accounting. ---
        let mut arena_live_size = 0usize;
        let mut freed_count = big_freed_count;
        for c in 0..heap::NUM_CLASSES {
            let live = per_class[c].0.load(Ordering::Relaxed);
            let freed = per_class[c].1.load(Ordering::Relaxed);
            arena_live_size += live as usize * heap::slot_size(c);
            freed_count += freed as usize;
            // Frontier reset: if less than half of [0, hwm) is live, re-fill
            // from the start next cycle to reuse the holes.
            if heap::hwm(c) > 2 * live {
                heap::reset_frontier(c);
            }
        }
        let new_total_live_size = arena_live_size + big_live_size;

        for slot in registry.values() {
            let alloc_state = &mut *slot.alloc.get();
            alloc_state.new_size_since_last_gc = 0;
            alloc_state.reset_claims();
        }

        if enable_stat_prints {
            eprintln!(
                "gc freed {freed_count} allocations in {:?} (mark {:?}, sweep {:?}); live {new_total_live_size} bytes",
                gc_start.elapsed(),
                mark_elapsed,
                sweep_start.elapsed(),
            );
        }

        TOTAL_LIVE_SIZE.store(new_total_live_size, Ordering::Release);

        // --- Phase 8: Resume. ---
        GC_EPOCH.fetch_add(1, Ordering::Release);
        GC_IN_PROGRESS.store(false, Ordering::Release);
        libc::syscall(
            libc::SYS_futex,
            &GC_EPOCH as *const AtomicU64,
            libc::FUTEX_WAKE,
            i32::MAX as i64,
        );
    }
}

// ---------------------------------------------------------------------------
// Marking.
// ---------------------------------------------------------------------------

/// One big allocation as seen by the mark workers (a read-only snapshot, plus
/// a mark flag).
pub(crate) struct BigSnap {
    pub base: usize,
    pub end: usize,
    pub size: usize,
    pub align: usize,
    pub mark_fn: usize,
    pub marked: AtomicBool,
}

/// Per-worker marking state. Created once per mark job; the same `*mut` is
/// threaded through `sol_gc_mark` so the C mark functions push into this
/// worker's worklist.
pub(crate) struct MarkContext {
    pub big_ptr: *const BigSnap,
    pub big_len: usize,
    /// Pending pointers to scan. Owned by the job closure; marking runs until
    /// it drains. Replaces the old unbounded recursion through `mark_atomic`.
    pub worklist: *mut Vec<usize>,
    // --- Batched mark-bit accumulator ---
    // Marking a slot only flips a bit in the mark bitmap. Consecutive marks of
    // a physically-sequential structure (e.g. a linked list whose nodes were
    // bump-allocated in order) hit the same 64-bit bitmap word, so we OR the
    // bits into a local accumulator and flush once per word with a single
    // atomic `fetch_or` — instead of one `fetch_or` per slot.
    /// Class of the word currently in the accumulator; `u32::MAX` = none.
    pub accum_class: u32,
    /// Bitmap word index currently in the accumulator.
    pub accum_word: usize,
    /// Mark bits accumulated for `(accum_class, accum_word)`, not yet flushed.
    pub accum_bits: u64,
}
impl MarkContext {
    #[inline]
    fn big(&self) -> &[BigSnap] {
        unsafe { std::slice::from_raw_parts(self.big_ptr, self.big_len) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_gc_mark(ctx: *mut u8, ptr: *mut u8) {
    // Called by the generated C mark functions for each pointer field. Just
    // enqueue it; the enclosing `drain` loop does the work — no recursion.
    let ctx = unsafe { &*(ctx as *const MarkContext) };
    unsafe { (*ctx.worklist).push(ptr as usize) };
}

#[derive(Clone)]
enum Root {
    StackRange(Range<*mut u8>),
    Register(*mut u8),
}
// Raw pointers in Root are GC-managed heap/stack addresses, used only during
// STW when all mutator threads are stopped.
unsafe impl Send for Root {}

/// Cheap pre-filter: could `v` point into a GC-managed allocation? When there
/// are no big allocations this is an exact arena-range test, which rejects
/// non-pointer words (tags, small integers) without a full `lookup_arena`.
/// With big allocations present it stays conservative (lets everything that
/// isn't an arena pointer through to the big-alloc binary search).
#[inline]
fn plausible(v: usize, arena_base: usize, big_len: usize) -> bool {
    v.wrapping_sub(arena_base) < heap::ARENA_SIZE || big_len != 0
}

/// Set the mark bit for `(class, slot)` via the per-word accumulator. Returns
/// `true` iff the slot was not already marked (so its children must be
/// scanned).
#[inline]
unsafe fn mark_slot_batched(ctx: &mut MarkContext, class: usize, slot: usize) -> bool {
    let word = slot >> 6;
    let mask = 1u64 << (slot & 63);
    if ctx.accum_class as usize == class && ctx.accum_word == word {
        let newly = ctx.accum_bits & mask == 0;
        ctx.accum_bits |= mask;
        return newly;
    }
    // Rolling over to a new word: flush the old one, start the new one.
    unsafe { flush_accum(ctx) };
    ctx.accum_class = class as u32;
    ctx.accum_word = word;
    ctx.accum_bits = mask;
    // A plain load is enough for the "newly marked?" answer: if another worker
    // has the bit set it has already scanned (or is scanning) that slot's
    // children, so skipping it here is safe; a stale `false` only costs a
    // redundant re-scan. Every accumulated bit is flushed with an atomic
    // `fetch_or` before the mark phase's barrier, so no mark is ever lost.
    unsafe { heap::mark_word_load(class, word) & mask == 0 }
}

/// Flush the accumulator into the global mark bitmap with one atomic RMW.
#[inline]
unsafe fn flush_accum(ctx: &mut MarkContext) {
    if ctx.accum_bits != 0 {
        unsafe { heap::mark_word_or(ctx.accum_class as usize, ctx.accum_word, ctx.accum_bits) };
        ctx.accum_bits = 0;
    }
}

/// Mark `p` if it falls inside a big (>1 GiB) allocation.
#[inline]
unsafe fn mark_big(ctx: &mut MarkContext, p: usize) {
    let (base, size, mark_fn_addr) = {
        let big = ctx.big();
        if big.is_empty() {
            return;
        }
        // Largest base <= p.
        let idx = match big.binary_search_by_key(&p, |s| s.base) {
            Ok(i) => i,
            Err(0) => return,
            Err(i) => i - 1,
        };
        let s = &big[idx];
        if p >= s.end {
            return;
        }
        if s.marked.swap(true, Ordering::Relaxed) {
            return;
        }
        (s.base, s.size, s.mark_fn)
    };
    let mark_fn: MarkFn = unsafe { std::mem::transmute::<usize, MarkFn>(mark_fn_addr) };
    unsafe {
        mark_fn(
            ctx as *mut MarkContext as *mut u8,
            base as *mut u8,
            size as u64,
        )
    };
}

/// Drain the worklist: mark each reachable object and enqueue its children.
/// A physically-linear chain (one child per object) is followed in place via
/// the inner loop, so the worklist stays empty and no work is pushed/popped.
unsafe fn drain(ctx: &mut MarkContext, arena_base: usize) {
    let big_len = ctx.big_len;
    while let Some(start) = unsafe { (*ctx.worklist).pop() } {
        let mut p = start;
        loop {
            let Some((class, slot, base, kind)) = (unsafe { heap::lookup_arena(p) }) else {
                unsafe { mark_big(ctx, p) };
                break;
            };
            if !unsafe { mark_slot_batched(ctx, class, slot) } {
                break; // already marked this cycle
            }
            match kind {
                MarkKind::Precise { mark_fn, size } => {
                    // The C mark fn enqueues this object's pointer fields.
                    unsafe { mark_fn(ctx as *mut MarkContext as *mut u8, base as *mut u8, size) };
                    break;
                }
                MarkKind::Conservative { slot_size } => {
                    // Scan the slot; follow the first child in place, enqueue
                    // the rest. For a one-child object this never touches the
                    // worklist.
                    let mut w = base as *const usize;
                    let end = (base + slot_size) as *const usize;
                    let mut next = 0usize;
                    let mut have_next = false;
                    while w < end {
                        let v = unsafe { *w };
                        w = unsafe { w.add(1) };
                        if plausible(v, arena_base, big_len) {
                            if have_next {
                                unsafe { (*ctx.worklist).push(v) };
                            } else {
                                next = v;
                                have_next = true;
                            }
                        }
                    }
                    if have_next {
                        p = next;
                        continue;
                    }
                    break;
                }
            }
        }
    }
    unsafe { flush_accum(ctx) };
}
