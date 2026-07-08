//! File-descriptor "arena": an address-space-only region whose pointers encode
//! open file descriptors as `addr - FD_BASE`.
//!
//! A Solar `FileDesc` value has the byte representation of `&Int32` and points
//! at `FD_BASE + fd`. The region itself is mapped `PROT_NONE` and is **never
//! read or written** — it exists purely to carve out a unique, GC-recognizable
//! address range so the collector can trace `FileDesc` references like any
//! other pointer. There is no per-fd storage in the region; the fd number is
//! recovered arithmetically.
//!
//! Allocated/marked state lives in two side bitmaps indexed by fd number
//! (`MAP_NORESERVE`, demand-paged, so the real cost is proportional to the
//! highest fd ever opened, not to the 4 GiB of reserved address space). After a
//! GC marks every reachable `FileDesc`, [`fd_sweep`] `close()`s each fd whose
//! slot went unmarked — GC-driven resource cleanup. This mirrors the heap's
//! alloc/mark-bitmap sweep (`heap::sweep_word_range`).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::init_cell::InitCell;

/// 4 GiB of address space: fd numbers `0..2^32` (covers every `i32` fd).
pub const FD_ARENA_SIZE: usize = 1usize << 32;
/// One bit per fd in each bitmap → `2^32 / 8` = 512 MiB reserved (NORESERVE).
const FD_BITMAP_TOTAL: usize = FD_ARENA_SIZE / 8;

/// Base of the fake arena. `FileDesc` for fd `n` is `FD_BASE + n`. Set once by
/// [`init`]; `!= 0` gates [`in_fd_arena`].
static FD_BASE: InitCell<usize> = InitCell::new(0);
static FD_ALLOC_BITS: InitCell<usize> = InitCell::new(0);
static FD_MARK_BITS: InitCell<usize> = InitCell::new(0);
/// Highest fd + 1 ever handed out; never decreases. Sweep only scans `[0, HWM)`.
static FD_HWM: AtomicU64 = AtomicU64::new(0);

/// A permanently-open "dead" fd: the read end of a pipe whose write end is
/// closed. Reading it yields immediate EOF, so any I/O on a fd that has been
/// [`sol_file_close`]d (via `dup2(DEAD_FD, fd)`) fails harmlessly. Set once by
/// [`init`]; `+1` so the unset state (`0`) is distinguishable from fd 0.
static DEAD_FD: InitCell<i32> = InitCell::new(0);

unsafe fn mmap(size: usize, prot: i32, what: &str) -> usize {
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    assert!(
        p != libc::MAP_FAILED,
        "solar fd arena: mmap of {size} bytes for {what} failed (errno {})",
        std::io::Error::last_os_error()
    );
    p as usize
}

/// Reserve the fake arena and its two side bitmaps. Idempotent; call once from
/// `sol_start` before any Solar code runs.
pub fn init() {
    if FD_BASE.get() != 0 {
        return;
    }
    unsafe {
        let alloc = mmap(
            FD_BITMAP_TOTAL,
            libc::PROT_READ | libc::PROT_WRITE,
            "fd alloc bitmap",
        );
        let mark = mmap(
            FD_BITMAP_TOTAL,
            libc::PROT_READ | libc::PROT_WRITE,
            "fd mark bitmap",
        );
        // The arena is never accessed: PROT_NONE both saves pages and traps any
        // accidental dereference of an opaque `FileDesc`.
        let arena = mmap(FD_ARENA_SIZE, libc::PROT_NONE, "fd arena");
        // SAFETY (here and below): `init` runs once from `sol_start`, before
        // any thread that reads these cells is spawned.
        FD_ALLOC_BITS.set(alloc);
        FD_MARK_BITS.set(mark);

        // The "dead" fd: read end of a pipe with the write end closed. Reads on
        // it return EOF; `sol_file_close` dup2's it over a fd to neuter the fd's
        // file without freeing the fd number (which a live `FileDesc` may still
        // hold). It is never closed for the life of the process.
        let mut fds = [0i32; 2];
        assert!(
            libc::pipe(fds.as_mut_ptr()) == 0,
            "solar fd arena: pipe() for dead fd failed (errno {})",
            std::io::Error::last_os_error()
        );
        libc::close(fds[1]); // drop the write end → reads on fds[0] hit EOF
        DEAD_FD.set(fds[0]);

        FD_BASE.set(arena);
    }
}

