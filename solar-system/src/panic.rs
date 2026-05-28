use std::io::Write;

/// Demangle a Solar function name from its C symbol.
/// Returns `None` if it's not a Solar function.
fn demangle_solar(symbol: &str) -> Option<String> {
    let name = symbol.strip_prefix("solar_")?;

    if name == "main" {
        return Some("main".into());
    }

    // __closure_N → <closure N>
    if let Some(n) = name.strip_prefix("__closure_") {
        return Some(format!("<closure {n}>"));
    }

    // __method_name_Type... → Type.name
    if let Some(rest) = name.strip_prefix("__method_") {
        if let Some(underscore_pos) = rest.find('_') {
            let method_name = &rest[..underscore_pos];
            let type_name = &rest[underscore_pos + 1..];
            return Some(format!("{type_name}.{method_name}"));
        }
        return Some(rest.into());
    }

    // __mod_foo__bar → foo::bar
    if let Some(rest) = name.strip_prefix("__mod_") {
        if let Some(pos) = rest.find("__") {
            let module = &rest[..pos];
            let item = &rest[pos + 2..];
            return Some(format!("{module}::{item}"));
        }
        return Some(rest.into());
    }

    Some(name.into())
}

fn capture_backtrace() -> Vec<*mut std::ffi::c_void> {
    let mut size = 64;
    loop {
        let mut addrs = vec![std::ptr::null_mut(); size];
        let n = unsafe { libc::backtrace(addrs.as_mut_ptr(), size as i32) } as usize;
        if n < size {
            addrs.truncate(n);
            return addrs;
        }
        size *= 2;
    }
}

/// Print an error message and a demangled Solar stack trace, then abort.
pub fn sol_panic_internal(msg: &str) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{msg}");
    let _ = writeln!(err, "\nStack trace:");

    let addrs = capture_backtrace();

    let mut frame_num = 0;
    for addr in &addrs {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        if unsafe { libc::dladdr(*addr, &mut info) } != 0 && !info.dli_sname.is_null() {
            let name_str = unsafe { std::ffi::CStr::from_ptr(info.dli_sname) }.to_string_lossy();
            if let Some(display_name) = demangle_solar(&name_str) {
                let _ = writeln!(err, "  {frame_num}: {display_name}");
                frame_num += 1;
            }
        }
    }

    if frame_num == 0 {
        let _ = writeln!(err, "  (no Solar frames found)");
    }

    let _ = err.flush();
    std::process::abort();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_panic(msg: *const u8, len: usize) -> ! {
    let slice = unsafe { std::slice::from_raw_parts(msg, len) };
    let text = std::str::from_utf8(slice).unwrap_or("<invalid utf8>");
    sol_panic_internal(text)
}

/// Install a panic hook that uses `sol_panic_internal` for nice Solar stack traces.
/// Called from `sol_start`.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "panic".into()
        };
        sol_panic_internal(&msg);
    }));
}
