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

use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};

/// 4 GiB of address space: fd numbers `0..2^32` (covers every `i32` fd).
pub const FD_ARENA_SIZE: usize = 1usize << 32;
/// One bit per fd in each bitmap → `2^32 / 8` = 512 MiB reserved (NORESERVE).
const FD_BITMAP_TOTAL: usize = FD_ARENA_SIZE / 8;

/// Base of the fake arena. `FileDesc` for fd `n` is `FD_BASE + n`. Set once by
/// [`init`]; `!= 0` gates [`in_fd_arena`].
static FD_BASE: AtomicUsize = AtomicUsize::new(0);
static FD_ALLOC_BITS: AtomicUsize = AtomicUsize::new(0);
static FD_MARK_BITS: AtomicUsize = AtomicUsize::new(0);
/// Highest fd + 1 ever handed out; never decreases. Sweep only scans `[0, HWM)`.
static FD_HWM: AtomicU64 = AtomicU64::new(0);

/// A permanently-open "dead" fd: the read end of a pipe whose write end is
/// closed. Reading it yields immediate EOF, so any I/O on a fd that has been
/// [`sol_file_close`]d (via `dup2(DEAD_FD, fd)`) fails harmlessly. Set once by
/// [`init`]; `+1` so the unset state (`0`) is distinguishable from fd 0.
static DEAD_FD: AtomicI32 = AtomicI32::new(0);

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
    if FD_BASE.load(Ordering::Relaxed) != 0 {
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
        FD_ALLOC_BITS.store(alloc, Ordering::Relaxed);
        FD_MARK_BITS.store(mark, Ordering::Relaxed);

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
        DEAD_FD.store(fds[0], Ordering::Relaxed);

        // Published last; `in_fd_arena` gates on `FD_BASE != 0`.
        FD_BASE.store(arena, Ordering::Relaxed);
    }
}

#[inline]
fn bit_mask(fd: usize) -> u64 {
    1u64 << (fd & 63)
}
#[inline]
unsafe fn alloc_word(fd: usize) -> *const AtomicU64 {
    unsafe { (FD_ALLOC_BITS.load(Ordering::Relaxed) as *const AtomicU64).add(fd >> 6) }
}
#[inline]
unsafe fn mark_word(fd: usize) -> *const AtomicU64 {
    unsafe { (FD_MARK_BITS.load(Ordering::Relaxed) as *const AtomicU64).add(fd >> 6) }
}

/// Open `path` for reading and writing (creating it if absent, mode 0666) and
/// return an opaque `FileDesc` pointer (`FD_BASE + fd`). The fd's allocated bit
/// is set so the next GC traces it. `O_TRUNC` is deliberately omitted: a
/// `FileDesc` opened purely to read an existing file must not destroy it.
///
/// Panics on failure so the returned pointer is always a valid, live fd — the
/// opaque-handle contract needs no sentinel value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_open(path_ptr: *const u8, path_len: usize) -> *mut u8 {
    let slot_ptr = crate::gc::MY_SLOT.get();
    assert!(
        !slot_ptr.is_null(),
        "sol_file_open called on unregistered thread"
    );
    let slot = unsafe { &*slot_ptr };

    // The whole body runs in a GC critical section. Two things require it:
    //  - The born-marked decision (read `SOL_CONCURRENT_MARKING`, conditionally
    //    set the mark bit) must not be interrupted by the STW signal, exactly
    //    like `sol_alloc`'s allocate-black.
    //  - The path buffer uses the system allocator. If the STW signal parked
    //    this thread mid-`malloc`/`free` (holding the allocator lock), the GC
    //    thread's own STW allocations would deadlock. Inside the section the
    //    signal only *defers*, so the buffer's whole lifetime — alloc, use by
    //    `open`, drop — stays unparkable; we self-suspend cleanly at the end.
    unsafe {
        crate::gc::with_signal_deferred(slot, || {
            // NUL-terminated copy of the (unterminated) Solar byte-slice path.
            let mut buf: Vec<u8> = Vec::with_capacity(path_len + 1);
            std::ptr::copy_nonoverlapping(path_ptr, buf.as_mut_ptr(), path_len);
            buf.set_len(path_len);
            buf.push(0);

            let fd = libc::open(
                buf.as_ptr() as *const libc::c_char,
                libc::O_RDWR | libc::O_CREAT,
                0o666 as libc::c_int,
            );
            assert!(
                fd >= 0,
                "file_open: could not open file (errno {})",
                std::io::Error::last_os_error()
            );
            let fd = fd as usize;

            // Mirror the heap's allocate path: set the allocated bit, advance the
            // HWM, and be born marked if a concurrent mark is already in flight.
            (*alloc_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            FD_HWM.fetch_max(fd as u64 + 1, Ordering::Relaxed);
            if crate::gc::SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
                (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed);
            }

            (FD_BASE.load(Ordering::Relaxed) + fd) as *mut u8
        })
    }
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
    let base = FD_BASE.load(Ordering::Relaxed);
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

