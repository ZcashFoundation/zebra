//! Asynchronous verification of transactions.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use chrono::{DateTime, Utc};
use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use tokio::sync::oneshot;
use tower::{
    buffer::Buffer,
    timeout::{error::Elapsed, Timeout},
    util::BoxService,
    Service, ServiceExt,
};
use tracing::Instrument;

use zcash_protocol::value::ZatBalance;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block,
    parameters::{Network, NetworkUpgrade},
    primitives::Groth16Proof,
    serialization::DateTime32,
    transaction::{
        self, HashType, SigHash, Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx,
    },
    transparent,
};

use zebra_node_services::mempool;
use zebra_script::{CachedFfiTransaction, Sigops};
use zebra_state as zs;

use crate::{error::TransactionError, primitives, script, BoxError};

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

/// A timeout applied to output lookup requests sent to the mempool. This is shorter than the
/// timeout for the state UTXO lookups because a block is likely to be mined every 75 seconds
/// after Blossom is active, changing the best chain tip and requiring re-verification of transactions
/// in the mempool.
///
/// This is how long Zebra will wait for an output to be added to the mempool before verification
/// of the transaction that spends it will fail.
const MEMPOOL_OUTPUT_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long to wait after responding to a mempool request with a transaction that creates new
/// transparent outputs before polling the mempool service so that it will try adding the verified
/// transaction and responding to any potential `AwaitOutput` requests.
///
/// This should be long enough for the mempool service's `Downloads` to finish processing the
/// response from the transaction verifier.
const POLL_MEMPOOL_DELAY: std::time::Duration = Duration::from_millis(50);

/// Asynchronous verification of block transactions.
///
/// # Correctness
///
/// Transaction verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`router`](`crate::router`)
/// module documentation for details.
pub struct BlockTxVerifier<ZS> {
    network: Network,
    state: Timeout<ZS>,
    script_verifier: script::Verifier,
}

impl<ZS> BlockTxVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Creates a new block transaction verifier.
    pub fn new(network: &Network, state: ZS) -> Self {
        Self {
            network: network.clone(),
            state: Timeout::new(state, UTXO_LOOKUP_TIMEOUT),
            script_verifier: script::Verifier,
        }
    }
}

/// Asynchronous verification of mempool transactions.
///
/// # Correctness
///
/// Transaction verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`router`](`crate::router`)
/// module documentation for details.
pub struct MempoolTxVerifier<ZS, Mempool> {
    network: Network,
    state: Timeout<ZS>,
    mempool: Option<Timeout<Mempool>>,
    script_verifier: script::Verifier,
    mempool_setup_rx: oneshot::Receiver<Mempool>,
}

impl<ZS, Mempool> MempoolTxVerifier<ZS, Mempool>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    /// Creates a new mempool transaction verifier.
    pub fn new(network: &Network, state: ZS, mempool_setup_rx: oneshot::Receiver<Mempool>) -> Self {
        Self {
            network: network.clone(),
            state: Timeout::new(state, UTXO_LOOKUP_TIMEOUT),
            mempool: None,
            script_verifier: script::Verifier,
            mempool_setup_rx,
        }
    }
}

impl<ZS>
    MempoolTxVerifier<
        ZS,
        Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>,
    >
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Creates a new mempool transaction verifier for tests using a closed
    /// mempool setup channel receiver.
    #[cfg(test)]
    pub fn new_for_tests(network: &Network, state: ZS) -> Self {
        Self {
            network: network.clone(),
            state: Timeout::new(state, UTXO_LOOKUP_TIMEOUT),
            mempool: None,
            script_verifier: script::Verifier,
            mempool_setup_rx: oneshot::channel().1,
        }
    }
}

/// A request to verify a transaction as part of a block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockRequest {
    /// The mined transaction ID of `transaction`.
    /// Used for efficiency: callers should already have this,
    /// so no need to recompute it in the verifier.
    pub transaction_hash: transaction::Hash,
    /// The transaction itself.
    pub transaction: Arc<Transaction>,
    /// Additional UTXOs which are known at the time of verification.
    pub known_utxos: Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
    /// The height of the block containing this transaction.
    pub height: block::Height,
    /// The time that the block was mined.
    pub time: DateTime<Utc>,
}

/// A request to verify a transaction as part of the mempool.
///
/// Mempool transactions do not have any additional UTXOs.
///
/// Note: coinbase transactions are invalid in the mempool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MempoolRequest {
    /// The transaction itself.
    pub transaction: UnminedTx,
    /// The height of the next block.
    ///
    /// The next block is the first block that could possibly contain a
    /// mempool transaction.
    pub height: block::Height,
}

/// A response to a block transaction verification request.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockResponse {
    /// The witnessed transaction ID for this transaction.
    ///
    /// [`BlockResponse`] responses can be uniquely identified by
    /// [`UnminedTxId::mined_id`], because the block's authorizing data root
    /// will be checked during contextual validation.
    pub tx_id: UnminedTxId,

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
    pub miner_fee: Option<Amount<NonNegative>>,

    /// The total number of transparent signature operations counted for block
    /// verification in this transaction: legacy sigops plus P2SH sigops.
    ///
    /// This value is used to enforce the block-level `MAX_BLOCK_SIGOPS` limit.
    pub sigops: u32,
}

/// A response to a mempool transaction verification request.
#[derive(Clone, Debug, PartialEq)]
pub struct MempoolResponse {
    /// The full content of the verified mempool transaction.
    /// Also contains the transaction fee and other associated fields.
    ///
    /// Mempool transactions always have a transaction fee,
    /// because coinbase transactions are rejected from the mempool.
    ///
    /// [`MempoolResponse`] responses are uniquely identified by the
    /// [`UnminedTxId`] variant for their transaction version.
    pub transaction: VerifiedUnminedTx,

