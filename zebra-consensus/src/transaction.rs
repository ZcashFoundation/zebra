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
    primitives::Groth16Proof,
    sapling,
    transaction::{self, HashType, Transaction},
    transparent,
};

use zebra_script::CachedFfiTransaction;
use zebra_state as zs;

use crate::{error::TransactionError, primitives, script, BoxError};

mod check;
#[cfg(test)]
mod tests;

/// An alias for a set of asynchronous checks that should succeed.
type AsyncChecks = FuturesUnordered<Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>>;

/// Asynchronous transaction verification.
///
/// # Correctness
///
/// Transaction verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`chain`](`crate::chain`)
/// module documentation for details.
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

impl Request {
    /// The transaction to verify that's in this request.
    pub fn transaction(&self) -> Arc<Transaction> {
        match self {
            Request::Block { transaction, .. } => transaction.clone(),
            Request::Mempool { transaction, .. } => transaction.clone(),
        }
    }

    /// The set of additional known unspent transaction outputs that's in this request.
    pub fn known_utxos(&self) -> Arc<HashMap<transparent::OutPoint, zs::Utxo>> {
        match self {
            Request::Block { known_utxos, .. } => known_utxos.clone(),
            Request::Mempool { known_utxos, .. } => known_utxos.clone(),
        }
    }

    /// The network upgrade to consider for the verification.
    ///
    /// This is based on the block height from the request, and the supplied `network`.
    pub fn upgrade(&self, network: Network) -> NetworkUpgrade {
        match self {
            Request::Block { height, .. } => NetworkUpgrade::current(network, *height),
            Request::Mempool { upgrade, .. } => *upgrade,
        }
    }
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

        let script_verifier = self.script_verifier.clone();
        let network = self.network;

        let tx = req.transaction();
        let span = tracing::debug_span!("tx", hash = %tx.hash());

        async move {
            tracing::trace!(?tx);

            // Do basic checks first
            check::has_inputs_and_outputs(&tx)?;

            let async_checks = match tx.as_ref() {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    tracing::debug!(?tx, "got transaction with wrong version");
                    return Err(TransactionError::WrongVersion);
                }
                Transaction::V4 {
                    inputs,
                    // outputs,
                    // lock_time,
                    // expiry_height,
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => {
                    Self::verify_v4_transaction(
                        req,
                        network,
                        script_verifier,
                        inputs,
                        joinsplit_data,
                        sapling_shielded_data,
                    )
                    .await?
                }
                Transaction::V5 { inputs, .. } => {
                    Self::verify_v5_transaction(req, network, script_verifier, inputs).await?
                }
            };

            Self::wait_for_checks(async_checks).await?;

            Ok(tx.hash())
        }
        .instrument(span)
        .boxed()
    }
}