/// Recover the raw fd number from a `FileDesc` pointer (`addr - FD_BASE`).
#[inline]
fn fd_from_ptr(fd_ptr: *mut u8) -> libc::c_int {
    let base = FD_BASE.load(Ordering::Relaxed);
    debug_assert!(
        base != 0 && (fd_ptr as usize).wrapping_sub(base) < FD_ARENA_SIZE,
        "FileDesc pointer is not in the fd arena"
    );
    (fd_ptr as usize).wrapping_sub(base) as libc::c_int
}

/// Read up to `dst_len` bytes from `fd` into `dst`, returning the count read (0
/// at EOF). Panics on a non-`EINTR` I/O error. Calls `read(2)` directly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_read(fd_ptr: *mut u8, dst: *mut u8, dst_len: usize) -> usize {
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
        crate::panic::sol_panic_internal(&format!("file_read failed: {err}"));
    }
}

/// Write up to `src_len` bytes from `src` to `fd`, returning the count actually
/// written (a single, possibly partial, `write(2)`). Panics on a non-`EINTR`
/// I/O error. Calls `write(2)` directly; the looping write-all lives in `@std`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_write_partial(
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
        crate::panic::sol_panic_internal(&format!("file_write_partial failed: {err}"));
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
    let base = FD_BASE.load(Ordering::Relaxed);
    debug_assert!(
        base != 0 && (fd_ptr as usize).wrapping_sub(base) < FD_ARENA_SIZE,
        "sol_file_close: pointer is not a FileDesc"
    );
    let fd = (fd_ptr as usize).wrapping_sub(base) as libc::c_int;
    let dead = DEAD_FD.load(Ordering::Relaxed);
    // dup2 onto the same fd is a no-op, so a dead fd value of 0 (pre-init) would
    // be wrong only if init never ran — which can't happen for compiled code.
    unsafe { libc::dup2(dead, fd) };
}

/// Does `v` point into the fd arena (i.e. is it a `FileDesc`)?
#[inline]
pub fn in_fd_arena(v: usize) -> bool {
    let base = FD_BASE.load(Ordering::Relaxed);
    base != 0 && v.wrapping_sub(base) < FD_ARENA_SIZE
}

/// Mark the fd referenced by `v`. No children to enqueue. Caller must ensure
/// `in_fd_arena(v)`.
#[inline]
pub unsafe fn fd_mark(v: usize) {
    let fd = v.wrapping_sub(FD_BASE.load(Ordering::Relaxed));
    unsafe { (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed) };
}

/// Is the fd referenced by `v` already marked this cycle? Used for white-only
/// shading in the write barriers. Caller must ensure `in_fd_arena(v)`.
#[inline]
pub unsafe fn is_marked(v: usize) -> bool {
    let fd = v.wrapping_sub(FD_BASE.load(Ordering::Relaxed));
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
    let abase = FD_ALLOC_BITS.load(Ordering::Relaxed) as *const AtomicU64;
    let mbase = FD_MARK_BITS.load(Ordering::Relaxed) as *const AtomicU64;
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