#[inline]
fn bit_mask(fd: usize) -> u64 {
    1u64 << (fd & 63)
}
#[inline]
unsafe fn alloc_word(fd: usize) -> *const AtomicU64 {
    unsafe { (FD_ALLOC_BITS.get() as *const AtomicU64).add(fd >> 6) }
}
#[inline]
unsafe fn mark_word(fd: usize) -> *const AtomicU64 {
    unsafe { (FD_MARK_BITS.get() as *const AtomicU64).add(fd >> 6) }
}

/// Open `path` with the given `open(2)` `flags` and creation `mode`, and return
/// an opaque `FileDesc` pointer (`FD_BASE + fd`). The fd's allocated bit is set
/// so the next GC traces it. `O_CLOEXEC` is always added so descriptors don't
/// leak across `exec`.
///
/// Throws a Solar exception on failure so the returned pointer is always a
/// valid, live fd — the opaque-handle contract needs no sentinel value.
/// `extern "C-unwind"` so the throw may unwind through the generated C frames
/// to the nearest `sol_try`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_open(
    path_ptr: *const u8,
    path_len: usize,
    flags: i64,
    mode: u64,
) -> *mut u8 {
    let slot_ptr = crate::gc::MY_SLOT.get();
    assert!(
        !slot_ptr.is_null(),
        "sol_file_open called on unregistered thread"
    );
    let slot = unsafe { &*slot_ptr };

    // NUL-terminated GC copy of the (unterminated) Solar byte-slice path — the
    // GC allocator, not the system malloc, so no critical section is needed
    // around its lifetime (a malloc'd buffer once forced the whole body into
    // one: the STW signal parking this thread mid-`malloc` would deadlock the
    // GC thread's own allocations).
    let path = copy_path(path_ptr, path_len);

    // O_CLOEXEC is always set so fds don't leak across exec.
    let fd = unsafe {
        libc::open(
            path,
            (flags as libc::c_int) | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("file_open failed: {err}"));
    }
    let fd = fd as usize;

    // Mirror the heap's allocate path: set the allocated bit, advance the HWM,
    // and be born marked if a concurrent mark is already in flight. The
    // born-marked decision (read `SOL_CONCURRENT_MARKING`, conditionally set
    // the mark bit) must not be interrupted by the STW signal — exactly like
    // `sol_alloc`'s allocate-black — so the registration runs in a GC critical
    // section. (An un-registered fd is never swept, so a cycle landing between
    // `open` and this section can't close it.)
    unsafe {
        crate::gc::with_signal_deferred(slot, || {
            (*alloc_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            FD_HWM.fetch_max(fd as u64 + 1, Ordering::Relaxed);
            if crate::gc::SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
                (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            }
        });
    }
    (FD_BASE.get() + fd) as *mut u8
}

/// Return a `FileDesc` for one of the process's standard streams (`fd`).
///
/// Standard streams are owned by the process for its whole lifetime, so the
/// returned handle is deliberately **not** registered in the alloc bitmap: the
/// collector traces it harmlessly (the arena address is recognized and its mark
/// bit may be set), but [`fd_sweep`] only closes fds whose *alloc* bit is set,
/// so stdin/stdout are never auto-closed regardless of reachability.
#[inline]
unsafe fn std_stream(fd: libc::c_int) -> *mut u8 {
    let base = FD_BASE.get();
    debug_assert!(base != 0, "std_stream called before fd arena init");
    (base + fd as usize) as *mut u8
}

/// `FileDesc` for the process's standard input (fd 0). Never auto-closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_stdin() -> *mut u8 {
    unsafe { std_stream(libc::STDIN_FILENO) }
}

/// `FileDesc` for the process's standard output (fd 1). Never auto-closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_stdout() -> *mut u8 {
    unsafe { std_stream(libc::STDOUT_FILENO) }
}

/// `FileDesc` for the process's standard error (fd 2). Never auto-closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_stderr() -> *mut u8 {
    unsafe { std_stream(libc::STDERR_FILENO) }
}

/// Recover the raw fd number from a `FileDesc` pointer (`addr - FD_BASE`).
#[inline]
fn fd_from_ptr(fd_ptr: *mut u8) -> libc::c_int {
    let base = FD_BASE.get();
    debug_assert!(
        base != 0 && (fd_ptr as usize).wrapping_sub(base) < FD_ARENA_SIZE,
        "FileDesc pointer is not in the fd arena"
    );
    (fd_ptr as usize).wrapping_sub(base) as libc::c_int
}

/// Read up to `dst_len` bytes from `fd` into `dst`, returning the count read (0
/// at EOF). Throws a Solar exception on a non-`EINTR` I/O error. Calls
/// `read(2)` directly.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_read(
    fd_ptr: *mut u8,
    dst: *mut u8,
    dst_len: usize,
) -> usize {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let n = unsafe { libc::read(fd, dst as *mut libc::c_void, dst_len) };
        if n >= 0 {
            return n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("file_read failed: {err}"));
    }
}

