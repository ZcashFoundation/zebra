//! Serialization types for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use zebra_chain::block::Height;

/// A transaction's location in the chain, by block height and transaction index.
///
/// This provides a chain-order list of transactions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct TransactionLocation {
    /// The block height of the transaction.
    pub height: Height,

    /// The index of the transaction in its block.
    pub index: u32,
}

impl TransactionLocation {
    /// Create a transaction location from a block height and index (as the native index integer type).
    #[allow(dead_code)]
    pub fn from_usize(height: Height, index: usize) -> TransactionLocation {
        TransactionLocation {
            height,
            index: index
                .try_into()
                .expect("all valid indexes are much lower than u32::MAX"),
        }
    }
}
