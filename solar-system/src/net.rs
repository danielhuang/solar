//! Socket intrinsics. Each function is exactly one syscall (plus `EINTR`
//! retry), mirroring `file.rs`'s conventions: sockets are `FileDesc`s in the
//! same GC-traced fd arena (`register_new_fd`), so `sol_file_read`/
//! `sol_file_write_partial` do socket I/O, `sol_file_close` neuters them, and
//! the collector auto-closes unreachable ones via `fd_sweep`. Socket addresses
//! cross the intrinsic boundary as raw `sockaddr` bytes (`&[Uint8]`) built and
//! parsed by `@std`'s `net.solar`, keeping each intrinsic a thin syscall
//! wrapper that works for any address family.

use crate::file::{fd_from_ptr, register_new_fd};

/// `socket(2)`. `SOCK_CLOEXEC` is always added so sockets don't leak across
/// `exec` (mirroring `sol_file_open`'s `O_CLOEXEC`). Throws a Solar exception
/// on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_create(
    domain: i64,
    socket_type: i64,
    protocol: i64,
) -> *mut u8 {
    let fd = unsafe {
        libc::socket(
            domain as libc::c_int,
            socket_type as libc::c_int | libc::SOCK_CLOEXEC,
            protocol as libc::c_int,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_create failed: {err}"));
    }
    unsafe { register_new_fd(fd as usize) }
}

/// `bind(2)` with raw `sockaddr` bytes. Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_bind(
    fd_ptr: *mut u8,
    addr_ptr: *const u8,
    addr_len: usize,
) {
    let fd = fd_from_ptr(fd_ptr);
    let rc = unsafe {
        libc::bind(
            fd,
            addr_ptr as *const libc::sockaddr,
            addr_len as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_bind failed: {err}"));
    }
}

/// `listen(2)`. Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_listen(fd_ptr: *mut u8, backlog: i64) {
    let fd = fd_from_ptr(fd_ptr);
    if unsafe { libc::listen(fd, backlog as libc::c_int) } != 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_listen failed: {err}"));
    }
}

/// `accept4(2)` with `SOCK_CLOEXEC`: block until a connection arrives and
/// return it as a fresh `FileDesc`. Retried on `EINTR` (e.g. the GC's
/// stop-the-world signal interrupting the wait — a mutator blocked here is
/// scanned by the signal handler like any other). Throws a Solar exception on
/// other errors.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_accept(fd_ptr: *mut u8) -> *mut u8 {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let conn = unsafe {
            libc::accept4(
                fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if conn >= 0 {
            return unsafe { register_new_fd(conn as usize) };
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("socket_accept failed: {err}"));
    }
}

/// `connect(2)` with raw `sockaddr` bytes. `EINTR` needs care here: after a
/// signal interrupts `connect`, the attempt continues asynchronously and a
/// plain retry reports `EALREADY` (in progress) until it resolves to
/// `EISCONN` (connected) or a real error — so both are treated as "keep
/// retrying"/"done" rather than failures. Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_connect(
    fd_ptr: *mut u8,
    addr_ptr: *const u8,
    addr_len: usize,
) {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let rc = unsafe {
            libc::connect(
                fd,
                addr_ptr as *const libc::sockaddr,
                addr_len as libc::socklen_t,
            )
        };
        if rc == 0 {
            return;
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) | Some(libc::EALREADY) => continue,
            Some(libc::EISCONN) => return,
            _ => crate::panic::throw_message(format_args!("socket_connect failed: {err}")),
        }
    }
}

/// `setsockopt(2)` with a `c_int` option value (covers `SO_REUSEADDR`,
/// `TCP_NODELAY`, `SO_KEEPALIVE`, …). Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_set_option(
    fd_ptr: *mut u8,
    level: i64,
    name: i64,
    value: i64,
) {
    let fd = fd_from_ptr(fd_ptr);
    let optval = value as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level as libc::c_int,
            name as libc::c_int,
            &optval as *const libc::c_int as *const libc::c_void,
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_set_option failed: {err}"));
    }
}

/// `getsockname(2)`: write the socket's local address (raw `sockaddr` bytes,
/// truncated to the buffer) into `dst` and return the address's full length.
/// Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_local_addr(
    fd_ptr: *mut u8,
    dst: *mut u8,
    dst_len: usize,
) -> usize {
    let fd = fd_from_ptr(fd_ptr);
    let mut len = dst_len as libc::socklen_t;
    let rc = unsafe { libc::getsockname(fd, dst as *mut libc::sockaddr, &mut len) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_local_addr failed: {err}"));
    }
    len as usize
}

/// `shutdown(2)`: `how` is 0/1/2 = read/write/both (`SHUT_RD`/`SHUT_WR`/
/// `SHUT_RDWR`). Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_socket_shutdown(fd_ptr: *mut u8, how: i64) {
    let fd = fd_from_ptr(fd_ptr);
    if unsafe { libc::shutdown(fd, how as libc::c_int) } != 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("socket_shutdown failed: {err}"));
    }
}
