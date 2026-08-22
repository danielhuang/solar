//! GC-traced file descriptors encoded as addresses in a reserved region.

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

/// Take ownership of a raw file descriptor and return its GC-traced
/// `FileDesc`. Once no reachable `FileDesc` contains this handle, sweeping may
/// close the descriptor. The caller must ensure `fd` is newly owned and is not
/// simultaneously managed elsewhere.
///
/// A negative descriptor is treated as the immediately preceding syscall's
/// failure sentinel and throws its saved OS error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_fd_from_raw(fd: libc::c_int) -> *mut u8 {
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        crate::panic::throw_message(format_args!("fd_from_raw failed: {err}"));
    }
    unsafe { register_new_fd(fd as usize) }
}

/// Borrow the raw descriptor represented by `fd_ptr` without transferring
/// ownership. The returned number remains valid only while the `FileDesc` is
/// live and has not been closed.
#[unsafe(no_mangle)]
pub extern "C" fn sol_fd_to_raw(fd_ptr: *mut u8) -> libc::c_int {
    fd_from_ptr(fd_ptr)
}

/// Register a freshly created fd (an opened file, a socket, an accepted
/// connection) in the arena bitmaps and return its `FileDesc` pointer.
///
/// Mirrors the heap's allocate path: set the allocated bit, advance the HWM,
/// and be born marked if a concurrent mark is already in flight. The
/// born-marked decision (read `SOL_CONCURRENT_MARKING`, conditionally set the
/// mark bit) must not be interrupted by the STW signal — exactly like
/// `sol_alloc`'s allocate-black — so the registration runs in a GC critical
/// section. (An un-registered fd is never swept, so a cycle landing between
/// the fd-producing syscall and this call can't close it.) The caller must be
/// a registered mutator thread.
pub(crate) unsafe fn register_new_fd(fd: usize) -> *mut u8 {
    unsafe {
        crate::gc::with_signal_deferred(|_| {
            (*alloc_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            FD_HWM.fetch_max(fd as u64 + 1, Ordering::Relaxed);
            if crate::gc::SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
                (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            }
        });
    }
    (FD_BASE.get() + fd) as *mut u8
}

/// Returns a standard stream without registering it for automatic closure.
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
pub(crate) fn fd_from_ptr(fd_ptr: *mut u8) -> libc::c_int {
    let base = FD_BASE.get();
    debug_assert!(
        base != 0 && (fd_ptr as usize).wrapping_sub(base) < FD_ARENA_SIZE,
        "FileDesc pointer is not in the fd arena"
    );
    (fd_ptr as usize).wrapping_sub(base) as libc::c_int
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

/// Replaces a file descriptor with the dead pipe without releasing its number.
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
