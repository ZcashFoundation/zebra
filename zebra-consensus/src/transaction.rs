use std::{
    collections::HashMap,
    future::Future,
    iter::FromIterator,
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
    block, orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::Groth16Proof,
    sapling,
    transaction::{self, HashType, SigHash, Transaction},
    transparent,
};

use zebra_script::CachedFfiTransaction;
use zebra_state as zs;

use crate::{error::TransactionError, primitives, script, BoxError};

mod check;
#[cfg(test)]
mod tests;

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
}

impl<ZS> Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    pub fn new(network: Network, script_verifier: script::Verifier<ZS>) -> Self {
        Self {
            network,
            script_verifier,
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
        known_utxos: Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
        /// The height of the block containing this transaction.
        height: block::Height,
    },
    /// Verify the supplied transaction as part of the mempool.
    ///
    /// Mempool transactions do not have any additional UTXOs.
    ///
    /// Note: coinbase transactions are invalid in the mempool
    Mempool {
        /// The transaction itself.
        transaction: Arc<Transaction>,
        /// The height of the next block.
        ///
        /// The next block is the first block that could possibly contain a
        /// mempool transaction.
        height: block::Height,
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
    pub fn known_utxos(&self) -> Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>> {
        match self {
            Request::Block { known_utxos, .. } => known_utxos.clone(),
            Request::Mempool { .. } => HashMap::new().into(),
        }
    }

    /// The height used to select the consensus rules for verifying this transaction.
    pub fn height(&self) -> block::Height {
        match self {
            Request::Block { height, .. } | Request::Mempool { height, .. } => *height,
        }
    }

    /// The network upgrade to consider for the verification.
    ///
    /// This is based on the block height from the request, and the supplied `network`.
    pub fn upgrade(&self, network: Network) -> NetworkUpgrade {
        NetworkUpgrade::current(network, self.height())
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
            unimplemented!("Zebra does not yet have a mempool (#2309)");
        }

        let script_verifier = self.script_verifier.clone();
        let network = self.network;

        let tx = req.transaction();
        let span = tracing::debug_span!("tx", hash = %tx.hash());

        async move {
            tracing::trace!(?tx);

            // Do basic checks first
            check::has_inputs_and_outputs(&tx)?;

            if tx.is_coinbase() {
                check::coinbase_tx_no_prevout_joinsplit_spend(&tx)?;
            }

            // [Canopy onward]: `vpub_old` MUST be zero.
            // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
            check::disabled_add_to_sprout_pool(&tx, req.height(), network)?;

            // "The consensus rules applied to valueBalance, vShieldedOutput, and bindingSig
            // in non-coinbase transactions MUST also be applied to coinbase transactions."
            //
            // This rule is implicitly implemented during Sapling and Orchard verification,
            // because they do not distinguish between coinbase and non-coinbase transactions.
            //
            // Note: this rule originally applied to Sapling, but we assume it also applies to Orchard.
            //
            // https://zips.z.cash/zip-0213#specification
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
                } => Self::verify_v4_transaction(
                    req,
                    network,
                    script_verifier,
                    inputs,
                    joinsplit_data,
                    sapling_shielded_data,
                )?,
                Transaction::V5 {
                    inputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => Self::verify_v5_transaction(
                    req,
                    network,
                    script_verifier,
                    inputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                )?,
            };

            async_checks.check().await?;

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
    fn verify_v4_transaction(
        request: Request,
        network: Network,
        script_verifier: script::Verifier<ZS>,
        inputs: &[transparent::Input],
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        sapling_shielded_data: &Option<sapling::ShieldedData<sapling::PerSpendAnchor>>,
    ) -> Result<AsyncChecks, TransactionError> {
        let tx = request.transaction();
        let upgrade = request.upgrade(network);
        let shielded_sighash = tx.sighash(upgrade, HashType::ALL, None);

        Ok(
            Self::verify_transparent_inputs_and_outputs(
                &request,
                network,
                inputs,
                script_verifier,
            )?
            .and(Self::verify_sprout_shielded_data(
                joinsplit_data,
                &shielded_sighash,
            ))
            .and(Self::verify_sapling_shielded_data(
                sapling_shielded_data,
                &shielded_sighash,
            )?),
        )
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
    /// - the sapling shielded data of the transaction, if any
    /// - the orchard shielded data of the transaction, if any
    fn verify_v5_transaction(
        request: Request,
        network: Network,
        script_verifier: script::Verifier<ZS>,
        inputs: &[transparent::Input],
        sapling_shielded_data: &Option<sapling::ShieldedData<sapling::SharedAnchor>>,
        orchard_shielded_data: &Option<orchard::ShieldedData>,
    ) -> Result<AsyncChecks, TransactionError> {
        let transaction = request.transaction();
        let upgrade = request.upgrade(network);
        let shielded_sighash = transaction.sighash(upgrade, HashType::ALL, None);

        Self::verify_v5_transaction_network_upgrade(&transaction, upgrade)?;

        let _async_checks = Self::verify_transparent_inputs_and_outputs(
            &request,
            network,
            inputs,
            script_verifier,
        )?
        .and(Self::verify_sapling_shielded_data(
            sapling_shielded_data,
            &shielded_sighash,
        )?)
        .and(Self::verify_orchard_shielded_data(
            orchard_shielded_data,
            &shielded_sighash,
        )?);

        // TODO:
        // - verify orchard shielded pool (ZIP-224) (#2105)
        // - ZIP-216 (#1798)
        // - ZIP-244 (#1874)
        // - validate bindingSigOrchard (#2103)
        // - remaining consensus rules (#2379)
        // - remove `should_panic` from tests

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
            // The script verifier only verifies PrevOut inputs and their corresponding UTXOs.
            // Coinbase transactions don't have any PrevOut inputs.
            Ok(AsyncChecks::new())
        } else {
            // feed all of the inputs to the script and shielded verifiers
            // the script_verifier also checks transparent sighashes, using its own implementation
            let cached_ffi_transaction = Arc::new(CachedFfiTransaction::new(transaction));
            let known_utxos = request.known_utxos();

            let script_checks = (0..inputs.len())
                .into_iter()
                .map(move |input_index| {
                    let request = script::Request {
                        cached_ffi_transaction: cached_ffi_transaction.clone(),
                        input_index,
                        known_utxos: known_utxos.clone(),
                        spend_restriction: request
                            .transaction()
                            .coinbase_spend_restriction(request.height()),
                        network,
                        height: request.height(),
                    };

                    script_verifier.clone().oneshot(request)
                })
                .collect();

            Ok(script_checks)
        }
    }

    /// Verifies a transaction's Sprout shielded join split data.
    fn verify_sprout_shielded_data(
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        shielded_sighash: &SigHash,
    ) -> AsyncChecks {
        let mut checks = AsyncChecks::new();

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

            checks.push(ed25519_verifier.oneshot(ed25519_item));
        }

        checks
    }

