//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use chrono::Utc;
use futures::stream::FuturesUnordered;
use futures_util::FutureExt;
use tokio::sync::{oneshot, watch, Semaphore};
use tokio_util::task::AbortOnDropHandle;
use tower::{util::BoxCloneService, Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    amount::Amount,
    block,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashSerialize,
    transaction::{UnminedTxId, WtxId},
    transparent,
};
use zebra_node_services::mempool;
use zebra_state as zs;

use crate::{error::*, transaction as tx, BoxError};

pub mod check;
mod error;
mod limits;
mod proposal;
pub mod request;

pub use error::VerifyBlockError;
pub use limits::{ShieldedActionCounts, MAX_BLOCK_SIGOPS};
// `block` is public under `fuzzing`, so keep the limits' paths it exposed before they moved.
#[cfg(feature = "fuzzing")]
pub use limits::{
    GLOBAL_SHIELDED_BUDGET, ORCHARD_BLOCK_ACTION_LIMIT, SAPLING_BLOCK_IO_LIMIT,
    SPROUT_BLOCK_JOIN_SPLIT_LIMIT,
};
pub use proposal::{DecodedBlock, PreparedBlockCache};
pub use request::Request;

use error::map_commit_error;
use proposal::{cache_proposal, normalized_proposal_header, proposal_key_and_size, ProposalCache};

#[cfg(test)]
mod tests;

/// Cache availability must not hold up block verification.
const MEMPOOL_REUSE_TIMEOUT: Duration = Duration::from_millis(10);

/// Asynchronous semantic block verification.
#[derive(Debug)]
pub struct SemanticBlockVerifier<S, V> {
    /// The network to be verified.
    network: Network,
    state_service: S,
    transaction_verifier: V,
    mempool: Option<BoxCloneService<mempool::Request, mempool::Response, BoxError>>,
    mempool_setup_rx:
        Option<oneshot::Receiver<BoxCloneService<mempool::Request, mempool::Response, BoxError>>>,
    /// Successful proposals, newest first; shared by all outstanding verification requests.
    proposals: Arc<watch::Sender<ProposalCache>>,
    /// Admission remains owned by blocking checks even if their requesting future is cancelled.
    blocking_checks: Arc<Semaphore>,
    #[cfg(test)]
    proposal_key_computations: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    block_size_serializations: Arc<std::sync::atomic::AtomicUsize>,
}

async fn run_blocking<T: Send + 'static>(
    slots: Arc<Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let permit = slots
        .acquire_owned()
        .await
        .expect("block-check admission is never closed");
    AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    }))
    .await
    .expect("block checks operate on decoded blocks")
}

/// Retry unavailable context only before scoring a negotiated body's post-admission failure.
async fn authenticate_early_relay<S>(
    state: S,
    block: Arc<block::Block>,
) -> Result<Option<zs::EarlyRelayGuard>, BoxError>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    tokio::time::timeout(Duration::from_secs(30), async move {
        loop {
            match state
                .clone()
                .oneshot(zs::Request::AuthenticateEarlyRelay(block.clone()))
                .await?
            {
                zs::Response::EarlyRelay(Some(guard)) => return Ok(Some(guard)),
                zs::Response::EarlyRelay(None) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => unreachable!("wrong response to early-relay authentication"),
            }
        }
    })
    .await
    .unwrap_or(Ok(None))
}

impl<S, V> SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    /// Creates a new SemanticBlockVerifier
    pub fn new(network: &Network, state_service: S, transaction_verifier: V) -> Self {
        Self {
            network: network.clone(),
            state_service,
            transaction_verifier,
            mempool: None,
            mempool_setup_rx: None,
            proposals: Arc::new(watch::channel(VecDeque::new()).0),
            blocking_checks: Arc::new(Semaphore::new(4)),
            #[cfg(test)]
            proposal_key_computations: Default::default(),
            #[cfg(test)]
            block_size_serializations: Default::default(),
        }
    }

    /// Enables optional reuse once the mempool service finishes starting.
    pub(crate) fn with_mempool(
        mut self,
        receiver: oneshot::Receiver<BoxCloneService<mempool::Request, mempool::Response, BoxError>>,
    ) -> Self {
        self.mempool_setup_rx = Some(receiver);
        self
    }

    /// Returns a read-only decoder sharing this verifier's successful proposal cache.
    pub fn prepared_blocks(&self) -> PreparedBlockCache {
        PreparedBlockCache(self.proposals.clone())
    }
}

