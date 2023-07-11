//! Diagnostic types and functions for Zebra tasks:
//! - OS thread handling
//! - async future task handling
//! - errors and panics

#[cfg(feature = "async-error")]
pub mod future;

pub mod thread;