    /// A list of spent [`transparent::OutPoint`]s that were found in
    /// the mempool's list of `created_outputs`.
    ///
    /// Used by the mempool to determine dependencies between transactions
    /// in the mempool and to avoid adding transactions with missing spends
    /// to its verified set.
    pub spent_mempool_outpoints: Vec<transparent::OutPoint>,
}

#[cfg(any(test, feature = "proptest-impl"))]
impl From<VerifiedUnminedTx> for MempoolResponse {
    fn from(transaction: VerifiedUnminedTx) -> Self {
        MempoolResponse {
            transaction,
            spent_mempool_outpoints: Vec::new(),
        }
    }
}

impl<ZS> Service<BlockRequest> for BlockTxVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    type Response = BlockResponse;
    type Error = TransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Block verification has no deferred startup dependencies: all required state is
        // provided at construction, and any missing UTXOs are handled during request
        // processing via state lookups.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BlockRequest) -> Self::Future {
        let script_verifier = self.script_verifier;
        let network = self.network.clone();
        let state = self.state.clone();

        let tx = req.transaction.clone();
        // Reuse the caller's precomputed hash instead of calling `Transaction::unmined_id()`,
        // which would re-serialize and re-hash the whole transaction.
        // `Transaction::auth_digest()` returns `None` for exactly the versions that
        // `UnminedTxId::from(&Transaction)` maps to `Legacy` (v1-v4), and `Some` for those it
        // maps to `Witnessed` (v5 onward), so deriving the variant from it stays correct if a
        // later transaction version is added.
        let tx_id = match tx.auth_digest() {
            None => UnminedTxId::Legacy(req.transaction_hash),
            Some(auth_digest) => UnminedTxId::Witnessed(transaction::WtxId {
                id: req.transaction_hash,
                auth_digest,
            }),
        };
        let height = req.height;
        let time = req.time;
        let known_utxos = req.known_utxos.clone();
        let nu = NetworkUpgrade::current(&network, height);
        let span = tracing::debug_span!("tx", ?tx_id);

        async move {
            tracing::trace!(?tx_id, ?req, "got tx verify request");

            // Do quick checks first
            check_structure_and_network_rules(tx.as_ref(), height, &network)?;

            if tx.is_coinbase() {
                check::coinbase_tx_no_prevout_joinsplit_spend(&tx)?;
            } else if !tx.is_valid_non_coinbase() {
                return Err(TransactionError::NonCoinbaseHasCoinbaseInput);
            }

            // Validate `nExpiryHeight` consensus rules
            if tx.is_coinbase() {
                check::coinbase_expiry_height(&height, &tx, &network)?;
            } else {
                check::non_coinbase_expiry_height(&height, &tx)?;
            }

            // Transaction invariants that apply regardless of request type or transaction version.
            // These are pure consensus rules over the transaction structure and must always hold.
            check_transaction_invariants(tx.as_ref(), height, &network)?;

            tracing::trace!(?tx_id, "passed quick checks");

            // Block transactions are checked against the block's own time directly.
            check::lock_time_has_passed(&tx, height, time)?;

            // "The consensus rules applied to valueBalance, vShieldedOutput, and bindingSig
            // in non-coinbase transactions MUST also be applied to coinbase transactions."
            //
            // This rule is implicitly implemented during Sapling and Orchard verification,
            // because they do not distinguish between coinbase and non-coinbase transactions.
            //
            // Note: this rule originally applied to Sapling, but we assume it also applies to Orchard.
            //
            // https://zips.z.cash/zip-0213#specification

            // Load spent UTXOs from the block context and state.
            // The UTXOs are required for almost all the async checks.
            let (spent_utxos, spent_outputs) =
                Self::block_spent_utxos(tx.clone(), known_utxos, state.clone()).await?;

            let cached_ffi_transaction =
                Arc::new(CachedFfiTransaction::new(tx.clone(), Arc::new(spent_outputs), nu).map_err(|_| TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu))?);

            tracing::trace!(?tx_id, "got state UTXOs");

            // Select version-specific async verification pipeline
            let async_checks = dispatch_version_verification(
                tx.as_ref(),
                nu,
                script_verifier,
                cached_ffi_transaction.clone()
            )?;

            tracing::trace!(?tx_id, "awaiting async checks...");

            async_checks.check().await?;

            tracing::trace!(?tx_id, "finished async checks");

            let miner_fee = if tx.is_coinbase() {
                None
            } else {
                Some(miner_fee(tx.as_ref(), &spent_utxos)?)
            };
            let sigops = tx.sigops().map_err(zebra_script::Error::from)?;

            Ok(BlockResponse {
                tx_id,
                miner_fee,
                // In block validation, the consensus sigop total must include P2SH
                // redeem-script sigops, matching zcashd's `ConnectBlock` which sums
                // `GetLegacySigOpCount` and `GetP2SHSigOpCount` per transaction before
                // comparing against `MAX_BLOCK_SIGOPS`. Coinbase inputs contribute zero P2SH
                // sigops. See
                // <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jv4h-j224-23cc>.
                sigops: sigops.saturating_add(cached_ffi_transaction.p2sh_sigops()),
            })
        }
            .inspect(move |result| {
                // Hide the transaction data to avoid filling the logs
                tracing::trace!(?tx_id, result = ?result.as_ref().map(|_tx| ()), "got tx verify result");
            })
            .instrument(span)
            .boxed()
    }
}