/// Write up to `src_len` bytes from `src` to `fd`, returning the count actually
/// written (a single, possibly partial, `write(2)`). Throws a Solar exception
/// on a non-`EINTR` I/O error. Calls `write(2)` directly; the looping write-all
/// lives in `@std`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_write_partial(
    fd_ptr: *mut u8,
    src: *const u8,
    src_len: usize,
) -> usize {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let n = unsafe { libc::write(fd, src as *const libc::c_void, src_len) };
        if n >= 0 {
            return n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("file_write_partial failed: {err}"));
    }
}

/// Read up to `dst_len` bytes from `fd` at absolute `offset` into `dst`,
/// returning the count read (0 at EOF). Does not move the file cursor. Calls
/// `pread(2)` directly; throws a Solar exception on a non-`EINTR` error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_read_at(
    fd_ptr: *mut u8,
    dst: *mut u8,
    dst_len: usize,
    offset: u64,
) -> usize {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let n =
            unsafe { libc::pread(fd, dst as *mut libc::c_void, dst_len, offset as libc::off_t) };
        if n >= 0 {
            return n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("file_read_at failed: {err}"));
    }
}

/// Write up to `src_len` bytes from `src` to `fd` at absolute `offset`,
/// returning the count actually written (a single, possibly partial,
/// `pwrite(2)`). Does not move the file cursor. Throws a Solar exception on a
/// non-`EINTR` error; the looping write-all lives in `@std`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_write_at(
    fd_ptr: *mut u8,
    src: *const u8,
    src_len: usize,
    offset: u64,
) -> usize {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        let n = unsafe {
            libc::pwrite(
                fd,
                src as *const libc::c_void,
                src_len,
                offset as libc::off_t,
            )
        };
        if n >= 0 {
            return n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("file_write_at failed: {err}"));
    }
}

/// Flush `fd`'s data and metadata to stable storage. Calls `fsync(2)` directly;
/// throws a Solar exception on a non-`EINTR` error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_sync(fd_ptr: *mut u8) {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        if unsafe { libc::fsync(fd) } == 0 {
            return;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("file_sync failed: {err}"));
    }
}

/// Apply a `flock(2)` operation to `fd`. `op` is the raw `LOCK_*` word built by
/// `@std` (`LOCK_SH`=1, `LOCK_EX`=2, `LOCK_NB`=4, `LOCK_UN`=8). Returns 1 on
/// success and 0 when a non-blocking request would have to wait (EWOULDBLOCK);
/// throws a Solar exception on any other non-`EINTR` error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_lock(fd_ptr: *mut u8, op: i64) -> u8 {
    let fd = fd_from_ptr(fd_ptr);
    loop {
        if unsafe { libc::flock(fd, op as libc::c_int) } == 0 {
            return 1;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() == std::io::ErrorKind::WouldBlock {
            return 0;
        }
        crate::panic::throw_message(format_args!("file_lock failed: {err}"));
    }
}

/// Copy an unterminated Solar byte-slice path into a NUL-terminated GC
/// allocation (`len + 1` bytes) and return its pointer. Uses `sol_alloc` — the
/// ordinary GC allocation path, not the system allocator — so callers need no
/// GC critical section around the buffer's lifetime; the caller must be a
/// registered mutator thread. The buffer holds no pointers (`mark_noop`) and
/// stays alive across the syscall via the caller's stack reference
/// (conservative scan); it becomes garbage as soon as the intrinsic returns.
fn copy_path(ptr: *const u8, len: usize) -> *const libc::c_char {
    let buf = unsafe {
        crate::mem::sol_alloc(len + 1, 1, crate::process::mark_noop as crate::mem::MarkFn)
    };
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, buf, len);
        *buf.add(len) = 0;
    }
    buf as *const libc::c_char
}

/// Run a path-taking syscall thunk with `EINTR` retry, throwing the canonical
/// `{what} failed: {err}` Solar exception when it reports an error.
fn path_call(what: &str, f: impl Fn() -> libc::c_int) {
    loop {
        if f() == 0 {
            return;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("{what} failed: {err}"));
    }
}

/// Delete the file at `path`. Calls `unlink(2)` directly; throws a Solar
/// exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_remove(path_ptr: *const u8, path_len: usize) {
    let path = copy_path(path_ptr, path_len);
    path_call("file_remove", || unsafe { libc::unlink(path) });
}

