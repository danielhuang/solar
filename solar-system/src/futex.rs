use std::ffi::c_void;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_futex_wait(ptr: *mut u32, expected: u32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr as *const c_void,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            std::ptr::null::<libc::timespec>(),
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
