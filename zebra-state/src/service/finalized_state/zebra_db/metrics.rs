//! Provides high-level database metrics.

use zebra_chain::block::{self, Block};

/// Update metrics before committing a block.
///
/// The metrics are updated after contextually validating a block,
/// but before writing its batch to the state.
pub(crate) fn block_precommit_metrics(block: &Block, hash: block::Hash, height: block::Height) {
    let transaction_count = block.transactions.len();
    let transparent_prevout_count = block
        .transactions
        .iter()
        .flat_map(|t| t.inputs().iter())
        .count()
        // Each block has a single coinbase input which is not a previous output.
        - 1;
    let transparent_newout_count = block
        .transactions
        .iter()
        .flat_map(|t| t.outputs().iter())
        .count();

    let sprout_nullifier_count = block.sprout_nullifiers().count();
    let sapling_nullifier_count = block.sapling_nullifiers().count();
    let orchard_nullifier_count = block.orchard_nullifiers().count();

    tracing::debug!(
        ?hash,
        ?height,
        transaction_count,
        transparent_prevout_count,
        transparent_newout_count,
        sprout_nullifier_count,
        sapling_nullifier_count,
        orchard_nullifier_count,
        "preparing to commit finalized {:?}block",
        if height.is_min() { "genesis " } else { "" }
    );

    metrics::counter!("state.finalized.block.count", 1);
    metrics::gauge!("state.finalized.block.height", height.0 as f64);

    metrics::counter!(
        "state.finalized.cumulative.transactions",
        transaction_count as u64
    );

    metrics::counter!(
        "state.finalized.cumulative.sprout_nullifiers",
        sprout_nullifier_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.sapling_nullifiers",
        sapling_nullifier_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.orchard_nullifiers",
        orchard_nullifier_count as u64
    );

    // The outputs from the genesis block can't be spent, so we skip them here.
    if !height.is_min() {
        metrics::counter!(
            "state.finalized.cumulative.transparent_prevouts",
            transparent_prevout_count as u64
        );
        metrics::counter!(
            "state.finalized.cumulative.transparent_newouts",
            transparent_newout_count as u64
        );
    }
}
