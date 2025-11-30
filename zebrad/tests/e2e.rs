//! End-to-end tests for Zebra.
//!
//! Long-running full-chain sync and wallet integration tests.

mod common;

#[path = "e2e/full_sync.rs"]
mod full_sync;

#[path = "e2e/wallet_integration.rs"]
mod wallet_integration;
