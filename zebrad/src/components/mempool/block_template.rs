//! Mempool-owned mining templates and per-request payout overrides.
//!
//! Transactions come only from verified storage snapshots matching the private mining parent.
//! The retained build and look-ahead coinbase finish even when their parent changes, so tip churn
//! cannot detach CPU-heavy proof work.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout, Instant, Sleep},
};
use tower::{buffer::Buffer, util::BoxService, ServiceExt};

use zebra_chain::{
    amount::{Amount, NegativeOrZero, NonNegative},
    block::{self, Height, MAX_BLOCK_BYTES},
    parameters::{subsidy::nsm_reissuance_is_active, Network, NetworkUpgrade},
    serialization::{DateTime32, ZcashSerialize},
    transaction::VerifiedUnminedTx,
};
use zebra_node_services::mempool::TransactionDependencies;
use zebra_rpc::{
    fetch_mining_chain_info, nsm_value_balance_for_next_block, proposal_block_from_template,
    select_mempool_transactions, BlockTemplateRequest, BlockTemplateResponse, CoinbaseCache,
    LongPollInput, MinerParams, TransactionTemplate, MEMPOOL_LONG_POLL_INTERVAL,
};
use zebra_state::{ReadRequest, ReadResponse};

use crate::{components::sync::BLOCK_VERIFY_TIMEOUT, BoxError};

use super::storage::Storage;

#[cfg(test)]
mod tests;

pub(super) type ReadState = Buffer<BoxService<ReadRequest, ReadResponse, BoxError>, ReadRequest>;
pub(super) type BlockVerifier =
    Buffer<BoxService<zebra_consensus::Request, block::Hash, BoxError>, zebra_consensus::Request>;

type CoinbaseTask = (Height, JoinHandle<TransactionTemplate<NegativeOrZero>>);
type Snapshot = (block::Hash, Vec<VerifiedUnminedTx>, TransactionDependencies);

/// Delay after a transient state or consensus error in the current mining context.
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// Coalesce a burst without allowing continuous changes to postpone publication.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(500);

struct Build {
    future: BoxFuture<
        'static,
        (
            Result<Option<BlockTemplateResponse>, BoxError>,
            Option<CoinbaseTask>,
        ),
    >,
    request: Option<BlockTemplateRequest>,
    coinbase_only: bool,
    observed_tip: Option<block::Hash>,
    observed_generation: u64,
}

/// The only owner of template publication and its bounded proof work.
pub(super) struct BlockTemplates {
    network: Network,
    miner_params: Option<MinerParams>,
    read_state: ReadState,
    verifier: BlockVerifier,
    coinbase_cache: CoinbaseCache,
    mining_tip_change: zebra_state::MiningTipChange,
    mining_generation: u64,
    published: watch::Sender<Option<Arc<BlockTemplateResponse>>>,
    published_for_tip: Option<block::Hash>,
    requests: mpsc::Receiver<BlockTemplateRequest>,
    build: Option<Build>,
    deferred_request: Option<BlockTemplateRequest>,
    override_retry: Pin<Box<Sleep>>,
    next_coinbase: Option<CoinbaseTask>,
    refresh: Pin<Box<Sleep>>,
    debounce: Pin<Box<Sleep>>,
    dirty: bool,
    was_failing: bool,
}

impl BlockTemplates {
    pub(super) fn new(
        network: Network,
        miner_params: Option<MinerParams>,
        read_state: ReadState,
        verifier: BlockVerifier,
        mining_tip_change: zebra_state::MiningTipChange,
    ) -> (
        Self,
        watch::Receiver<Option<Arc<BlockTemplateResponse>>>,
        mpsc::Sender<BlockTemplateRequest>,
    ) {
        let (published, templates) = watch::channel(None);
        let (request_sender, requests) = mpsc::channel(1);
        (
            Self {
                network,
                miner_params,
                read_state,
                verifier,
                mining_tip_change,
                mining_generation: 0,
                coinbase_cache: CoinbaseCache::default(),
                published,
                published_for_tip: None,
                requests,
                build: None,
                deferred_request: None,
                override_retry: Box::pin(sleep(Duration::ZERO)),
                next_coinbase: None,
                refresh: Box::pin(sleep(Duration::ZERO)),
                debounce: Box::pin(sleep(Duration::ZERO)),
                dirty: true,
                was_failing: false,
            },
            templates,
            request_sender,
        )
    }

