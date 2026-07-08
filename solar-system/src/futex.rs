use std::ffi::c_void;

/// Block until `*ptr != expected`, a wake arrives, or `timeout_ns` nanoseconds
/// elapse (`u64::MAX` = wait forever). Spurious early returns are allowed —
/// any signal (e.g. the GC's stop-the-world signal) interrupts the wait and it
/// is not restarted — so callers must recheck their condition and re-wait.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_futex_wait(ptr: *mut u32, expected: u32, timeout_ns: u64) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let timeout: *const libc::timespec = if timeout_ns == u64::MAX {
        std::ptr::null()
    } else {
        ts.tv_sec = (timeout_ns / 1_000_000_000) as libc::time_t;
        ts.tv_nsec = (timeout_ns % 1_000_000_000) as _;
        &ts
    };
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr as *const c_void,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            timeout,
        );
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_futex_wake(ptr: *mut u32, count: u32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr as *const c_void,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            count as i32,
        );
    }
}
