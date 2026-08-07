//! Block verifier request type.

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{block::Block, transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation, then asks the state to perform contextual validation and commit the block
    Commit(Arc<Block>),
    /// Performs semantic validation but skips checking proof of work,
    /// then asks the state to perform contextual validation.
    /// Does not commit the block to the state.
    CheckProposal(Arc<Block>),
    /// Performs the same checks as [`Request::CheckProposal`], but skips the cryptographic
    /// verification (transparent scripts, shielded proofs, and signatures) of the
    /// transactions in `already_verified`.
    ///
    /// # Correctness
    ///
    /// This is only sound for a proposal that this node assembled from its own mempool, and
    /// only for transactions that this node itself cryptographically verified when it
    /// accepted them into that mempool. Cryptographic validity does not depend on where a
    /// transaction is verified, so re-checking it for a block would repeat work this node has
    /// already done. Every check that *does* depend on the verification context still runs,
    /// which is what makes this request able to detect a mempool transaction that block
    /// verification would reject. See
    /// <https://github.com/ZcashFoundation/zebra/issues/9301>.
    ///
    /// Listing a transaction that this node did not verify lets an invalid transaction pass
    /// as valid. In particular, do not use this request for blocks or transactions received
    /// from the network or submitted over RPC.
    CheckOwnProposal {
        /// The proposed block.
        block: Arc<Block>,
        /// The mined IDs of the transactions whose cryptographic verification can be skipped.
        already_verified: Arc<HashSet<transaction::Hash>>,
    },
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Commit(block) => block,
            Request::CheckProposal(block) => block,
            Request::CheckOwnProposal { block, .. } => block,
        })
    }

    /// Returns `true` if the request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(_) => false,
            Request::CheckProposal(_) | Request::CheckOwnProposal { .. } => true,
        }
    }

    /// Returns `true` if the cryptographic verification of the transaction identified by
    /// `transaction_hash` has already been performed by this node, and can be skipped.
    ///
    /// Always `false` unless this is a [`Request::CheckOwnProposal`]; see its documentation
    /// for why that variant is sound.
    pub fn crypto_already_verified(&self, transaction_hash: &transaction::Hash) -> bool {
        match self {
            Request::Commit(_) | Request::CheckProposal(_) => false,
            Request::CheckOwnProposal {
                already_verified, ..
            } => already_verified.contains(transaction_hash),
        }
    }
}
