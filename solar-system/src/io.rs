use crate::panic::sol_panic_internal;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_write_stdout(ptr: *const u8, len: usize) {
    let mut written = 0usize;
    while written < len {
        let n = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                ptr.add(written) as *const libc::c_void,
                len - written,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            sol_panic_internal(&format!("write_stdout failed: {err}"));
        }
        if n == 0 {
            sol_panic_internal("write_stdout failed: wrote 0 bytes");
        }
        written += n as usize;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_read_stdin(ptr: *mut u8, len: usize) -> usize {
    loop {
        let n = unsafe { libc::read(libc::STDIN_FILENO, ptr as *mut libc::c_void, len) };
        if n >= 0 {
            return n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        sol_panic_internal(&format!("read_stdin failed: {err}"));
    }
}
