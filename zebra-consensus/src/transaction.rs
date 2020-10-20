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
use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use thiserror::Error;
use tower::{Service, ServiceExt};

use zebra_chain::{
    parameters::NetworkUpgrade,
    primitives::{ed25519, redjubjub},
    transaction::{self, HashType, Transaction},
};

use zebra_state as zs;

use crate::{script, BoxError};

/// Internal transaction verification service.
///
/// After verification, the transaction future completes. State changes are
/// handled by `BlockVerifier` or `MempoolTransactionVerifier`.
pub struct Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    script_verifier: script::Verifier<ZS>,
    // spend_verifier: groth16::Verifier,
    // output_verifier: groth16::Verifier,
    // joinsplit_verifier: groth16::Verifier,
}

impl<ZS> Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    // XXX: how should this struct be constructed?
    pub fn new(script_verifier: script::Verifier<ZS>) -> Self {
        Self { script_verifier }
    }
}

#[non_exhaustive]
#[derive(Debug, Display, Error)]
pub enum VerifyTransactionError {
    /// Only V4 and later transactions can be verified.
    WrongVersion,
    /// Could not verify a transparent script
    Script(#[from] zebra_script::Error),
    /// Could not verify a Groth16 proof of a JoinSplit/Spend/Output description
    // XXX change this when we align groth16 verifier errors with bellman
    // and add a from annotation when the error type is more precise
    Groth16(BoxError),
    /// Could not verify a Ed25519 signature with JoinSplitData
    Ed25519(#[from] ed25519::Error),
    /// Could not verify a RedJubjub signature with ShieldedData
    RedJubjub(redjubjub::Error),
    /// An error that arises from implementation details of the verification service
    Internal(BoxError),
}

impl From<BoxError> for VerifyTransactionError {
    fn from(err: BoxError) -> Self {
        match err.downcast::<redjubjub::Error>() {
            Ok(e) => VerifyTransactionError::RedJubjub(*e),
            Err(e) => VerifyTransactionError::Internal(e),
        }
    }
}

/// Specifies whether a transaction should be verified as part of a block or as
/// part of the mempool.
///
/// Transaction verification has slightly different consensus rules, depending on
/// whether the transaction is to be included in a block on in the mempool.
pub enum Request {
    /// Verify the supplied transaction as part of a block.
    Block(Arc<Transaction>),
    /// Verify the supplied transaction as part of the mempool.
    Mempool(Arc<Transaction>),
}

impl<ZS> Service<Request> for Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    type Response = transaction::Hash;
    type Error = VerifyTransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    // TODO: break up each chunk into its own method
    fn call(&mut self, req: Request) -> Self::Future {
        let is_mempool = match req {
            Request::Block(_) => false,
            Request::Mempool(_) => true,
        };
        if is_mempool {
            // XXX determine exactly which rules apply to mempool transactions
            unimplemented!();
        }

        let tx = match req {
            Request::Block(tx) => tx,
            Request::Mempool(tx) => tx,
        };

        let mut redjubjub_verifier = crate::primitives::redjubjub::VERIFIER.clone();
        async move {
            match &*tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    Err(VerifyTransactionError::WrongVersion)
                }
                Transaction::V4 {
                    // inputs,
                    // outputs,
                    // lock_time,
                    // expiry_height,
                    value_balance,
                    joinsplit_data,
                    shielded_data,
                    ..
                } => {
                    // A set of asynchronous checks which must all succeed.
                    // We finish by waiting on these below.
                    let mut async_checks = FuturesUnordered::new();

                    // Handle transparent inputs and outputs.
                    // These are left unimplemented!() pending implementation
                    // of the async script RFC.
                    #[allow(clippy::if_same_then_else)] // delete when filled in
                    if tx.is_coinbase() {
                        // do something special for coinbase transactions
                    } else {
                        // otherwise, check no coinbase inputs
                        // feed all of the inputs to the script verifier
                    }

                    let sighash = tx.sighash(
                        NetworkUpgrade::Sapling, // TODO: pass this in
                        HashType::ALL,           // TODO: check these
                        None,                    // TODO: check these
                    );

                    if let Some(_joinsplit_data) = joinsplit_data {
                        // XXX create a method on JoinSplitData
                        // that prepares groth16::Items with the correct proofs
                        // and proof inputs, handling interstitial treestates
                        // correctly.

                        // Then, pass those items to self.joinsplit to verify them.

                        // XXX refactor this into a nicely named check function
                        // ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
                        //     .and_then(|vk| vk.verify(&joinsplit_data.sig, sighash))
                        //     .map_err(VerifyTransactionError::Ed25519)
                    }

                    if let Some(shielded_data) = shielded_data {
                        for spend in shielded_data.spends() {
                            // TODO: check that spend.cv and spend.rk are NOT of small
                            // order.
                            // https://zips.z.cash/protocol/canopy.pdf#spenddesc

                            // Queue the validation of the RedJubjub spend
                            // authorization signature for each Spend
                            // description while adding the resulting future to
                            // our collection of async checks that (at a
                            // minimum) must pass for the transaction to verify.
                            let rsp = redjubjub_verifier
                                .ready_and()
                                .await?
                                .call((spend.rk, spend.spend_auth_sig, &sighash).into());

                            async_checks.push(rsp.boxed());

                            // TODO: prepare public inputs for spends, then create
                            // a groth16::Item and pass to self.spend

                            // Queue the verification of the Groth16 spend proof
                            // for each Spend description while adding the
                            // resulting future to our collection of async
                            // checks that (at a minimum) must pass for the
                            // transaction to verify.
                        }

                        shielded_data.outputs().for_each(|_output| {
                            // TODO: check that output.cv and output.epk are NOT of small
                            // order.
                            // https://zips.z.cash/protocol/canopy.pdf#outputdesc

                            // TODO: prepare public inputs for outputs, then create
                            // a groth16::Item and pass to self.output

                            // Queue the verification of the Groth16 output
                            // proof for each Output description while adding
                            // the resulting future to our collection of async
                            // checks that (at a minimum) must pass for the
                            // transaction to verify.
                        });

                        let bvk = shielded_data.binding_verification_key(*value_balance);
                        let rsp = redjubjub_verifier
                            .ready_and()
                            .await?
                            .call((bvk, shielded_data.binding_sig, &sighash).into())
                            .boxed();
                        async_checks.push(rsp);
                    }

                    // Finally, wait for all asynchronous checks to complete
                    // successfully, or fail verification if they error.
                    while let Some(check) = async_checks.next().await {
                        check?;
                    }

                    Ok(tx.hash())
                }
            }
        }
        .boxed()
    }
}
