use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Fixed-size worker pool used by the collector.
pub struct ThreadPool {
    sender: flume::Sender<Job>,
    /// Number of worker threads. The parallel marker submits exactly this many
    /// jobs (each runs until quiescence), so this must equal the count that can
    /// run concurrently — else submitted jobs queue and never start.
    size: usize,
    /// Submitted-but-not-finished count. `submit` fetch_adds; workers
    /// fetch_sub after the closure returns; `wait_for_all` FUTEX_WAITs
    /// while > 0; the worker that drives it to zero FUTEX_WAKEs.
    unfinished: Arc<AtomicU32>,
}

impl ThreadPool {
    fn new(size: usize) -> Self {
        let (sender, receiver) = flume::unbounded::<Job>();
        let unfinished = Arc::new(AtomicU32::new(0));
        for i in 0..size {
            let receiver = receiver.clone();
            let unfinished = unfinished.clone();
            thread::Builder::new()
                .name(format!("solar-pool-{i}"))
                .stack_size(4 * 1024 * 1024 * 1024)
                .spawn(move || {
                    crate::gc::block_gc_signal();
                    loop {
                        let job = match receiver.recv() {
                            Ok(j) => j,
                            Err(_) => return, // all senders dropped; pool shutting down
                        };
                        job();
                        if unfinished.fetch_sub(1, Ordering::AcqRel) == 1 {
                            unsafe {
                                libc::syscall(
                                    libc::SYS_futex,
                                    &*unfinished as *const AtomicU32,
                                    libc::FUTEX_WAKE,
                                    i32::MAX as i64,
                                );
                            }
                        }
                    }
                })
                .unwrap();
        }
        ThreadPool {
            sender,
            size,
            unfinished,
        }
    }

    /// Number of worker threads.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Submits a job for asynchronous execution.
    pub fn submit(&self, f: impl FnOnce() + Send + 'static) {
        // Publish the unfinished count before making the job visible.
        self.unfinished.fetch_add(1, Ordering::AcqRel);
        self.sender.send(Box::new(f)).unwrap();
    }

    /// Blocks until all submitted jobs finish.
    pub fn wait_for_all(&self) {
        loop {
            let cur = self.unfinished.load(Ordering::Acquire);
            if cur == 0 {
                return;
            }
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &*self.unfinished as *const AtomicU32,
                    libc::FUTEX_WAIT,
                    cur,
                    std::ptr::null::<libc::timespec>(),
                );
            }
        }
    }
}

/// Global collector worker pool.
pub static THREAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| {
    // `SOLAR_THREAD_POOL_SIZE` overrides the worker count (must be > 0);
    // otherwise default to the available parallelism.
    let n = std::env::var("SOLAR_THREAD_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1)
        });
    ThreadPool::new(n)
});
