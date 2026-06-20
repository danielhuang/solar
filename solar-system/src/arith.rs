#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_add_int(a: i64, b: i64) -> i64 {
    a.checked_add(b).expect("integer overflow in addition")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_sub_int(a: i64, b: i64) -> i64 {
    a.checked_sub(b).expect("integer overflow in subtraction")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_mul_int(a: i64, b: i64) -> i64 {
    a.checked_mul(b)
        .expect("integer overflow in multiplication")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_div_int(a: i64, b: i64) -> i64 {
    a.checked_div(b)
        .expect("integer division by zero or overflow")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_mod_int(a: i64, b: i64) -> i64 {
    a.checked_rem(b)
        .expect("integer modulo by zero or overflow")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_add_uint(a: u64, b: u64) -> u64 {
    a.checked_add(b).expect("unsigned overflow in addition")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_sub_uint(a: u64, b: u64) -> u64 {
    a.checked_sub(b).expect("unsigned overflow in subtraction")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_mul_uint(a: u64, b: u64) -> u64 {
    a.checked_mul(b)
        .expect("unsigned overflow in multiplication")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_div_uint(a: u64, b: u64) -> u64 {
    a.checked_div(b).expect("unsigned division by zero")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_checked_mod_uint(a: u64, b: u64) -> u64 {
    a.checked_rem(b).expect("unsigned modulo by zero")
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
