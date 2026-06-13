use std::arch::asm;
use std::cell::{Cell, UnsafeCell};
use std::collections::{BTreeMap, HashMap};
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
// Tri-color gray frontier.
//
// Kept deliberately SEPARATE from the alloc/mark bitmaps in `heap`: the mark
// bitmap is the "black / reached" set, this queue is the "gray" set of
// reached-but-not-yet-scanned pointer values. It is fed by the STW root scans
// and, during concurrent marking, by the write barrier and `sol_memcpy` (via
// per-thread buffers); it is drained by the GC thread's marker.
// ---------------------------------------------------------------------------

/// Global gray queue. Mutators append (in batches, via `gray_enqueue`) only
/// while `SOL_CONCURRENT_MARKING` is set; the GC thread drains it.
pub(crate) static GRAY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Per-thread gray buffer capacity. The buffer is pre-reserved to this size at
/// thread registration and flushed to `GRAY` on reaching it, so the barrier
/// never reallocates on the hot path.
pub(crate) const GRAY_BUF_CAP: usize = 512;

/// True while the current marking cycle has any big (>1 GiB) allocation. The
/// barrier consults it to decide whether a non-arena pointer could still be a
/// big-alloc pointer worth enqueuing.
pub(crate) static MARKING_HAS_BIG: AtomicBool = AtomicBool::new(false);

/// Append `v` to this thread's gray buffer, flushing to `GRAY` at capacity.
/// Caller must own `slot` (single producer) and must run inside
/// `with_signal_deferred` — the flush takes the `GRAY` lock, and the GC thread
/// also takes it during a pause, so being parked here would deadlock. (The
/// pre-reserved buffer never reallocates: we flush when it reaches capacity.)
#[inline]
unsafe fn gray_enqueue_raw(slot: &ThreadSlot, v: usize) {
    let buf = unsafe { &mut *slot.gray_buf.get() };
    buf.push(v);
    if buf.len() >= GRAY_BUF_CAP {
        GRAY.lock().unwrap().extend_from_slice(buf);
        buf.clear();
    }
}

/// Run `f` (which touches per-thread GC structures and may lock `GRAY`) with
/// the STW signal deferred, then honor any stop that arrived meanwhile. Mirrors
/// `sol_alloc`'s critical-section discipline: if the GC signal fires while we're
/// inside, the handler sees `in_alloc` and defers (records `gc_pending_epoch`)
/// instead of parking us mid-flush; we self-suspend cleanly at the end.
#[inline]
unsafe fn with_signal_deferred(slot: &ThreadSlot, f: impl FnOnce()) {
    slot.in_alloc.store(true, Ordering::Relaxed);
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    f();
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    slot.in_alloc.store(false, Ordering::Release);
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    let pending = slot.gc_pending_epoch.load(Ordering::Acquire);
    if pending != 0 {
        slot.gc_pending_epoch.store(0, Ordering::Relaxed);
        unsafe { self_suspend(slot, pending) };
    }
}

/// Flush a thread's residual gray buffer into `GRAY`. Called at STW (owner
/// stopped) or by the owner itself during `unregister_thread`.
pub(crate) unsafe fn flush_gray_buf(slot: &ThreadSlot) {
    let buf = unsafe { &mut *slot.gray_buf.get() };
    if !buf.is_empty() {
        GRAY.lock().unwrap().extend_from_slice(buf);
        buf.clear();
    }
}

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
    /// Per-thread gray buffer (single-producer: this thread's barrier /
    /// `sol_memcpy`). Flushed to `GRAY` at capacity by the owner, and drained
    /// by the GC thread at STW pause 2 while this thread is stopped.
    pub(crate) gray_buf: UnsafeCell<Vec<usize>>,
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
// - the owning thread accesses `alloc` and `gray_buf` single-threadedly;
// - the GC thread accesses `alloc` only during STW (all mutators stopped), and
//   `gray_buf` only during STW pause 2 (likewise stopped).
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
// Dedicated GC thread + cycle trigger.
//
// A single dedicated thread owns every collection. Mutators never collect; on
// the heap-growth heuristic they call `request_gc`, which just wakes the GC
// thread and returns — the mutator keeps running while the cycle proceeds. This
// removes the old asymmetry where the triggering thread became the collector
// (snapshotting its own registers/stack and special-casing its own tid).
// ---------------------------------------------------------------------------

