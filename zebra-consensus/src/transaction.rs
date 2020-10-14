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
use futures::{stream::FuturesUnordered, FutureExt};
use thiserror::Error;
use tower::Service;
use tower::ServiceExt;

use zebra_chain::{
    parameters::NetworkUpgrade,
    primitives::{ed25519, redjubjub},
    transaction::{self, HashType, Transaction},
};

use zebra_state as zs;

use crate::{primitives::groth16, script, BoxError};

mod check;

/// Internal transaction verification service.
///
/// After verification, the transaction future completes. State changes are
/// handled by `BlockVerifier` or `MempoolTransactionVerifier`.
pub(crate) struct TransactionVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    script_verifier: script::Verifier<ZS>,
    spend_verifier: groth16::Verifier,
    output_verifier: groth16::Verifier,
    joinsplit_verifier: groth16::Verifier,
}

impl<ZS> TransactionVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    pub(crate) fn new(
        script_verifier: script::Verifier<ZS>,
        spend_verifier: groth16::Verifier,
        output_verifier: groth16::Verifier,
        joinsplit_verifier: groth16::Verifier,
    ) -> Self {
        Self {
            script_verifier,
            spend_verifier,
            output_verifier,
            joinsplit_verifier,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Display, Error)]
pub enum VerifyTransactionError {
    /// Only V4 and later transactions can be verified.
    WrongVersion,
    /// A transaction MUST move money around.
    NoTransfer,
    /// The balance of money moving around doesn't compute.
    BadBalance,
    /// Violation of coinbase rules.
    Coinbase,
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

impl<ZS> Service<Request> for TransactionVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    type Response = transaction::Hash;
    type Error = VerifyTransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    // TODO: break up each chunk into its own method
    fn call(&mut self, req: Request) -> Self::Future {
        let mut redjubjub_verifier = crate::primitives::redjubjub::VERIFIER.clone();

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

        async move {
            match &*tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    Err(VerifyTransactionError::WrongVersion)
                }
                Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    joinsplit_data,
                    shielded_data,
                } => {
                    // Contains a set of asynchronous checks, all of which must
                    // resolve to Ok(()) for verification to succeed (in addition to
                    // any other checks)
                    let async_checks = FuturesUnordered::new();

                    // Handle transparent inputs and outputs.
                    // These are left unimplemented!() pending implementation
                    // of the async script RFC.
                    if tx.is_coinbase() {
                        // do something special for coinbase transactions
                        check::coinbase_tx_does_not_spend_shielded(&tx)?;
                    } else {
                        // otherwise, check no coinbase inputs
                        // feed all of the inputs to the script verifier
                        unimplemented!();
                    }

                    check::some_money_is_spent(&tx)?;
                    check::any_coinbase_inputs_no_transparent_outputs(&tx)?;

                    let sighash = tx.sighash(
                        NetworkUpgrade::Sapling, // TODO: pass this in
                        HashType::ALL,           // TODO: check these
                        None,                    // TODO: check these
                    );

                    if let Some(joinsplit_data) = joinsplit_data {
                        // XXX create a method on JoinSplitData
                        // that prepares groth16::Items with the correct proofs
                        // and proof inputs, handling interstitial treestates
                        // correctly.

                        // Then, pass those items to self.joinsplit to verify them.

                        check::validate_joinsplit_sig(joinsplit_data, sighash.as_bytes())?;
                    }
                    if let Some(shielded_data) = shielded_data {
                        check::shielded_balances_match(&shielded_data, *value_balance)?;

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

                        shielded_data.outputs().for_each(|output| {
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
                        let bsk = check::balancing_value_balances(&shielded_data, *value_balance);

                        // Queue the validation of the RedJubjub binding
                        // signature with the batch verifier service while
                        // adding the resulting future to our collection of
                        // async checks that (at a minimum) must pass for the
                        // transaction to verify.
                        let rsp = redjubjub_verifier
                            .ready_and()
                            .await?
                            .call((bsk, shielded_data.binding_sig, &sighash).into())
                            .boxed();
                        async_checks.push(rsp)
                    }

                    unimplemented!()
                }
            }
        }
        .boxed()
    }
}