    #[cfg(test)]
    pub(super) fn mining_tip_change(&self) -> zebra_state::MiningTipChange {
        self.mining_tip_change.clone()
    }

    pub(super) fn mark_changed(&mut self) {
        if !self.dirty {
            self.debounce
                .as_mut()
                .reset(Instant::now() + CHANGE_DEBOUNCE);
        }
        self.dirty = true;
    }

    /// Poll owned work without withholding readiness from other mempool requests.
    pub(super) fn poll(
        &mut self,
        cx: &mut Context<'_>,
        storage: Option<(&Storage, block::Hash)>,
    ) -> Poll<Result<(), BoxError>> {
        if self.mining_tip_change.receiver.has_changed()? {
            // Forced equal-value notifications invalidate preparation too, including validation
            // completion and operator mutation. Never publish an older in-flight verdict.
            self.mining_generation = self.mining_generation.wrapping_add(1);
            self.mark_changed();
            // An error for the previous context must not delay work for the new parent.
            self.refresh.as_mut().reset(Instant::now());
        }
        let mining_parent = *self.mining_tip_change.receiver.borrow_and_update();
        let tip = mining_parent.map(|(_, hash)| hash);
        if let Some(build) = &mut self.build {
            let Poll::Ready((mut result, next_coinbase)) = build.future.poll_unpin(cx) else {
                return Poll::Ready(Ok(()));
            };
            let build = self
                .build
                .take()
                .expect("the completed build is still owned");
            self.next_coinbase = next_coinbase;
            let superseded =
                self.mining_generation != build.observed_generation || tip != build.observed_tip;
            if superseded {
                // Discard errors as well as successful verdicts for an obsolete mining context.
                result = Ok(None);
            }

            if result
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|template| {
                    !template.is_valid_for_tip(
                        template.previous_block_hash(),
                        &self.network,
                        DateTime32::now(),
                    )
                })
            {
                // Testnet's abbreviated standard-difficulty range expired during verification.
                // Retain proof ownership and refetch chain info before publishing any work.
                if let Some(request) = build.request {
                    self.deferred_request = Some(request);
                    self.override_retry.as_mut().reset(Instant::now());
                } else {
                    self.refresh.as_mut().reset(Instant::now());
                }
            } else if let Some(request) = build.request {
                match result {
                    Ok(Some(template)) => {
                        let _ = request.response.send(Ok(template));
                    }
                    // A superseded snapshot is retryable work, not an RPC failure.
                    Ok(None) => {
                        self.deferred_request = Some(request);
                        self.override_retry.as_mut().reset(Instant::now());
                    }
                    Err(error) => {
                        let _ = request.response.send(Err(error));
                    }
                }
            } else {
                match result {
                    Ok(Some(template)) => {
                        if self.was_failing {
                            tracing::info!("block template builds recovered");
                            self.was_failing = false;
                        }
                        let next_height = Height(template.height()).next().ok();
                        let now = DateTime32::now();
                        let refresh_after = if template.max_time() >= now {
                            (template.max_time().saturating_duration_since(now).to_std()
                                + Duration::from_secs(1))
                            .min(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL))
                        } else {
                            Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)
                        };
                        self.published.send_replace(Some(Arc::new(template)));
                        self.published_for_tip = tip;
                        self.refresh.as_mut().reset(Instant::now() + refresh_after);
                        if build.coinbase_only && self.dirty {
                            // Publish new-tip coinbase work first, then fill from the latest
                            // storage snapshot without delaying the initial fill.
                            self.refresh.as_mut().reset(Instant::now());
                        }
                        if let (Some(height), Some(miner_params)) =
                            (next_height, &self.miner_params)
                        {
                            start_precomputing_coinbase(
                                &mut self.next_coinbase,
                                &self.network,
                                miner_params,
                                height,
                            );
                        }
                    }
                    result => {
                        // Superseded snapshots need fresh work, not transient-error backoff.
                        let retry_delay = if result.is_ok() {
                            Duration::ZERO
                        } else {
                            RETRY_DELAY
                        };
                        if let Err(error) = result {
                            if self.was_failing {
                                tracing::debug!(?error, "failed to build a block template");
                            } else {
                                tracing::warn!(
                                    ?error,
                                    "failed to build a block template; retaining the last valid work"
                                );
                                self.was_failing = true;
                            }
                        }
                        self.dirty = self.mining_generation != build.observed_generation;
                        self.refresh.as_mut().reset(Instant::now() + retry_delay);
                    }
                }
            }
        }

        let needs_tip = self.published.borrow().as_ref().is_none_or(|template| {
            tip != self.published_for_tip
                && tip.is_some_and(|tip| template.previous_block_hash() != tip)
        });
        // Mining parents are announced after their state context is committed. A configured
        // regtest miner can run without enabled storage, but not without Canopy-capable context.
        // Foreground overrides still go through the normal chain-info and proposal error paths.
        let default_available = self.miner_params.is_some()
            && (storage.is_some() || self.network.is_regtest())
            && mining_parent.is_some_and(|(height, _)| {
                height.next().is_ok_and(|height| {
                    NetworkUpgrade::current(&self.network, height) >= NetworkUpgrade::Canopy
                })
            });
        // Poll even under override load: due refreshes and error recovery must make progress.
        let elapsed = self.refresh.as_mut().poll(cx).is_ready();
        let changed = self.dirty && self.debounce.as_mut().poll(cx).is_ready();
        let prioritize_default =
            default_available && (elapsed || ((needs_tip || changed) && !self.was_failing));

        // Bound override backlog independently of the transaction download queue. A cancelled
        // caller must not cause a new proof, but still let the next queued request make progress.
        let request = if prioritize_default {
            None
        } else if self.deferred_request.is_some()
            && self.override_retry.as_mut().poll(cx).is_ready()
        {
            self.deferred_request.take()
        } else if self.deferred_request.is_some() {
            // Keep the retained caller in its bounded retry slot.
            None
        } else {
            match self.requests.poll_recv(cx) {
                Poll::Ready(Some(request)) => Some(request),
                Poll::Ready(None) | Poll::Pending => None,
            }
        };
        if request
            .as_ref()
            .is_some_and(|request| request.response.is_closed())
        {
            cx.waker().wake_by_ref();
            return Poll::Ready(Ok(()));
        }
        let is_override = request.is_some();
        // Same-tip changes use the first change's debounce, independently of the backstop.
        if !is_override && !prioritize_default {
            return Poll::Ready(Ok(()));
        }

        let coinbase_only = !is_override && needs_tip;
        let fill_after_coinbase =
            coinbase_only && storage.is_some_and(|(storage, _)| !storage.transactions().is_empty());
        let snapshot = if coinbase_only {
            None
        } else {
            storage.map(|(storage, tip)| {
                (
                    tip,
                    storage.transactions().values().cloned().collect(),
                    storage.transaction_dependencies().clone(),
                )
            })
        };
        let miner_params = request
            .as_ref()
            .map(|request| request.miner_params.clone())
            .or_else(|| self.miner_params.clone())
            .expect("default builds require configured parameters; overrides supply their own");
        // Coinbase proofs are bound to payout, data, and memo. Overrides never share this cache
        // or consume the default miner's look-ahead proof.
        let coinbase_cache = if is_override {
            CoinbaseCache::default()
        } else {
            self.coinbase_cache.clone()
        };
        let next_coinbase = self.next_coinbase.take();
        self.build = Some(Build {
            future: build(
                self.network.clone(),
                miner_params,
                coinbase_cache,
                self.read_state.clone(),
                self.verifier.clone(),
                snapshot,
                next_coinbase,
                !is_override,
            )
            .boxed(),
            request,
            coinbase_only,
            observed_tip: tip,
            observed_generation: self.mining_generation,
        });
        if !is_override {
            self.dirty = fill_after_coinbase;
        }
        // A newly stored future has not registered any wakeups yet.
        cx.waker().wake_by_ref();
        Poll::Ready(Ok(()))
    }
}

