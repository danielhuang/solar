//! Checked arithmetic intrinsics.

use crate::panic::throw_str;

/// Adds signed integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_add_int(a: i64, b: i64) -> i64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => throw_str("integer overflow in addition"),
    }
}

/// Subtracts signed integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_sub_int(a: i64, b: i64) -> i64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => throw_str("integer overflow in subtraction"),
    }
}

/// Multiplies signed integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mul_int(a: i64, b: i64) -> i64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => throw_str("integer overflow in multiplication"),
    }
}

/// Divides signed integers or throws on overflow or division by zero.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_div_int(a: i64, b: i64) -> i64 {
    match a.checked_div(b) {
        Some(v) => v,
        None if b == 0 => throw_str("integer division by zero"),
        None => throw_str("integer overflow in division"),
    }
}

/// Computes signed remainder or throws on overflow or division by zero.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mod_int(a: i64, b: i64) -> i64 {
    match a.checked_rem(b) {
        Some(v) => v,
        None if b == 0 => throw_str("integer modulo by zero"),
        None => throw_str("integer overflow in modulo"),
    }
}

/// Adds unsigned integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_add_uint(a: u64, b: u64) -> u64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => throw_str("integer overflow in addition"),
    }
}

/// Subtracts unsigned integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_sub_uint(a: u64, b: u64) -> u64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => throw_str("integer overflow in subtraction"),
    }
}

/// Multiplies unsigned integers or throws on overflow.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mul_uint(a: u64, b: u64) -> u64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => throw_str("integer overflow in multiplication"),
    }
}

/// Divides unsigned integers or throws on division by zero.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_div_uint(a: u64, b: u64) -> u64 {
    match a.checked_div(b) {
        Some(v) => v,
        None => throw_str("integer division by zero"),
    }
}

/// Computes unsigned remainder or throws on division by zero.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_checked_mod_uint(a: u64, b: u64) -> u64 {
    match a.checked_rem(b) {
        Some(v) => v,
        None => throw_str("integer modulo by zero"),
    }
}

/// Writes the low and high halves of `a * b + carry + add`.
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
