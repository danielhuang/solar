use std::arch::asm;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use rustix_futex_sync::RwLock;

use crate::gc::{
    BIG_ALLOCS, BigAlloc, MY_SLOT, ORPHANED_TOTAL_ALLOCATIONS, THREAD_REGISTRY, ThreadAllocState,
    ThreadSlot, block_gc_signal, unblock_gc_signal,
};

// ---------------------------------------------------------------------------
// GC_LOCK: prevents the GC from running while a thread is between
// sol_thread_spawn and register_thread.  Spawners hold a read lock
// (keeping the env on their stack so the GC can find it).  The GC
// holds a write lock for the entire STW cycle.
// ---------------------------------------------------------------------------

pub(crate) static GC_LOCK: RwLock<()> = RwLock::new(());

// ---------------------------------------------------------------------------
// Thread registration
// ---------------------------------------------------------------------------

fn register_thread(stack_base: *mut usize) {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    let slot = Box::new(ThreadSlot {
        stack_base,
        stack_top: AtomicPtr::new(std::ptr::null_mut()),
        saved_regs: std::array::from_fn(|_| AtomicU64::new(0)),
        alloc: UnsafeCell::new(ThreadAllocState::new()),
        in_alloc: AtomicBool::new(false),
        gc_pending_epoch: AtomicU64::new(0),
        gc_waiting_epoch: AtomicU64::new(0),
    });
    let slot_ptr: *const ThreadSlot = &*slot;
    block_gc_signal();
    THREAD_REGISTRY.write().unwrap().insert(tid, slot);
    MY_SLOT.set(slot_ptr);
    unblock_gc_signal();
}

fn unregister_thread() {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    // Hold GC_LOCK.read() so the GC can't run while we're unregistering.
    // Without this, we deadlock: block_gc_signal stops the thread from
    // responding to signals, while THREAD_REGISTRY.write() blocks behind
    // the GC's read lock — and the GC is waiting for this thread to ack.
    let _gc_guard = GC_LOCK.read();
    block_gc_signal();
    if let Some(slot) = THREAD_REGISTRY.write().unwrap().remove(&tid) {
        let alloc_state = slot.alloc.into_inner();
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
    // Clear MY_SLOT before unblocking so a pending signal sees null
    // and returns early, rather than accessing the freed slot.
    MY_SLOT.set(std::ptr::null());
    unblock_gc_signal();
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

        // Thread is now registered and visible to the GC.
        // Drop the read guard so the GC can acquire its write lock.
        drop(gc_guard);

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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_thread_spawn(
    fn_ptr: unsafe extern "C" fn(*mut c_void),
    env: *mut c_void,
) {
    struct SendArgs {
        fn_ptr: unsafe extern "C" fn(*mut c_void),
        env: *mut c_void,
        gc_guard: Option<
            rustix_futex_sync::lock_api::RwLockReadGuard<'static, rustix_futex_sync::RawRwLock, ()>,
        >,
    }
    unsafe impl Send for SendArgs {}

    let gc_guard = GC_LOCK.read();
    let args = SendArgs {
        fn_ptr,
        env,
        gc_guard: Some(gc_guard),
    };
    std::thread::spawn(move || {
        let args = args;
        unsafe { sol_thread_start(args.fn_ptr, args.env, args.gc_guard) };
    });
}
