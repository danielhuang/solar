#![allow(clippy::missing_safety_doc)]

use std::env;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub mod arith;
pub mod file;
pub mod futex;
pub mod gc;
pub mod heap;
pub mod init_cell;
pub mod mem;
pub mod net;
pub mod panic;
pub mod process;
pub mod thread;
pub mod thread_pool;
pub mod time;

pub(crate) fn read_env_bool(name: &str) -> bool {
    match env::var(name).as_deref() {
        Ok("1" | "true") => true,
        Ok("0" | "false") => false,
        Err(env::VarError::NotPresent) => false,
        Err(e) => panic!("Failed to read {name} environment variable: {e}"),
        Ok(x) => panic!("Invalid value for {name} environment variable: {x}"),
    }
}

/// Force-disable the GC (bump-allocator mode: allocate, never collect). Emitted
/// by codegen into `main` *before* `sol_start` in the **debug** build only — that
/// pipeline's simplified single-clang compile does not run the write-barrier
/// pass, so a real collection could free live objects whose stored pointers were
/// never shaded. `sol_start` OR-folds this into the `SOLAR_DISABLE_GC` env flag
/// (rather than overwriting it) so a call here always sticks.
#[unsafe(no_mangle)]
pub extern "C" fn sol_disable_gc() {
    // SAFETY: called before `sol_start`, single-threaded.
    unsafe { gc::DISABLE_GC.set(true) };
}

/// A `static` slot registered as a GC root. Codegen emits one entry per
/// pointer-carrying `static` global; the collector runs `mark_fn` over the
/// slot (`addr`, `size`) at both stop-the-world root scans.
#[repr(C)]
pub struct StaticEntry {
    pub addr: *mut u8,
    pub size: u64,
    pub mark_fn: mem::MarkFn,
}
// SAFETY: entries live in the program's immutable static data and point at
// global slots valid for the process lifetime. The GC thread only reads the
// slots during stop-the-world pauses (no mutator is running).
unsafe impl Send for StaticEntry {}
unsafe impl Sync for StaticEntry {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_start(
    solar_main: unsafe extern "C" fn(*mut c_void),
    statics: *const StaticEntry,
    statics_len: usize,
) {
    let start = Instant::now();
    panic::install_panic_hook();

    // SAFETY: no threads exist yet; these `InitCell` writes all happen before
    // the thread pool / GC thread / main mutator thread are spawned below.
    unsafe {
        gc::ENABLE_STAT_PRINTS.set(read_env_bool("SOLAR_PRINT_GC_STATS"));
        gc::ENABLE_ALLOC_PRINTS.set(read_env_bool("SOLAR_PRINT_ALLOCS"));
        // OR-fold so a prior `sol_disable_gc()` call (debug builds) is preserved
        // rather than overwritten by the env flag.
        gc::DISABLE_GC.set(gc::DISABLE_GC.get() | read_env_bool("SOLAR_DISABLE_GC"));
    }

    gc::install_signal_handler();
    heap::init();
    file::init();
    LazyLock::force(&thread_pool::THREAD_POOL);

    // The generated statics root table lives in the program's immutable data
    // for the process lifetime.
    let statics: &'static [StaticEntry] = if statics.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(statics, statics_len) }
    };

    // Dedicated collector thread. Mutators only ever wake it (via request_gc);
    // collection runs concurrently on this thread.
    let gc_handle = gc::spawn_gc_thread(statics);

    // Run main via sol_thread_start (registers thread, calls entry, unregisters)
    unsafe {
        thread::sol_thread_start(solar_main, null_mut(), None);
    }

    // Main has unregistered; stop the collector before touching the heap for
    // stats so no cycle races with the reads below.
    gc::shutdown_gc_thread(gc_handle);

    // Stats printing (after the main thread unregistered, outside STW).
    let enable_stat_prints = gc::ENABLE_STAT_PRINTS.get();
    if enable_stat_prints {
        let total_allocations = gc::ORPHANED_TOTAL_ALLOCATIONS.load(Ordering::Relaxed);
        if gc::DISABLE_GC.get() {
            eprintln!("gc was disabled");
            eprintln!("total allocations: {total_allocations}");
        } else {
            let (mut live_count, mut live_size) = heap::live_slots();
            let big = gc::BIG_ALLOCS.lock().unwrap();
            live_count += big.len();
            live_size += big.values().map(|a| a.size).sum::<usize>();
            drop(big);
            eprintln!("memory used: {live_size} bytes");
            eprintln!("{live_count}/{total_allocations} allocations live");
        }
        eprintln!("total time: {:?}", start.elapsed());
        if total_allocations > 0 {
            eprintln!(
                "avg {:?} per allocation (includes non-allocation time)",
                start.elapsed().div_f64(total_allocations as f64)
            )
        }
    }
}
