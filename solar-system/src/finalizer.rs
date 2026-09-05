//! Weak callback registrations, promoted to roots before sweeping.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

use crate::gc::with_signal_deferred;

type RegisterTls = Option<unsafe extern "C" fn()>;
type InitTls = Option<unsafe extern "C" fn(*mut c_void)>;

struct Registration {
    callback: usize,
    pending: bool,
    dispatched: bool,
    register_tls: RegisterTls,
    init_tls: InitTls,
}

static REGISTRATIONS: LazyLock<Mutex<HashMap<usize, Registration>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers a fresh, immutable, heap-allocated `fn()` value. The caller must
/// retain its reference for the desired lifetime and register it only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_register_finalizer(
    callback: *mut u8,
    register_tls: RegisterTls,
    init_tls: InitTls,
) {
    if crate::gc::DISABLE_GC.get() {
        return;
    }
    unsafe {
        with_signal_deferred(|_| {
            // A function value is 16 bytes, so its allocation is in the arena.
            assert!(
                (callback as usize).wrapping_sub(crate::heap::arena_base())
                    < crate::heap::ARENA_SIZE
            );
            let old = REGISTRATIONS.lock().unwrap().insert(
                callback as usize,
                Registration {
                    callback: callback as usize,
                    pending: false,
                    dispatched: false,
                    register_tls,
                    init_tls,
                },
            );
            assert!(old.is_none(), "callback record registered twice");
        });
    }
}

/// Adds queued and executing callback records to the STW root snapshot.
pub(crate) fn roots(out: &mut Vec<usize>) {
    out.extend(
        REGISTRATIONS
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.pending)
            .map(|r| r.callback),
    );
}

/// Snapshots all dead registrations before any capture graph is marked.
/// Called after remark, while mutators are stopped, before every kind of sweep.
pub(crate) unsafe fn discover() -> Vec<usize> {
    let mut registrations = REGISTRATIONS.lock().unwrap();
    let mut roots = Vec::new();
    for r in registrations.values_mut() {
        if !r.pending && !unsafe { crate::heap::is_marked_addr(r.callback) } {
            r.pending = true;
            roots.push(r.callback);
        }
    }
    roots
}

/// Starts a registered mutator for this cycle's callbacks after sweeping.
/// Pending records remain explicit roots until each callback returns.
pub(crate) unsafe fn dispatch() {
    let mut registrations = REGISTRATIONS.lock().unwrap();
    let mut batch = Vec::new();
    let mut hooks = None;
    for r in registrations.values_mut() {
        if r.pending && !r.dispatched {
            r.dispatched = true;
            batch.push(r.callback);
            hooks = Some((r.register_tls, r.init_tls));
        }
    }
    drop(registrations);
    if let Some((register_tls, init_tls)) = hooks {
        unsafe {
            crate::thread::sol_thread_spawn(
                run_batch,
                Box::into_raw(Box::new(batch)).cast(),
                register_tls,
                init_tls,
            );
        }
    }
}

unsafe extern "C" fn run_batch(batch: *mut c_void) {
    let batch = unsafe { Box::from_raw(batch.cast::<Vec<usize>>()) };
    for &callback in batch.iter() {
        unsafe {
            let words = callback as *const usize;
            let function: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(*words);
            function(*words.add(1) as *mut c_void);
            with_signal_deferred(|_| {
                REGISTRATIONS.lock().unwrap().remove(&callback).unwrap();
            });
        }
    }
}
