//! Asynchronous verification of transactions.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::{DateTime, Utc};
use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use tower::{timeout::Timeout, Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block, orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::Groth16Proof,
    sapling,
    serialization::DateTime32,
    transaction::{
        self, HashType, SigHash, Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx,
    },
    transparent::{self, OrderedUtxo},
};

use zebra_script::CachedFfiTransaction;
use zebra_state as zs;

use crate::{error::TransactionError, groth16::DescriptionWrapper, primitives, script, BoxError};

pub mod check;
#[cfg(test)]
mod tests;

/// A timeout applied to UTXO lookup requests.
///
/// The exact value is non-essential, but this should be long enough to allow
/// out-of-order verification of blocks (UTXOs are not required to be ready
/// immediately) while being short enough to:
///   * prune blocks that are too far in the future to be worth keeping in the
///     queue,
///   * fail blocks that reference invalid UTXOs, and
///   * fail blocks that reference UTXOs from blocks that have temporarily failed
///     to download, because a peer sent Zebra a bad list of block hashes. (The
///     UTXO verification failure will restart the sync, and re-download the
///     chain in the correct order.)
const UTXO_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6 * 60);

/// Asynchronous transaction verification.
///
/// # Correctness
///
/// Transaction verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`router`](`crate::router`)
/// module documentation for details.
#[derive(Debug, Clone)]
pub struct Verifier<ZS> {
    network: Network,
    state: Timeout<ZS>,
    script_verifier: script::Verifier,
}

impl<ZS> Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Create a new transaction verifier.
    pub fn new(network: &Network, state: ZS) -> Self {
        Self {
            network: network.clone(),
            state: Timeout::new(state, UTXO_LOOKUP_TIMEOUT),
            script_verifier: script::Verifier,
        }
    }
}

/// Specifies whether a transaction should be verified as part of a block or as
/// part of the mempool.
///
/// Transaction verification has slightly different consensus rules, depending on
/// whether the transaction is to be included in a block on in the mempool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Verify the supplied transaction as part of a block.
    Block {
        /// The transaction itself.
        transaction: Arc<Transaction>,
        /// Additional UTXOs which are known at the time of verification.
        known_utxos: Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
        /// The height of the block containing this transaction.
        height: block::Height,
        /// The time that the block was mined.
        time: DateTime<Utc>,
    },
    /// Verify the supplied transaction as part of the mempool.
    ///
    /// Mempool transactions do not have any additional UTXOs.
    ///
    /// Note: coinbase transactions are invalid in the mempool
    Mempool {
        /// The transaction itself.
        transaction: UnminedTx,
        /// The height of the next block.
        ///
        /// The next block is the first block that could possibly contain a
        /// mempool transaction.
        height: block::Height,
    },
}

/// The response type for the transaction verifier service.
/// Responses identify the transaction that was verified.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    /// A response to a block transaction verification request.
    Block {
        /// The witnessed transaction ID for this transaction.
        ///
        /// [`Response::Block`] responses can be uniquely identified by
        /// [`UnminedTxId::mined_id`], because the block's authorizing data root
        /// will be checked during contextual validation.
        tx_id: UnminedTxId,

        /// The miner fee for this transaction.
        ///
        /// `None` for coinbase transactions.
        ///
        /// # Consensus
        ///
        /// > The remaining value in the transparent transaction value pool
        /// > of a coinbase transaction is destroyed.
        ///
        /// <https://zips.z.cash/protocol/protocol.pdf#transactions>
        miner_fee: Option<Amount<NonNegative>>,

        /// The number of legacy signature operations in this transaction's
        /// transparent inputs and outputs.
        legacy_sigop_count: u64,
    },

    /// A response to a mempool transaction verification request.
    Mempool {
        /// The full content of the verified mempool transaction.
        /// Also contains the transaction fee and other associated fields.
        ///
        /// Mempool transactions always have a transaction fee,
        /// because coinbase transactions are rejected from the mempool.
        ///
        /// [`Response::Mempool`] responses are uniquely identified by the
        /// [`UnminedTxId`] variant for their transaction version.
        transaction: VerifiedUnminedTx,
    },
}

impl From<VerifiedUnminedTx> for Response {
    fn from(transaction: VerifiedUnminedTx) -> Self {
        Response::Mempool { transaction }
    }
}

impl Request {
    /// The transaction to verify that's in this request.
    pub fn transaction(&self) -> Arc<Transaction> {
        match self {
            Request::Block { transaction, .. } => transaction.clone(),
            Request::Mempool { transaction, .. } => transaction.transaction.clone(),
        }
    }

