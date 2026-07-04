//! Globals that are written during single-threaded startup and only read
//! after: an `UnsafeCell` with plain (non-atomic) loads instead of an atomic.
//!
//! Not a synchronization primitive. Soundness rests on the runtime's startup
//! ordering: every `set` happens in `sol_start` (or earlier — an `.init_array`
//! constructor, or codegen's pre-`sol_start` `sol_disable_gc` call) before the
//! GC thread, the thread pool, or any mutator thread exists. Spawning those
//! threads is the publication point — `thread::spawn` synchronizes-with the
//! new thread, so the plain reads afterwards see the initialized values.

use std::cell::UnsafeCell;

pub struct InitCell<T>(UnsafeCell<T>);

// SAFETY: mutation is confined to pre-thread startup (see module docs); after
// that the cell is read-only, and a `Copy` value is freely shareable.
unsafe impl<T: Copy + Send> Sync for InitCell<T> {}

impl<T: Copy> InitCell<T> {
    pub const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    #[inline]
    pub fn get(&self) -> T {
        unsafe { *self.0.get() }
    }

    /// # Safety
    /// Must only be called during single-threaded startup, before any thread
    /// that may `get` this cell has been spawned.
    #[inline]
    pub unsafe fn set(&self, v: T) {
        unsafe { *self.0.get() = v }
    }
}
