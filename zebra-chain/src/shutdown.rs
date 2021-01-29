//! Shutdown related code.
//!
//! A global flag indicates when the application is shutting down so actions can be taken
//! at different parts of the codebase.

use std::sync::atomic::AtomicBool;

/// A flag to indicate if zebrad is shutting down.
pub static IS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Returns true if the application is shutting down.
///
/// Returns false otherwise.
pub fn is_shutting_down() -> bool {
    use std::sync::atomic::Ordering;
    // ## Correctness:
    //
    // Since we're shutting down, and this is a one-time operation,
    // performance is not important. So we use the strongest memory
    // ordering.
    // https://doc.rust-lang.org/nomicon/atomics.html#sequentially-consistent
    IS_SHUTTING_DOWN.load(Ordering::SeqCst)
}