/// Bumped by `request_gc` to ask for a cycle; the GC thread FUTEX_WAITs on it.
static GC_REQUEST: AtomicU64 = AtomicU64::new(0);
/// Set while a request is outstanding so an allocation storm issues at most one
/// wakeup per cycle (the GC thread clears it when it starts serving).
static GC_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set at shutdown so the GC thread exits its loop.
static GC_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Ask the GC thread to run a cycle. Non-blocking.
pub(crate) fn request_gc() {
    if GC_REQUESTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        GC_REQUEST.fetch_add(1, Ordering::Release);
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                &GC_REQUEST as *const AtomicU64,
                libc::FUTEX_WAKE,
                1i32 as i64,
            );
        }
    }
}

/// Spawn the dedicated collector thread. It blocks the GC signal (it is never a
/// mutator and must not stop itself) and is never entered into THREAD_REGISTRY,
/// so the stop-the-world signal sweep never targets it.
pub(crate) fn spawn_gc_thread() -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("solar-gc".into())
        .spawn(|| {
            block_gc_signal();
            gc_thread_main();
        })
        .unwrap()
}

/// Stop the collector thread and join it. Call after the last mutator has
/// unregistered, before reading heap stats.
pub(crate) fn shutdown_gc_thread(handle: std::thread::JoinHandle<()>) {
    GC_SHUTDOWN.store(true, Ordering::Release);
    GC_REQUEST.fetch_add(1, Ordering::Release);
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            &GC_REQUEST as *const AtomicU64,
            libc::FUTEX_WAKE,
            1i32 as i64,
        );
    }
    handle.join().unwrap();
}

fn gc_thread_main() {
    // Start at 0 (not a load): a request that arrived between spawn and here
    // already bumped GC_REQUEST to 1, and must be served — initializing from
    // the load would skip it and leave GC_REQUESTED stuck true, starving the
    // collector.
    let mut served = 0u64;
    loop {
        if GC_SHUTDOWN.load(Ordering::Acquire) {
            return;
        }
        let cur = GC_REQUEST.load(Ordering::Acquire);
        if cur == served {
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &GC_REQUEST as *const AtomicU64,
                    libc::FUTEX_WAIT,
                    cur as u32,
                    std::ptr::null::<libc::timespec>(),
                );
            }
            continue;
        }
        served = cur;
        // Let allocations during this cycle arm a fresh request for the next.
        GC_REQUESTED.store(false, Ordering::Release);
        unsafe { run_gc_cycle() };
    }
}

// ---------------------------------------------------------------------------
// Stop-the-world primitives (run on the GC thread; mutators are the targets).
// ---------------------------------------------------------------------------