    /// The unverified mempool transaction, if this is a mempool request.
    pub fn mempool_transaction(&self) -> Option<UnminedTx> {
        match self {
            Request::Block { .. } => None,
            Request::Mempool { transaction, .. } => Some(transaction.clone()),
        }
    }

    /// The unmined transaction ID for the transaction in this request.
    pub fn tx_id(&self) -> UnminedTxId {
        match self {
            // TODO: get the precalculated ID from the block verifier
            Request::Block { transaction, .. } => transaction.unmined_id(),
            Request::Mempool { transaction, .. } => transaction.id,
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

    /// The block time used for lock time consensus rules validation.
    pub fn block_time(&self) -> Option<DateTime<Utc>> {
        match self {
            Request::Block { time, .. } => Some(*time),
            Request::Mempool { .. } => None,
        }
    }

    /// The network upgrade to consider for the verification.
    ///
    /// This is based on the block height from the request, and the supplied `network`.
    pub fn upgrade(&self, network: &Network) -> NetworkUpgrade {
        NetworkUpgrade::current(network, self.height())
    }

    /// Returns true if the request is a mempool request.
    pub fn is_mempool(&self) -> bool {
        matches!(self, Request::Mempool { .. })
    }
}

impl Response {
    /// The verified mempool transaction, if this is a mempool response.
    pub fn into_mempool_transaction(self) -> Option<VerifiedUnminedTx> {
        match self {
            Response::Block { .. } => None,
            Response::Mempool { transaction, .. } => Some(transaction),
        }
    }

    /// The unmined transaction ID for the transaction in this response.
    pub fn tx_id(&self) -> UnminedTxId {
        match self {
            Response::Block { tx_id, .. } => *tx_id,
            Response::Mempool { transaction, .. } => transaction.transaction.id,
        }
    }

    /// The miner fee for the transaction in this response.
    ///
    /// Coinbase transactions do not have a miner fee,
    /// and they don't need UTXOs to calculate their value balance,
    /// because they don't spend any inputs.
    pub fn miner_fee(&self) -> Option<Amount<NonNegative>> {
        match self {
            Response::Block { miner_fee, .. } => *miner_fee,
            Response::Mempool { transaction, .. } => Some(transaction.miner_fee),
        }
    }

    /// The number of legacy transparent signature operations in this transaction's
    /// inputs and outputs.
    pub fn legacy_sigop_count(&self) -> u64 {
        match self {
            Response::Block {
                legacy_sigop_count, ..
            } => *legacy_sigop_count,
            Response::Mempool { transaction, .. } => transaction.legacy_sigop_count,
        }
    }

    /// Returns true if the request is a mempool request.
    pub fn is_mempool(&self) -> bool {
        match self {
            Response::Block { .. } => false,
            Response::Mempool { .. } => true,
        }
    }
}

impl<ZS> Service<Request> for Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    type Response = Response;
    type Error = TransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    // TODO: break up each chunk into its own method
    fn call(&mut self, req: Request) -> Self::Future {
        let script_verifier = self.script_verifier;
        let network = self.network.clone();
        let state = self.state.clone();

        let tx = req.transaction();
        let tx_id = req.tx_id();
        let span = tracing::debug_span!("tx", ?tx_id);

        async move {
            tracing::trace!(?tx_id, ?req, "got tx verify request");

            // Do quick checks first
            check::has_inputs_and_outputs(&tx)?;
            check::has_enough_orchard_flags(&tx)?;

            // Validate the coinbase input consensus rules
            if req.is_mempool() && tx.is_coinbase() {
                return Err(TransactionError::CoinbaseInMempool);
            }

            if tx.is_coinbase() {
                check::coinbase_tx_no_prevout_joinsplit_spend(&tx)?;
            } else if !tx.is_valid_non_coinbase() {
                return Err(TransactionError::NonCoinbaseHasCoinbaseInput);
            }

            // Validate `nExpiryHeight` consensus rules
            if tx.is_coinbase() {
                check::coinbase_expiry_height(&req.height(), &tx, &network)?;
            } else {
                check::non_coinbase_expiry_height(&req.height(), &tx)?;
            }

            // Consensus rule:
            //
            // > Either v_{pub}^{old} or v_{pub}^{new} MUST be zero.
            //
            // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
            check::joinsplit_has_vpub_zero(&tx)?;

            // [Canopy onward]: `vpub_old` MUST be zero.
            // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
            check::disabled_add_to_sprout_pool(&tx, req.height(), &network)?;

            check::spend_conflicts(&tx)?;

            tracing::trace!(?tx_id, "passed quick checks");

            if let Some(block_time) = req.block_time() {
                check::lock_time_has_passed(&tx, req.height(), block_time)?;
            } else {
                // Skip the state query if we don't need the time for this check.
                let next_median_time_past = if tx.lock_time_is_time() {
                    // This state query is much faster than loading UTXOs from the database,
                    // so it doesn't need to be executed in parallel
                    let state = state.clone();
                    Some(Self::mempool_best_chain_next_median_time_past(state).await?.to_chrono())
                } else {
                    None
                };

                // This consensus check makes sure Zebra produces valid block templates.
                check::lock_time_has_passed(&tx, req.height(), next_median_time_past)?;
            }

            // "The consensus rules applied to valueBalance, vShieldedOutput, and bindingSig
            // in non-coinbase transactions MUST also be applied to coinbase transactions."
            //
            // This rule is implicitly implemented during Sapling and Orchard verification,
            // because they do not distinguish between coinbase and non-coinbase transactions.
            //
            // Note: this rule originally applied to Sapling, but we assume it also applies to Orchard.
            //
            // https://zips.z.cash/zip-0213#specification

            // Load spent UTXOs from state.
            // The UTXOs are required for almost all the async checks.
            let load_spent_utxos_fut =
                Self::spent_utxos(tx.clone(), req.known_utxos(), req.is_mempool(), state.clone());
            let (spent_utxos, spent_outputs) = load_spent_utxos_fut.await?;

            // WONTFIX: Return an error for Request::Block as well to replace this check in
            //       the state once #2336 has been implemented?
            if req.is_mempool() {
                Self::check_maturity_height(&req, &spent_utxos)?;
            }

            let cached_ffi_transaction =
                Arc::new(CachedFfiTransaction::new(tx.clone(), spent_outputs));

            tracing::trace!(?tx_id, "got state UTXOs");

            let mut async_checks = match tx.as_ref() {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                    tracing::debug!(?tx, "got transaction with wrong version");
                    return Err(TransactionError::WrongVersion);
                }
                Transaction::V4 {
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => Self::verify_v4_transaction(
                    &req,
                    &network,
                    script_verifier,
                    cached_ffi_transaction.clone(),
                    joinsplit_data,
                    sapling_shielded_data,
                )?,
                Transaction::V5 {
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                }
                => Self::verify_v5_and_v6_transaction(
                    &req,
                    &network,
                    script_verifier,
                    cached_ffi_transaction.clone(),
                    sapling_shielded_data,
                    orchard_shielded_data,
                )?,
                #[cfg(feature = "tx_v6")]
                Transaction::V6 {
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => Self::verify_v5_and_v6_transaction(
                    &req,
                    &network,
                    script_verifier,
                    cached_ffi_transaction.clone(),
                    sapling_shielded_data,
                    orchard_shielded_data,
                )?,
            };

            if let Some(unmined_tx) = req.mempool_transaction() {
                let check_anchors_and_revealed_nullifiers_query = state
                    .clone()
                    .oneshot(zs::Request::CheckBestChainTipNullifiersAndAnchors(
                        unmined_tx,
                    ))
                    .map(|res| {
                        assert!(
                            res? == zs::Response::ValidBestChainTipNullifiersAndAnchors,
                            "unexpected response to CheckBestChainTipNullifiersAndAnchors request"
                        );
                        Ok(())
                    }
                );

                async_checks.push(check_anchors_and_revealed_nullifiers_query);
            }

            tracing::trace!(?tx_id, "awaiting async checks...");

            // If the Groth16 parameter download hangs,
            // Zebra will timeout here, waiting for the async checks.
            async_checks.check().await?;

            tracing::trace!(?tx_id, "finished async checks");

            // Get the `value_balance` to calculate the transaction fee.
            let value_balance = tx.value_balance(&spent_utxos);

            // Calculate the fee only for non-coinbase transactions.
            let mut miner_fee = None;
            if !tx.is_coinbase() {
                // TODO: deduplicate this code with remaining_transaction_value()?
                miner_fee = match value_balance {
                    Ok(vb) => match vb.remaining_transaction_value() {
                        Ok(tx_rtv) => Some(tx_rtv),
                        Err(_) => return Err(TransactionError::IncorrectFee),
                    },
                    Err(_) => return Err(TransactionError::IncorrectFee),
                };
            }

            let legacy_sigop_count = cached_ffi_transaction.legacy_sigop_count()?;

            let rsp = match req {
                Request::Block { .. } => Response::Block {
                    tx_id,
                    miner_fee,
                    legacy_sigop_count,
                },
                Request::Mempool { transaction, .. } => {
                    let transaction = VerifiedUnminedTx::new(
                        transaction,
                        miner_fee.expect(
                            "unexpected mempool coinbase transaction: should have already rejected",
                        ),
                        legacy_sigop_count,
                    )?;
                    Response::Mempool { transaction }
                },
            };

            Ok(rsp)
        }
        .inspect(move |result| {
            // Hide the transaction data to avoid filling the logs
            tracing::trace!(?tx_id, result = ?result.as_ref().map(|_tx| ()), "got tx verify result");
        })
        .instrument(span)
        .boxed()
    }
}

trait OrchardTransaction {
    const SUPPORTED_NETWORK_UPGRADES: &'static [NetworkUpgrade];
}

impl OrchardTransaction for orchard::OrchardVanilla {
    // FIXME: is this a correct set of Nu values?
    const SUPPORTED_NETWORK_UPGRADES: &'static [NetworkUpgrade] = &[
        NetworkUpgrade::Nu5,
        NetworkUpgrade::Nu6,
        #[cfg(feature = "tx_v6")]
        NetworkUpgrade::Nu7,
    ];
}

#[cfg(feature = "tx_v6")]
impl OrchardTransaction for orchard::OrchardZSA {
    const SUPPORTED_NETWORK_UPGRADES: &'static [NetworkUpgrade] = &[NetworkUpgrade::Nu7];
}

impl<ZS> Verifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Fetches the median-time-past of the *next* block after the best state tip.
    ///
    /// This is used to verify that the lock times of mempool transactions
    /// can be included in any valid next block.
    async fn mempool_best_chain_next_median_time_past(
        state: Timeout<ZS>,
    ) -> Result<DateTime32, TransactionError> {
        let query = state
            .clone()
            .oneshot(zs::Request::BestChainNextMedianTimePast);

        if let zebra_state::Response::BestChainNextMedianTimePast(median_time_past) = query
            .await
            .map_err(|e| TransactionError::ValidateMempoolLockTimeError(e.to_string()))?
        {
            Ok(median_time_past)
        } else {
            unreachable!("Request::BestChainNextMedianTimePast always responds with BestChainNextMedianTimePast")
        }
    }

