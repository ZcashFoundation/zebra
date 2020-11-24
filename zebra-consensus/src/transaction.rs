use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

#[allow(unused_imports)]
use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
#[allow(unused_imports)]
use tower::{Service, ServiceExt};
use tracing::Instrument;

#[allow(unused_imports)]
use zebra_chain::{
    parameters::NetworkUpgrade,
    transaction::{self, HashType, Transaction},
    transparent,
};

use zebra_state as zs;

use crate::{error::TransactionError, script, BoxError};

mod check;

/// Asynchronous transaction verification.
#[derive(Debug, Clone)]
pub struct Verifier<ZS> {
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

/// Specifies whether a transaction should be verified as part of a block or as
/// part of the mempool.
///
/// Transaction verification has slightly different consensus rules, depending on
/// whether the transaction is to be included in a block on in the mempool.
#[allow(dead_code)]
pub enum Request {
    /// Verify the supplied transaction as part of a block.
    Block {
        transaction: Arc<Transaction>,
        /// Additional UTXOs which are known at the time of verification.
        known_utxos: Arc<HashMap<transparent::OutPoint, zs::Utxo>>,
    },
    /// Verify the supplied transaction as part of the mempool.
    Mempool {
        transaction: Arc<Transaction>,
        /// Additional UTXOs which are known at the time of verification.
        known_utxos: Arc<HashMap<transparent::OutPoint, zs::Utxo>>,
    },
}

impl<ZS> Service<Request> for Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    type Response = transaction::Hash;
    type Error = TransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    // TODO: break up each chunk into its own method
    fn call(&mut self, req: Request) -> Self::Future {
        let is_mempool = match req {
            Request::Block { .. } => false,
            Request::Mempool { .. } => true,
        };
        if is_mempool {
            // XXX determine exactly which rules apply to mempool transactions
            unimplemented!();
        }

        #[allow(unused_variables)]
        let (tx, known_utxos) = match req {
            Request::Block {
                transaction,
                known_utxos,
            } => (transaction, known_utxos),
            Request::Mempool {
                transaction,
                known_utxos,
            } => (transaction, known_utxos),
        };

        #[allow(unused_variables, unused_mut)]
        let mut redjubjub_verifier = crate::primitives::redjubjub::VERIFIER.clone();
        #[allow(unused_variables, unused_mut)]
        let mut script_verifier = self.script_verifier.clone();
        let span = tracing::debug_span!("tx", hash = %tx.hash());
        async move {
            tracing::trace!(?tx);
            match &*tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    tracing::debug!(?tx, "got transaction with wrong version");
                    Err(TransactionError::WrongVersion)
                }
                #[allow(unused_variables)]
                Transaction::V4 {
                    inputs,
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
                    /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
                    let mut async_checks = FuturesUnordered::new();
                    */

                    // Handle transparent inputs and outputs.
                    if tx.is_coinbase() {
                        // do something special for coinbase transactions
                        check::coinbase_tx_no_joinsplit_or_spend(&tx)?;
                    } else {
                        // otherwise, check no coinbase inputs
                        // feed all of the inputs to the script verifier
                        /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
                        for input_index in 0..inputs.len() {
                            let rsp = script_verifier.ready_and().await?.call(script::Request {
                                known_utxos: known_utxos.clone(),
                                transaction: tx.clone(),
                                input_index,
                            });

                            async_checks.push(rsp);
                        }
                        */
                    }

                    check::has_inputs_and_outputs(&tx)?;

                    /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
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
                    */

                    if let Some(shielded_data) = shielded_data {
                        check::shielded_balances_match(&shielded_data, *value_balance)?;
                        /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
                        for spend in shielded_data.spends() {
                            // TODO: check that spend.cv and spend.rk are NOT of small
                            // order.
                            // https://zips.z.cash/protocol/protocol.pdf#spenddesc

                            // Queue the validation of the RedJubjub spend
                            // authorization signature for each Spend
                            // description while adding the resulting future to
                            // our collection of async checks that (at a
                            // minimum) must pass for the transaction to verify.
                            /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
                            let rsp = redjubjub_verifier
                                .ready_and()
                                .await?
                                .call((spend.rk, spend.spend_auth_sig, &sighash).into());

                            async_checks.push(rsp.boxed());
                            */
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
                            // https://zips.z.cash/protocol/protocol.pdf#outputdesc

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
                        */
                    }

                    // Finally, wait for all asynchronous checks to complete
                    // successfully, or fail verification if they error.
                    /* #1367: temporarily disable this code, because the hard-coded Sapling value stops us validating post-Blossom blocks
                    while let Some(check) = async_checks.next().await {
                        tracing::trace!(?check, remaining = async_checks.len());
                        check?;
                    }
                    */

                    Ok(tx.hash())
                }
            }
        }
        .instrument(span)
        .boxed()
    }
}
