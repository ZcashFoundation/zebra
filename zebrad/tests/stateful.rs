//! Stateful tests for Zebra.
//!
//! Require cached blockchain state.

mod common;

#[path = "stateful/checkpoint_sync.rs"]
mod checkpoint_sync;

#[path = "stateful/lightwalletd.rs"]
mod lightwalletd;

#[path = "stateful/mining_rpcs.rs"]
mod mining_rpcs;

#[path = "stateful/state_format.rs"]
mod state_format;
