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

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

/// Open `path` (read-only) and return an opaque `FileDesc` pointer
/// (`FD_BASE + fd`). The fd's allocated bit is set so the next GC traces it.
///
/// Panics on failure so the returned pointer is always a valid, live fd — the
/// opaque-handle contract needs no sentinel value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_file_open(path_ptr: *const u8, path_len: usize) -> *mut u8 {
    // Build a NUL-terminated copy of the (unterminated) Solar byte-slice path.
    let mut buf: Vec<u8> = Vec::with_capacity(path_len + 1);
    unsafe {
        std::ptr::copy_nonoverlapping(path_ptr, buf.as_mut_ptr(), path_len);
        buf.set_len(path_len);
    }
    buf.push(0);

    let fd = unsafe { libc::open(buf.as_ptr() as *const libc::c_char, libc::O_RDONLY) };
    assert!(
        fd >= 0,
        "file_open: could not open file (errno {})",
        std::io::Error::last_os_error()
    );
    let fd = fd as usize;

    // Mirror the heap's allocate path: set the allocated bit, advance the HWM,
    // and be born marked if a concurrent mark is already in flight (so this
    // cycle cannot sweep an fd that became live mid-mark).
    unsafe { (*alloc_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed) };
    FD_HWM.fetch_max(fd as u64 + 1, Ordering::Relaxed);
    if crate::gc::SOL_CONCURRENT_MARKING.load(Ordering::Relaxed) {
        unsafe { (*mark_word(fd)).fetch_or(bit_mask(fd), Ordering::Relaxed) };
    }

    (FD_BASE.load(Ordering::Relaxed) + fd) as *mut u8
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
