use std::io::{Read, Write};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_print_int(val: i64) {
    let _ = writeln!(std::io::stdout(), "{val}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_write_stdout(ptr: *const u8, len: usize) {
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut out = std::io::stdout();
    out.write_all(slice).unwrap();
    out.flush().unwrap();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_read_stdin(ptr: *mut u8, len: usize) -> usize {
    let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
    std::io::stdin().read(slice).unwrap_or(0)
}
