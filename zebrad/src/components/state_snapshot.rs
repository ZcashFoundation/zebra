//! Starting a pruned node from a state snapshot.
//!
//! Before `zebrad start` initializes the state, [`decision`] decides whether
//! an empty, pruned node bootstraps from a snapshot. If it does, [`bootstrap`]
//! verifies and imports it. [`scheduler`] downloads snapshot pieces, and
//! [`progress`] names the stages the bootstrap logs. [`serving`] publishes
//! this node's own snapshots to version 2 peers.

pub mod bootstrap;
pub mod decision;
pub mod progress;
pub mod scheduler;
pub mod serving;
