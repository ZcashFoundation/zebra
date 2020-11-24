//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
    collections::HashMap,
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
    block::{self, Block},
    parameters::Network,
    parameters::NetworkUpgrade,
    transparent,
    work::equihash,
};
use zebra_state as zs;

use crate::{error::*, transaction};
use crate::{script, BoxError};

mod check;
mod subsidy;
#[cfg(test)]
mod tests;

/// Asynchronous block verification.
#[derive(Debug)]
pub struct BlockVerifier<S> {
    /// The network to be verified.
    network: Network,
    state_service: S,
    transaction_verifier: transaction::Verifier<S>,
}

// TODO: dedupe with crate::error::BlockError
#[non_exhaustive]
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
    Transaction(#[source] TransactionError),
}

impl<S> BlockVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    pub fn new(network: Network, state_service: S) -> Self {
        let branch = NetworkUpgrade::Sapling.branch_id().unwrap();
        let script_verifier = script::Verifier::new(state_service.clone(), branch);
        let transaction_verifier = transaction::Verifier::new(script_verifier);

        Self {
            network,
            state_service,
            transaction_verifier,
        }
    }
}

impl<S> Service<Arc<Block>> for BlockVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
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

        // TODO(jlusby): Error = Report, handle errors from state_service.
        async move {
            let hash = block.hash();
            // Check that this block is actually a new block.
            tracing::trace!("checking that block is not already in state");
            match state_service
                .ready_and()
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
            if height > block::Height::MAX {
                Err(BlockError::MaxHeight(height, hash, block::Height::MAX))?;
            }

            // Do the difficulty checks first, to raise the threshold for
            // attacks that use any other fields.
            check::difficulty_is_valid(&block.header, network, &height, &hash)?;
            check::equihash_solution_is_valid(&block.header)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Field validity and structure checks
            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;
            check::coinbase_is_first(&block)?;
            check::subsidy_is_valid(&block, network)?;

            // TODO: context-free header verification: merkle root

            let mut async_checks = FuturesUnordered::new();

            let known_utxos = new_outputs(&block);
            for transaction in &block.transactions {
                let rsp = transaction_verifier
                    .ready_and()
                    .await
                    .expect("transaction verifier is always ready")
                    .call(transaction::Request::Block {
                        transaction: transaction.clone(),
                        known_utxos: known_utxos.clone(),
                    });
                async_checks.push(rsp);
            }
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                result.map_err(VerifyBlockError::Transaction)?;
            }

            // Update the metrics after all the validation is finished
            tracing::trace!("verified block");
            metrics::gauge!("block.verified.block.height", height.0 as _);
            metrics::counter!("block.verified.block.count", 1);

            // Finally, submit the block for contextual verification.
            let new_outputs = Arc::try_unwrap(known_utxos)
                .expect("all verification tasks using known_utxos are complete");
            let prepared_block = zs::PreparedBlock {
                block,
                hash,
                height,
                new_outputs,
            };
            match state_service
                .ready_and()
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

fn new_outputs(block: &Block) -> Arc<HashMap<transparent::OutPoint, zs::Utxo>> {
    let mut new_outputs = HashMap::default();
    let height = block.coinbase_height().expect("block has coinbase height");
    for transaction in &block.transactions {
        let hash = transaction.hash();
        let from_coinbase = transaction.is_coinbase();
        for (index, output) in transaction.outputs().iter().cloned().enumerate() {
            let index = index as u32;
            new_outputs.insert(
                transparent::OutPoint { hash, index },
                zs::Utxo {
                    output,
                    height,
                    from_coinbase,
                },
            );
        }
    }

    Arc::new(new_outputs)
}
