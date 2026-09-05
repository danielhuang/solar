//! Resident-memory limit shared by the interpreters and GC-disabled runtime.

use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

const LIMIT: usize = 1 << 30;
const CHECK_INTERVAL: Duration = Duration::from_millis(10);

/// Monitors total resident process memory until the guard is dropped.
///
/// Linux's smaps rollup includes resident heap, stacks, and interpreter overhead
/// without counting untouched virtual arena reservations. Sampling permits
/// transient overshoot between checks. Exceeding the limit aborts the process,
/// rather than raising a catchable Solar exception.
pub struct MemoryLimit {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl MemoryLimit {
    /// Starts monitoring with a 1 GiB resident-memory limit.
    ///
    /// Requires Linux procfs. Aborts if the limit is exceeded or memory usage
    /// cannot be read. Dropping the guard stops and joins the monitor.
    pub fn start() -> Self {
        Self::with_limit(LIMIT)
    }

    fn with_limit(limit: usize) -> Self {
        check(limit);
        let (stop, stopped) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("solar-memory-limit".into())
            .spawn(move || {
                while stopped.recv_timeout(CHECK_INTERVAL) == Err(mpsc::RecvTimeoutError::Timeout) {
                    check(limit);
                }
                check(limit);
            })
            .unwrap();
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for MemoryLimit {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        self.worker.take().unwrap().join().unwrap();
    }
}

fn resident_bytes() -> std::io::Result<usize> {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup")?;
    let bytes = rollup.lines().find_map(|line| {
        line.strip_prefix("Rss:")?
            .split_whitespace()
            .next()?
            .parse::<usize>()
            .ok()?
            .checked_mul(1024)
    });
    bytes.ok_or_else(|| std::io::Error::other("missing or invalid resident memory size"))
}

fn check(limit: usize) {
    match resident_bytes() {
        Ok(bytes) if bytes <= limit => return,
        Ok(bytes) => eprintln!(
            "memory limit exceeded: {bytes} resident bytes > {limit} bytes (GC is disabled)"
        ),
        Err(error) => eprintln!("cannot enforce memory limit: {error}"),
    }
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn memory_limit_child() {
        let Ok(mode) = std::env::var("SOLAR_MEMORY_LIMIT_TEST_CHILD") else {
            return;
        };
        if mode == "initial" {
            let _limit = MemoryLimit::with_limit(0);
            panic!("initial check did not abort");
        }
        let limit = MemoryLimit::with_limit(resident_bytes().unwrap() + (8 << 20));
        if mode == "stopped" {
            drop(limit);
            let allocation = vec![1u8; 32 << 20];
            std::hint::black_box(&allocation);
            std::thread::sleep(CHECK_INTERVAL * 3);
            return;
        }
        let allocation = vec![1u8; 32 << 20];
        std::hint::black_box(&allocation);
        std::thread::sleep(Duration::from_secs(5));
        panic!("memory limit did not abort");
    }

    #[test]
    fn aborts_when_initial_memory_or_later_growth_exceeds_limit() {
        use std::os::unix::process::ExitStatusExt;

        for mode in ["initial", "growth"] {
            let output = child(mode);
            assert_eq!(output.status.signal(), Some(6), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("memory limit exceeded"));
        }
    }

    #[test]
    fn dropping_guard_stops_monitoring() {
        let output = child("stopped");
        assert!(output.status.success(), "{output:?}");
    }

    fn child(mode: &str) -> std::process::Output {
        Command::new("sh")
            .args(["-c", "ulimit -c 0; exec \"$@\"", "memory-limit-test"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "memory_limit::tests::memory_limit_child",
                "--nocapture",
            ])
            .env("SOLAR_MEMORY_LIMIT_TEST_CHILD", mode)
            .output()
            .unwrap()
    }
}