    /// Verifies a transaction's Sapling shielded data.
    fn verify_sapling_shielded_data<A>(
        sapling_shielded_data: &Option<sapling::ShieldedData<A>>,
        shielded_sighash: &SigHash,
    ) -> Result<AsyncChecks, TransactionError>
    where
        A: sapling::AnchorVariant + Clone,
        sapling::Spend<sapling::PerSpendAnchor>: From<(sapling::Spend<A>, A::Shared)>,
    {
        let mut async_checks = AsyncChecks::new();

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
                async_checks.push(
                    primitives::groth16::SPEND_VERIFIER
                        .clone()
                        .oneshot(primitives::groth16::ItemWrapper::from(&spend).into()),
                );

                // Consensus rule: The spend authorization signature
                // MUST be a valid SpendAuthSig signature over
                // SigHash using rk as the validating key.
                //
                // Queue the validation of the RedJubjub spend
                // authorization signature for each Spend
                // description while adding the resulting future to
                // our collection of async checks that (at a
                // minimum) must pass for the transaction to verify.
                async_checks.push(
                    primitives::redjubjub::VERIFIER
                        .clone()
                        .oneshot((spend.rk, spend.spend_auth_sig, shielded_sighash).into()),
                );
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
                async_checks.push(
                    primitives::groth16::OUTPUT_VERIFIER
                        .clone()
                        .oneshot(primitives::groth16::ItemWrapper::from(output).into()),
                );
            }

            let bvk = sapling_shielded_data.binding_verification_key();

            async_checks.push(
                primitives::redjubjub::VERIFIER
                    .clone()
                    .oneshot((bvk, sapling_shielded_data.binding_sig, &shielded_sighash).into()),
            );
        }

        Ok(async_checks)
    }

    /// Verifies a transaction's Orchard shielded data.
    fn verify_orchard_shielded_data(
        orchard_shielded_data: &Option<orchard::ShieldedData>,
        shielded_sighash: &SigHash,
    ) -> Result<AsyncChecks, TransactionError> {
        let mut async_checks = AsyncChecks::new();

        if let Some(orchard_shielded_data) = orchard_shielded_data {
            for authorized_action in orchard_shielded_data.actions.iter().cloned() {
                let (action, spend_auth_sig) = authorized_action.into_parts();
                // Consensus rule: The spend authorization signature
                // MUST be a valid SpendAuthSig signature over
                // SigHash using rk as the validating key.
                //
                // Queue the validation of the RedPallas spend
                // authorization signature for each Action
                // description while adding the resulting future to
                // our collection of async checks that (at a
                // minimum) must pass for the transaction to verify.
                async_checks.push(
                    primitives::redpallas::VERIFIER
                        .clone()
                        .oneshot((action.rk, spend_auth_sig, &shielded_sighash).into()),
                );
            }
        }

        Ok(async_checks)
    }
}

/// A set of unordered asynchronous checks that should succeed.
///
/// A wrapper around [`FuturesUnordered`] with some auxiliary methods.
struct AsyncChecks(FuturesUnordered<Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>>);

impl AsyncChecks {
    /// Create an empty set of unordered asynchronous checks.
    pub fn new() -> Self {
        AsyncChecks(FuturesUnordered::new())
    }

    /// Push a check into the set.
    pub fn push(&mut self, check: impl Future<Output = Result<(), BoxError>> + Send + 'static) {
        self.0.push(check.boxed());
    }

    /// Push a set of checks into the set.
    ///
    /// This method can be daisy-chained.
    pub fn and(mut self, checks: AsyncChecks) -> Self {
        self.0.extend(checks.0);
        self
    }

    /// Wait until all checks in the set finish.
    ///
    /// If any of the checks fail, this method immediately returns the error and cancels all other
    /// checks by dropping them.
    async fn check(mut self) -> Result<(), BoxError> {
        // Wait for all asynchronous checks to complete
        // successfully, or fail verification if they error.
        while let Some(check) = self.0.next().await {
            tracing::trace!(?check, remaining = self.0.len());
            check?;
        }

        Ok(())
    }
}

impl<F> FromIterator<F> for AsyncChecks
where
    F: Future<Output = Result<(), BoxError>> + Send + 'static,
{
    fn from_iter<I>(iterator: I) -> Self
    where
        I: IntoIterator<Item = F>,
    {
        AsyncChecks(iterator.into_iter().map(FutureExt::boxed).collect())
    }
}