impl<ZS> BlockTxVerifier<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Looks up UTXOs spent by `tx` from the best chain state, also checking
    /// `known_utxos` for UTXOs from earlier transactions in the same block.
    ///
    /// Returns an `OutPoint -> Utxo` map and a vec of `Output`s in the same
    /// order as the matching inputs in `tx`.
    async fn block_spent_utxos(
        tx: Arc<Transaction>,
        known_utxos: Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
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
        // Pre-allocate with None so we can fill each slot by input index, preserving input order.
        let mut spent_outputs: Vec<Option<transparent::Output>> = vec![None; inputs.len()];

        for (input_idx, input) in inputs.iter().enumerate() {
            if let transparent::Input::PrevOut { outpoint, .. } = input {
                tracing::trace!("awaiting outpoint lookup");

                let utxo = if let Some(output) = known_utxos.get(outpoint) {
                    tracing::trace!("UTXO in known_utxos, discarding query");
                    output.utxo.clone()
                } else {
                    let response = state
                        .clone()
                        .oneshot(zebra_state::Request::AwaitUtxo(*outpoint))
                        .await
                        .map_err(|boxed_error| match boxed_error.downcast::<Elapsed>() {
                            Ok(_) => TransactionError::TransparentInputNotFound,
                            Err(boxed_error) => TransactionError::from(boxed_error),
                        })?;

                    if let zebra_state::Response::Utxo(utxo) = response {
                        utxo
                    } else {
                        unreachable!("AwaitUtxo always responds with Utxo")
                    }
                };
                tracing::trace!(?utxo, "got UTXO");
                spent_outputs[input_idx] = Some(utxo.output.clone());
                spent_utxos.insert(*outpoint, utxo);
            }
        }

        let spent_outputs: Vec<transparent::Output> = spent_outputs.into_iter().flatten().collect();

        Ok((spent_utxos, spent_outputs))
    }
}

impl<ZS, Mempool> Service<MempoolRequest> for MempoolTxVerifier<ZS, Mempool>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    type Response = MempoolResponse;
    type Error = TransactionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Opportunistically install the mempool service once startup wiring provides it.
        // The verifier remains ready even before that happens: requests that require
        // mempool-only outputs will fail during verification if the mempool handle is
        // still unavailable.
        if self.mempool.is_none() {
            if let Ok(mempool) = self.mempool_setup_rx.try_recv() {
                self.mempool = Some(Timeout::new(mempool, MEMPOOL_OUTPUT_LOOKUP_TIMEOUT));
            }
        }

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: MempoolRequest) -> Self::Future {
        let script_verifier = self.script_verifier;
        let network = self.network.clone();
        let state = self.state.clone();
        let mempool = self.mempool.clone();

        let tx = req.transaction.transaction.clone();
        let tx_id = req.transaction.id;
        let height = req.height;
        let unmined_tx = req.transaction.clone();
        let nu = NetworkUpgrade::current(&network, height);
        let span = tracing::debug_span!("tx", ?tx_id);

        async move {
            tracing::trace!(?tx_id, ?req, "got tx verify request");

            // Do quick checks first
            check_structure_and_network_rules(tx.as_ref(), height, &network)?;

            // Validate the coinbase input consensus rules
            if tx.is_coinbase() {
                return Err(TransactionError::CoinbaseInMempool);
            }

            if !tx.is_valid_non_coinbase() {
                return Err(TransactionError::NonCoinbaseHasCoinbaseInput);
            }

            // Validate `nExpiryHeight` consensus rules
            check::non_coinbase_expiry_height(&height, &tx)?;

            // Transaction invariants that apply regardless of request type or transaction version.
            // These are pure consensus rules over the transaction structure and must always hold.
            check_transaction_invariants(tx.as_ref(), height, &network)?;

            tracing::trace!(?tx_id, "passed quick checks");

            // Mempool transactions are checked against the next median-time-past from state.
            Self::verify_mempool_lock_time(tx.as_ref(), height, state.clone()).await?;

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
            let (spent_utxos, spent_outputs, spent_mempool_outpoints) =
                Self::mempool_spent_utxos(tx.clone(), height, state.clone(), mempool.clone()).await?;

            // Mempool transactions have no block context, so there are no outputs from
            // earlier transactions in the same block to consider.
            check_maturity_height(tx.clone(), height, &network, &spent_utxos)?;

            // Reject non-standard input scripts (oversized or non-push-only
            // scriptSigs, and high-sigop P2SH redeem scripts) *before*
            // doing expensive script verification, to avoid DoS attacks on
            // the script interpreter.
            check::mempool_standard_input_scripts(tx.as_ref(), &spent_outputs)?;

            // Apply ZIP-317 policy before expensive cryptographic verification.
            let miner_fee = miner_fee(tx.as_ref(), &spent_utxos)?;
            let unpaid_actions = transaction::zip317::unpaid_actions(&unmined_tx, miner_fee);
            transaction::zip317::mempool_checks(unpaid_actions, miner_fee, unmined_tx.size)?;

            let cached_ffi_transaction =
                Arc::new(CachedFfiTransaction::new(tx.clone(), Arc::new(spent_outputs), nu).map_err(|_| TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu))?);

            tracing::trace!(?tx_id, "got state UTXOs");

            // Select version-specific async verification pipeline
            let mut async_checks = dispatch_version_verification(
                tx.as_ref(),
                nu,
                script_verifier,
                cached_ffi_transaction.clone()
            )?;

            let check_anchors_and_revealed_nullifiers_query = state
                .clone()
                .oneshot(zs::Request::CheckBestChainTipNullifiersAndAnchors(
                    unmined_tx.clone(),
                ))
                .map(|res| {
                    assert!(
                        res? == zs::Response::ValidBestChainTipNullifiersAndAnchors,
                        "unexpected response to CheckBestChainTipNullifiersAndAnchors request"
                    );
                    Ok(())
                });

            async_checks.push(check_anchors_and_revealed_nullifiers_query);

            tracing::trace!(?tx_id, "awaiting async checks...");

            async_checks.check().await?;

            tracing::trace!(?tx_id, "finished async checks");

            let sigops = tx.sigops().map_err(zebra_script::Error::from)?;

            // TODO: `spent_outputs` may not align with `tx.inputs()` when a transaction
            // spends both chain and mempool UTXOs (mempool outputs are appended last by
            // `mempool_spent_utxos()`), causing policy checks to pair the wrong input with
            // the wrong spent output.
            // https://github.com/ZcashFoundation/zebra/issues/10346
            let spent_outputs = cached_ffi_transaction.all_previous_outputs().clone();

            let transaction = VerifiedUnminedTx::new(
                unmined_tx,
                miner_fee,
                sigops,
                cached_ffi_transaction.p2sh_sigops(),
                spent_outputs.into(),
            )?;

            if let Some(mut mempool) = mempool {
                tokio::spawn(async move {
                    // Best-effort poll of the mempool to provide a timely response to
                    // `sendrawtransaction` RPC calls or `AwaitOutput` mempool calls.
                    tokio::time::sleep(POLL_MEMPOOL_DELAY).await;
                    let _ = mempool
                        .ready()
                        .await
                        .expect("mempool poll_ready() method should not return an error")
                        .call(mempool::Request::CheckForVerifiedTransactions)
                        .await;
                });
            }

            Ok(MempoolResponse { transaction, spent_mempool_outpoints })
        }
            .inspect(move |result| {
                // Hide the transaction data to avoid filling the logs
                tracing::trace!(?tx_id, result = ?result.as_ref().map(|_tx| ()), "got tx verify result");
            })
            .instrument(span)
            .boxed()
    }
}

