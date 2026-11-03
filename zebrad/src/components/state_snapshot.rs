//! Starting a pruned node from a state snapshot.
//!
//! Before `zebrad start` initializes the state, [`decision`] decides whether
//! an empty, pruned node bootstraps from a snapshot. [`scheduler`] downloads
//! snapshot pieces, and [`progress`] names the stages the bootstrap logs.

pub mod decision;
pub mod progress;
pub mod scheduler;
