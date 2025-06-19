//! A tonic RPC server for Zebra's indexer API.

use std::sync::Arc;

use zebra_chain::{block, serialization::ZcashSerialize};

#[cfg(test)]
mod tests;

pub mod methods;
pub mod server;

// The generated indexer proto
tonic::include_proto!("zebra.indexer.rpc");

pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("indexer_descriptor");

impl BlockHashAndHeight {
    /// Create a new [`BlockHashAndHeight`] from a [`block::Hash`] and [`block::Height`].
    pub fn new(hash: block::Hash, block::Height(height): block::Height) -> Self {
        let hash = hash.bytes_in_display_order().to_vec();
        BlockHashAndHeight { hash, height }
    }

    /// Try to convert a [`BlockHashAndHeight`] into a tuple of a block hash and height.
    pub fn try_into_hash_and_height(self) -> Option<(block::Hash, block::Height)> {
        self.hash
            .try_into()
            .map(|bytes| block::Hash::from_bytes_in_display_order(&bytes))
            .map_err(|bytes: Vec<_>| {
                tracing::warn!(
                    "failed to convert BlockHash to Hash, unexpected len: {}",
                    bytes.len()
                )
            })
            .ok()
            .and_then(|hash| self.height.try_into().ok().map(|height| (hash, height)))
    }
}

impl BlockAndHash {
    /// Create a new [`BlockHashAndHeight`] from a [`block::Hash`] and [`block::Height`].
    ///
    /// # Panics
    ///
    /// This function will panic if the block serialization fails (if the header version is invalid).
    pub fn new(hash: block::Hash, block: Arc<block::Block>) -> Self {
        BlockAndHash {
            hash: hash.bytes_in_display_order().to_vec(),
            data: block
                .zcash_serialize_to_vec()
                .expect("block serialization should not fail"),
        }
    }
}
