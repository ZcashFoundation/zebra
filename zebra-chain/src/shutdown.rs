//! Shutdown related code.
//!
//! A global flag indicates when the application is shutting down so actions can be taken
//! at different parts of the codebase.

use std::sync::atomic::AtomicBool;

/// A flag to indicate if zebrad is shutting down.
pub static IS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
