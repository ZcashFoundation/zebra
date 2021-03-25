use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    block,
    parameters::{Network, NetworkUpgrade},
    transaction::{self, HashType, Transaction},
    transparent,
};

use zebra_script::CachedFfiTransaction;
use zebra_state as zs;

use crate::{error::TransactionError, primitives, script, BoxError};

mod check;

/// Asynchronous transaction verification.
#[derive(Debug, Clone)]
pub struct Verifier<ZS> {
    network: Network,
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
    pub fn new(network: Network, script_verifier: script::Verifier<ZS>) -> Self {
        // let (spend_verifier, output_verifier, joinsplit_verifier) = todo!();

        Self {
            network,
            script_verifier,
            // spend_verifier,
            // output_verifier,
            // joinsplit_verifier,
        }
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
        /// The transaction itself.
        transaction: Arc<Transaction>,
        /// Additional UTXOs which are known at the time of verification.
        known_utxos: Arc<HashMap<transparent::OutPoint, zs::Utxo>>,
        /// The height of the block containing this transaction, used to
        /// determine the applicable network upgrade.
        height: block::Height,
    },
    /// Verify the supplied transaction as part of the mempool.
    Mempool {
        /// The transaction itself.
        transaction: Arc<Transaction>,
        /// Additional UTXOs which are known at the time of verification.
        known_utxos: Arc<HashMap<transparent::OutPoint, zs::Utxo>>,
        /// Bug: this field should be the next block height, because some
        /// consensus rules depend on the exact height. See #1683.
        upgrade: NetworkUpgrade,
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

        let (tx, known_utxos, upgrade) = match req {
            Request::Block {
                transaction,
                known_utxos,
                height,
            } => {
                let upgrade = NetworkUpgrade::current(self.network, height);
                (transaction, known_utxos, upgrade)
            }
            Request::Mempool {
                transaction,
                known_utxos,
                upgrade,
            } => (transaction, known_utxos, upgrade),
        };

        let mut spend_verifier = primitives::groth16::SPEND_VERIFIER.clone();
        let mut output_verifier = primitives::groth16::OUTPUT_VERIFIER.clone();

        let mut redjubjub_verifier = primitives::redjubjub::VERIFIER.clone();
        let mut script_verifier = self.script_verifier.clone();

        let span = tracing::debug_span!("tx", hash = %tx.hash());

        async move {
            tracing::trace!(?tx);
            match &*tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    tracing::debug!(?tx, "got transaction with wrong version");
                    Err(TransactionError::WrongVersion)
                }
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
                    let mut async_checks = FuturesUnordered::new();

                    // Do basic checks first
                    check::has_inputs_and_outputs(&tx)?;

                    // Handle transparent inputs and outputs.
                    if tx.is_coinbase() {
                        check::coinbase_tx_no_joinsplit_or_spend(&tx)?;
                    } else {
                        // feed all of the inputs to the script and shielded verifiers
                        let cached_ffi_transaction =
                            Arc::new(CachedFfiTransaction::new(tx.clone()));

                        for input_index in 0..inputs.len() {
                            let rsp = script_verifier.ready_and().await?.call(script::Request {
                                upgrade,
                                known_utxos: known_utxos.clone(),
                                cached_ffi_transaction: cached_ffi_transaction.clone(),
                                input_index,
                            });

                            async_checks.push(rsp);
                        }
                    }

                    let shielded_sighash = tx.sighash(
                        upgrade,
                        HashType::ALL,
                        None,
                    );

                    if let Some(joinsplit_data) = joinsplit_data {
                        // XXX create a method on JoinSplitData
                        // that prepares groth16::Items with the correct proofs
                        // and proof inputs, handling interstitial treestates
                        // correctly.

                        // Then, pass those items to self.joinsplit to verify them.
                        check::validate_joinsplit_sig(joinsplit_data, shielded_sighash.as_bytes())?;
                    }

                    if let Some(shielded_data) = shielded_data {
                        check::shielded_balances_match(&shielded_data, *value_balance)?;

                        for spend in shielded_data.spends() {
                            // Consensus rule: cv and rk MUST NOT be of small
                            // order, i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]rk
                            // MUST NOT be 𝒪_J.
                            //
                            // https://zips.z.cash/protocol/protocol.pdf#spenddesc
                            check::spend_cv_rk_not_small_order(spend)?;

                            // Consensus rule: The proof π_ZKSpend MUST be valid
                            // given a primary input formed from the other
                            // fields except spendAuthSig.
                            //
                            // Queue the verification of the Groth16 spend proof
                            // for each Spend description while adding the
                            // resulting future to our collection of async
                            // checks that (at a minimum) must pass for the
                            // transaction to verify.
                            let spend_rsp = spend_verifier
                                .ready_and()
                                .await?
                                .call(primitives::groth16::ItemWrapper::from(spend).into());

                            async_checks.push(spend_rsp.boxed());

                            // Consensus rule: The spend authorization signature
                            // MUST be a valid SpendAuthSig signature over
                            // SigHash using rk as the validating key.
                            //
                            // Queue the validation of the RedJubjub spend
                            // authorization signature for each Spend
                            // description while adding the resulting future to
                            // our collection of async checks that (at a
                            // minimum) must pass for the transaction to verify.
                            let rsp = redjubjub_verifier
                                .ready_and()
                                .await?
                                .call((spend.rk, spend.spend_auth_sig, &shielded_sighash).into());

                            async_checks.push(rsp.boxed());
                        }

                        for output in shielded_data.outputs() {
                            // Consensus rule: cv and wpk MUST NOT be of small
                            // order, i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]wpk
                            // MUST NOT be 𝒪_J.
                            //
                            // https://zips.z.cash/protocol/protocol.pdf#outputdesc
                            check::output_cv_epk_not_small_order(output)?;

                            // Consensus rule: The proof π_ZKOutput MUST be
                            // valid given a primary input formed from the other
                            // fields except C^enc and C^out.
                            //
                            // Queue the verification of the Groth16 output
                            // proof for each Output description while adding
                            // the resulting future to our collection of async
                            // checks that (at a minimum) must pass for the
                            // transaction to verify.
                            let output_rsp = output_verifier
                                .ready_and()
                                .await?
                                .call(primitives::groth16::ItemWrapper::from(output).into());

                            async_checks.push(output_rsp.boxed());
                        }

                        let bvk = shielded_data.binding_verification_key(*value_balance);

                        // TODO: enable async verification and remove this block - #1939
                        {
                            let item: zebra_chain::primitives::redjubjub::batch::Item = (bvk, shielded_data.binding_sig, &shielded_sighash).into();
                            item.verify_single().unwrap_or_else(|binding_sig_error| {
                                let binding_sig_error = binding_sig_error.to_string();
                                tracing::warn!(%binding_sig_error, "ignoring");
                                metrics::counter!("zebra.error.sapling.binding",
                                                  1,
                                                  "kind" => binding_sig_error);
                            });
                            // Ignore errors until binding signatures are fixed
                            //.map_err(|e| BoxError::from(Box::new(e)))?;
                        }

                        let _rsp = redjubjub_verifier
                            .ready_and()
                            .await?
                            .call((bvk, shielded_data.binding_sig, &shielded_sighash).into())
                            .boxed();

                        // TODO: stop ignoring binding signature errors - #1939
                        // async_checks.push(rsp);
                    }

                    // Finally, wait for all asynchronous checks to complete
                    // successfully, or fail verification if they error.
                    while let Some(check) = async_checks.next().await {
                        tracing::trace!(?check, remaining = async_checks.len());
                        check?;
                    }

                    Ok(tx.hash())
                }
                Transaction::V5 { .. } => {
                    unimplemented!("v5 transaction validation as specified in ZIP-216, ZIP-224, ZIP-225, and ZIP-244")
                }
            }
        }
        .instrument(span)
        .boxed()
    }
}