    /// Wait for the UTXOs that are being spent by the given transaction.
    ///
    /// `known_utxos` are additional UTXOs known at the time of validation (i.e.
    /// from previous transactions in the block).
    ///
    /// Returns a tuple with a OutPoint -> Utxo map, and a vector of Outputs
    /// in the same order as the matching inputs in the transaction.
    async fn spent_utxos(
        tx: Arc<Transaction>,
        known_utxos: Arc<HashMap<transparent::OutPoint, OrderedUtxo>>,
        is_mempool: bool,
        state: Timeout<ZS>,
    ) -> Result<
        (
            HashMap<transparent::OutPoint, transparent::Utxo>,
            Vec<transparent::Output>,
        ),
        TransactionError,
    > {
        let inputs = tx.inputs();
        let mut spent_utxos = HashMap::new();
        let mut spent_outputs = Vec::new();
        for input in inputs {
            if let transparent::Input::PrevOut { outpoint, .. } = input {
                tracing::trace!("awaiting outpoint lookup");
                // Currently, Zebra only supports known UTXOs in block transactions.
                // But it might support them in the mempool in future.
                let utxo = if let Some(output) = known_utxos.get(outpoint) {
                    tracing::trace!("UXTO in known_utxos, discarding query");
                    output.utxo.clone()
                } else if is_mempool {
                    let query = state
                        .clone()
                        .oneshot(zs::Request::UnspentBestChainUtxo(*outpoint));
                    if let zebra_state::Response::UnspentBestChainUtxo(utxo) = query.await? {
                        utxo.ok_or(TransactionError::TransparentInputNotFound)?
                    } else {
                        unreachable!("UnspentBestChainUtxo always responds with Option<Utxo>")
                    }
                } else {
                    let query = state
                        .clone()
                        .oneshot(zebra_state::Request::AwaitUtxo(*outpoint));
                    if let zebra_state::Response::Utxo(utxo) = query.await? {
                        utxo
                    } else {
                        unreachable!("AwaitUtxo always responds with Utxo")
                    }
                };
                tracing::trace!(?utxo, "got UTXO");
                spent_outputs.push(utxo.output.clone());
                spent_utxos.insert(*outpoint, utxo);
            } else {
                continue;
            }
        }
        Ok((spent_utxos, spent_outputs))
    }

