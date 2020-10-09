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

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use displaydoc::Display;
use futures::{FutureExt, TryFutureExt};
use thiserror::Error;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    parameters::{Network, NetworkUpgrade::Sapling},
    primitives::{ed25519, groth16, redjubjub},
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
/// After verification, the transaction future completes. State changes are
/// handled by `BlockVerifier` or `MempoolTransactionVerifier`.
#[derive(Default)]
pub(crate) struct TransactionVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    script: ScriptVerifier<S>,
    groth16: groth16::Verifier<S>,
}

#[non_exhaustive]
#[derive(Debug, Display, Error)]
pub enum VerifyTransactionError {
    /// Could not verify a transparent script
    Script(VerifyScriptError),
    /// Could not verify a Groth16 proof of a JoinSplit/Spend/Output description
    Groth16(VerifyGroth16Error),
    /// Could not verify a Ed25519 signature with JoinSplitData
    Ed25519(ed25519::Error),
    /// Could not verify a RedJubjub signature with ShieldedData
    RedJubjub(redjubjub::Error),
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
        match (self.script.poll_ready(cx), self.groth16.poll_ready(cx)) {
            // First, fail if either service fails.
            (Poll::Ready(Err(e)), _) => Poll::Ready(Err(VerifyTransactionError::Script(e))),
            (_, Poll::Ready(Err(e))) => Poll::Ready(Err(VerifyTransactionError::Groth16(e))),
            // Second, we're unready if either service is unready.
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            // Finally, we're ready if both services are ready and OK.
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

            ed25519::VerificationKey::try_From(pub_key)
                .and_then(|vk| vk.verify(sig, tx.data_to_be_signed()))
                .map_err(VerifyTransactionError::Ed25519)
        }

        if let Some(shielded_data_d) = shielded_data {
            let sighash = tx.sighash(
                Network::Sapling, // TODO: pass this in
                HashType::ALL,    // TODO: check these
                None,             // TODO: check these
            );

            shielded_data_d.spends().for_each(|spend| {
                // TODO: check that spend.cv and spend.rk are NOT of small
                // order.
                // https://zips.z.cash/protocol/canopy.pdf#spenddesc

                // Verify the spend authorization signature for each Spend
                // description.
                spend
                    .rk
                    .verify(sighash, spend.spend_auth_sig)
                    .map_err(VerifyTransactionError::Redjubjub);

                self.groth16
                    .call(spend.into())
                    .map_err(VerifyTransactionError::VerifyGroth16Error)
                    .boxed()
            });

            shielded_data_d.outputs().for_each(|output| {
                // TODO: check that output.cv and output.epk are NOT of small
                // order.
                // https://zips.z.cash/protocol/canopy.pdf#outputdesc

                self.groth16
                    .call(output.into())
                    .map_err(VerifyTransactionError::VerifyGroth16Error)
                    .boxed()
            });

            let ShieldedData {
                first,
                rest_spends,
                rest_outputs,
                binding_sig,
            } = shielded_data_d;

            // Checks the balance.
            //
            // The net value of Spend transfers minus Output transfers in a
            // transaction is called the balancing value, measured in zatoshi as
            // a signed integer v_balance.
            //
            // Consistency of v_balance with the value commitments in Spend
            // descriptions and Output descriptions is enforced by the binding
            // signature.
            //
            // Instead of generating a key pair at random, we generate it as a
            // function of the value commitments in the Spend descriptions and
            // Output descriptions of the transaction, and the balancing value.
            //
            // https://zips.z.cash/protocol/canopy.pdf#saplingbalance
            let bsk = shielded_data_d.binding_validating_key(value_balance);
            bsk.verify(sighash, &binding_sig)
                .map_err(VerifyTransactionError::Redjubjub);
        }
    }
}