impl<ZS, Mempool> MempoolTxVerifier<ZS, Mempool>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    /// Validates mempool lock-time consensus rules.
    ///
    /// Queries state only for time-based lock times.
    async fn verify_mempool_lock_time(
        tx: &Transaction,
        height: block::Height,
        state: Timeout<ZS>,
    ) -> Result<(), TransactionError> {
        // Skip the state query if we don't need the time for this check.
        let next_median_time_past = if tx.lock_time_is_time() {
            // This state query is much faster than loading UTXOs from the database,
            // so it doesn't need to be executed in parallel
            Some(
                Self::mempool_best_chain_next_median_time_past(state)
                    .await?
                    .to_chrono(),
            )
        } else {
            None
        };

        // This consensus check makes sure Zebra produces valid block templates.
        check::lock_time_has_passed(tx, height, next_median_time_past)?;

        Ok(())
    }

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

    /// Looks up UTXOs spent by a mempool `tx`, first querying the best chain state
    /// and then the mempool for inputs whose outputs are not present in the best chain.
    ///
    /// `height` is the next block height, used to construct `Utxo` values for
    /// outputs sourced from the mempool.
    ///
    /// Returns an `OutPoint -> Utxo` map, a vec of `Output`s in the same order
    /// as the matching inputs in `tx`, and a vec of `OutPoint`s that were
    /// sourced from the mempool rather than the best chain.
    async fn mempool_spent_utxos(
        tx: Arc<Transaction>,
        height: block::Height,
        state: Timeout<ZS>,
        mempool: Option<Timeout<Mempool>>,
    ) -> Result<
        (
            HashMap<transparent::OutPoint, transparent::Utxo>,
            Vec<transparent::Output>,
            Vec<transparent::OutPoint>,
        ),
        TransactionError,
    > {
        let inputs = tx.inputs();
        let mut spent_utxos = HashMap::new();
        // Pre-allocate with None so we can fill each slot by input index, preserving input order
        // even when chain and mempool UTXOs are fetched in separate passes.
        let mut spent_outputs: Vec<Option<transparent::Output>> = vec![None; inputs.len()];
        // Stores (input_idx, outpoint) for UTXOs not found in the best chain (fetched from mempool later).
        let mut spent_mempool_outpoints: Vec<(usize, transparent::OutPoint)> = Vec::new();

        for (input_idx, input) in inputs.iter().enumerate() {
            if let transparent::Input::PrevOut { outpoint, .. } = input {
                tracing::trace!("awaiting outpoint lookup");

                let query = state
                    .clone()
                    .oneshot(zs::Request::UnspentBestChainUtxo(*outpoint));

                let zebra_state::Response::UnspentBestChainUtxo(utxo) = query
                    .await
                    .map_err(|_| TransactionError::TransparentInputNotFound)?
                else {
                    unreachable!("UnspentBestChainUtxo always responds with Option<Utxo>")
                };

                let Some(utxo) = utxo else {
                    spent_mempool_outpoints.push((input_idx, *outpoint));
                    continue;
                };

                tracing::trace!(?utxo, "got UTXO");
                spent_outputs[input_idx] = Some(utxo.output.clone());
                spent_utxos.insert(*outpoint, utxo);
            }
        }

        if let Some(mempool) = mempool {
            for &(input_idx, spent_mempool_outpoint) in &spent_mempool_outpoints {
                let query = mempool
                    .clone()
                    .oneshot(mempool::Request::AwaitOutput(spent_mempool_outpoint));

                let output = match query.await {
                    Ok(mempool::Response::UnspentOutput(output)) => output,
                    Ok(_) => unreachable!("UnspentOutput always responds with UnspentOutput"),
                    Err(err) => {
                        return match err.downcast::<Elapsed>() {
                            Ok(_) => Err(TransactionError::TransparentInputNotFound),
                            Err(err) => Err(err.into()),
                        };
                    }
                };

                spent_outputs[input_idx] = Some(output.clone());
                spent_utxos.insert(
                    spent_mempool_outpoint,
                    // Assume the Utxo height will be next height after the best chain tip height
                    //
                    // # Correctness
                    //
                    // If the tip height changes while an unmined transaction is being verified,
                    // the transaction must be re-verified before being added to the mempool.
                    transparent::Utxo::new(output, height, false),
                );
            }
        } else if !spent_mempool_outpoints.is_empty() {
            return Err(TransactionError::TransparentInputNotFound);
        }

        // Convert back to return types; slots are in input order.
        let spent_outputs: Vec<transparent::Output> = spent_outputs.into_iter().flatten().collect();
        let spent_mempool_outpoints: Vec<transparent::OutPoint> = spent_mempool_outpoints
            .into_iter()
            .map(|(_, op)| op)
            .collect();

        Ok((spent_utxos, spent_outputs, spent_mempool_outpoints))
    }
}

