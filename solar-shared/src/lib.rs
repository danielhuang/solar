//! Utilities shared by the Solar compiler and native runtime.

/// Resident-memory limits for execution without garbage collection.
pub mod memory_limit;

/// Lazy capture and symbolization of exception stack traces.
pub mod trace;

/// Tag of the Unit payload, and high-bit prefix for other concrete Any tags.
pub const ANY_UNIT_TAG: u64 = 0x00ff_0000_0000_0000;
