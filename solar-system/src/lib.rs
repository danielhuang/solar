#![allow(clippy::missing_safety_doc)]

use std::env;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub mod arith;
pub mod futex;
pub mod gc;
pub mod heap;
pub mod io;
pub mod mem;
pub mod panic;
pub mod thread;
pub mod thread_pool;

pub(crate) fn read_env_bool(name: &str) -> bool {
    match env::var(name).as_deref() {
        Ok("1" | "true") => true,
        Ok("0" | "false") => false,
        Err(env::VarError::NotPresent) => false,
        Err(e) => panic!("Failed to read {name} environment variable: {e}"),
        Ok(x) => panic!("Invalid value for {name} environment variable: {x}"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_start(solar_main: unsafe extern "C" fn(*mut c_void)) {
    let start = Instant::now();
    panic::install_panic_hook();

    gc::ENABLE_STAT_PRINTS.store(read_env_bool("SOLAR_PRINT_GC_STATS"), Ordering::Relaxed);
    gc::ENABLE_ALLOC_PRINTS.store(read_env_bool("SOLAR_PRINT_ALLOCS"), Ordering::Relaxed);
    gc::DISABLE_GC.store(read_env_bool("SOLAR_DISABLE_GC"), Ordering::Relaxed);

    gc::install_signal_handler();
    heap::init();
    LazyLock::force(&thread_pool::THREAD_POOL);

    // Dedicated collector thread. Mutators only ever wake it (via request_gc);
    // collection runs concurrently on this thread.
    let gc_handle = gc::spawn_gc_thread();

    // Run main via sol_thread_start (registers thread, calls entry, unregisters)
    unsafe {
        thread::sol_thread_start(solar_main, null_mut(), None);
    }

    // Main has unregistered; stop the collector before touching the heap for
    // stats so no cycle races with the reads below.
    gc::shutdown_gc_thread(gc_handle);

    // Stats printing (after the main thread unregistered, outside STW).
    let enable_stat_prints = gc::ENABLE_STAT_PRINTS.load(Ordering::Relaxed);
    if enable_stat_prints {
        let total_allocations = gc::ORPHANED_TOTAL_ALLOCATIONS.load(Ordering::Relaxed);
        if gc::DISABLE_GC.load(Ordering::Relaxed) {
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
        if total_allocations > 0 {
            eprintln!(
                "avg {:?} per allocation (includes non-allocation time)",
                start.elapsed().div_f64(total_allocations as f64)
            )
        }
    }
}