/// Build and validate a snapshot, returning retained look-ahead work even on failure.
#[allow(clippy::too_many_arguments)]
async fn build<ReadStateService, Verifier>(
    network: Network,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    read_state: ReadStateService,
    verifier: Verifier,
    snapshot: Option<Snapshot>,
    mut next_coinbase: Option<CoinbaseTask>,
    use_precomputed_coinbase: bool,
) -> (
    Result<Option<BlockTemplateResponse>, BoxError>,
    Option<CoinbaseTask>,
)
where
    ReadStateService: zebra_state::ReadState,
    Verifier: zebra_consensus::router::service_trait::BlockVerifierService,
{
    let result = async {
        let chain_info = timeout(
            BLOCK_VERIFY_TIMEOUT,
            fetch_mining_chain_info(read_state.clone()),
        )
        .await??;
        let height = chain_info.tip_height.next()?;
        let parent_nsm_value_balance = nsm_value_balance_for_next_block(&network, &chain_info);
        coinbase_cache.select(height, parent_nsm_value_balance);
        if NetworkUpgrade::current(&network, height) < NetworkUpgrade::Canopy
            || (chain_info.chain_history_root.is_none()
                && NetworkUpgrade::Heartwood.activation_height(&network) != Some(height))
        {
            return Err("block templates require post-Canopy chain information".into());
        }
        let (transactions, dependencies) = match snapshot {
            Some((tip, transactions, dependencies)) if tip == chain_info.tip_hash => {
                (transactions, dependencies)
            }
            // Verified storage belongs to the ordinary chain, never a speculative parent.
            Some(_) | None => Default::default(),
        };
        let long_poll_id = LongPollInput::new(
            chain_info.tip_height,
            chain_info.tip_hash,
            chain_info.max_time,
            transactions.iter().map(|tx| tx.transaction.id),
        )
        .generate_id();
        if mining_tip_hash(read_state.clone()).await? != Some(chain_info.tip_hash) {
            return Ok(None);
        }
        // Selection measures coinbase resources without proving. Only queue the actual fee-paying
        // coinbase below, and bypass the blocking selection task for empty work.
        let (network, miner_params, selected, fees) = if transactions.is_empty() {
            (network, miner_params, Vec::new(), Amount::zero())
        } else {
            tokio::task::spawn_blocking(move || -> Result<_, BoxError> {
                let selected = select_mempool_transactions(
                    &network,
                    height,
                    &miner_params,
                    transactions,
                    dependencies,
                    parent_nsm_value_balance,
                );
                let fees = selected
                    .iter()
                    .map(|tx| tx.miner_fee)
                    .sum::<zebra_chain::amount::Result<Amount<NonNegative>>>()?;
                Ok((network, miner_params, selected, fees))
            })
            .await??
        };
        if use_precomputed_coinbase
            && parent_nsm_value_balance.is_none()
            && fees == Amount::<NonNegative>::zero()
        {
            store_precomputed_coinbase(&mut next_coinbase, height, &coinbase_cache).await;
        }
        let permit = if miner_params.has_shielded_component()
            && coinbase_cache
                .get(height, fees, parent_nsm_value_balance)
                .is_none()
        {
            Some(coinbase_cache.proof_permit().await)
        } else {
            None
        };
        if mining_tip_hash(read_state.clone()).await? != Some(chain_info.tip_hash) {
            return Ok(None);
        }

        // Await the blocking task even across tip changes; dropping it cannot cancel proving.
        let (template, proposal) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            // Prepare the actual coinbase fallibly before the response helper's cached path.
            if coinbase_cache
                .get(height, fees, parent_nsm_value_balance)
                .is_none()
            {
                coinbase_cache.store(
                    height,
                    fees,
                    parent_nsm_value_balance,
                    TransactionTemplate::new_coinbase_with_parent_pools(
                        &network,
                        height,
                        &miner_params,
                        fees,
                        parent_nsm_value_balance,
                    )?,
                );
            }
            let mut template = BlockTemplateResponse::from_transactions(
                &network,
                &coinbase_cache,
                &miner_params,
                &chain_info,
                long_poll_id,
                selected,
                None,
            );
            let mut proposal = proposal_block_from_template(&template, None, &network)?;
            // Preflight the PoW-agnostic family, not the deterministic helper's zero-nonce
            // final hash, which might already be committed or locally invalidated.
            // The normalized work ID still binds every other header and body byte.
            Arc::make_mut(&mut proposal.header).nonce = rand::random::<[u8; 32]>().into();
            if u64::try_from(proposal.zcash_serialized_size())? > MAX_BLOCK_BYTES {
                return Err::<_, BoxError>("block template exceeds the block size limit".into());
            }
            template.set_work_id(zebra_state::proposal_key(&proposal));
            Ok((template, Arc::new(proposal)))
        })
        .await??;

        let check = verifier.oneshot(zebra_consensus::Request::CheckProposal(proposal));
        tokio::pin!(check);
        let validity = match timeout(BLOCK_VERIFY_TIMEOUT, &mut check).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "waiting for timed-out template verification to finish"
                );
                // A timeout cannot cancel cryptographic work already dispatched by consensus.
                // Keep the one build slot occupied until that same request drains.
                let _ = check.await;
                Err(error.into())
            }
        };
        if mining_tip_hash(read_state).await? != Some(template.previous_block_hash()) {
            return Ok(None);
        }
        validity?;
        Ok(Some(template))
    }
    .await;
    (result, next_coinbase)
}

