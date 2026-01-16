//! Provides high-level database metrics.

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block},
    value_balance::ValueBalance,
};

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

    metrics::counter!("state.finalized.block.count").increment(1);
    metrics::gauge!("state.finalized.block.height").set(height.0 as f64);

    metrics::counter!("state.finalized.cumulative.transactions")
        .increment(transaction_count as u64);

    metrics::counter!("state.finalized.cumulative.sprout_nullifiers")
        .increment(sprout_nullifier_count as u64);
    metrics::counter!("state.finalized.cumulative.sapling_nullifiers")
        .increment(sapling_nullifier_count as u64);
    metrics::counter!("state.finalized.cumulative.orchard_nullifiers")
        .increment(orchard_nullifier_count as u64);

    // The outputs from the genesis block can't be spent, so we skip them here.
    if !height.is_min() {
        metrics::counter!("state.finalized.cumulative.transparent_prevouts")
            .increment(transparent_prevout_count as u64);
        metrics::counter!("state.finalized.cumulative.transparent_newouts")
            .increment(transparent_newout_count as u64);
    }
}

/// Update value pool metrics after calculating the new chain value pool.
///
/// These metrics expose the current balances of each shielded pool and total supply
/// for monitoring and observability purposes.
///
/// These metrics enable:
/// - Visualizing pool balances over time in Grafana dashboards
/// - Setting up alerts for anomalous conditions
/// - Monitoring total chain supply
///
/// The values are in zatoshis (1 ZEC = 100,000,000 zatoshis).
pub(crate) fn value_pool_metrics(value_pool: &ValueBalance<NonNegative>) {
    // Individual pool balances (zatoshis)
    metrics::gauge!("state.finalized.value_pool.transparent")
        .set(u64::from(value_pool.transparent_amount()) as f64);
    metrics::gauge!("state.finalized.value_pool.sprout")
        .set(u64::from(value_pool.sprout_amount()) as f64);
    metrics::gauge!("state.finalized.value_pool.sapling")
        .set(u64::from(value_pool.sapling_amount()) as f64);
    metrics::gauge!("state.finalized.value_pool.orchard")
        .set(u64::from(value_pool.orchard_amount()) as f64);
    metrics::gauge!("state.finalized.value_pool.deferred")
        .set(u64::from(value_pool.deferred_amount()) as f64);

    // Total chain supply (sum of all pools)
    let total_supply = u64::from(value_pool.transparent_amount())
        + u64::from(value_pool.sprout_amount())
        + u64::from(value_pool.sapling_amount())
        + u64::from(value_pool.orchard_amount())
        + u64::from(value_pool.deferred_amount());
    metrics::gauge!("state.finalized.chain_supply.total").set(total_supply as f64);
}