/// Performs basic structural validation and Orchard-related network upgrade rules.
fn check_structure_and_network_rules(
    tx: &Transaction,
    height: block::Height,
    network: &Network,
) -> Result<(), TransactionError> {
    // The network upgrade active at this height is used by several of the checks below;
    // `NetworkUpgrade::current` rebuilds the activation-height map on each call, so compute it
    // once and share it rather than recomputing it per check.
    let network_upgrade = NetworkUpgrade::current(network, height);

    check::has_inputs_and_outputs(tx)?;
    check::has_enough_orchard_flags(tx)?;
    // NU6.3 / Ironwood flag rules (no-ops for pre-v6 transactions).
    check::has_enough_ironwood_flags(tx)?;
    check::orchard_cross_address_disabled(tx)?;
    // [NU6.3 onward] valueBalanceOrchard must be non-negative (Orchard pool frozen against new
    // inflows; see `orchard_value_balance_non_negative`).
    check::orchard_value_balance_non_negative(tx, network_upgrade)?;
    // [NU6.3 onward] Coinbase transactions must have an empty Orchard component (new shielded
    // coinbase value is routed to the Ironwood pool instead).
    check::coinbase_orchard_component_empty(tx, network_upgrade)?;
    check::consensus_branch_id(tx, height, network)?;

    // Soft fork: temporarily require transactions to not contain Orchard actions.
    //
    // This soft fork was added while NU 6.1 was the active epoch on the Zcash
    // chain, but we apply it uniformly even if NU 6.1 is not active in case it is
    // ported to other chains with a different sequence of NUs.
    //
    // This will be treated as "Rules that apply generally before the next NU"
    // when we add the NU that re-enables Orchard actions.
    if network.is_orchard_temporarily_disabled(height) && tx.orchard_shielded_data().is_some() {
        return Err(TransactionError::Other(
            "transaction has Orchard actions (temporarily disabled)".into(),
        ));
    }

    // From the network upgrade that re-enables Orchard actions (NU6.2), require
    // that any Orchard proof has the canonical length for its number of actions.
    // A proof that is present but not canonically sized can be padded with
    // arbitrary trailing data without affecting its validity, allowing excess
    // bandwidth and storage costs to be imposed while paying only fees sized to a
    // canonical proof (GHSA-jfw5-j458-pfv6).
    //
    // This is a constricting rule, so it is gated on that network upgrade:
    // Orchard actions mined before it, under earlier rules that did not enforce
    // the proof size, must remain valid so that nodes can sync and reindex the
    // chain before the soft fork that temporarily disabled Orchard. Orchard
    // bundles are deserialized leniently, so the size is checked here, where the
    // block height is available, rather than during parsing.
    //
    // The gate activates at the NU6.2 activation height committed in
    // MAINNET/TESTNET_ACTIVATION_HEIGHTS. See
    // `Network::orchard_canonical_proof_size_rule_active`.
    if network.orchard_canonical_proof_size_rule_active(height) {
        if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
            if !orchard_shielded_data.proof_size_is_canonical() {
                return Err(TransactionError::OrchardProofSize);
            }
        }
    }

    // The Ironwood bundle's Halo2 proof must also have a canonical size. Ironwood only exists
    // from NU6.3 onward (there is no legacy lenient period as there was for Orchard), so this is
    // enforced unconditionally whenever an Ironwood bundle is present. Like the Orchard bundle,
    // Ironwood bundles are deserialized leniently, so the size is checked here rather than during
    // parsing.
    if let Some(ironwood_shielded_data) = tx.ironwood_shielded_data() {
        if !ironwood_shielded_data.proof_size_is_canonical() {
            return Err(TransactionError::IronwoodProofSize);
        }
    }

    Ok(())
}

/// Validates transaction invariants.
fn check_transaction_invariants(
    tx: &Transaction,
    height: block::Height,
    network: &Network,
) -> Result<(), TransactionError> {
    // Consensus rule:
    //
    // > Either v_{pub}^{old} or v_{pub}^{new} MUST be zero.
    //
    // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
    check::joinsplit_has_vpub_zero(tx)?;

    // [Canopy onward]: `vpub_old` MUST be zero.
    // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
    check::disabled_add_to_sprout_pool(tx, height, network)?;

    check::spend_conflicts(tx)?;

    Ok(())
}

