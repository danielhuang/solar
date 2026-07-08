fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(clock, &mut ts) };
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

/// CLOCK_MONOTONIC in nanoseconds. The epoch is unspecified — only differences
/// are meaningful.
#[unsafe(no_mangle)]
pub extern "C" fn sol_monotonic_time() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC)
}

/// Wall-clock time as nanoseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn sol_system_time() -> u64 {
    clock_ns(libc::CLOCK_REALTIME)
}