/// Delete the **empty** directory at `path`. Calls `rmdir(2)` directly; throws
/// a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_dir_remove(path_ptr: *const u8, path_len: usize) {
    let path = copy_path(path_ptr, path_len);
    path_call("dir_remove", || unsafe { libc::rmdir(path) });
}

/// Atomically rename `old` to `new` (replacing `new` if it exists, like
/// `rename(2)`). Throws a Solar exception on error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_rename(
    old_ptr: *const u8,
    old_len: usize,
    new_ptr: *const u8,
    new_len: usize,
) {
    let old = copy_path(old_ptr, old_len);
    let new = copy_path(new_ptr, new_len);
    path_call("file_rename", || unsafe { libc::rename(old, new) });
}

/// Create the directory at `path` with permission bits `mode`. Calls `mkdir(2)`
/// directly; throws a Solar exception on error (including `EEXIST`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_dir_create(path_ptr: *const u8, path_len: usize, mode: u64) {
    let path = copy_path(path_ptr, path_len);
    path_call("dir_create", || unsafe {
        libc::mkdir(path, mode as libc::mode_t)
    });
}

/// `stat(2)` the file at `path`, writing its size in bytes, its mtime as
/// nanoseconds since the Unix epoch, and its kind (0 = regular file, 1 =
/// directory, 2 = other) through the three out-pointers. Returns 1 on success;
/// returns 0 with the out-params zeroed when the path does not exist (`ENOENT`,
/// or `ENOTDIR` for a non-directory path component); throws a Solar exception
/// on any other non-`EINTR` error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_file_stat(
    path_ptr: *const u8,
    path_len: usize,
    size_out: *mut u64,
    mtime_out: *mut u64,
    kind_out: *mut u64,
) -> u8 {
    let path = copy_path(path_ptr, path_len);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    loop {
        if unsafe { libc::stat(path, &mut st) } == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if matches!(err.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) {
            unsafe {
                *size_out = 0;
                *mtime_out = 0;
                *kind_out = 0;
            }
            return 0;
        }
        crate::panic::throw_message(format_args!("file_stat failed: {err}"));
    }
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFREG => 0,
        libc::S_IFDIR => 1,
        _ => 2,
    };
    unsafe {
        *size_out = st.st_size as u64;
        *mtime_out = (st.st_mtime as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(st.st_mtime_nsec as u64);
        *kind_out = kind;
    }
    1
}

/// Read one `getdents64(2)` batch of entries from a directory opened with
/// `O_DIRECTORY`, building a `&[&[Uint8]]` into `out` (same GC-construction
/// invariants as `sol_args` — see `process.rs`). Each entry is one byte-slice:
/// a kind byte (0 = regular file, 1 = directory, 2 = other/unknown) followed by
/// the name bytes. Entries come in directory order, **including** `"."` and
/// `".."` (`@std` filters them), so an empty result always means the directory
/// is exhausted. Callers loop until then; one intrinsic call is one syscall.
/// Throws a Solar exception on a non-`EINTR` error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_dir_read(fd_ptr: *mut u8, out: *mut u8) {
    let fd = fd_from_ptr(fd_ptr);
    // Stack buffer for the raw dirent batch: no system-allocator use, so no GC
    // critical section is needed (the `sol_alloc` calls below are the ordinary
    // GC allocation path, called here from a registered mutator thread).
    let mut buf = [0u8; 32 * 1024];
    let nread = loop {
        let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len()) };
        if n >= 0 {
            break n as usize;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        crate::panic::throw_message(format_args!("dir_read failed: {err}"));
    };

    // linux_dirent64 layout: d_ino u64 @0, d_off i64 @8, d_reclen u16 @16,
    // d_type u8 @18, d_name (NUL-terminated) @19.
    let reclen_at =
        |pos: usize| -> usize { u16::from_le_bytes([buf[pos + 16], buf[pos + 17]]) as usize };

    let mut count = 0usize;
    let mut pos = 0usize;
    while pos < nread {
        count += 1;
        pos += reclen_at(pos);
    }

    // Outer array of `count` fat pointers — zeroed before the per-entry allocs
    // below can trigger a GC (invariant (1) in process.rs).
    let outer = unsafe {
        crate::mem::sol_alloc(
            count * 16,
            8,
            crate::process::mark_ptr_array as crate::mem::MarkFn,
        )
    };
    unsafe { std::ptr::write_bytes(outer, 0, count * 16) };

    let mut pos = 0usize;
    for i in 0..count {
        let d_type = buf[pos + 18];
        let kind: u8 = match d_type {
            libc::DT_REG => 0,
            libc::DT_DIR => 1,
            _ => 2,
        };
        let name_ptr = unsafe { buf.as_ptr().add(pos + 19) };
        let name_len = unsafe { libc::strlen(name_ptr as *const libc::c_char) };
        let len = 1 + name_len;
        let entry = unsafe {
            crate::mem::sol_alloc(len, 1, crate::process::mark_noop as crate::mem::MarkFn)
        };
        unsafe {
            *entry = kind;
            std::ptr::copy_nonoverlapping(name_ptr, entry.add(1), name_len);
            let slot = outer.add(i * 16);
            *(slot as *mut *mut u8) = entry;
            *(slot.add(8) as *mut u64) = len as u64;
        }
        pos += reclen_at(pos);
    }

    unsafe {
        *(out as *mut *mut u8) = outer;
        *(out.add(8) as *mut u64) = count as u64;
    }
}

