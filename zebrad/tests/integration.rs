//! Integration tests for Zebra.
//!
//! Zebrad needs to be running, but no cached state required.

mod common;

#[path = "integration/conflicts.rs"]
mod conflicts;

#[path = "integration/endpoints.rs"]
mod endpoints;

#[path = "integration/sync.rs"]
mod sync;
