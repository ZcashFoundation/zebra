//! Tests for mempool storage.

mod prop;
mod vectors;

/// Force an over-budget insertion after a test has published the original verified set.
pub(in crate::components::mempool) fn set_tx_cost_limit(storage: &mut super::Storage, limit: u64) {
    storage.tx_cost_limit = limit;
}
