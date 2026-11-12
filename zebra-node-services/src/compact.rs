//! Reconstructed blocks shared by the network and consensus services.

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    serialization::{SerializationError, ZcashSerialize},
    transaction,
};

#[cfg(test)]
mod tests;

/// An immutable block with its serialized size and checked transaction hashes.
///
/// The constructor rejects blocks without transactions or above the block size limit, computes the
/// transaction hashes, rejects duplicates, and checks the hashes against this exact header's Merkle
/// root. It trusts that the serialized size is exact. This does not validate authorizing data or
/// network-specific consensus rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconstructedBlock {
    block: Arc<Block>,
    consumed_size: usize,
    transaction_hashes: Arc<[transaction::Hash]>,
}

impl ReconstructedBlock {
    /// Checks the block's transactions against its header, and keeps the hashes as evidence.
    ///
    /// `consumed_size` must be the exact serialized size of `block`. Verifiers use it instead of
    /// serializing the block for the block size limit, so sizes above that limit are rejected, but
    /// the size is only checked to be exact in debug builds.
    ///
    /// # Errors
    ///
    /// Returns an error if the block has no transactions, if `consumed_size` is above
    /// [`MAX_BLOCK_BYTES`], if the block has a duplicate transaction, or if its transaction hashes
    /// do not match the header's Merkle root.
    ///
    /// # Panics
    ///
    /// In debug builds, if `consumed_size` is not the serialized size of `block`.
    pub fn new(block: Arc<Block>, consumed_size: usize) -> Result<Self, SerializationError> {
        debug_assert_eq!(
            consumed_size,
            block.zcash_serialized_size(),
            "callers must supply the exact serialized size of the block"
        );

        if block.transactions.is_empty() {
            return Err(SerializationError::Parse(
                "reconstructed block has no transactions",
            ));
        }

        if u64::try_from(consumed_size).unwrap_or(u64::MAX) > MAX_BLOCK_BYTES {
            return Err(SerializationError::Parse(
                "reconstructed block exceeds the block size limit",
            ));
        }

        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();

        let mut seen = HashSet::with_capacity(transaction_hashes.len());
        if transaction_hashes.iter().any(|hash| !seen.insert(*hash)) {
            return Err(SerializationError::Parse(
                "reconstructed block has a duplicate transaction",
            ));
        }

        if transaction_hashes
            .iter()
            .copied()
            .collect::<block::merkle::Root>()
            != block.header.merkle_root
        {
            return Err(SerializationError::Parse(
                "reconstructed block transactions do not match the Merkle root",
            ));
        }

        Ok(Self {
            block,
            consumed_size,
            transaction_hashes,
        })
    }

    /// Returns the immutable block this evidence was checked against.
    pub fn block(&self) -> &Arc<Block> {
        &self.block
    }

    /// Returns the serialized size and Merkle-checked hashes only for the exact immutable block.
    ///
    /// A different allocation, even with identical bytes or a shared header, must be checked again.
    pub fn evidence_for(&self, block: &Arc<Block>) -> Option<(usize, &Arc<[transaction::Hash]>)> {
        Arc::ptr_eq(&self.block, block).then_some((self.consumed_size, &self.transaction_hashes))
    }

    /// Consumes the evidence, returning the block and its serialized size.
    pub fn into_parts(self) -> (Arc<Block>, usize) {
        (self.block, self.consumed_size)
    }
}
