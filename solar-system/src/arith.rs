//! Checked arithmetic backing Solar's `+ - * / %`. Overflow and division by
//! zero are *user* errors, not runtime invariant violations, so they throw a
//! catchable Solar exception (see `panic::throw_str`) instead of panicking.
//! All functions are `extern "C-unwind"` so the throw may unwind back through
//! the generated C frames to the nearest `sol_try`.
//!
//! The messages are part of the language's observable behavior: the
//! interpreters (`ast_interp`/`ir_interp`) throw byte-identical strings so the
//! three backends stay in lockstep.

use crate::panic::throw_str;

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_add_int(a: i64, b: i64) -> i64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => throw_str("integer overflow in addition"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_sub_int(a: i64, b: i64) -> i64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => throw_str("integer overflow in subtraction"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mul_int(a: i64, b: i64) -> i64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => throw_str("integer overflow in multiplication"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_div_int(a: i64, b: i64) -> i64 {
    match a.checked_div(b) {
        Some(v) => v,
        None if b == 0 => throw_str("integer division by zero"),
        None => throw_str("integer overflow in division"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mod_int(a: i64, b: i64) -> i64 {
    match a.checked_rem(b) {
        Some(v) => v,
        None if b == 0 => throw_str("integer modulo by zero"),
        None => throw_str("integer overflow in modulo"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_add_uint(a: u64, b: u64) -> u64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => throw_str("integer overflow in addition"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_sub_uint(a: u64, b: u64) -> u64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => throw_str("integer overflow in subtraction"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mul_uint(a: u64, b: u64) -> u64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => throw_str("integer overflow in multiplication"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_div_uint(a: u64, b: u64) -> u64 {
    match a.checked_div(b) {
        Some(v) => v,
        None => throw_str("integer division by zero"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mod_uint(a: u64, b: u64) -> u64 {
    match a.checked_rem(b) {
        Some(v) => v,
        None => throw_str("integer modulo by zero"),
    }
}

/// Full 128-bit multiply-add: computes `a*b + carry + add` (which never
/// overflows 128 bits) and writes the low/high 64-bit halves through the two
/// out-params. Backs the `carrying_mul_add` intrinsic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_carrying_mul_add(
    a: u64,
    b: u64,
    carry: u64,
    add: u64,
    out_lo: *mut u64,
    out_hi: *mut u64,
) {
    let (lo, hi) = a.carrying_mul_add(b, carry, add);
    unsafe {
        *out_lo = lo;
        *out_hi = hi;
    }
}