/// Checks that every transparent coinbase output spent by `tx` has matured
/// by `height`.
///
/// This check applies only to mempool transactions. Block transactions are
/// checked during contextual validation in the state (see #2336).
///
/// Calls [`check::tx_transparent_coinbase_spends_maturity`] with an empty
/// `block_new_outputs` map, since mempool transactions have no block context.
///
/// Returns `Ok(())` if every transparent coinbase output spent by the transaction is
/// mature and valid for the given height, or a [`TransactionError`] if the transaction
/// spends transparent coinbase outputs that are immature and invalid for the given height.
fn check_maturity_height(
    tx: Arc<Transaction>,
    height: block::Height,
    network: &Network,
    spent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
) -> Result<(), TransactionError> {
    check::tx_transparent_coinbase_spends_maturity(
        network,
        tx,
        height,
        Arc::new(HashMap::new()),
        spent_utxos,
    )
}

/// Dispatches version-specific async verification checks for `tx`.
///
/// `nu` is the network upgrade active at the transaction's verification height,
/// pre-computed by the caller using [`NetworkUpgrade::current`].
///
/// Returns [`TransactionError::WrongVersion`] for V1-V3 transactions, which
/// are not supported by any network upgrade Zebra verifies.
fn dispatch_version_verification(
    tx: &Transaction,
    nu: NetworkUpgrade,
    script_verifier: script::Verifier,
    cached_ffi_transaction: Arc<CachedFfiTransaction>,
) -> Result<AsyncChecks, TransactionError> {
    match tx {
        Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
            tracing::debug!(?tx, "got transaction with wrong version");
            Err(TransactionError::WrongVersion)
        }
        Transaction::V4 { joinsplit_data, .. } => verify_v4_transaction(
            tx,
            nu,
            script_verifier,
            cached_ffi_transaction,
            joinsplit_data,
        ),
        Transaction::V5 { .. } => {
            verify_v5_transaction(tx, nu, script_verifier, cached_ffi_transaction)
        }
        Transaction::V6 { .. } => {
            verify_v6_transaction(tx, nu, script_verifier, cached_ffi_transaction)
        }
    }
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
/// - the `tx` transaction to verify
/// - the `nu` network upgrade active at the transaction's verification height
/// - the `script_verifier` to use for verifying the transparent transfers
/// - the prepared `cached_ffi_transaction` used by the script verifier
/// - the Sprout `joinsplit_data` shielded data in the transaction
#[allow(clippy::unwrap_in_result)]
fn verify_v4_transaction(
    tx: &Transaction,
    nu: NetworkUpgrade,
    script_verifier: script::Verifier,
    cached_ffi_transaction: Arc<CachedFfiTransaction>,
    joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
) -> Result<AsyncChecks, TransactionError> {
    verify_v4_transaction_network_upgrade(tx, nu)?;

    let sapling_bundle = cached_ffi_transaction.sighasher().sapling_bundle();

    let sighash = cached_ffi_transaction
        .sighasher()
        .sighash(HashType::ALL, None);

    Ok(
        verify_transparent_inputs_and_outputs(tx, script_verifier, cached_ffi_transaction)?
            .and(verify_sprout_shielded_data(joinsplit_data, &sighash)?)
            .and(verify_sapling_bundle(sapling_bundle, &sighash)),
    )
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
        | NetworkUpgrade::Nu6_1
        | NetworkUpgrade::Nu6_2
        | NetworkUpgrade::Nu6_3 => Ok(()),

        #[cfg(zcash_unstable = "zfuture")]
        NetworkUpgrade::ZFuture => Ok(()),

        // Does not support V4 transactions
        NetworkUpgrade::Genesis
        | NetworkUpgrade::BeforeOverwinter
        | NetworkUpgrade::Overwinter
        | NetworkUpgrade::Nu7 => Err(TransactionError::UnsupportedByNetworkUpgrade(
            transaction.version(),
            network_upgrade,
        )),
    }
}

/// Verify a V5 transaction.
///
/// Returns a set of asynchronous checks that must all succeed for the transaction to be
/// considered valid. These checks include:
///
/// - transaction support by the selected network upgrade, as checked by
///   [`verify_v5_transaction_network_upgrade`]
/// - transparent transfers
/// - sapling shielded data (TODO)
/// - orchard shielded data (TODO)
///
/// The parameters of this method are:
///
/// - the `tx` transaction to verify
/// - the `nu` network upgrade active at the transaction's verification height
/// - the `script_verifier` to use for verifying the transparent transfers
/// - the prepared `cached_ffi_transaction` used by the script verifier
#[allow(clippy::unwrap_in_result)]
fn verify_v5_transaction(
    tx: &Transaction,
    nu: NetworkUpgrade,
    script_verifier: script::Verifier,
    cached_ffi_transaction: Arc<CachedFfiTransaction>,
) -> Result<AsyncChecks, TransactionError> {
    verify_v5_transaction_network_upgrade(tx, nu)?;

    let sapling_bundle = cached_ffi_transaction.sighasher().sapling_bundle();
    let orchard_bundle = cached_ffi_transaction.sighasher().orchard_bundle();

    let sighash = cached_ffi_transaction
        .sighasher()
        .sighash(HashType::ALL, None);

    Ok(
        verify_transparent_inputs_and_outputs(tx, script_verifier, cached_ffi_transaction)?
            .and(verify_sapling_bundle(sapling_bundle, &sighash))
            .and(verify_orchard_bundle(orchard_bundle, &sighash, nu)),
    )
}

