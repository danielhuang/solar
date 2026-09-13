use std::io::Write;

/// Print an error message and a demangled Solar stack trace, then abort.
///
/// Frames are resolved with the `backtrace` crate (the symbolizer std and samply
/// use), which reads the executable's `.symtab` + DWARF. That's what lets it name
/// the generated `solar_*` functions even though codegen emits them `static`
/// (local) — `dladdr`, which only consults the dynamic symbol table, can't. It
/// also recovers inlined frames, so optimized builds still show the call chain.
pub fn sol_panic_internal(msg: &str) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{msg}");
    let _ = writeln!(err, "\nStack trace:");

    let mut frame_num = 0;
    backtrace::trace(|frame| {
        backtrace::resolve_frame(frame, |sym| {
            // A frame can resolve to several symbols (inlined call chain); show each.
            if let Some(name) = sym.name().and_then(|n| n.as_str())
                && let Some(display_name) = solar_shared::trace::demangle_solar(name)
            {
                let _ = writeln!(err, "  {frame_num}: {display_name}");
                frame_num += 1;
            }
        });
        true
    });

    if frame_num == 0 {
        let _ = writeln!(err, "  (no Solar frames found)");
    }

    let _ = err.flush();
    std::process::abort();
}

/// Installs the runtime panic hook.
///
/// Recoverable Solar exceptions are allowed to continue unwinding.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if info.payload().is::<SolarException>() {
            return;
        }
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

/// A Solar byte or address slice, represented by a pointer and element count.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SolSlice {
    ptr: *const u8,
    len: usize,
}

/// The native layout of the `@std::Exception` type. All three fields
/// occupy aligned 16-byte slots in declaration order.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct SolarException {
    message: SolSlice,
    payload: [usize; 2],
    trace: SolSlice,
}

// A panic payload stays on its originating mutator thread. The GC critical
// section protects all three references until the handler roots them again.
unsafe impl Send for SolarException {}

std::thread_local! {
    static TRY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

static UNIT_PAYLOAD: u8 = 0;

#[cfg(not(test))]
unsafe extern "C" {
    /// Implemented by each generated program; returns a static NUL-terminated name.
    fn sol_payload_type_name(tag: u64) -> *const std::ffi::c_char;
}

// Runtime unit tests have no generated program and only use the Unit payload.
#[cfg(test)]
unsafe fn sol_payload_type_name(tag: u64) -> *const std::ffi::c_char {
    assert_eq!(tag, solar_shared::ANY_UNIT_TAG);
    c"Unit".as_ptr()
}

unsafe fn copy_bytes(bytes: &[u8], align: usize) -> SolSlice {
    let ptr =
        unsafe { crate::mem::sol_alloc_impl(bytes.len().max(1), align, crate::process::mark_noop) };
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    SolSlice {
        ptr,
        len: bytes.len(),
    }
}

unsafe fn capture_backtrace() -> SolSlice {
    let addresses = solar_shared::trace::capture();
    let bytes =
        unsafe { std::slice::from_raw_parts(addresses.as_ptr().cast(), addresses.len() * 8) };
    let mut trace = unsafe { copy_bytes(bytes, 8) };
    trace.len = addresses.len();
    trace
}

unsafe fn new_exception(message: SolSlice, payload: [usize; 2]) -> SolarException {
    SolarException {
        message,
        payload,
        trace: unsafe { capture_backtrace() },
    }
}

unsafe fn exception_text(exception: &SolarException) -> String {
    let message =
        unsafe { std::slice::from_raw_parts(exception.message.ptr, exception.message.len) };
    let addresses = unsafe {
        std::slice::from_raw_parts(exception.trace.ptr.cast::<usize>(), exception.trace.len)
    };
    let payload =
        unsafe { std::ffi::CStr::from_ptr(sol_payload_type_name(exception.payload[1] as u64)) }
            .to_string_lossy();
    format!(
        "{}\nPayload type: {}\n{}",
        String::from_utf8_lossy(message),
        payload,
        solar_shared::trace::format(addresses)
    )
}

/// Captures unresolved addresses into a GC-allocated slice of Uint values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_capture_backtrace(dst: *mut SolSlice) {
    let slot = crate::gc::MY_SLOT.get();
    assert!(!slot.is_null());
    unsafe { crate::gc::begin_critical_section(&*slot) };
    unsafe { dst.write(capture_backtrace()) };
    unsafe { crate::gc::end_critical_section(&*slot) };
}

