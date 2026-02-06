//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::Utc;
use futures::stream::FuturesUnordered;
use futures_util::FutureExt;
use thiserror::Error;
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    amount::Amount,
    block,
    parameters::{subsidy::SubsidyError, Network},
    transaction, transparent,
    work::equihash,
};
use zebra_state::{self as zs, CommitSemanticallyVerifiedBlockRequest, MappedRequest};

use crate::{error::*, transaction as tx, BoxError};

pub mod check;
pub mod request;
pub mod subsidy;

pub use request::Request;

#[cfg(test)]
mod tests;

/// Asynchronous semantic block verification.
#[derive(Debug)]
pub struct SemanticBlockVerifier<S, V> {
    /// The network to be verified.
    network: Network,
    state_service: S,
    transaction_verifier: V,
}

/// Block verification errors.
// TODO: dedupe with crate::error::BlockError
#[non_exhaustive]
#[allow(missing_docs)]
#[derive(Debug, Error, derive_new::new)]
pub enum VerifyBlockError {
    #[error("unable to verify depth for block {hash} from chain state during block verification")]
    Depth { source: BoxError, hash: block::Hash },

    #[error(transparent)]
    Block {
        #[from]
        source: BlockError,
    },

    #[error(transparent)]
    Equihash {
        #[from]
        source: equihash::Error,
    },

    #[error(transparent)]
    Time(zebra_chain::block::BlockTimeError),