impl<ZS> Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Verify a V4 transaction.
    ///
    /// Returns a set of asynchronous checks that must all succeed for the transaction to be
    /// considered valid. These checks include:
    ///
    /// - transparent transfers
    /// - sprout shielded data
    /// - sapling shielded data
    ///
    /// The parameters of this method are:
    ///
    /// - the `request` to verify (that contains the transaction and other metadata, see [`Request`]
    ///   for more information)
    /// - the `network` to consider when verifying
    /// - the `script_verifier` to use for verifying the transparent transfers
    /// - the transparent `inputs` in the transaction
    /// - the Sprout `joinsplit_data` shielded data in the transaction
    /// - the `sapling_shielded_data` in the transaction
    async fn verify_v4_transaction(
        request: Request,
        network: Network,
        script_verifier: script::Verifier<ZS>,
        inputs: &[transparent::Input],
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        sapling_shielded_data: &Option<sapling::ShieldedData<sapling::PerSpendAnchor>>,
    ) -> Result<AsyncChecks, TransactionError> {
        let mut spend_verifier = primitives::groth16::SPEND_VERIFIER.clone();
        let mut output_verifier = primitives::groth16::OUTPUT_VERIFIER.clone();

        let mut redjubjub_verifier = primitives::redjubjub::VERIFIER.clone();

        // A set of asynchronous checks which must all succeed.
        // We finish by waiting on these below.
        let mut async_checks = AsyncChecks::new();

        let tx = request.transaction();
        let upgrade = request.upgrade(network);

        // Add asynchronous checks of the transparent inputs and outputs
        async_checks.extend(Self::verify_transparent_inputs_and_outputs(
            &request,
            network,
            inputs,
            script_verifier,
        )?);

        let shielded_sighash = tx.sighash(upgrade, HashType::ALL, None);

        async_checks.extend(Self::verify_sprout_shielded_data(
            joinsplit_data,
            &shielded_sighash,
        ));

        if let Some(sapling_shielded_data) = sapling_shielded_data {
            for spend in sapling_shielded_data.spends_per_anchor() {
                // Consensus rule: cv and rk MUST NOT be of small
                // order, i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]rk
                // MUST NOT be 𝒪_J.
                //
                // https://zips.z.cash/protocol/protocol.pdf#spenddesc
                check::spend_cv_rk_not_small_order(&spend)?;

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
                    .call(primitives::groth16::ItemWrapper::from(&spend).into());

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

            for output in sapling_shielded_data.outputs() {
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

            let bvk = sapling_shielded_data.binding_verification_key();

            // TODO: enable async verification and remove this block - #1939
            {
                let item: zebra_chain::primitives::redjubjub::batch::Item =
                    (bvk, sapling_shielded_data.binding_sig, &shielded_sighash).into();
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
                .call((bvk, sapling_shielded_data.binding_sig, &shielded_sighash).into())
                .boxed();

            // TODO: stop ignoring binding signature errors - #1939
            // async_checks.push(rsp);
        }

        Ok(async_checks)
    }

    /// Verify a V5 transaction.
    ///
    /// Returns a set of asynchronous checks that must all succeed for the transaction to be
    /// considered valid. These checks include:
    ///
    /// - transaction support by the considered network upgrade (see [`Request::upgrade`])
    /// - transparent transfers
    /// - sapling shielded data (TODO)
    /// - orchard shielded data (TODO)
    ///
    /// The parameters of this method are:
    ///
    /// - the `request` to verify (that contains the transaction and other metadata, see [`Request`]
    ///   for more information)
    /// - the `network` to consider when verifying
    /// - the `script_verifier` to use for verifying the transparent transfers
    /// - the transparent `inputs` in the transaction
    async fn verify_v5_transaction(
        request: Request,
        network: Network,
        script_verifier: script::Verifier<ZS>,
        inputs: &[transparent::Input],
    ) -> Result<AsyncChecks, TransactionError> {
        Self::verify_v5_transaction_network_upgrade(
            &request.transaction(),
            request.upgrade(network),
        )?;

        let _async_checks = Self::verify_transparent_inputs_and_outputs(
            &request,
            network,
            inputs,
            script_verifier,
        )?;

        // TODO:
        // - verify sapling shielded pool (#1981)
        // - verify orchard shielded pool (ZIP-224) (#2105)
        // - ZIP-216 (#1798)
        // - ZIP-244 (#1874)

        unimplemented!("V5 transaction validation is not yet complete");
    }

    /// Verifies if a V5 `transaction` is supported by `network_upgrade`.
    fn verify_v5_transaction_network_upgrade(
        transaction: &Transaction,
        network_upgrade: NetworkUpgrade,
    ) -> Result<(), TransactionError> {
        match network_upgrade {
            // Supports V5 transactions
            NetworkUpgrade::Nu5 => Ok(()),

            // Does not support V5 transactions
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom
            | NetworkUpgrade::Heartwood
            | NetworkUpgrade::Canopy => Err(TransactionError::UnsupportedByNetworkUpgrade(
                transaction.version(),
                network_upgrade,
            )),
        }
    }

    /// Verifies if a transaction's transparent `inputs` are valid using the provided
    /// `script_verifier`.
    fn verify_transparent_inputs_and_outputs(
        request: &Request,
        network: Network,
        inputs: &[transparent::Input],
        script_verifier: script::Verifier<ZS>,
    ) -> Result<AsyncChecks, TransactionError> {
        let transaction = request.transaction();

        if transaction.is_coinbase() {
            check::coinbase_tx_no_prevout_joinsplit_spend(&transaction)?;

            Ok(AsyncChecks::new())
        } else {
            // feed all of the inputs to the script and shielded verifiers
            // the script_verifier also checks transparent sighashes, using its own implementation
            let cached_ffi_transaction = Arc::new(CachedFfiTransaction::new(transaction));
            let known_utxos = request.known_utxos();
            let upgrade = request.upgrade(network);

            let script_checks = (0..inputs.len())
                .into_iter()
                .map(move |input_index| {
                    let request = script::Request {
                        upgrade,
                        known_utxos: known_utxos.clone(),
                        cached_ffi_transaction: cached_ffi_transaction.clone(),
                        input_index,
                    };

                    script_verifier.clone().oneshot(request).boxed()
                })
                .collect();

            Ok(script_checks)
        }
    }

    /// Verifies a transaction's Sprout shielded join split data.
    fn verify_sprout_shielded_data(
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        shielded_sighash: &blake2b_simd::Hash,
    ) -> AsyncChecks {
        let checks = AsyncChecks::new();

        if let Some(joinsplit_data) = joinsplit_data {
            // XXX create a method on JoinSplitData
            // that prepares groth16::Items with the correct proofs
            // and proof inputs, handling interstitial treestates
            // correctly.

            // Then, pass those items to self.joinsplit to verify them.

            // Consensus rule: The joinSplitSig MUST represent a
            // valid signature, under joinSplitPubKey, of the
            // sighash.
            //
            // Queue the validation of the JoinSplit signature while
            // adding the resulting future to our collection of
            // async checks that (at a minimum) must pass for the
            // transaction to verify.
            //
            // https://zips.z.cash/protocol/protocol.pdf#sproutnonmalleability
            // https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
            let ed25519_verifier = primitives::ed25519::VERIFIER.clone();
            let ed25519_item =
                (joinsplit_data.pub_key, joinsplit_data.sig, shielded_sighash).into();

            checks.push(ed25519_verifier.oneshot(ed25519_item).boxed());
        }

        checks
    }

    /// Await a set of checks that should all succeed.
    ///
    /// If any of the checks fail, this method immediately returns the error and cancels all other
    /// checks by dropping them.
    async fn wait_for_checks(mut checks: AsyncChecks) -> Result<(), TransactionError> {
        // Wait for all asynchronous checks to complete
        // successfully, or fail verification if they error.
        while let Some(check) = checks.next().await {
            tracing::trace!(?check, remaining = checks.len());
            check?;
        }

        Ok(())
    }
}