/// Use the private mining parent only for mining work, never transaction admission.
async fn mining_tip_hash<State: zebra_state::ReadState>(
    read_state: State,
) -> Result<Option<block::Hash>, BoxError> {
    let ReadResponse::Tip(tip) = timeout(
        BLOCK_VERIFY_TIMEOUT,
        read_state.oneshot(ReadRequest::MiningTip),
    )
    .await??
    else {
        unreachable!("state service returned the wrong response to a MiningTip request");
    };
    Ok(tip.map(|(_, hash)| hash))
}

/// Retain an unfinished proof even when a reorg changes the desired height.
fn start_precomputing_coinbase(
    next_coinbase: &mut Option<CoinbaseTask>,
    network: &Network,
    miner_params: &MinerParams,
    height: Height,
) {
    // A future parent's NSM balance is unknown until that parent is committed.
    // Keep any already-owned proof, but do not start parent-independent NSM work.
    if nsm_reissuance_is_active(height, network) {
        return;
    }
    if next_coinbase
        .as_ref()
        .is_some_and(|(precomputed_height, task)| {
            *precomputed_height == height || !task.is_finished()
        })
    {
        return;
    }
    let (network, miner_params) = (network.clone(), miner_params.clone());
    *next_coinbase = Some((
        height,
        tokio::spawn(async move {
            let permit = if miner_params.has_shielded_component() {
                Some(CoinbaseCache::default().proof_permit().await)
            } else {
                None
            };
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
                    .expect(
                        "configured miner parameters and the next height produce a valid coinbase",
                    )
            })
            .await
            .expect("the retained coinbase builder completes without panicking")
        }),
    ));
}

/// Only consume a look-ahead proof for its original height and miner parameters.
async fn store_precomputed_coinbase(
    next_coinbase: &mut Option<CoinbaseTask>,
    height: Height,
    coinbase_cache: &CoinbaseCache,
) {
    if next_coinbase
        .as_ref()
        .is_none_or(|(precomputed_height, _)| *precomputed_height != height)
    {
        return;
    }
    let (_, coinbase) = next_coinbase
        .take()
        .expect("the precomputed height was checked");
    match coinbase.await {
        Ok(coinbase) => coinbase_cache.store(height, Amount::zero(), None, coinbase),
        Err(error) => tracing::warn!(?error, "precomputed coinbase transaction task failed"),
    }
}