/// Verifies if a V5 `transaction` is supported by `network_upgrade`.
fn verify_v5_transaction_network_upgrade(
    transaction: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> Result<(), TransactionError> {
    match network_upgrade {
        // Supports V5 transactions
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
        NetworkUpgrade::Nu5
        | NetworkUpgrade::Nu6
        | NetworkUpgrade::Nu6_1
        | NetworkUpgrade::Nu6_2
        | NetworkUpgrade::Nu6_3
        | NetworkUpgrade::Nu7 => Ok(()),

        #[cfg(zcash_unstable = "zfuture")]
        NetworkUpgrade::ZFuture => Ok(()),

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

/// Verifies a V6 (NU6.3 / Ironwood) transaction's shielded data.
///
/// Differs from [`verify_v5_transaction`] in the Orchard verifier: a v6 Orchard bundle
/// commits to the NU6.3 cross-address circuit, so it (and the Ironwood bundle) verify under the
/// NU6.3 key, not the v5 fixed key.
fn verify_v6_transaction(
    tx: &Transaction,
    nu: NetworkUpgrade,
    script_verifier: script::Verifier,
    cached_ffi_transaction: Arc<CachedFfiTransaction>,
) -> Result<AsyncChecks, TransactionError> {
    verify_v6_transaction_network_upgrade(tx, nu)?;

    let sapling_bundle = cached_ffi_transaction.sighasher().sapling_bundle();
    let orchard_bundle = cached_ffi_transaction.sighasher().orchard_bundle();
    let ironwood_bundle = cached_ffi_transaction.sighasher().ironwood_bundle();

    let sighash = cached_ffi_transaction
        .sighasher()
        .sighash(HashType::ALL, None);

    // The Ironwood bundle reuses the Orchard Action proof system and the NU6.3 circuit key, so
    // it is verified the same way as the v6 Orchard bundle (against the NU6.3 key).
    Ok(
        verify_transparent_inputs_and_outputs(tx, script_verifier, cached_ffi_transaction)?
            .and(verify_sapling_bundle(sapling_bundle, &sighash))
            .and(verify_orchard_v6_bundle(orchard_bundle, &sighash))
            .and(verify_orchard_v6_bundle(ironwood_bundle, &sighash)),
    )
}

/// Verifies that a V6 `transaction` is supported by `network_upgrade`.
///
/// V6 transactions are only valid from NU6.3 onward.
fn verify_v6_transaction_network_upgrade(
    transaction: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> Result<(), TransactionError> {
    match network_upgrade {
        NetworkUpgrade::Nu6_3 | NetworkUpgrade::Nu7 => Ok(()),

        #[cfg(zcash_unstable = "zfuture")]
        NetworkUpgrade::ZFuture => Ok(()),

        // V6 transactions are not valid before NU6.3.
        NetworkUpgrade::Genesis
        | NetworkUpgrade::BeforeOverwinter
        | NetworkUpgrade::Overwinter
        | NetworkUpgrade::Sapling
        | NetworkUpgrade::Blossom
        | NetworkUpgrade::Heartwood
        | NetworkUpgrade::Canopy
        | NetworkUpgrade::Nu5
        | NetworkUpgrade::Nu6
        | NetworkUpgrade::Nu6_1
        | NetworkUpgrade::Nu6_2 => Err(TransactionError::UnsupportedByNetworkUpgrade(
            transaction.version(),
            network_upgrade,
        )),
    }
}

/// Verifies if a transaction's transparent inputs are valid using the provided
/// `script_verifier` and `cached_ffi_transaction`.
///
/// Returns the asynchronous script verification checks for transparent inputs in `tx`.
fn verify_transparent_inputs_and_outputs(
    tx: &Transaction,
    script_verifier: script::Verifier,
    cached_ffi_transaction: Arc<CachedFfiTransaction>,
) -> Result<AsyncChecks, TransactionError> {
    if tx.is_coinbase() {
        // The script verifier only verifies PrevOut inputs and their corresponding UTXOs.
        // Coinbase transactions don't have any PrevOut inputs.
        Ok(AsyncChecks::new())
    } else {
        // feed all of the inputs to the script verifier
        let inputs = tx.inputs();

        let script_checks = (0..inputs.len())
            .map(move |input_index| {
                let request = script::Request {
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
                primitives::groth16::Item::from_joinsplit(joinsplit, &joinsplit_data.pub_key)?,
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
        let ed25519_item = (joinsplit_data.pub_key, joinsplit_data.sig, shielded_sighash).into();

        checks.push(ed25519_verifier.oneshot(ed25519_item));
    }

    Ok(checks)
}

/// Verifies a transaction's Sapling shielded data.
fn verify_sapling_bundle(
    bundle: Option<sapling_crypto::Bundle<sapling_crypto::bundle::Authorized, ZatBalance>>,
    sighash: &SigHash,
) -> AsyncChecks {
    let mut async_checks = AsyncChecks::new();

    // The Sapling batch verifier checks the following consensus rules:
    //
    // # Consensus
    //
    // > The proof π_ZKSpend MUST be valid given a primary input formed from the other fields
    // > except spendAuthSig.
    //
    // > The spend authorization signature MUST be a valid SpendAuthSig signature over SigHash
    // > using rk as the validating key.
    //
    // > [NU5 onward] As specified in § 5.4.7 ‘RedDSA, RedJubjub, and RedPallas’ on p. 88, the
    // > validation of the 𝑅 component of the signature changes to prohibit non-canonical
    // > encodings.
    //
    // https://zips.z.cash/protocol/protocol.pdf#spenddesc
    //
    // # Consensus
    //
    // > The proof π_ZKOutput MUST be valid given a primary input formed from the other fields
    // > except C^enc and C^out.
    //
    // https://zips.z.cash/protocol/protocol.pdf#outputdesc
    //
    // # Consensus
    //
    // > The Spend transfers and Action transfers of a transaction MUST be consistent with its
    // > vbalanceSapling value as specified in § 4.13 ‘Balance and Binding Signature (Sapling)’.
    //
    // https://zips.z.cash/protocol/protocol.pdf#spendsandoutputs
    //
    // # Consensus
    //
    // > [Sapling onward] If effectiveVersion ≥ 4 and nSpendsSapling + nOutputsSapling > 0,
    // > then:
    // >
    // > – let bvk^{Sapling} and SigHash be as defined in § 4.13;
    // > – bindingSigSapling MUST represent a valid signature under the transaction binding
    // >   validating key bvk Sapling of SigHash — i.e.
    // >   BindingSig^{Sapling}.Validate_{bvk^{Sapling}}(SigHash, bindingSigSapling ) = 1.
    //
    // Note that the `if` part is indirectly enforced, since the `sapling_shielded_data` is only
    // parsed if those conditions apply in [`Transaction::zcash_deserialize`].
    //
    // > [NU5 onward] As specified in § 5.4.7, the validation of the 𝑅 component of the
    // > signature changes to prohibit non-canonical encodings.
    //
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    if let Some(bundle) = bundle {
        async_checks.push(
            primitives::sapling::VERIFIER
                .clone()
                .oneshot(primitives::sapling::Item::new(bundle, *sighash)),
        );
    }

    async_checks
}

/// Verifies a **v5** transaction's Orchard bundle.
///
/// A v5 Orchard bundle commits to the Orchard Action circuit of the block's era, so the
/// verifying key is selected by `network_upgrade` via
/// [`primitives::halo2::orchard_v5_verifier_for`]: the historical insecure key before NU6.2, the
/// fixed key from NU6.2 until NU6.3, and the NU6.3 key from NU6.3 onward. The Orchard-pool
/// cross-address restriction applies to every Orchard Action from NU6.3 onward regardless of
/// transaction version (ZIP 229), so a v5 bundle at NU6.3 uses the NU6.3 circuit — the same key
/// as v6 Orchard and Ironwood bundles — not the fixed one.
fn verify_orchard_bundle(
    bundle: Option<::orchard::bundle::Bundle<::orchard::bundle::Authorized, ZatBalance>>,
    sighash: &SigHash,
    network_upgrade: NetworkUpgrade,
) -> AsyncChecks {
    queue_orchard_bundle(
        || primitives::halo2::orchard_v5_verifier_for(network_upgrade),
        bundle,
        sighash,
    )
}

/// Verifies a **v6** transaction's Orchard bundle.
///
/// A v6 Orchard bundle commits to the NU6.3 cross-address circuit, so it always verifies under
/// the NU6.3 key ([`primitives::halo2::orchard_v6_verifier`]), independent of the block's
/// network upgrade (v6 transactions only exist from NU6.3 onward). The Ironwood bundle reuses
/// the same verifier.
fn verify_orchard_v6_bundle(
    bundle: Option<::orchard::bundle::Bundle<::orchard::bundle::Authorized, ZatBalance>>,
    sighash: &SigHash,
) -> AsyncChecks {
    queue_orchard_bundle(primitives::halo2::orchard_v6_verifier, bundle, sighash)
}

/// Queues an Orchard-shaped bundle's single aggregated Halo2 proof against a verifier.
///
/// # Consensus
///
/// > The proof 𝜋 MUST be valid given a primary input (cv, rt^{Orchard}, nf, rk, cm_x,
/// > enableSpends, enableOutputs)
///
/// <https://zips.z.cash/protocol/protocol.pdf#actiondesc>
///
/// Unlike Sapling, Orchard shielded transactions have a single aggregated Halo2 proof per
/// transaction, even with multiple Actions, so it is queued for verification only once instead
/// of once per Action description. The choice of verifying key is the caller's; see
/// [`verify_orchard_bundle`] and [`verify_orchard_v6_bundle`].
///
/// `select_verifier` is only invoked when a bundle is present, so a bundle-less transaction
/// never forces the (lazily initialized) verifier services.
fn queue_orchard_bundle(
    select_verifier: impl FnOnce() -> &'static primitives::halo2::VerifierService,
    bundle: Option<::orchard::bundle::Bundle<::orchard::bundle::Authorized, ZatBalance>>,
    sighash: &SigHash,
) -> AsyncChecks {
    let mut async_checks = AsyncChecks::new();

    if let Some(bundle) = bundle {
        async_checks.push(
            select_verifier()
                .clone()
                .oneshot(primitives::halo2::Item::new(bundle, *sighash)),
        );
    }

    async_checks
}

/// Calculates the miner fee from the transaction's value balance.
fn miner_fee(
    tx: &Transaction,
    spent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
) -> Result<Amount<NonNegative>, TransactionError> {
    match tx.value_balance(spent_utxos) {
        Ok(value_balance) => value_balance
            .remaining_transaction_value()
            .map_err(|_| TransactionError::IncorrectFee),
        Err(_) => Err(TransactionError::IncorrectFee),
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
