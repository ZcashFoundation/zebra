//! Shared state reading code.
//!
//! Used by [`StateService`][1] and [`ReadStateService`][2] to read from the
//! best [`Chain`][5] in the [`NonFinalizedState`][3], and the database in the
//! [`FinalizedState`][4].
//!
//! [1]: super::StateService
//! [2]: super::ReadStateService
//! [3]: super::non_finalized_state::NonFinalizedState
//! [4]: super::finalized_state::FinalizedState
//! [5]: super::Chain

pub mod address;
pub mod block;
pub mod find;
pub mod tree;

#[cfg(test)]
mod tests;

pub use address::{
    balance::transparent_balance,
    tx_id::transparent_tx_ids,
    utxo::{transparent_utxos, AddressUtxos, ADDRESS_HEIGHTS_FULL_RANGE},
};
pub use block::{block, block_header, transaction};
pub use find::{
    chain_contains_hash, find_chain_hashes, find_chain_headers, hash_by_height, height_by_hash,
    tip_height,
};
pub use tree::{orchard_tree, sapling_tree};
