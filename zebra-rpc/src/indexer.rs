//! A tonic RPC server for Zebra's indexer API.

use std::sync::Arc;

use zebra_chain::{
    block,
    serialization::{
        BytesInDisplayOrder, SerializationError, ZcashDeserialize, ZcashDeserializeInto,
        ZcashSerialize,
    },
};

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
            .zip(self.height.try_into().ok())
    }
}

impl BlockAndHash {
    /// Creates a new [`BlockAndHash`] from a [`block::Hash`] and [`block::Height`].
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

    /// Creates a new [`BlockAndHash`] from a consensus-serialized [`block::SerializedBlock`],
    /// returning the message together with the parsed [`block::Hash`].
    ///
    /// Only the block header prefix is deserialized to derive the hash; the transaction bytes are
    /// forwarded without deserializing or copying them.
    ///
    /// # Errors
    ///
    /// Returns a [`SerializationError`] if the leading header bytes cannot be deserialized.
    pub(crate) fn try_from_serialized(
        block: block::SerializedBlock,
    ) -> Result<(Self, block::Hash), SerializationError> {
        let mut cursor = std::io::Cursor::new(block.as_ref());
        let header = block::Header::zcash_deserialize(&mut cursor)?;
        let hash = block::Hash::from(&header);

        Ok((
            BlockAndHash {
                hash: hash.bytes_in_display_order().to_vec(),
                data: block.into_bytes(),
            },
            hash,
        ))
    }

    /// Try to convert a [`BlockAndHash`] into a tuple of a decoded block and hash.
    pub fn decode(self) -> Option<(block::Block, block::Hash)> {
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
            .and_then(|hash| {
                self.data
                    .zcash_deserialize_into()
                    .map_err(|err| tracing::warn!(?err, "failed to deserialize block",))
                    .ok()
                    .zip(Some(hash))
            })
    }
}
