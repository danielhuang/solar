//! `args()` / `env()` intrinsics: expose the process command line and
//! environment to Solar code as `&[&[Uint8]]` (a slice of byte-slices), each
//! byte-slice freshly **copied** into GC memory so it outlives the OS-owned
//! `argv`/`environ` storage and is owned by the collector.
//!
//! The bytes are copied **directly** from the OS-owned `argv` / `environ`
//! arrays into GC allocations — there are no intermediate Rust heap
//! allocations. `argv` is captured by an `.init_array` constructor (on Linux
//! these run with the same `(argc, argv, envp)` as `main`, before any Solar
//! code); `environ` is read live at the call. Both arrays, and the
//! NUL-terminated strings they point at, are stable, non-GC memory for the
//! process lifetime, so copying out of them across `sol_alloc` calls is sound.
//!
//! Building the nested structure means calling `sol_alloc` from Rust, which
//! requires upholding by hand the invariants codegen normally provides:
//!
//! 1. **Zeroing.** Arena memory from `sol_alloc` is uninitialized. The outer
//!    fat-pointer array is reachable (via this stack frame's conservative scan)
//!    and born black the moment it exists, so a GC triggered by a later
//!    per-item `sol_alloc` would run its mark function over not-yet-filled
//!    slots. We `write_bytes(.., 0, ..)` it before filling so those slots hold
//!    null data pointers (which the marker ignores). The byte buffers need no
//!    zeroing: the copy overwrites every exposed byte and their mark fn is a
//!    no-op, so the rounded-up tail is never traced.
//! 2. **Mark functions.** Byte buffers contain no pointers (`mark_noop`); the
//!    outer array is a vector of `&[Uint8]` fat pointers, marked word-by-word
//!    (`mark_ptr_array`, matching codegen's `_mark_ptr_array` — the length word
//!    is filtered out by the collector's plausibility check).
//! 3. **Write barriers.** None needed. Every byte buffer is allocated
//!    immediately before being stored into the outer array, so it is either
//!    born black (marking already active) or reachable from `outer`/the `buf`
//!    local as a root when marking begins — never a white pointer stored into a
//!    black object, which is the only case the insertion barrier guards.
//! 4. **Thread.** Both are called from compiled Solar code on a registered
//!    mutator thread, so `sol_alloc`'s registration assert holds.

use std::ffi::{c_char, c_int};

use crate::gc::sol_gc_mark;
use crate::init_cell::InitCell;
use crate::mem::{MarkFn, sol_alloc};

unsafe extern "C" {
    /// The process environment: a NUL-terminated array of `KEY=VALUE` C strings,
    /// owned by libc for the process lifetime.
    static environ: *const *const c_char;
}

// `argc`/`argv` captured before `main`. There is no portable global for the
// command line (unlike `environ`), so an `.init_array` constructor records it.
// If for some reason the constructor never runs, `ARGC` stays 0 and `args()`
// returns empty rather than reading a null pointer.
static ARGC: InitCell<usize> = InitCell::new(0);
static ARGV: InitCell<usize> = InitCell::new(0);

unsafe extern "C" fn capture_args(
    argc: c_int,
    argv: *const *const c_char,
    _envp: *const *const c_char,
) {
    // SAFETY: `.init_array` constructors run before `main`, single-threaded.
    unsafe {
        ARGC.set(argc.max(0) as usize);
        ARGV.set(argv as usize);
    }
}

#[used]
#[unsafe(link_section = ".init_array")]
static CAPTURE_ARGS: unsafe extern "C" fn(c_int, *const *const c_char, *const *const c_char) =
    capture_args;

/// Mark function for the byte buffers: they hold raw bytes, no pointers.
/// Also used by `panic::throw_message` for runtime-thrown message buffers.
pub(crate) unsafe extern "C" fn mark_noop(_ctx: *mut u8, _obj: *mut u8, _size: u64) {}

/// Mark function for the outer array of `&[Uint8]` fat pointers. Treats every
/// 8-byte word as a candidate pointer — the data pointer at +0 of each 16-byte
/// element is a real heap edge; the length at +8 is filtered out by the
/// collector's plausibility check. Mirrors codegen's `_mark_ptr_array`.
unsafe extern "C" fn mark_ptr_array(ctx: *mut u8, obj: *mut u8, size: u64) {
    let mut off = 0u64;
    while off < size {
        let p = unsafe { *(obj.add(off as usize) as *const *mut u8) };
        unsafe { sol_gc_mark(ctx, p) };
        off += 8;
    }
}

/// Build a `&[&[Uint8]]` of `n` entries into `out`, copying `len` bytes from
/// `entry(i) = (ptr, len)` into a fresh GC allocation for each. `ptr` must point
/// at stable, non-GC memory for the duration of the call.
unsafe fn build(out: *mut u8, n: usize, entry: impl Fn(usize) -> (*const u8, usize)) {
    // Outer array: `n` `&[Uint8]` fat pointers, 16 bytes / 8-byte aligned each.
    // `n == 0` is possible (an empty environment); `sol_alloc(0, ..)` is fine.
    let outer = unsafe { sol_alloc(n * 16, 8, mark_ptr_array as MarkFn) };
    // See invariant (1): zero before any further allocation can trigger a GC.
    unsafe { std::ptr::write_bytes(outer, 0, n * 16) };

    for i in 0..n {
        let (ptr, len) = entry(i);
        // A zero-length entry (e.g. an explicit empty CLI argument) needs no
        // backing storage: a null data pointer with length 0 is a valid empty
        // slice and avoids a useless allocation.
        let buf = if len == 0 {
            std::ptr::null_mut()
        } else {
            let buf = unsafe { sol_alloc(len, 1, mark_noop as MarkFn) };
            unsafe { std::ptr::copy_nonoverlapping(ptr, buf, len) };
            buf
        };
        let slot = unsafe { outer.add(i * 16) };
        unsafe {
            *(slot as *mut *mut u8) = buf;
            *(slot.add(8) as *mut u64) = len as u64;
        }
    }

    unsafe {
        *(out as *mut *mut u8) = outer;
        *(out.add(8) as *mut u64) = n as u64;
    }
}

/// `args() -> &[&[Uint8]]`: the full process command line including `argv[0]`,
/// each argument copied out of `argv` into GC memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_args(out: *mut u8) {
    let argc = ARGC.get();
    let argv = ARGV.get() as *const *const c_char;
    unsafe {
        build(out, argc, |i| {
            let s = *argv.add(i);
            (s as *const u8, libc::strlen(s))
        });
    }
}

/// `env() -> &[&[Uint8]]`: the environment as `KEY=VALUE` byte strings (the
/// POSIX `environ` layout — copied verbatim out of `environ`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_env(out: *mut u8) {
    let envp = unsafe { environ };
    // Count the NUL-terminated list of entries.
    let mut n = 0usize;
    if !envp.is_null() {
        while !unsafe { *envp.add(n) }.is_null() {
            n += 1;
        }
    }
    unsafe {
        build(out, n, |i| {
            let s = *envp.add(i);
            (s as *const u8, libc::strlen(s))
        });
    }
}