    /// Error when attempting to commit a block after semantic verification.
    #[error("unable to commit block after semantic verification: {0}")]
    Commit(#[from] zs::CommitBlockError),

    #[error("unable to validate block proposal: failed semantic verification (proof of work is not checked for proposals): {0}")]
    // TODO: make this into a concrete type (see #5732)
    ValidateProposal(#[source] BoxError),

    #[error("invalid transaction: {0}")]
    Transaction(#[from] TransactionError),

    #[error("invalid block subsidy: {0}")]
    Subsidy(#[from] SubsidyError),

    /// Errors originating from the state service, which may arise from general failures in interacting with the state.
    /// This is for errors that are not specifically related to block depth or commit failures.
    #[error("state service error for block {hash}: {source}")]
    Tower { source: BoxError, hash: block::Hash },
}

impl VerifyBlockError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        match self {
            VerifyBlockError::Block { source, .. } => source.is_duplicate_request(),
            VerifyBlockError::Commit(commit_err) => commit_err.is_duplicate_request(),
            _ => false,
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        // TODO: Adjust these values based on zcashd (#9258).
        use VerifyBlockError::*;
        match self {
            Block { source } => source.misbehavior_score(),
            Equihash { .. } => 100,
            _other => 0,
        }
    }
}

/// The maximum allowed number of legacy signature check operations in a block.
///
/// This consensus rule is not documented, so Zebra follows the `zcashd` implementation.
/// We re-use some `zcashd` C++ script code via `zebra-script` and `zcash_script`.
///
/// See:
/// <https://github.com/zcash/zcash/blob/bad7f7eadbbb3466bebe3354266c7f69f607fcfd/src/consensus/consensus.h#L30>
pub const MAX_BLOCK_SIGOPS: u32 = 20_000;

impl<S, V> SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    V::Future: Send + 'static,
{
    /// Creates a new SemanticBlockVerifier
    pub fn new(network: &Network, state_service: S, transaction_verifier: V) -> Self {
        Self {
            network: network.clone(),
            state_service,
            transaction_verifier,
        }
    }
}

impl<S, V> Service<Request> for SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    V::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyBlockError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We use the state for contextual verification, and we expect those
        // queries to be fast. So we don't need to call
        // `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let mut state = self.state_service.clone();
        let mut transaction_verifier = self.transaction_verifier.clone();
        let network = self.network.clone();

        let block = request.block();

        // We don't include the block hash, because it's likely already in a parent span
        let span = tracing::debug_span!("block", height = ?block.coinbase_height());

        async move {
            let hash = block.hash();
            // Check that this block is actually a new block.
            tracing::trace!("checking that block is not already in state");
            match state
                .ready()
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
                .call(zs::Request::KnownBlock(hash))
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
            {
                zs::Response::KnownBlock(Some(location)) => {
                    return Err(BlockError::AlreadyInChain(hash, location).into())
                }
                zs::Response::KnownBlock(None) => {}
                _ => unreachable!("wrong response to Request::KnownBlock"),
            }

            tracing::trace!("performing block checks");
            let height = block
                .coinbase_height()
                .ok_or(BlockError::MissingHeight(hash))?;

            // Zebra does not support heights greater than
            // [`block::Height::MAX`].
            if height > block::Height::MAX {
                Err(BlockError::MaxHeight(height, hash, block::Height::MAX))?;
            }

            // > The block data MUST be validated and checked against the server's usual
            // > acceptance rules (excluding the check for a valid proof-of-work).
            // <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
            if request.is_proposal() || network.disable_pow() {
                check::difficulty_threshold_is_valid(&block.header, &network, &height, &hash)?;
            } else {
                // Do the difficulty checks first, to raise the threshold for
                // attacks that use any other fields.
                check::difficulty_is_valid(&block.header, &network, &height, &hash)?;
                check::equihash_solution_is_valid(&block.header)?;
            }

            // Next, check the Merkle root validity, to ensure that
            // the header binds to the transactions in the blocks.

            // Precomputing this avoids duplicating transaction hash computations.
            let transaction_hashes: Arc<[_]> =
                block.transactions.iter().map(|t| t.hash()).collect();

            check::merkle_root_validity(&network, &block, &transaction_hashes)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Quick field validity and structure checks
            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;
            let coinbase_tx = check::coinbase_is_first(&block)?;

            let expected_block_subsidy =
                zebra_chain::parameters::subsidy::block_subsidy(height, &network)?;

            // See [ZIP-1015](https://zips.z.cash/zip-1015).
            let deferred_pool_balance_change =
                check::subsidy_is_valid(&block, &network, expected_block_subsidy)?;

            // Now do the slower checks

            // Check compatibility with ZIP-212 shielded Sapling and Orchard coinbase output decryption
            tx::check::coinbase_outputs_are_decryptable(&coinbase_tx, &network, height)?;

            // Send transactions to the transaction verifier to be checked
            let mut async_checks = FuturesUnordered::new();

            let known_utxos = Arc::new(transparent::new_ordered_outputs(
                &block,
                &transaction_hashes,
            ));

            let known_outpoint_hashes: Arc<HashSet<transaction::Hash>> =
                Arc::new(known_utxos.keys().map(|outpoint| outpoint.hash).collect());

            for (&transaction_hash, transaction) in
                transaction_hashes.iter().zip(block.transactions.iter())
            {
                let rsp = transaction_verifier
                    .ready()
                    .await
                    .expect("transaction verifier is always ready")
                    .call(tx::Request::Block {
                        transaction_hash,
                        transaction: transaction.clone(),
                        known_outpoint_hashes: known_outpoint_hashes.clone(),
                        known_utxos: known_utxos.clone(),
                        height,
                        time: block.header.time,
                    });
                async_checks.push(rsp);
            }
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            // Get the transaction results back from the transaction verifier.

            // Sum up some block totals from the transaction responses.
            let mut sigops = 0;
            let mut block_miner_fees = Ok(Amount::zero());

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                let response = result
                    .map_err(Into::into)
                    .map_err(VerifyBlockError::Transaction)?;

                assert!(
                    matches!(response, tx::Response::Block { .. }),
                    "unexpected response from transaction verifier: {response:?}"
                );

                sigops += response.sigops();

                // Coinbase transactions consume the miner fee,
                // so they don't add any value to the block's total miner fee.
                if let Some(miner_fee) = response.miner_fee() {
                    block_miner_fees += miner_fee;
                }
            }

            // Check the summed block totals

            if sigops > MAX_BLOCK_SIGOPS {
                Err(BlockError::TooManyTransparentSignatureOperations {
                    height,
                    hash,
                    sigops,
                })?;
            }

            let block_miner_fees =
                block_miner_fees.map_err(|amount_error| BlockError::SummingMinerFees {
                    height,
                    hash,
                    source: amount_error,
                })?;

            check::miner_fees_are_valid(
                &coinbase_tx,
                height,
                block_miner_fees,
                expected_block_subsidy,
                deferred_pool_balance_change,
                &network,
            )?;

            // Finally, submit the block for contextual verification.
            let new_outputs = Arc::into_inner(known_utxos)
                .expect("all verification tasks using known_utxos are complete");

            let prepared_block = zs::SemanticallyVerifiedBlock {
                block,
                hash,
                height,
                new_outputs,
                transaction_hashes,
                deferred_pool_balance_change: Some(deferred_pool_balance_change),
            };

            // Return early for proposal requests.
            if request.is_proposal() {
                return match state
                    .ready()
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                    .call(zs::Request::CheckBlockProposalValidity(prepared_block))
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                {
                    zs::Response::ValidBlockProposal => Ok(hash),
                    _ => unreachable!("wrong response for CheckBlockProposalValidity"),
                };
            }

            CommitSemanticallyVerifiedBlockRequest::new(prepared_block)
                .map_err(&mut state, |e| VerifyBlockError::new_tower(e, hash))
                .await
                .inspect(|&res| assert_eq!(res, hash, "state must commit correct hash"))
        }
        .instrument(span)
        .boxed()
    }
}