/// Resolves an instruction address into a GC-allocated, demangled byte string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_resolve_address(dst: *mut SolSlice, address: usize) {
    let slot = crate::gc::MY_SLOT.get();
    assert!(!slot.is_null());
    unsafe { crate::gc::begin_critical_section(&*slot) };
    {
        let text = solar_shared::trace::resolve_address(address);
        unsafe { dst.write(copy_bytes(text.as_bytes(), 1)) };
    }
    unsafe { crate::gc::end_critical_section(&*slot) };
}

fn throw_raw(exception: SolarException) -> ! {
    if TRY_DEPTH.get() == 0 {
        let text = unsafe { exception_text(&exception) };
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "uncaught exception: {text}");
        let _ = stderr.flush();
        std::process::abort();
    }
    std::panic::panic_any(exception)
}

/// Unwinds with an existing exception, retaining its original trace and aliases.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_throw(exception: *const SolarException) -> ! {
    let slot = crate::gc::MY_SLOT.get();
    assert!(!slot.is_null(), "sol_throw called on unregistered thread");
    unsafe { crate::gc::begin_critical_section(&*slot) };
    throw_raw(unsafe { *exception })
}

/// Throws a runtime failure with a Unit payload and a trace from the failure site.
pub(crate) fn throw_str(msg: &'static str) -> ! {
    let slot = crate::gc::MY_SLOT.get();
    assert!(!slot.is_null(), "throw_str called on unregistered thread");
    unsafe { crate::gc::begin_critical_section(&*slot) };
    let exception = unsafe {
        new_exception(
            SolSlice {
                ptr: msg.as_ptr(),
                len: msg.len(),
            },
            unit_payload(),
        )
    };
    throw_raw(exception)
}

fn unit_payload() -> [usize; 2] {
    [
        std::ptr::addr_of!(UNIT_PAYLOAD) as usize,
        solar_shared::ANY_UNIT_TAG as usize,
    ]
}

/// Formats and captures a runtime failure inside the critical section protecting
/// Rust allocator use and the exception's GC references across unwinding.
pub(crate) fn throw_message(args: std::fmt::Arguments) -> ! {
    if let Some(message) = args.as_str() {
        throw_str(message);
    }
    let slot = crate::gc::MY_SLOT.get();
    assert!(
        !slot.is_null(),
        "throw_message called on unregistered thread"
    );
    unsafe { crate::gc::begin_critical_section(&*slot) };
    let text = args.to_string();
    let message = unsafe { copy_bytes(text.as_bytes(), 1) };
    let exception = unsafe { new_exception(message, unit_payload()) };
    throw_raw(exception)
}

/// Runs a Solar body and invokes its handler with the caught exception.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn sol_try(
    body_fn: unsafe extern "C-unwind" fn(*mut std::ffi::c_void),
    body_env: *mut std::ffi::c_void,
    handler_fn: unsafe extern "C-unwind" fn(*mut std::ffi::c_void, SolarException),
    handler_env: *mut std::ffi::c_void,
) {
    TRY_DEPTH.set(TRY_DEPTH.get() + 1);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        body_fn(body_env)
    }));
    TRY_DEPTH.set(TRY_DEPTH.get() - 1);
    if let Err(payload) = result {
        match payload.downcast::<SolarException>() {
            Ok(exc) => {
                let exception = *exc;
                // Release the Rust allocation while allocator use is still protected.
                drop(exc);
                // Force a complete stack copy before permitting collection. The
                // black box after the handler also keeps all fields live through it.
                std::hint::black_box(&exception);
                let slot = crate::gc::MY_SLOT.get();
                assert!(!slot.is_null());
                unsafe { crate::gc::end_critical_section(&*slot) };
                unsafe { handler_fn(handler_env, exception) };
                std::hint::black_box(&exception);
            }
            Err(other) => std::panic::resume_unwind(other),
        }
    }
}
