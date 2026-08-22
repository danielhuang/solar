//! Native runtime for compiled Solar programs.

#![allow(clippy::missing_safety_doc)]

use std::env;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// Checked arithmetic intrinsics.
pub mod arith;
/// File and directory intrinsics.
pub mod file;
/// Futex intrinsics.
pub mod futex;
/// Garbage collector.
pub mod gc;
/// Size-class heap.
pub mod heap;
/// Startup-initialized global cells.
pub mod init_cell;
/// Allocation and memory intrinsics.
pub mod mem;
/// Panic and exception support.
pub mod panic;
/// Process arguments and environment.
pub mod process;
/// Thread lifecycle support.
pub mod thread;
/// Runtime worker pool.
pub mod thread_pool;
/// Clock intrinsics.
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

/// Enables bump-allocation mode before runtime startup.
#[unsafe(no_mangle)]
pub extern "C" fn sol_disable_gc() {
    // SAFETY: called before `sol_start`, single-threaded.
    unsafe { gc::DISABLE_GC.set(true) };
}

/// Enables swept-arena access checking before runtime startup.
#[unsafe(no_mangle)]
pub extern "C" fn sol_enable_gc_san() {
    // SAFETY: called before `sol_start`, single-threaded.
    unsafe { gc::GC_SAN.set(true) };
}

/// Asserts that every arena slot touched by `[addr, addr + size)` is allocated.
#[unsafe(no_mangle)]
pub extern "C" fn sol_gc_san_check(addr: *const u8, size: usize) {
    if size == 0 {
        return;
    }
    heap::assert_allocated_range(addr as usize, size);
}

/// A mutable global or thread-local slot registered as a GC root.
#[repr(C)]
pub struct StaticEntry {
    /// Address of the global slot.
    pub addr: *mut u8,
    /// Slot size in bytes.
    pub size: u64,
    /// Function that traces pointers stored in the slot.
    pub mark_fn: mem::MarkFn,
}
// SAFETY: entries live in generated static or thread-local data and point at
// slots valid for the owning registration's lifetime. The GC thread only reads
// the slots during stop-the-world pauses (no mutator is running).
unsafe impl Send for StaticEntry {}
unsafe impl Sync for StaticEntry {}

/// Initializes the runtime, runs the Solar entry point, and shuts down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_start(
    solar_main: unsafe extern "C" fn(*mut c_void),
    statics: *const StaticEntry,
    statics_len: usize,
    register_tls: Option<unsafe extern "C" fn()>,
) {
    let start = Instant::now();
    panic::install_panic_hook();

    unsafe {
        gc::ENABLE_STAT_PRINTS.set(read_env_bool("SOLAR_PRINT_GC_STATS"));
        gc::ENABLE_ALLOC_PRINTS.set(read_env_bool("SOLAR_PRINT_ALLOCS"));
        gc::DISABLE_GC.set(gc::DISABLE_GC.get() | read_env_bool("SOLAR_DISABLE_GC"));
    }

    gc::install_signal_handler();
    heap::init();
    file::init();
    process::init_num_cpus();
    LazyLock::force(&thread_pool::THREAD_POOL);

    let statics: &'static [StaticEntry] = if statics.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(statics, statics_len) }
    };

    let gc_handle = gc::spawn_gc_thread(statics);

    unsafe {
        let gc_guard = thread::GC_LOCK.read();
        thread::sol_thread_start(solar_main, null_mut(), register_tls, None, Some(gc_guard));
    }

    gc::shutdown_gc_thread(gc_handle);

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