/// Signal every registered thread to stop at `target_epoch`, then wait until
/// each acknowledges. Caller holds `GC_LOCK.write()` and the registry guard.
unsafe fn signal_and_wait(registry: &HashMap<i32, Box<ThreadSlot>>, target_epoch: u64) {
    let pid = unsafe { libc::getpid() };
    let sig = gc_signal();
    for &tid in registry.keys() {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
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
        unsafe {
            (*info_ptr).si_value = libc::sigval {
                sival_ptr: target_epoch as *mut libc::c_void,
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
    for slot in registry.values() {
        loop {
            // Read once: the value checked against `target_epoch` must be the
            // same one passed to FUTEX_WAIT, else a store+wake between two loads
            // leaves us parked on the already-acked value forever.
            let cur = slot.gc_waiting_epoch.load(Ordering::Acquire);
            if cur >= target_epoch {
                break;
            }
            unsafe {
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
}

/// Resume all stopped threads by advancing the global epoch (single writer:
/// the GC thread). `target_epoch` is monotonically `prev + 1` per pause.
unsafe fn resume_world(target_epoch: u64) {
    GC_EPOCH.store(target_epoch, Ordering::Release);
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            &GC_EPOCH as *const AtomicU64,
            libc::FUTEX_WAKE,
            i32::MAX as i64,
        );
    }
}

/// Merge threads' pending big-allocs into `BIG_ALLOCS` and snapshot it for the
/// marker. Called at STW pause 1.
unsafe fn snapshot_big_allocs(registry: &HashMap<i32, Box<ThreadSlot>>) -> Arc<Vec<BigSnap>> {
    let mut big = BIG_ALLOCS.lock().unwrap();
    for slot in registry.values() {
        let st = unsafe { &mut *slot.alloc.get() };
        for b in st.big_allocs.drain(..) {
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
}

/// Drain big-allocs born during concurrent marking out of thread-local lists.
/// They are conservatively live this cycle (allocated black). Called at pause 2.
unsafe fn drain_born_big(registry: &HashMap<i32, Box<ThreadSlot>>) -> Vec<BigAllocLocal> {
    let mut born = Vec::new();
    for slot in registry.values() {
        let st = unsafe { &mut *slot.alloc.get() };
        born.append(&mut st.big_allocs);
    }
    born
}

/// Conservatively scan one *stopped* thread's used stack + saved registers,
/// pushing plausible pointer values into `out`. Done while the thread is
/// stopped so the marker never reads a live mutator stack.
unsafe fn scan_thread_roots(
    slot: &ThreadSlot,
    arena_base: usize,
    big_len: usize,
    out: &mut Vec<usize>,
) {
    let base = slot.stack_base;
    let top = slot.stack_top.load(Ordering::Acquire);
    if !top.is_null() && !base.is_null() && top < base {
        let mut word = unsafe { top.add(1) } as *const usize;
        let end = unsafe { base.add(1) } as *const usize;
        while word < end {
            let v = unsafe { *word };
            if plausible(v, arena_base, big_len) {
                out.push(v);
            }
            word = unsafe { word.add(1) };
        }
    }
    for reg in &slot.saved_regs {
        let v = reg.load(Ordering::Acquire) as usize;
        if plausible(v, arena_base, big_len) {
            out.push(v);
        }
    }
}

/// Drain the global gray queue to empty on the calling thread, following heap
/// edges. Used both for concurrent marking (mutators concurrently append) and
/// for the pause-2 remark drain (world stopped).
unsafe fn drain_gray(big_snapshot: &Arc<Vec<BigSnap>>) {
    let arena_base = heap::arena_base();
    let big_len = big_snapshot.len();
    loop {
        let mut batch = {
            let mut g = GRAY.lock().unwrap();
            if g.is_empty() {
                break;
            }
            std::mem::take(&mut *g)
        };
        let mut ctx = MarkContext {
            big_ptr: big_snapshot.as_ptr(),
            big_len,
            worklist: &mut batch,
            accum_class: u32::MAX,
            accum_word: 0,
            accum_bits: 0,
        };
        unsafe { drain(&mut ctx, arena_base) };
    }
}

/// Parallel arena sweep (still STW). Returns `(live_bytes, freed_slots)`.
unsafe fn parallel_sweep_arena() -> (usize, usize) {
    let pool = &*crate::thread_pool::THREAD_POOL;
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
                let (live, freed) = unsafe { heap::sweep_word_range(c, ws, we) };
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
    pool.wait_for_all();
    let mut live_size = 0usize;
    let mut freed_count = 0usize;
    for c in 0..heap::NUM_CLASSES {
        let live = per_class[c].0.load(Ordering::Relaxed);
        let freed = per_class[c].1.load(Ordering::Relaxed);
        live_size += live as usize * heap::slot_size(c);
        freed_count += freed as usize;
        // Frontier reset: if less than half of [0, hwm) is live, re-fill from
        // the start next cycle to reuse the holes.
        if heap::hwm(c) > 2 * live {
            heap::reset_frontier(c);
        }
    }
    (live_size, freed_count)
}

/// Sweep big allocations (STW). Snapshot survivors are kept; born-during-mark
/// allocations are kept unconditionally. Returns `(live_bytes, freed_count)`.
unsafe fn sweep_big(
    big_snapshot: &Arc<Vec<BigSnap>>,
    born_big: Vec<BigAllocLocal>,
) -> (usize, usize) {
    let mut live_size = 0usize;
    let mut freed_count = 0usize;
    let mut big = BIG_ALLOCS.lock().unwrap();
    big.clear();
    for s in big_snapshot.iter() {
        if s.marked.load(Ordering::Relaxed) {
            live_size += s.size;
            big.insert(
                s.base,
                BigAlloc {
                    size: s.size,
                    align: s.align,
                    mark_fn: s.mark_fn,
                },
            );
        } else {
            freed_count += 1;
            let layout =
                std::alloc::Layout::from_size_align(s.size.max(1), s.align.max(1)).unwrap();
            unsafe { std::alloc::dealloc(s.base as *mut u8, layout) };
        }
    }
    for b in born_big {
        live_size += b.size;
        big.insert(
            b.base,
            BigAlloc {
                size: b.size,
                align: b.align,
                mark_fn: b.mark_fn,
            },
        );
    }
    (live_size, freed_count)
}

// ---------------------------------------------------------------------------
// GC cycle: STW root scan → concurrent mark → STW remark + sweep.
// ---------------------------------------------------------------------------

unsafe fn run_gc_cycle() {
    let enable_stat_prints = ENABLE_STAT_PRINTS.load(Ordering::Relaxed);
    let gc_start = std::time::Instant::now();
    if enable_stat_prints {
        eprintln!("running gc (concurrent)");
    }
    let arena_base = heap::arena_base();

    // ===== STW pause 1: snapshot, scan roots, enable the barrier. =====
    let pause1_start = std::time::Instant::now();
    let epoch1 = GC_EPOCH.load(Ordering::Acquire) + 1;
    let big_snapshot: Arc<Vec<BigSnap>>;
    {
        let _wg = crate::thread::GC_LOCK.write();
        let registry = THREAD_REGISTRY.read().unwrap();
        unsafe { signal_and_wait(&registry, epoch1) };

        big_snapshot = unsafe { snapshot_big_allocs(&registry) };
        let big_len = big_snapshot.len();

        // Materialize root pointer *values* into GRAY while threads are stopped.
        let mut roots: Vec<usize> = Vec::new();
        for slot in registry.values() {
            unsafe { scan_thread_roots(slot, arena_base, big_len, &mut roots) };
        }
        GRAY.lock().unwrap().extend_from_slice(&roots);

        // Turn the barrier on before resuming so no later store is missed.
        MARKING_HAS_BIG.store(big_len != 0, Ordering::Release);
        SOL_CONCURRENT_MARKING.store(true, Ordering::Release);

        unsafe { resume_world(epoch1) };
    }
    let pause1_elapsed = pause1_start.elapsed();

    // ===== Concurrent mark: drain GRAY while mutators run. =====
    let mark_start = std::time::Instant::now();
    unsafe { drain_gray(&big_snapshot) };
    let mark_elapsed = mark_start.elapsed();

    // ===== STW pause 2: disable barrier, remark, sweep. =====
    let pause2_start = std::time::Instant::now();
    let epoch2 = epoch1 + 1;
    let sweep_elapsed;
    let new_total_live_size;
    let freed_count;
    {
        let _wg = crate::thread::GC_LOCK.write();
        let registry = THREAD_REGISTRY.read().unwrap();
        unsafe { signal_and_wait(&registry, epoch2) };

        // No mutator is running, so none is mid-store: stop the barrier.
        SOL_CONCURRENT_MARKING.store(false, Ordering::Release);
        MARKING_HAS_BIG.store(false, Ordering::Release);

        let born_big = unsafe { drain_born_big(&registry) };

        // Flush residual gray buffers + re-scan roots, then drain to fixpoint.
        let mut remark: Vec<usize> = Vec::new();
        for slot in registry.values() {
            let buf = unsafe { &mut *slot.gray_buf.get() };
            remark.append(buf);
            unsafe { scan_thread_roots(slot, arena_base, big_snapshot.len(), &mut remark) };
        }
        GRAY.lock().unwrap().extend_from_slice(&remark);
        unsafe { drain_gray(&big_snapshot) };

        // Sweep (still parallel under STW).
        let sweep_start = std::time::Instant::now();
        let (arena_live, arena_freed) = unsafe { parallel_sweep_arena() };
        let (big_live, big_freed) = unsafe { sweep_big(&big_snapshot, born_big) };
        sweep_elapsed = sweep_start.elapsed();

        new_total_live_size = arena_live + big_live;
        freed_count = arena_freed + big_freed;

        for slot in registry.values() {
            let st = unsafe { &mut *slot.alloc.get() };
            st.new_size_since_last_gc = 0;
            st.reset_claims();
        }
        TOTAL_LIVE_SIZE.store(new_total_live_size, Ordering::Release);

        unsafe { resume_world(epoch2) };
    }
    let pause2_elapsed = pause2_start.elapsed();

    if enable_stat_prints {
        eprintln!(
            "gc freed {freed_count} allocations in {:?} (pause1 {pause1_elapsed:?}, concurrent mark {mark_elapsed:?}, pause2 {pause2_elapsed:?}, sweep {sweep_elapsed:?}); live {new_total_live_size} bytes",
            gc_start.elapsed(),
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

/// True while a concurrent marking phase is running. Set during STW pause 1
/// (before mutators resume) and cleared during STW pause 2; `sol_write_barrier`
/// reads it on every instrumented store. Exported with external linkage
/// (`no_mangle`): otherwise LTO could prove a never-externally-written flag and
/// fold the barrier fast path away.
#[unsafe(no_mangle)]
pub static SOL_CONCURRENT_MARKING: AtomicBool = AtomicBool::new(false);

/// Dijkstra-style insertion write barrier. The compiler's `write_barriers` pass
/// inserts a call after every store of a potentially-heap pointer `val` to a
/// non-stack destination `dst` (and after `llvm.memcpy`/`memmove` via the bulk
/// barrier). Inserting after `opt -O3` keeps LLVM's allocation elision intact;
/// the final `clang -O3` link inlines this fast path into the instrumented
/// stores.
///
/// While marking is active it *shades* `val`: enqueues it onto the gray
/// frontier so the marker scans it, preserving the tri-color invariant when a
/// pointer is stored into an already-scanned (black) object. `val` may be null
/// (e.g. vectorized stores with no single SSA value) — nothing to shade.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_write_barrier(dst: *mut u8, val: *mut u8) {
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { write_barrier_slow(dst, val) };
    }
}

#[cold]
#[inline(never)]
unsafe fn write_barrier_slow(_dst: *mut u8, val: *mut u8) {
    let v = val as usize;
    if v == 0 {
        return;
    }
    // Cheap filter: enqueue only values that could point into the arena (or any
    // value if big allocations exist this cycle). The marker rejects the rest
    // via `lookup_arena` / `mark_big`.
    let in_arena = v.wrapping_sub(heap::arena_base()) < heap::ARENA_SIZE;
    if !in_arena && !MARKING_HAS_BIG.load(Ordering::Relaxed) {
        return;
    }
    let slot = MY_SLOT.get();
    if slot.is_null() {
        return;
    }
    let slot = unsafe { &*slot };
    unsafe { with_signal_deferred(slot, || gray_enqueue_raw(slot, v)) };
}

/// Bulk write barrier for optimizer-generated `llvm.memcpy`/`memmove` (and any
/// other aggregate copy the compiler's pass instruments). Conservatively shades
/// the destination region when marking is active. The compiler inserts a call
/// to this after such intrinsics whose destination is not stack-derived.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_gc_memcpy_barrier(dst: *mut u8, size: usize) {
    if SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { memcpy_barrier(dst, size) };
    }
}

/// Conservatively shade a region just copied by `sol_memcpy` while marking is
/// active: every pointer-aligned word that could be a heap pointer is enqueued.
/// Closes the aggregate-copy hole the per-store barrier can't see.
#[inline]
pub(crate) unsafe fn memcpy_barrier(dst: *mut u8, size: usize) {
    let slot = MY_SLOT.get();
    if slot.is_null() {
        return;
    }
    let slot = unsafe { &*slot };
    let arena_base = heap::arena_base();
    let has_big = MARKING_HAS_BIG.load(Ordering::Relaxed);
    unsafe {
        with_signal_deferred(slot, || {
            let mut w = dst as *const usize;
            let end = (dst as *const u8).add(size & !7) as *const usize;
            while w < end {
                let v = *w;
                if v != 0 && (v.wrapping_sub(arena_base) < heap::ARENA_SIZE || has_big) {
                    gray_enqueue_raw(slot, v);
                }
                w = w.add(1);
            }
        })
    };
}

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
                        // Relaxed atomic load: during concurrent marking a
                        // mutator may be writing this word. On x86 an aligned
                        // word load/store is atomic, so we read either the old
                        // or new pointer — both are safe (a newly stored value
                        // is independently shaded by the write barrier).
                        let v = unsafe { (*(w as *const AtomicUsize)).load(Ordering::Relaxed) };
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
