//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
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
    block::{self, Block},
    parameters::Network,
    transparent,
    work::equihash,
};
use zebra_state as zs;

use crate::{error::*, transaction as tx, BoxError};

pub mod check;
mod subsidy;

#[cfg(test)]
mod tests;

/// Asynchronous block verification.
#[derive(Debug)]
pub struct BlockVerifier<S, V> {
    /// The network to be verified.
    network: Network,
    state_service: S,
    transaction_verifier: V,
}

// TODO: dedupe with crate::error::BlockError
#[non_exhaustive]
#[allow(missing_docs)]
#[derive(Debug, Error)]
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

    #[error("unable to commit block after semantic verification")]
    Commit(#[source] BoxError),

    #[error("invalid transaction")]
    Transaction(#[from] TransactionError),
}

/// The maximum allowed number of legacy signature check operations in a block.
///
/// This consensus rule is not documented, so Zebra follows the `zcashd` implementation.
/// We re-use some `zcashd` C++ script code via `zebra-script` and `zcash_script`.
///
/// See:
/// <https://github.com/zcash/zcash/blob/bad7f7eadbbb3466bebe3354266c7f69f607fcfd/src/consensus/consensus.h#L30>
pub const MAX_BLOCK_SIGOPS: u64 = 20_000;

impl<S, V> BlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    V::Future: Send + 'static,
{
    pub fn new(network: Network, state_service: S, transaction_verifier: V) -> Self {
        Self {
            network,
            state_service,
            transaction_verifier,
        }
    }
}

impl<S, V> Service<Arc<Block>> for BlockVerifier<S, V>
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

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let mut state_service = self.state_service.clone();
        let mut transaction_verifier = self.transaction_verifier.clone();
        let network = self.network;

        // We don't include the block hash, because it's likely already in a parent span
        let span = tracing::debug_span!("block", height = ?block.coinbase_height());

        async move {
            let hash = block.hash();
            // Check that this block is actually a new block.
            tracing::trace!("checking that block is not already in state");
            match state_service
                .ready()
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
                .call(zs::Request::Depth(hash))
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
            {
                zs::Response::Depth(Some(depth)) => {
                    return Err(BlockError::AlreadyInChain(hash, depth).into())
                }
                zs::Response::Depth(None) => {}
                _ => unreachable!("wrong response to Request::Depth"),
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

            // Do the difficulty checks first, to raise the threshold for
            // attacks that use any other fields.
            check::difficulty_is_valid(&block.header, network, &height, &hash)?;
            check::equihash_solution_is_valid(&block.header)?;

            // Next, check the Merkle root validity, to ensure that
            // the header binds to the transactions in the blocks.

            // Precomputing this avoids duplicating transaction hash computations.
            let transaction_hashes: Arc<[_]> =
                block.transactions.iter().map(|t| t.hash()).collect();

            check::merkle_root_validity(network, &block, &transaction_hashes)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Quick field validity and structure checks
            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;
            let coinbase_tx = check::coinbase_is_first(&block)?;
            check::subsidy_is_valid(&block, network)?;

            // Now do the slower checks

            // Check compatibility with ZIP-212 shielded Sapling and Orchard coinbase output decryption
            tx::check::coinbase_outputs_are_decryptable(&coinbase_tx, network, height)?;

            // Send transactions to the transaction verifier to be checked
            let mut async_checks = FuturesUnordered::new();

            let known_utxos = Arc::new(transparent::new_ordered_outputs(
                &block,
                &transaction_hashes,
            ));
            for transaction in &block.transactions {
                let rsp = transaction_verifier
                    .ready()
                    .await
                    .expect("transaction verifier is always ready")
                    .call(tx::Request::Block {
                        transaction: transaction.clone(),
                        known_utxos: known_utxos.clone(),
                        height,
                        time: block.header.time,
                    });
                async_checks.push(rsp);
            }
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            // Get the transaction results back from the transaction verifier.

            // Sum up some block totals from the transaction responses.
            let mut legacy_sigop_count = 0;
            let mut block_miner_fees = Ok(Amount::zero());

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                let response = result
                    .map_err(Into::into)
                    .map_err(VerifyBlockError::Transaction)?;

                assert!(
                    matches!(response, tx::Response::Block { .. }),
                    "unexpected response from transaction verifier: {:?}",
                    response
                );

                legacy_sigop_count += response
                    .legacy_sigop_count()
                    .expect("block transaction responses must have a legacy sigop count");

                // Coinbase transactions consume the miner fee,
                // so they don't add any value to the block's total miner fee.
                if let Some(miner_fee) = response.miner_fee() {
                    block_miner_fees += miner_fee;
                }
            }

            // Check the summed block totals

            if legacy_sigop_count > MAX_BLOCK_SIGOPS {
                Err(BlockError::TooManyTransparentSignatureOperations {
                    height,
                    hash,
                    legacy_sigop_count,
                })?;
            }

            let block_miner_fees =
                block_miner_fees.map_err(|amount_error| BlockError::SummingMinerFees {
                    height,
                    hash,
                    source: amount_error,
                })?;
            check::miner_fees_are_valid(&block, network, block_miner_fees)?;

            // Finally, submit the block for contextual verification.
            let new_outputs = Arc::try_unwrap(known_utxos)
                .expect("all verification tasks using known_utxos are complete");

            let prepared_block = zs::PreparedBlock {
                block,
                hash,
                height,
                new_outputs,
                transaction_hashes,
            };
            match state_service
                .ready()
                .await
                .map_err(VerifyBlockError::Commit)?
                .call(zs::Request::CommitBlock(prepared_block))
                .await
                .map_err(VerifyBlockError::Commit)?
            {
                zs::Response::Committed(committed_hash) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    Ok(hash)
                }
                _ => unreachable!("wrong response for CommitBlock"),
            }
        }
        .instrument(span)
        .boxed()
    }
}
