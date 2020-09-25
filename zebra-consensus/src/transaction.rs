//! Transaction verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting transactions from blocks or the mempool (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of transactions against the chain state
//!     (awaits an up-to-date chain)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.
//!
//! This is an internal module. Use `verify::BlockVerifier` for blocks and their
//! transactions, or `mempool::MempoolTransactionVerifier` for mempool transactions.

use displaydoc::Display;
use futures::{FutureExt, TryFutureExt};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use thiserror::Error;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    parameters::{Network, NetworkUpgrade::Sapling},
    primitives::{ed25519, redjubjub::Error as RedjubjubError},
    sapling,
    sighash::HashType,
    sprout,
    transaction::{self, HashType, JoinSplitData, ShieldedData, Transaction},
    transparent::{self, Script},
};

use zebra_state as zs;

use crate::{primitives::redjubjub, BoxError, Config};

/// Internal transaction verification service.
///
/// After verification, the transaction future completes. State changes are handled by
/// `BlockVerifier` or `MempoolTransactionVerifier`.
///
/// `TransactionVerifier` is not yet implemented.
#[derive(Default)]
pub(crate) struct TransactionVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    script: ScriptVerifier<S>,
    joinsplit: JoinSplitVerifier<S>,
    spend: SpendVerifier<S>,
    output: OutputVerifier<S>,
    redjubjub: redjubjub::Verifier<S>,
    ed25519: ed25519::batch::Verifier<S>,
}

#[non_exhaustive]
#[derive(Debug, Display, Error)]
pub enum VerifyTransactionError {
    ///
    Script(VerifyScriptError),
    ///
    JoinSplit(VerifyJoinSplitError),
    ///
    Ed25519(ed25519::Error),
    ///
    Spend(VerifySpendError),
    ///
    Output(VerifyOutputError),
    ///
    Redjubjub(RedjubjubError),
}

impl<S> Service<Arc<Transaction>> for TransactionVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = transaction::Hash;
    type Error = VerifyTransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // TODO: This lorge and must be busted up
        match (
            self.script.poll_ready(cx),
            self.joinsplit.poll_ready(cx),
            self.ed25519.poll_ready(cx),
            self.spend.poll_ready(cx),
            self.output.poll_ready(cx),
            self.redjubjub.poll_ready(cx),
        ) {
            (Poll::Ready(Err(e)), _) => Poll::Ready(Err(VerifyTransactionError::Checkpoint(e))),

            (_, Poll::Ready(Err(e))) => Poll::Ready(Err(VerifyTransactionError::Block(e))),

            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,

            (Poll::Ready(Ok(())), Poll::Ready(Ok(()))) => Poll::Ready(Ok(())),
        }
    }

    // TODO: break up each chunk into its own method
    fn call(&mut self, tx: Arc<Transaction>) -> Self::Future {
        let Transaction::V4 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            value_balance,
            joinsplit_data,
            shielded_data,
        } = tx;

        if !tx.is_coinbase() && (!inputs.isEmpty() || !outputs.isEmpty()) {
            let scripts = inputs
                .iter()
                .chain(outputs.iter())
                .filter_map(|input| match input {
                    transparent::Input::PrevOut { unlock_script, .. } => Some(unlock_script),
                    transparent::Output { lock_script, .. } => Some(lock_script),
                    _ => None,
                })
                .collect();

            scripts.iter().for_each(|script| {
                self.script
                    .call(script)
                    .map_err(VerifyTransactionError::VerifyScriptError)
                    .boxed()
            });
        }

        if let Some(joinsplit_d) = joinsplit_data {
            joinsplit_d.joinsplits().for_each(|joinsplit| {
                self.joinsplit
                    .call(joinsplit)
                    .map_err(VerifyTransactionError::VerifyJoinSplitError)
                    .boxed()
            });

            let JoinSplitData {
                first,
                rest,
                pub_key,
                sig,
            } = joinsplit_d;

            let msg = tx.data_to_be_signed();
            self.ed25519
                .call((pub_key.into(), sig.into(), msg.into()))
                .map_err(VerifyTransactionError::Ed25519)
                .boxed();
        }

        if let Some(shielded_data_d) = shielded_data {
            shielded_data_d.spends().for_each(|spend| {
                self.spend
                    .call(spend)
                    .map_err(VerifyTransactionError::VerifySpendError)
                    .boxed()
            });

            shielded_data_d.outputs().for_each(|output| {
                self.output
                    .call(output)
                    .map_err(VerifyTransactionError::VerifyOutputError)
                    .boxed()
            });

            let ShieldedData {
                first,
                rest_spends,
                rest_outputs,
                binding_sig,
            } = shielded_data_d;

            self.redjubjub
                .call((
                    pub_key.into(),
                    binding_sig.into(),
                    tx.sighash(
                        Network::Sapling, // TODO: pass this in
                        HashType::ALL,    // TODO: check these
                        None,             // TODO: check these
                    ),
                ))
                .map_err(VerifyTransactionError::Redjubjub)
                .boxed();
        }
    }
}
