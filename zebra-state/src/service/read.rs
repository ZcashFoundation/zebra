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
pub mod find;
pub mod tree;

#[cfg(feature = "getblocktemplate-rpcs")]
pub mod difficulty;

#[cfg(test)]
mod tests;

pub use address::{
    balance::transparent_balance,
    tx_id::transparent_tx_ids,
    utxo::{address_utxos, AddressUtxos, ADDRESS_HEIGHTS_FULL_RANGE},
};
pub use block::{
    any_utxo, block, block_header, transaction, transaction_hashes_for_block, unspent_utxo, utxo,
};
pub use find::{
    best_tip, block_locator, chain_contains_hash, depth, find_chain_hashes, find_chain_headers,
    hash_by_height, height_by_hash, next_median_time_past, tip, tip_height,
};
pub use tree::{orchard_tree, sapling_tree};

#[cfg(feature = "getblocktemplate-rpcs")]
pub use difficulty::get_block_template_chain_info;

/// If a finalized state query is interrupted by a new finalized block,
/// retry this many times.
///
/// Once we're at the tip, we expect up to 2 blocks to arrive at the same time.
/// If any more arrive, the client should wait until we're synchronised with our peers.
pub const FINALIZED_STATE_QUERY_RETRIES: usize = 3;