/// "Close" the file behind a `FileDesc` without freeing its fd number.
///
/// A plain `close(fd)` would return the fd number to the kernel, which could
/// then hand it back out from a later `open` — and any escaped `FileDesc` still
/// holding that number would silently alias the new file. Instead we `dup2` the
/// process-wide [`DEAD_FD`] over it: the underlying file is closed atomically by
/// `dup2`, but the fd number stays occupied (now referring to the dead pipe), so
/// stale `FileDesc`s see only harmless EOF. The fd keeps its allocated bit, so
/// the collector still traces it and eventually `close`s the dead-pipe dup when
/// no live `FileDesc` remains.
///
/// Needs no GC critical section: it neither allocates nor touches the alloc/mark
/// bitmaps, and `fd_sweep` (the only other toucher of this fd) runs under STW.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_close(fd_ptr: *mut u8) {
    let base = FD_BASE.get();
    debug_assert!(
        base != 0 && (fd_ptr as usize).wrapping_sub(base) < FD_ARENA_SIZE,
        "sol_file_close: pointer is not a FileDesc"
    );
    let fd = (fd_ptr as usize).wrapping_sub(base) as libc::c_int;
    let dead = DEAD_FD.get();
    // dup2 onto the same fd is a no-op, so a dead fd value of 0 (pre-init) would
    // be wrong only if init never ran — which can't happen for compiled code.
    unsafe { libc::dup2(dead, fd) };
}

/// Does `v` point into the fd arena (i.e. is it a `FileDesc`)?
#[inline]
pub fn in_fd_arena(v: usize) -> bool {
    let base = FD_BASE.get();
    base != 0 && v.wrapping_sub(base) < FD_ARENA_SIZE
}

/// Mark the fd referenced by `v`. No children to enqueue. Caller must ensure
/// `in_fd_arena(v)`.
#[inline]
pub unsafe fn fd_mark(v: usize) {
    let fd = v.wrapping_sub(FD_BASE.get());
    unsafe { (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed) };
}

/// Is the fd referenced by `v` already marked this cycle? Used for white-only
/// shading in the write barriers. Caller must ensure `in_fd_arena(v)`.
#[inline]
pub unsafe fn is_marked(v: usize) -> bool {
    let fd = v.wrapping_sub(FD_BASE.get());
    unsafe { (*mark_word(fd)).load(Ordering::Relaxed) & bit_mask(fd) != 0 }
}

/// Close every fd that is allocated but went unmarked this cycle, clear its
/// alloc bit, and reset the mark bitmap for the next cycle. Returns the number
/// of fds closed. Runs single-threaded under STW pause 2 (like the heap sweep).
pub unsafe fn fd_sweep() -> usize {
    let hwm = FD_HWM.load(Ordering::Relaxed) as usize;
    if hwm == 0 {
        return 0;
    }
    let words = (hwm + 63) >> 6;
    let abase = FD_ALLOC_BITS.get() as *const AtomicU64;
    let mbase = FD_MARK_BITS.get() as *const AtomicU64;
    let mut closed = 0usize;
    for wi in 0..words {
        let aw = unsafe { &*abase.add(wi) };
        let mw = unsafe { &*mbase.add(wi) };
        let a = aw.load(Ordering::Relaxed);
        let m = mw.load(Ordering::Relaxed);
        let mut dead = a & !m; // allocated but unmarked → close
        while dead != 0 {
            let fd = (wi << 6) + dead.trailing_zeros() as usize;
            dead &= dead - 1;
            unsafe { libc::close(fd as libc::c_int) };
            closed += 1;
        }
        aw.store(a & m, Ordering::Relaxed); // survivors keep their alloc bit
        mw.store(0, Ordering::Relaxed); // clear marks for the next cycle
    }
    closed
}
