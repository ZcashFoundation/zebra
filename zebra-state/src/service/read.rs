//! Shared state reading code.
//!
//! Used by [`StateService`][1] and [`ReadStateService`][2] to read from the
//! best [`Chain`][5] in the [`NonFinalizedState`][3], and the database in the
//! [`FinalizedState`][4].
//!
//! [1]: service::StateService
//! [2]: service::ReadStateService
//! [3]: service::non_finalized_state::NonFinalizedState
//! [4]: service::finalized_state::FinalizedState
//! [5]: service::non_finalized_state::Chain

// Tidy up some doc links
#[allow(unused_imports)]
use crate::service;

pub mod address;
pub mod block;
pub mod difficulty;
pub mod find;
pub mod tree;

#[cfg(test)]
mod tests;

pub use address::{
    balance::transparent_balance,
    tx_id::transparent_tx_ids,
    utxo::{address_utxos, AddressUtxos},
};
pub use block::{
    any_block, any_transaction, any_utxo, block, block_and_size, block_header, block_info,
    mined_transaction, transaction_hashes_for_any_block, transaction_hashes_for_block,
    unspent_utxo,
};

#[cfg(feature = "indexer")]
pub use block::spending_transaction_hash;

pub use find::{
    best_tip, block_locator, depth, finalized_state_contains_block_hash, find_chain_hashes,
    find_chain_headers, hash_by_height, height_by_hash, next_median_time_past,
    non_finalized_state_contains_block_hash, tip, tip_height, tip_with_value_balance,
};
pub use tree::{orchard_subtrees, orchard_tree, sapling_subtrees, sapling_tree};

#[cfg(any(test, feature = "proptest-impl"))]
#[allow(unused_imports)]
pub use address::utxo::ADDRESS_HEIGHTS_FULL_RANGE;

/// If a finalized state query is interrupted by a new finalized block,
/// retry this many times.
///
/// Once we're at the tip, we expect up to 2 blocks to arrive at the same time.
/// If any more arrive, the client should wait until we're synchronised with our peers.
pub const FINALIZED_STATE_QUERY_RETRIES: usize = 3;
