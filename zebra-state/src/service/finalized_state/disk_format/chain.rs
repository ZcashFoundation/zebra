//! Chain data serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::collections::BTreeMap;

use bincode::Options;

use zebra_chain::{
    amount::NonNegative, block::Height, history_tree::NonEmptyHistoryTree, parameters::Network,
    primitives::zcash_history, value_balance::ValueBalance,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

impl IntoDisk for ValueBalance<NonNegative> {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
    }
}

impl FromDisk for ValueBalance<NonNegative> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        ValueBalance::from_bytes(array).unwrap()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryTreeParts {
    network: Network,
    size: u32,
    peaks: BTreeMap<u32, zcash_history::Entry>,
    current_height: Height,
}

impl IntoDisk for NonEmptyHistoryTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let data = HistoryTreeParts {
            network: self.network(),
            size: self.size(),
            peaks: self.peaks().clone(),
            current_height: self.current_height(),
        };
        bincode::DefaultOptions::new()
            .serialize(&data)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for NonEmptyHistoryTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let parts: HistoryTreeParts = bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect(
                "deserialization format should match the serialization format used by IntoDisk",
            );
        NonEmptyHistoryTree::from_cache(
            parts.network,
            parts.size,
            parts.peaks,
            parts.current_height,
        )
        .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}
