//! Shutdown related code.
//!
//! A global flag indicates when the application is shutting down so actions can be taken
//! at different parts of the codebase.

use std::sync::atomic::{AtomicBool, Ordering};

/// A flag to indicate if Zebra is shutting down.
///
/// Initialized to `false` at startup.
pub static IS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Returns true if the application is shutting down.
///
/// Returns false otherwise.
pub fn is_shutting_down() -> bool {
    // ## Correctness:
    //
    // Since we're shutting down, and this is a one-time operation,
    // performance is not important. So we use the strongest memory
    // ordering.
    // https://doc.rust-lang.org/nomicon/atomics.html#sequentially-consistent
    IS_SHUTTING_DOWN.load(Ordering::SeqCst)
}

/// Sets the Zebra shutdown flag to `true`.
pub fn set_shutting_down() {
    IS_SHUTTING_DOWN.store(true, Ordering::SeqCst);
}