    /// Accepts `request`, a transaction verifier [`&Request`](Request),
    /// and `spent_utxos`, a HashMap of UTXOs in the chain that are spent by this transaction.
    ///
    /// Gets the `transaction`, `height`, and `known_utxos` for the request and checks calls
    /// [`check::tx_transparent_coinbase_spends_maturity`] to verify that every transparent
    /// coinbase output spent by the transaction will have matured by `height`.
    ///
    /// Returns `Ok(())` if every transparent coinbase output spent by the transaction is
    /// mature and valid for the request height, or a [`TransactionError`] if the transaction
    /// spends transparent coinbase outputs that are immature and invalid for the request height.
    pub fn check_maturity_height(
        request: &Request,
        spent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<(), TransactionError> {
        check::tx_transparent_coinbase_spends_maturity(
            request.transaction(),
            request.height(),
            request.known_utxos(),
            spent_utxos,
        )
    }

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
    /// - the prepared `cached_ffi_transaction` used by the script verifier
    /// - the Sprout `joinsplit_data` shielded data in the transaction
    /// - the `sapling_shielded_data` in the transaction
    #[allow(clippy::unwrap_in_result)]
    fn verify_v4_transaction(
        request: &Request,
        network: &Network,
        script_verifier: script::Verifier,
        cached_ffi_transaction: Arc<CachedFfiTransaction>,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        sapling_shielded_data: &Option<sapling::ShieldedData<sapling::PerSpendAnchor>>,
    ) -> Result<AsyncChecks, TransactionError> {
        let tx = request.transaction();
        let upgrade = request.upgrade(network);

        Self::verify_v4_transaction_network_upgrade(&tx, upgrade)?;

        let shielded_sighash = tx.sighash(
            upgrade
                .branch_id()
                .expect("Overwinter-onwards must have branch ID, and we checkpoint on Canopy"),
            HashType::ALL,
            cached_ffi_transaction.all_previous_outputs(),
            None,
        );

        Ok(Self::verify_transparent_inputs_and_outputs(
            request,
            network,
            script_verifier,
            cached_ffi_transaction,
        )?
        .and(Self::verify_sprout_shielded_data(
            joinsplit_data,
            &shielded_sighash,
        )?)
        .and(Self::verify_sapling_shielded_data(
            sapling_shielded_data,
            &shielded_sighash,
        )?))
    }

    /// Verifies if a V4 `transaction` is supported by `network_upgrade`.
    fn verify_v4_transaction_network_upgrade(
        transaction: &Transaction,
        network_upgrade: NetworkUpgrade,
    ) -> Result<(), TransactionError> {
        match network_upgrade {
            // Supports V4 transactions
            //
            // # Consensus
            //
            // > [Sapling to Canopy inclusive, pre-NU5] The transaction version number MUST be 4,
            // > and the version group ID MUST be 0x892F2085.
            //
            // > [NU5 onward] The transaction version number MUST be 4 or 5.
            // > If the transaction version number is 4 then the version group ID MUST be 0x892F2085.
            // > If the transaction version number is 5 then the version group ID MUST be 0x26A7270A.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // Note: Here we verify the transaction version number of the above two rules, the group
            // id is checked in zebra-chain crate, in the transaction serialize.
            NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom
            | NetworkUpgrade::Heartwood
            | NetworkUpgrade::Canopy
            | NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu7 => Ok(()),

            // Does not support V4 transactions
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter => Err(TransactionError::UnsupportedByNetworkUpgrade(
                transaction.version(),
                network_upgrade,
            )),
        }
    }

    /// Verify a V5/V6 transaction.
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
    /// - the prepared `cached_ffi_transaction` used by the script verifier
    /// - the sapling shielded data of the transaction, if any
    /// - the orchard shielded data of the transaction, if any
    #[allow(clippy::unwrap_in_result)]
    fn verify_v5_and_v6_transaction<V: primitives::halo2::OrchardVerifier + OrchardTransaction>(
        request: &Request,
        network: &Network,
        script_verifier: script::Verifier,
        cached_ffi_transaction: Arc<CachedFfiTransaction>,
        sapling_shielded_data: &Option<sapling::ShieldedData<sapling::SharedAnchor>>,
        orchard_shielded_data: &Option<orchard::ShieldedData<V>>,
    ) -> Result<AsyncChecks, TransactionError> {
        let transaction = request.transaction();
        let upgrade = request.upgrade(network);

        Self::verify_v5_and_v6_transaction_network_upgrade::<V>(&transaction, upgrade)?;

        let shielded_sighash = transaction.sighash(
            upgrade
                .branch_id()
                .expect("Overwinter-onwards must have branch ID, and we checkpoint on Canopy"),
            HashType::ALL,
            cached_ffi_transaction.all_previous_outputs(),
            None,
        );

        Ok(Self::verify_transparent_inputs_and_outputs(
            request,
            network,
            script_verifier,
            cached_ffi_transaction,
        )?
        .and(Self::verify_sapling_shielded_data(
            sapling_shielded_data,
            &shielded_sighash,
        )?)
        .and(Self::verify_orchard_shielded_data(
            orchard_shielded_data,
            &shielded_sighash,
        )?))
    }

    /// Verifies if a V5/V6 `transaction` is supported by `network_upgrade`.
    fn verify_v5_and_v6_transaction_network_upgrade<
        V: primitives::halo2::OrchardVerifier + OrchardTransaction,
    >(
        transaction: &Transaction,
        network_upgrade: NetworkUpgrade,
    ) -> Result<(), TransactionError> {
        if V::SUPPORTED_NETWORK_UPGRADES.contains(&network_upgrade) {
            // FIXME: Extend this comment to include V6. Also, it may be confusing to
            // mention version group IDs and other rules here since they aren’t actually
            // checked. This function only verifies compatibility between the transaction
            // version and the network upgrade.

            // Supports V5/V6 transactions
            //
            // # Consensus
            //
            // > [NU5 onward] The transaction version number MUST be 4 or 5.
            // > If the transaction version number is 4 then the version group ID MUST be 0x892F2085.
            // > If the transaction version number is 5 then the version group ID MUST be 0x26A7270A.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // Note: Here we verify the transaction version number of the above rule, the group
            // id is checked in zebra-chain crate, in the transaction serialize.
            Ok(())
        } else {
            // Does not support V5/V6 transactions
            Err(TransactionError::UnsupportedByNetworkUpgrade(
                transaction.version(),
                network_upgrade,
            ))
        }
    }

    /// Verifies if a transaction's transparent inputs are valid using the provided
    /// `script_verifier` and `cached_ffi_transaction`.
    ///
    /// Returns script verification responses via the `utxo_sender`.
    fn verify_transparent_inputs_and_outputs(
        request: &Request,
        network: &Network,
        script_verifier: script::Verifier,
        cached_ffi_transaction: Arc<CachedFfiTransaction>,
    ) -> Result<AsyncChecks, TransactionError> {
        let transaction = request.transaction();

        if transaction.is_coinbase() {
            // The script verifier only verifies PrevOut inputs and their corresponding UTXOs.
            // Coinbase transactions don't have any PrevOut inputs.
            Ok(AsyncChecks::new())
        } else {
            // feed all of the inputs to the script verifier
            // the script_verifier also checks transparent sighashes, using its own implementation
            let inputs = transaction.inputs();
            let upgrade = request.upgrade(network);

            let script_checks = (0..inputs.len())
                .map(move |input_index| {
                    let request = script::Request {
                        upgrade,
                        cached_ffi_transaction: cached_ffi_transaction.clone(),
                        input_index,
                    };

                    script_verifier.oneshot(request)
                })
                .collect();

            Ok(script_checks)
        }
    }

    /// Verifies a transaction's Sprout shielded join split data.
    fn verify_sprout_shielded_data(
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
        shielded_sighash: &SigHash,
    ) -> Result<AsyncChecks, TransactionError> {
        let mut checks = AsyncChecks::new();

        if let Some(joinsplit_data) = joinsplit_data {
            for joinsplit in joinsplit_data.joinsplits() {
                // # Consensus
                //
                // > The proof π_ZKJoinSplit MUST be valid given a
                // > primary input formed from the relevant other fields and h_{Sig}
                //
                // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
                //
                // Queue the verification of the Groth16 spend proof
                // for each JoinSplit description while adding the
                // resulting future to our collection of async
                // checks that (at a minimum) must pass for the
                // transaction to verify.
                checks.push(primitives::groth16::JOINSPLIT_VERIFIER.oneshot(
                    DescriptionWrapper(&(joinsplit, &joinsplit_data.pub_key)).try_into()?,
                ));
            }

            // # Consensus
            //
            // > If effectiveVersion ≥ 2 and nJoinSplit > 0, then:
            // > - joinSplitPubKey MUST be a valid encoding of an Ed25519 validating key
            // > - joinSplitSig MUST represent a valid signature under
            //     joinSplitPubKey of dataToBeSigned, as defined in § 4.11
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // The `if` part is indirectly enforced, since the `joinsplit_data`
            // is only parsed if those conditions apply in
            // [`Transaction::zcash_deserialize`].
            //
            // The valid encoding is defined in
            //
            // > A valid Ed25519 validating key is defined as a sequence of 32
            // > bytes encoding a point on the Ed25519 curve
            //
            // https://zips.z.cash/protocol/protocol.pdf#concreteed25519
            //
            // which is enforced during signature verification, in both batched
            // and single verification, when decompressing the encoded point.
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

        Ok(checks)
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
                // # Consensus
                //
                // > The proof π_ZKSpend MUST be valid
                // > given a primary input formed from the other
                // > fields except spendAuthSig.
                //
                // https://zips.z.cash/protocol/protocol.pdf#spenddesc
                //
                // Queue the verification of the Groth16 spend proof
                // for each Spend description while adding the
                // resulting future to our collection of async
                // checks that (at a minimum) must pass for the
                // transaction to verify.
                async_checks.push(
                    primitives::groth16::SPEND_VERIFIER
                        .clone()
                        .oneshot(DescriptionWrapper(&spend).try_into()?),
                );

                // # Consensus
                //
                // > The spend authorization signature
                // > MUST be a valid SpendAuthSig signature over
                // > SigHash using rk as the validating key.
                //
                // This is validated by the verifier.
                //
                // > [NU5 onward] As specified in § 5.4.7 ‘RedDSA, RedJubjub,
                // > and RedPallas’ on p. 88, the validation of the 𝑅
                // > component of the signature changes to prohibit non-canonical encodings.
                //
                // This is validated by the verifier, inside the `redjubjub` crate.
                // It calls [`jubjub::AffinePoint::from_bytes`] to parse R and
                // that enforces the canonical encoding.
                //
                // https://zips.z.cash/protocol/protocol.pdf#spenddesc
                //
                // Queue the validation of the RedJubjub spend
                // authorization signature for each Spend
                // description while adding the resulting future to
                // our collection of async checks that (at a
                // minimum) must pass for the transaction to verify.
                async_checks.push(
                    primitives::redjubjub::VERIFIER
                        .clone()
                        .oneshot((spend.rk.into(), spend.spend_auth_sig, shielded_sighash).into()),
                );
            }

            for output in sapling_shielded_data.outputs() {
                // # Consensus
                //
                // > The proof π_ZKOutput MUST be
                // > valid given a primary input formed from the other
                // > fields except C^enc and C^out.
                //
                // https://zips.z.cash/protocol/protocol.pdf#outputdesc
                //
                // Queue the verification of the Groth16 output
                // proof for each Output description while adding
                // the resulting future to our collection of async
                // checks that (at a minimum) must pass for the
                // transaction to verify.
                async_checks.push(
                    primitives::groth16::OUTPUT_VERIFIER
                        .clone()
                        .oneshot(DescriptionWrapper(output).try_into()?),
                );
            }

            // # Consensus
            //
            // > The Spend transfers and Action transfers of a transaction MUST be
            // > consistent with its vbalanceSapling value as specified in § 4.13
            // > ‘Balance and Binding Signature (Sapling)’.
            //
            // https://zips.z.cash/protocol/protocol.pdf#spendsandoutputs
            //
            // > [Sapling onward] If effectiveVersion ≥ 4 and
            // > nSpendsSapling + nOutputsSapling > 0, then:
            // > – let bvk^{Sapling} and SigHash be as defined in § 4.13;
            // > – bindingSigSapling MUST represent a valid signature under the
            // >   transaction binding validating key bvk Sapling of SigHash —
            // >   i.e. BindingSig^{Sapling}.Validate_{bvk^{Sapling}}(SigHash, bindingSigSapling ) = 1.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // This is validated by the verifier. The `if` part is indirectly
            // enforced, since the `sapling_shielded_data` is only parsed if those
            // conditions apply in [`Transaction::zcash_deserialize`].
            //
            // >   [NU5 onward] As specified in § 5.4.7, the validation of the 𝑅 component
            // >   of the signature changes to prohibit non-canonical encodings.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // This is validated by the verifier, inside the `redjubjub` crate.
            // It calls [`jubjub::AffinePoint::from_bytes`] to parse R and
            // that enforces the canonical encoding.

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
    fn verify_orchard_shielded_data<V: primitives::halo2::OrchardVerifier>(
        orchard_shielded_data: &Option<orchard::ShieldedData<V>>,
        shielded_sighash: &SigHash,
    ) -> Result<AsyncChecks, TransactionError> {
        let mut async_checks = AsyncChecks::new();

        if let Some(orchard_shielded_data) = orchard_shielded_data {
            // # Consensus
            //
            // > The proof 𝜋 MUST be valid given a primary input (cv, rt^{Orchard},
            // > nf, rk, cm_x, enableSpends, enableOutputs)
            //
            // https://zips.z.cash/protocol/protocol.pdf#actiondesc
            //
            // Unlike Sapling, Orchard shielded transactions have a single
            // aggregated Halo2 proof per transaction, even with multiple
            // Actions in one transaction. So we queue it for verification
            // only once instead of queuing it up for every Action description.
            async_checks.push(
                V::get_verifier()
                    .clone()
                    .oneshot(primitives::halo2::Item::from(orchard_shielded_data)),
            );

            for authorized_action in orchard_shielded_data.actions.iter().cloned() {
                let (action, spend_auth_sig) = authorized_action.into_parts();

                // # Consensus
                //
                // > - Let SigHash be the SIGHASH transaction hash of this transaction, not
                // >   associated with an input, as defined in § 4.10 using SIGHASH_ALL.
                // > - The spend authorization signature MUST be a valid SpendAuthSig^{Orchard}
                // >   signature over SigHash using rk as the validating key — i.e.
                // >   SpendAuthSig^{Orchard}.Validate_{rk}(SigHash, spendAuthSig) = 1.
                // >   As specified in § 5.4.7, validation of the 𝑅 component of the
                // >   signature prohibits non-canonical encodings.
                //
                // https://zips.z.cash/protocol/protocol.pdf#actiondesc
                //
                // This is validated by the verifier, inside the [`reddsa`] crate.
                // It calls [`pallas::Affine::from_bytes`] to parse R and
                // that enforces the canonical encoding.
                //
                // Queue the validation of the RedPallas spend
                // authorization signature for each Action
                // description while adding the resulting future to
                // our collection of async checks that (at a
                // minimum) must pass for the transaction to verify.
                async_checks.push(primitives::redpallas::VERIFIER.clone().oneshot(
                    primitives::redpallas::Item::from_spendauth(
                        action.rk,
                        spend_auth_sig,
                        &shielded_sighash,
                    ),
                ));
            }

            let bvk = orchard_shielded_data.binding_verification_key();

            // # Consensus
            //
            // > The Action transfers of a transaction MUST be consistent with
            // > its v balanceOrchard value as specified in § 4.14.
            //
            // https://zips.z.cash/protocol/protocol.pdf#actions
            //
            // > [NU5 onward] If effectiveVersion ≥ 5 and nActionsOrchard > 0, then:
            // > – let bvk^{Orchard} and SigHash be as defined in § 4.14;
            // > – bindingSigOrchard MUST represent a valid signature under the
            // >   transaction binding validating key bvk^{Orchard} of SigHash —
            // >   i.e. BindingSig^{Orchard}.Validate_{bvk^{Orchard}}(SigHash, bindingSigOrchard) = 1.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // This is validated by the verifier. The `if` part is indirectly
            // enforced, since the `orchard_shielded_data` is only parsed if those
            // conditions apply in [`Transaction::zcash_deserialize`].
            //
            // >   As specified in § 5.4.7, validation of the 𝑅 component of the signature
            // >   prohibits non-canonical encodings.
            //
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            //
            // This is validated by the verifier, inside the `reddsa` crate.
            // It calls [`pallas::Affine::from_bytes`] to parse R and
            // that enforces the canonical encoding.

            async_checks.push(primitives::redpallas::VERIFIER.clone().oneshot(
                primitives::redpallas::Item::from_binding(
                    bvk,
                    orchard_shielded_data.binding_sig,
                    &shielded_sighash,
                ),
            ));
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
