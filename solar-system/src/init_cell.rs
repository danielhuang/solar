//! Globals initialized before runtime threads are spawned.

use std::cell::UnsafeCell;

/// A copyable value initialized during single-threaded startup.
pub struct InitCell<T>(UnsafeCell<T>);

// SAFETY: mutation is confined to pre-thread startup (see module docs); after
// that the cell is read-only, and a `Copy` value is freely shareable.
unsafe impl<T: Copy + Send> Sync for InitCell<T> {}

impl<T: Copy> InitCell<T> {
    /// Creates a cell with an initial value.
    pub const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    #[inline]
    /// Returns the stored value.
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
