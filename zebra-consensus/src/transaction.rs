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
    convert::TryFrom,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use displaydoc::Display;
use futures::{stream::FuturesUnordered, FutureExt, TryFutureExt};
use thiserror::Error;
use tower::{Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    parameters::{Network, NetworkUpgrade::Sapling},
    primitives::{ed25519, redjubjub},
    transaction::{self, HashType, JoinSplitData, ShieldedData, Transaction},
    transparent::{self, Script},
};

use zebra_script;
use zebra_state as zs;

use crate::{primitives::groth16, script, BoxError, Config};

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
    RedJubjub(#[from] redjubjub::Error),
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

        match *tx {
            Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                async { Err(VerifyTransactionError::WrongVersion) }.boxed()
            }
            Transaction::V4 {
                inputs,
                outputs,
                lock_time,
                expiry_height,
                value_balance,
                joinsplit_data,
                shielded_data,
            } => async move {
                // Handle transparent inputs and outputs.
                // These are left unimplemented!() pending implementation
                // of the async script RFC.
                if tx.is_coinbase() {
                    // do something special for coinbase transactions
                    unimplemented!();
                } else {
                    // otherwise, check no coinbase inputs
                    // feed all of the inputs to the script verifier
                    unimplemented!();
                }

                // Contains a set of asynchronous checks, all of which must
                // resolve to Ok(()) for verification to succeed (in addition to
                // any other checks)
                let _async_checks = FuturesUnordered::<
                    Box<dyn Future<Output = Result<(), VerifyTransactionError>>>,
                >::new();

                if let Some(joinsplit_data) = joinsplit_data {
                    // XXX create a method on JoinSplitData
                    // that prepares groth16::Items with the correct proofs
                    // and proof inputs, handling interstitial treestates
                    // correctly.

                    // Then, pass those items to self.joinsplit to verify them.

                    // XXX refactor this into a nicely named check function
                    ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
                        .and_then(|vk| vk.verify(&joinsplit_data.sig, tx.data_to_be_signed()))
                        .map_err(VerifyTransactionError::Ed25519)
                }

                if let Some(shielded_data) = shielded_data {
                    let sighash = tx.sighash(
                        Network::Sapling, // TODO: pass this in
                        HashType::ALL,    // TODO: check these
                        None,             // TODO: check these
                    );

                    shielded_data.spends().for_each(|spend| {
                        // TODO: check that spend.cv and spend.rk are NOT of small
                        // order.
                        // https://zips.z.cash/protocol/canopy.pdf#spenddesc

                        // Verify the spend authorization signature for each Spend
                        // description.
                        spend
                            .rk
                            .verify(sighash.into(), spend.spend_auth_sig)
                            .map_err(VerifyTransactionError::RedJubjub);

                        // TODO: prepare public inputs for spends, then create
                        // a groth16::Item and pass to self.spend
                        unimplemented!()
                    });

                    shielded_data.outputs().for_each(|output| {
                        // TODO: check that output.cv and output.epk are NOT of small
                        // order.
                        // https://zips.z.cash/protocol/canopy.pdf#outputdesc

                        // TODO: prepare public inputs for outputs, then create
                        // a groth16::Item and pass to self.output
                        unimplemented!()
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
                    let bsk = shielded_data.binding_validating_key(value_balance);
                    bsk.verify(sighash, &shielded_data.binding_sig)
                        .map_err(VerifyTransactionError::RedJubjub);
                }

                unimplemented!()
            }
            .boxed(),
        }
    }
}