impl<S, V> Service<Request> for SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyBlockError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We use the state for contextual verification, and we expect those
        // queries to be fast. So we don't need to call
        // `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Stamp the time the verifier received this block: the non-finalized state's
        // `Chain::cmp` uses it to prefer the first-received chain on equal-work ties.
        let received_time = Instant::now();
        if let Some(receiver) = self.mempool_setup_rx.as_mut() {
            match receiver.try_recv() {
                Ok(mempool) => {
                    self.mempool = Some(mempool);
                    self.mempool_setup_rx = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => self.mempool_setup_rx = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let mempool = self.mempool.clone();

        let mut state_service = self.state_service.clone();
        let mut transaction_verifier = self.transaction_verifier.clone();
        let network = self.network.clone();
        let proposals = self.proposals.clone();
        let blocking_checks = self.blocking_checks.clone();
        #[cfg(test)]
        let proposal_key_computations = self.proposal_key_computations.clone();
        #[cfg(test)]
        let block_size_serializations = self.block_size_serializations.clone();

        let block = request.block();

        // We don't include the block hash, because it's likely already in a parent span
        let span = tracing::debug_span!("block", height = ?block.coinbase_height());

        async move {
            let mut early_relay_authenticated = false;
            let mut early_relay = None;
            let mut pending_authentication = None;
            let result = async {
            let phase_start = Instant::now();
            let hash = block.hash();

            // Exact decoder matches avoid hashing the body again, but only while the matched
            // entry belongs to this verifier's cache. Evictions and foreign handles fall back.
            let decoded_match = match &request {
                Request::CommitDecoded(decoded, _) => {
                    decoded.matched.as_ref().and_then(|(entry, size)| {
                        proposals
                            .borrow()
                            .iter()
                            .find(|proposal| Arc::as_ptr(&proposal.contextual) == entry.as_ptr())
                            .map(|proposal| (proposal.contextual.clone(), *size))
                    })
                }
                _ => None,
            };

            // Check that this block is actually a new block. A solved proposal skips this state
            // round trip: its commit reports a duplicate hash or height itself.
            if decoded_match.is_none() {
                tracing::trace!("checking that block is not already in state");
                match state_service
                    .ready()
                    .await
                    .map_err(|source| VerifyBlockError::Depth { source, hash })?
                    .call(zs::Request::KnownBlock(hash))
                    .await
                    .map_err(|source| VerifyBlockError::Depth { source, hash })?
                {
                    zs::Response::KnownBlock(Some(location)) => {
                        return Err(BlockError::AlreadyInChain(hash, location).into())
                    }
                    zs::Response::KnownBlock(None) => {}
                    _ => unreachable!("wrong response to Request::KnownBlock"),
                }
            }

            tracing::trace!("performing block checks");
            let height = block
                .coinbase_height()
                .ok_or(BlockError::MissingHeight(hash))?;

            // Zebra does not support heights greater than
            // [`block::Height::MAX`].
            if height > block::Height::MAX {
                Err(BlockError::MaxHeight(height, hash, block::Height::MAX))?;
            }

            // > The block data MUST be validated and checked against the server's usual
            // > acceptance rules (excluding the check for a valid proof-of-work).
            // <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
            let header = block.header.clone();
            let pow_network = network.clone();
            let skip_pow = request.is_proposal() || network.disable_pow();
            run_blocking(blocking_checks.clone(), move || {
                if skip_pow {
                    check::difficulty_threshold_is_valid(&header, &pow_network, &height, &hash)?;
                } else {
                    check::difficulty_is_valid(&header, &pow_network, &height, &hash)?;
                    check::equihash_solution_is_valid(&header)?;
                }
                Ok::<_, VerifyBlockError>(())
            })
            .await?;

            // Hash only when populating the cache or when an unmatched block could reuse it.
            let proposal_key = if decoded_match.is_none()
                && (request.is_proposal() || {
                    let header = normalized_proposal_header(&block.header);
                    proposals.borrow().iter().any(|proposal| {
                        normalized_proposal_header(&proposal.block.header) == header
                    })
                })
            {
                let block = block.clone();
                Some(
                    AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
                        #[cfg(test)]
                        proposal_key_computations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        proposal_key_and_size(&block)
                    }))
                    .await
                    .expect("proposal hashing serializes an already-decoded block into an infallible writer"),
                )
            } else {
                None
            };
            let size = decoded_match
                .as_ref()
                .map(|(_, size)| *size)
                .or_else(|| proposal_key.map(|(_, size)| size))
                .or(match &request {
                    Request::CommitDecoded(decoded, _) => decoded.consumed_size,
                    _ => None,
                });
            if size.is_some_and(|size| {
                u64::try_from(size).expect("block sizes fit u64") > block::MAX_BLOCK_BYTES
            }) {
                return Err(VerifyBlockError::OversizedBlock);
            }
            let cached = decoded_match.map(|(contextual, _)| contextual).or_else(|| {
                let (key, _) = proposal_key.filter(|_| !request.is_proposal())?;
                proposals
                    .borrow()
                    .iter()
                    .find(|proposal| {
                        proposal.key == key
                            && proposal.block.header.previous_block_hash
                                == block.header.previous_block_hash
                    })
                    .map(|proposal| proposal.contextual.clone())
            });

            // Cache hits already bind the Merkle root through exact bytes or the proposal key.
            // On misses, bind the body before announcing early work and reuse these hashes below.
            let transaction_hashes = if cached.is_none() {
                let block = block.clone();
                let network = network.clone();
                Some(run_blocking(blocking_checks, move || {
                    if size.is_none() {
                        #[cfg(test)]
                        block_size_serializations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if u64::try_from(block.zcash_serialized_size()).unwrap_or(u64::MAX)
                            > block::MAX_BLOCK_BYTES
                        {
                            return Err(VerifyBlockError::OversizedBlock);
                        }
                    }
                    let hashes: Arc<[_]> = block.transactions.iter().map(|t| t.hash()).collect();
                    check::merkle_root_validity(&network, &block, &hashes)?;
                    Ok::<_, VerifyBlockError>(hashes)
                }).await?)
            } else {
                None
            };

            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Quick field validity and structure checks
            let coinbase_tx = check::coinbase_is_first(&block)?;

            // From the NSM reissuance height, the block subsidy depends on the
            // NSM value balance after the parent block, which is not committed yet while
            // blocks are verified concurrently. The state checks the subsidy, funding streams,
            // and miner fees during contextual validation instead, where the parent's chain value
            // pools are known.
            let nsm_reissuance_is_active =
                zebra_chain::parameters::subsidy::nsm_reissuance_is_active(height, &network);

            let expected_subsidy_and_deferred_pool_balance_change = if nsm_reissuance_is_active {
                None
            } else {
                let expected_block_subsidy =
                    zebra_chain::parameters::subsidy::block_subsidy(height, &network)?;

                // See [ZIP-1015](https://zips.z.cash/zip-1015).
                let deferred_pool_balance_change =
                    check::subsidy_is_valid(&block, &network, expected_block_subsidy)?;

                Some((expected_block_subsidy, deferred_pool_balance_change))
            };

            // Bound shielded work before output recovery or proof verification.
            check::shielded_action_limits_are_valid(&block, &network, height, hash)?;
            let require_authentication = matches!(&request, Request::CommitEarlyRelay(_))
                && NetworkUpgrade::current(&network, height) >= NetworkUpgrade::Nu5;
            // A solved proposal is advertised through its validated-commit notification and
            // commits within milliseconds, so early relay is not worth its state round trip.
            let solved_proposal = cached.is_some() && !require_authentication;
            early_relay = if request.is_proposal()
                || solved_proposal
                || NetworkUpgrade::current(&network, height) < NetworkUpgrade::Nu5
            {
                None
            } else {
                match state_service.clone().oneshot(zs::Request::EarlyRelay {
                    block: block.clone(),
                    require_authentication,
                }).await {
                    Ok(zs::Response::EarlyRelay(guard)) => {
                        // Negotiation identifies capability, not whether this body was committed.
                        // Keep verifying without parent context; authenticate before scoring a
                        // later punitive failure instead of retrying the whole verification.
                        early_relay_authenticated = require_authentication && guard.is_some();
                        if require_authentication && guard.is_none() {
                            pending_authentication = Some((state_service.clone(), block.clone()));
                        }
                        guard
                    }
                    Err(source) if require_authentication => {
                        return Err(match source.downcast::<zs::ValidateContextError>() {
                            Ok(source) => VerifyBlockError::EarlyRelayAdmission(*source),
                            Err(source) => VerifyBlockError::StateService { source, hash },
                        });
                    }
                    _ => None,
                }
            };
            if require_authentication
                && early_relay.as_ref().is_some_and(zs::EarlyRelayGuard::is_rejected)
            {
                return Err(VerifyBlockError::EarlyRelayRejected);
            }

            // Announce early work on this block. Its guard is awaited after the commit, so the
            // announcement does not delay the commit and the work stays alive while it runs.
            let mining_candidate = if request.is_proposal() {
                None
            } else {
                let mut state_service = state_service.clone();
                match state_service.ready().await {
                    Ok(state_service) => {
                        Some(state_service.call(zs::Request::MiningCandidate(block.clone())))
                    }
                    Err(_) => None,
                }
            };
            // Dropping the guard withdraws the work, so it is held until the commit finishes.
            let hold_mining_candidate = async {
                match mining_candidate {
                    Some(announcement) => announcement.await.ok(),
                    None => None,
                }
            };

            if let Some(cached) = cached {
                // Cache hits skip transaction verification, so record their checks and commit
                // under the same phases as other blocks.
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "block_checks"
                )
                .record(phase_start.elapsed().as_secs_f64());
                let phase_start = Instant::now();
                let mut contextual = (*cached).clone();
                contextual.rebind(block, received_time);
                let commit = match request {
                    Request::CommitMined(_, notifications)
                    | Request::CommitDecoded(_, Some(notifications)) => {
                        contextual.into_mined_request(notifications)
                    }
                    Request::Commit(_) | Request::CommitEarlyRelay(_) | Request::CommitDecoded(_, None) => {
                        zs::Request::CommitContextuallyVerifiedBlock(Box::new(contextual))
                    }
                    Request::CheckProposal(_) => unreachable!("proposals never reuse the cache"),
                };
                let (result, _mining_candidate) =
                    tokio::join!(state_service.oneshot(commit), hold_mining_candidate);
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "state_commit"
                )
                .record(phase_start.elapsed().as_secs_f64());
                return match result {
                    Ok(zs::Response::Committed(committed_hash)) => {
                        assert_eq!(committed_hash, hash, "state must commit correct hash");
                        Ok(hash)
                    }
                    Err(source) => Err(map_commit_error(source, hash)),
                    _ => unreachable!("wrong response for contextual block commit"),
                };
            }

            let transaction_hashes =
                transaction_hashes.expect("cache hits return after committing the block");


            // Now do the slower checks

            // Check compatibility with ZIP-212 shielded Sapling and Orchard coinbase output decryption
            tx::check::coinbase_outputs_are_decryptable(&coinbase_tx, &network, height)?;

            if !request.is_proposal() {
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "block_checks"
                )
                .record(phase_start.elapsed().as_secs_f64());
            }
            let phase_start = Instant::now();

            // Query only this block's candidates, against the mempool's processed tip.
            let candidates = if let Some(mempool) = mempool {
                match tokio::time::timeout(
                    MEMPOOL_REUSE_TIMEOUT,
                    mempool.oneshot(mempool::Request::VerifiedTransactionsForBlock {
                        transaction_hashes: transaction_hashes.clone(),
                        parent_hash: block.header.previous_block_hash,
                    }),
                ).await {
                    Ok(Ok(mempool::Response::VerifiedTransactionsForBlock(transactions))) => {
                        transactions
                    }
                    _ => HashMap::new(),
                }
            } else {
                HashMap::new()
            };
            let verified = if candidates.is_empty() {
                HashMap::new()
            } else {
                let block = block.clone();
                let transaction_hashes = transaction_hashes.clone();
                let network = network.clone();
                tokio::task::spawn_blocking(move || {
                    let mut candidates = candidates;
                    let upgrade = NetworkUpgrade::current(&network, height);
                    for (&hash, transaction) in transaction_hashes.iter().zip(&block.transactions) {
                        let Some(candidate) = candidates.get(&hash) else { continue };
                        // Admission records the tip height only after confirming it still matches
                        // the verification request. Older entries are safe only within that epoch.
                        let same_epoch = candidate.height
                            .and_then(|tip| tip + 1)
                            .is_some_and(|height| NetworkUpgrade::current(&network, height) == upgrade);
                        if transaction.is_coinbase() || !same_epoch {
                            candidates.remove(&hash);
                            continue;
                        }
                        let id = match transaction.auth_digest() {
                            None => UnminedTxId::Legacy(hash),
                            Some(auth_digest) => UnminedTxId::Witnessed(WtxId { id: hash, auth_digest }),
                        };
                        if id != candidate.transaction.id {
                            candidates.remove(&hash);
                            continue;
                        }
                        // Cached proofs and scripts do not establish expiry or block lock time.
                        tx::check_cached_transaction(transaction, height, block.header.time, &network)?;
                    }
                    Ok::<_, TransactionError>(candidates)
                }).await.expect("candidate checks operate on decoded transactions")?
            };

            // Send transactions to the transaction verifier to be checked
            let mut async_checks = FuturesUnordered::new();

            let known_utxos = Arc::new(transparent::new_ordered_outputs(
                &block,
                &transaction_hashes,
            ));
            let mut batch_flush = None;
            let mut sigops = 0;
            let mut block_miner_fees = Ok(Amount::zero());

            // Dispatch shielded transactions first: their proof batches are the longest checks, so
            // starting them before the transparent checks shortens the block's critical path.
            let transactions = || transaction_hashes.iter().zip(block.transactions.iter());
            for (&transaction_hash, transaction) in transactions()
                .filter(|(_, tx)| tx.has_shielded_data())
                .chain(transactions().filter(|(_, tx)| !tx.has_shielded_data()))
            {
                if let Some(cached) = verified.get(&transaction_hash) {
                    sigops += cached.block_sigop_count();
                    block_miner_fees += cached.miner_fee;
                    continue;
                }
                if transaction.has_shielded_data() && batch_flush.is_none() {
                    let shielded_transactions = transactions()
                        .filter(|(hash, tx)| tx.has_shielded_data() && !verified.contains_key(hash))
                        .count();
                    batch_flush = Some(crate::primitives::flush::BlockFlush::register(
                        &known_utxos,
                        shielded_transactions,
                    ));
                }
                let rsp = transaction_verifier
                    .ready()
                    .await
                    .expect("transaction verifier is always ready")
                    .call(tx::BlockRequest {
                        transaction_hash,
                        transaction: transaction.clone(),
                        known_utxos: known_utxos.clone(),
                        height,
                        time: block.header.time,
                    });
                async_checks.push(rsp);
            }
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            // Get the transaction results back from the transaction verifier.

            // Sum up some block totals from the transaction responses.
            let mut transaction_error = None;

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                let response = match result {
                    Ok(response) if transaction_error.is_none() => response,
                    Ok(_) => continue,
                    Err(error) => {
                        // Dropping sibling futures would not stop their dispatched CPU work.
                        // Keep the first error and drain the existing checks before returning it.
                        if transaction_error.is_none() {
                            transaction_error = Some(error);
                        }
                        continue;
                    }
                };

                sigops += response.sigops;

                // Coinbase transactions consume the miner fee,
                // so they don't add any value to the block's total miner fee.
                if let Some(miner_fee) = response.miner_fee {
                    block_miner_fees += miner_fee;
                }
            }
            // Remove the address-keyed registration before releasing the shared UTXO allocation.
            drop(batch_flush);
            if let Some(error) = transaction_error {
                return Err(VerifyBlockError::Transaction(error.into()));
            }

            if !request.is_proposal() {
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "transaction_verification"
                )
                .record(phase_start.elapsed().as_secs_f64());
            }

            // Check the summed block totals

            if sigops > MAX_BLOCK_SIGOPS {
                Err(BlockError::TooManyTransparentSignatureOperations {
                    height,
                    hash,
                    sigops,
                })?;
            }

            let block_miner_fees =
                block_miner_fees.map_err(|amount_error| BlockError::SummingMinerFees {
                    height,
                    hash,
                    source: amount_error,
                })?;

            if let Some((expected_block_subsidy, deferred_pool_balance_change)) =
                expected_subsidy_and_deferred_pool_balance_change
            {
                check::miner_fees_are_valid(
                    &coinbase_tx,
                    height,
                    block_miner_fees,
                    expected_block_subsidy,
                    deferred_pool_balance_change,
                    &network,
                )?;
            }

            // Finally, submit the block for contextual verification.
            let new_outputs = Arc::into_inner(known_utxos)
                .expect("all verification tasks using known_utxos are complete");

            let prepared_block = zs::SemanticallyVerifiedBlock {
                block,
                hash,
                height,
                new_outputs,
                transaction_hashes,
                received_time: Some(received_time),
            };

            // Return early for proposal requests.
            if request.is_proposal() {
                let block = prepared_block.block.clone();
                return match state_service
                    .ready()
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                    .call(zs::Request::CheckBlockProposalValidity(prepared_block))
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                {
                    zs::Response::ValidBlockProposal(contextual) => {
                        let (key, size) = proposal_key.expect("proposals always have a cache key");
                        let transaction_bytes = size - block.header.zcash_serialized_size();
                        cache_proposal(&proposals, key, block, transaction_bytes, contextual).await;
                        Ok(hash)
                    }
                    _ => unreachable!("wrong response for CheckBlockProposalValidity"),
                };
            }
            let commit = match request {
                Request::CommitMined(_, notifications)
                | Request::CommitDecoded(_, Some(notifications)) => zs::Request::CommitMinedBlock {
                    block: prepared_block,
                    contextual: None,
                    notifications,
                },
                Request::Commit(_) | Request::CommitEarlyRelay(_) | Request::CommitDecoded(_, None) => {
                    zs::Request::CommitSemanticallyVerifiedBlock(prepared_block)
                }
                Request::CheckProposal(_) => unreachable!("proposals return after validation"),
            };

            let phase_start = Instant::now();
            let (result, _mining_candidate) = tokio::join!(
                async {
                    state_service
                        .ready()
                        .await
                        .map_err(|source| VerifyBlockError::StateService { source, hash })?
                        .call(commit)
                        .await
                        .map_err(|source| map_commit_error(source, hash))
                },
                hold_mining_candidate
            );
            metrics::histogram!(
                "zebra.consensus.block.phase.duration_seconds",
                "phase" => "state_commit"
            )
            .record(phase_start.elapsed().as_secs_f64());

            match result {
                Ok(zs::Response::Committed(committed_hash)) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    Ok(hash)
                }

                Err(error) => Err(error),

                _ => unreachable!("wrong response for semantic block commit"),
            }
            }.await;
            let Err(error) = result else {
                return result;
            };
            if error.misbehavior_score() != 0 && !error.is_auth_commitment_mismatch() {
                if let Some((state, block)) = pending_authentication {
                    let hash = block.hash();
                    match authenticate_early_relay(state, block).await {
                        Ok(Some(guard)) => {
                            early_relay_authenticated = true;
                            early_relay = Some(guard);
                        }
                        Ok(None) => {
                            return Err(VerifyBlockError::EarlyRelayUnavailable(Box::new(error)));
                        }
                        Err(source) => {
                            return Err(match source.downcast::<zs::ValidateContextError>() {
                                Ok(source) => VerifyBlockError::EarlyRelayAdmission(*source),
                                Err(source) => VerifyBlockError::StateService { source, hash },
                            });
                        }
                    }
                }
            }
            if let Some(guard) = early_relay.as_ref().filter(|_| error.is_deterministic_rejection()) {
                guard.reject();
            }
            Err(error.for_early_relay(early_relay_authenticated))
        }
        .instrument(span)
        .boxed()
    }
}
