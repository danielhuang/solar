use std::arch::asm;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use rustix_futex_sync::RwLock;

use crate::gc::{
    BIG_ALLOCS, BigAlloc, MY_SLOT, ORPHANED_TOTAL_ALLOCATIONS, THREAD_REGISTRY, ThreadAllocState,
    ThreadSlot,
};

// ---------------------------------------------------------------------------
// GC_LOCK: prevents the GC from running while a thread is between
// sol_thread_spawn and register_thread.  Spawners hold a read lock
// (keeping the env on their stack so the GC can find it).  The GC
// holds a write lock for the entire STW cycle.
// ---------------------------------------------------------------------------

pub(crate) static GC_LOCK: RwLock<()> = RwLock::new(());

/// Wait for the lifecycle lock without preventing an active GC pause.
fn gc_read_lock()
-> rustix_futex_sync::lock_api::RwLockReadGuard<'static, rustix_futex_sync::RawRwLock, ()> {
    loop {
        if let Some(guard) = GC_LOCK.try_read() {
            return guard;
        }
        if !MY_SLOT.get().is_null() {
            crate::gc::safepoint();
        }
        std::thread::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Thread registration
// ---------------------------------------------------------------------------

fn register_thread(stack_base: *mut usize) {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    let slot = Box::new(ThreadSlot {
        stack_base,
        stack_top: AtomicPtr::new(std::ptr::null_mut()),
        saved_regs: std::array::from_fn(|_| AtomicU64::new(0)),
        tls_statics: std::ptr::null(),
        tls_statics_len: 0,
        alloc: UnsafeCell::new(ThreadAllocState::new()),
        // Pre-reserve so the write barrier never reallocates on its hot path.
        gray_buf: UnsafeCell::new(Vec::with_capacity(crate::gc::GRAY_BUF_CAP)),
        in_syscall: AtomicBool::new(false),
        gc_waiting_epoch: AtomicU64::new(0),
    });
    let slot_ptr: *const ThreadSlot = &*slot;
    THREAD_REGISTRY.write().unwrap().insert(tid, slot);
    MY_SLOT.set(slot_ptr);
}

fn unregister_thread() {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    // Exclude stop-the-world pauses while removing the slot and publishing
    // its remaining roots and allocations. The collector takes GC_LOCK.write()
    // before selecting registered threads to signal.
    let _gc_guard = gc_read_lock();
    // Delayed GC signals must never inspect a slot after it has been freed.
    MY_SLOT.set(std::ptr::null());
    if let Some(slot) = THREAD_REGISTRY.write().unwrap().remove(&tid) {
        if slot.tls_statics_len != 0 {
            let entries =
                unsafe { std::slice::from_raw_parts(slot.tls_statics, slot.tls_statics_len) };
            let mut retired = crate::gc::RETIRED_TLS_STATICS.lock().unwrap();
            retired.extend(entries.iter().map(|entry| crate::StaticEntry {
                addr: entry.addr,
                size: entry.size,
                mark_fn: entry.mark_fn,
            }));
        }
        // Flush any pointers this thread shaded but didn't yet publish to GRAY.
        // (Objects reachable only from its stack and never stored to the heap
        // are legitimately dead; anything published went through the barrier.)
        unsafe { crate::gc::flush_gray_buf(&slot) };
        let mut alloc_state = slot.alloc.into_inner();
        alloc_state.reset_claims();
        ORPHANED_TOTAL_ALLOCATIONS.fetch_add(alloc_state.total_allocations, Ordering::Relaxed);
        // The thread's arena allocations live in the global bitmaps already;
        // only its not-yet-published big allocations need handing over.
        if !alloc_state.big_allocs.is_empty() {
            let mut big = BIG_ALLOCS.lock().unwrap();
            for b in alloc_state.big_allocs {
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
    }
}

// ---------------------------------------------------------------------------
// Thread entry points
// ---------------------------------------------------------------------------

/// Per-thread entry point. Captures stack base, registers thread, drops
/// the optional GC read guard, executes entry_fn(env) via asm call
/// (forces stack frame), then unregisters on return.
pub unsafe fn sol_thread_start(
    entry_fn: unsafe extern "C" fn(*mut c_void),
    env: *mut c_void,
    register_tls: Option<unsafe extern "C" fn()>,
    init_tls: Option<unsafe extern "C" fn(*mut c_void)>,
    gc_guard: Option<
        rustix_futex_sync::lock_api::RwLockReadGuard<'static, rustix_futex_sync::RawRwLock, ()>,
    >,
) {
    unsafe extern "C" fn thread_inner(
        entry_fn: unsafe extern "C" fn(*mut c_void),
        env: *mut c_void,
    ) {
        unsafe {
            entry_fn(env);
        }
    }

    unsafe {
        let rsp: *mut usize;
        asm!("mov {}, rsp", out(reg) rsp);
        register_thread(rsp);

        // The spawner's GC read guard keeps the collector from observing this
        // slot until its immutable TLS root descriptors have been installed.
        if let Some(register_tls) = register_tls {
            register_tls();
        }

        // Thread is now registered and visible to the GC.
        // Drop the read guard so the GC can acquire its write lock.
        drop(gc_guard);

        if let Some(init_tls) = init_tls {
            init_tls(std::ptr::null_mut());
        }

        asm!(
            "call {func}",
            func = sym thread_inner,
            in("rdi") entry_fn,
            in("rsi") env,
            clobber_abi("C"),
        );

        unregister_thread();
    }
}

/// Spawns a registered Solar mutator thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_thread_spawn(
    fn_ptr: unsafe extern "C" fn(*mut c_void),
    env: *mut c_void,
    register_tls: Option<unsafe extern "C" fn()>,
    init_tls: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    struct SendArgs {
        fn_ptr: unsafe extern "C" fn(*mut c_void),
        env: *mut c_void,
        register_tls: Option<unsafe extern "C" fn()>,
        init_tls: Option<unsafe extern "C" fn(*mut c_void)>,
        gc_guard: Option<
            rustix_futex_sync::lock_api::RwLockReadGuard<'static, rustix_futex_sync::RawRwLock, ()>,
        >,
    }
    unsafe impl Send for SendArgs {}

    let gc_guard = gc_read_lock();
    let args = SendArgs {
        fn_ptr,
        env,
        register_tls,
        init_tls,
        gc_guard: Some(gc_guard),
    };
    std::thread::spawn(move || {
        let args = args;
        unsafe {
            sol_thread_start(
                args.fn_ptr,
                args.env,
                args.register_tls,
                args.init_tls,
                args.gc_guard,
            )
        };
    });
}

/// Attaches generated TLS root descriptors to the current registered thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_thread_register_statics(
    statics: *const crate::StaticEntry,
    statics_len: usize,
) {
    let slot = crate::gc::MY_SLOT.get();
    assert!(!slot.is_null(), "TLS statics require a registered thread");
    // SAFETY: called once by the owning thread while its registration GC read
    // guard is held. The descriptors and their TLS slots live until exit.
    let slot = unsafe { &mut *(slot.cast_mut()) };
    slot.tls_statics = statics;
    slot.tls_statics_len = statics_len;
}

/// Allocates a stable cell for one thread-local static. Cells intentionally
/// live until process exit because a Solar reference may outlive its owner.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_thread_static_alloc(size: usize, align: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size.max(1), align.max(1)).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "failed to allocate thread-local static");
    ptr
}
