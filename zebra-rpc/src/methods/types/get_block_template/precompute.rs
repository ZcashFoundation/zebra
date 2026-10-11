//! A precomputed block template for the `getblocktemplate` RPC.
//!
//! Miners call `getblocktemplate` far more often than the chain tip or the mempool change, and
//! building a template needs a state read, a mempool read, ZIP-317 transaction selection, and a
//! coinbase transaction, which re-runs a shielded proof when the miner address has a shielded
//! component. So [`run()`] keeps a template for the current chain tip ready in a [`TemplateCache`],
//! and the RPC only has to check that the template still extends the tip.
//!
//! Mempool changes trigger a throttled rebuild; the periodic refresh remains a backstop. The RPC
//! ignores a template whose previous block hash isn't the current tip, and [`run()`] publishes a
//! coinbase-only template for a new tip as soon as it sees one.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use jsonrpsee::{core::RpcResult, types::ErrorObjectOwned};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
    time::{sleep, sleep_until, Instant},
};

use tower::ServiceExt;

use zebra_chain::{
    amount::{Amount, NegativeOrZero},
    block::{self, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    transaction::UnminedTxId,
    work::difficulty::ParameterDifficulty,
};
use zebra_node_services::mempool::{self, MempoolChange, MempoolService};
use zebra_state::{ReadRequest, ReadResponse, ReadState};

use crate::{
    methods::types::{long_poll::LongPollInput, transaction::TransactionTemplate},
    server::error::MapError,
};

use super::{
    check_synced_to_tip, constants::MEMPOOL_LONG_POLL_INTERVAL, fetch_chain_info,
    fetch_mempool_transactions, BlockTemplateResponse, CoinbaseCache, MinerParams,
};

#[cfg(test)]
mod tests;

/// Maximum duration of one cache/state wait before rechecking synchronization and updater ownership.
pub(crate) const NEW_TIP_TIMEOUT: Duration = Duration::from_secs(1);

/// The same bounded cache wait for a miner address with a shielded component.
pub(crate) const SHIELDED_NEW_TIP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `getblocktemplate` waits for [`run()`] to publish a template for the current chain
/// tip, when it pays `miner_params`.
pub(crate) fn new_tip_timeout(miner_params: &MinerParams) -> Duration {
    if miner_params.has_shielded_component() {
        SHIELDED_NEW_TIP_TIMEOUT
    } else {
        NEW_TIP_TIMEOUT
    }
}

/// How long [`run()`] waits before retrying, when Zebra isn't synced to the chain tip, or the state
/// and the mempool disagree about the tip.
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Bounds state/mempool snapshot reads, without cancelling a started coinbase proof.
pub(crate) const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum pause after event-driven builds, so idle arrivals start immediately and bursts
/// coalesce without extending the window. Slow builds wait at least their own duration.
pub(crate) const MEMPOOL_THROTTLE: Duration = Duration::from_millis(100);

/// Reading inputs owns no proof or mutex permit.
const BUILD_FETCHING: u8 = 0;
/// An unpolled mutex waiter can reserve the next permit, so it cannot be retained across replacement.
const BUILD_WAITING_FOR_PERMIT: u8 = 1;
/// Blocking work owns its permit independently and must be allowed to finish.
const BUILD_STARTED: u8 = 2;

/// A block template for the block after the current chain tip, shared between [`run()`] and the
/// `getblocktemplate` RPC.
#[derive(Clone)]
pub(crate) struct TemplateCache(Arc<watch::Sender<Option<TemplateState>>>, Arc<AtomicBool>);

struct TemplateState {
    template: Option<Arc<BlockTemplateResponse>>,
    published_at: Instant,
    error: Option<(Option<block::Hash>, ErrorObjectOwned)>,
}

impl Default for TemplateCache {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(None)), Arc::default())
    }
}

/// Releases updater ownership and wakes requests if the updater stops or is cancelled.
struct UpdaterGuard(TemplateCache);

impl Drop for UpdaterGuard {
    fn drop(&mut self) {
        self.0 .1.store(false, Ordering::Release);
        self.0 .0.send_modify(|_| {});
    }
}

/// A subscription to the templates [`run()`] publishes.
pub(crate) struct TemplateChanges(watch::Receiver<Option<TemplateState>>);

impl TemplateChanges {
    /// Waits for a template published since this subscription was created, or since the last wait
    /// returned.
    ///
    /// Returns immediately if one was published in the meantime, so a caller that reads the cache
    /// and then waits cannot miss the publish in between.
    pub(crate) async fn changed(&mut self) {
        if self.0.changed().await.is_err() {
            // No sender, so no template will ever be published. Waiting forever lets the caller's
            // other wait conditions drive it, where returning would spin it.
            std::future::pending::<()>().await
        }
    }

    /// Consumes publications observed while waiting for another fallback build.
    pub(crate) fn take_changed(&mut self) -> bool {
        if self.0.has_changed().unwrap_or(false) {
            self.0.borrow_and_update();
            true
        } else {
            false
        }
    }
}

impl TemplateCache {
    /// Whether the updater, rather than an individual request, owns template construction.
    pub(crate) fn updater_is_running(&self) -> bool {
        self.1.load(Ordering::Acquire)
    }

    fn claim_updater(&self) -> Option<UpdaterGuard> {
        self.1
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| UpdaterGuard(self.clone()))
    }

    /// Returns `true` if the precomputed template extends `tip_hash`.
    fn holds_tip(&self, tip_hash: block::Hash) -> bool {
        self.0
            .borrow()
            .as_ref()
            .and_then(|state| state.template.as_ref())
            .is_some_and(|template| template.previous_block_hash == tip_hash)
    }

    /// Returns `true` if no [`run()`] task has published a template yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.borrow().is_none()
    }

    /// Unowned work must be refreshed at the normal mempool polling interval.
    pub(crate) fn fallback_refresh_at(&self) -> Option<Instant> {
        if self.updater_is_running() {
            return None;
        }
        self.0
            .borrow()
            .as_ref()
            .map(|state| state.published_at + Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL))
    }

    /// Returns a failed build and its snapshot's tip, which is absent if no snapshot was obtained.
    pub(crate) fn build_error(&self) -> Option<(Option<block::Hash>, ErrorObjectOwned)> {
        if !self.updater_is_running() {
            return None;
        }
        self.0
            .borrow()
            .as_ref()
            .and_then(|state| state.error.clone())
    }

    fn record_error(&self, error: (Option<block::Hash>, ErrorObjectOwned)) {
        self.0.send_modify(|state| {
            let state = state.get_or_insert_with(|| TemplateState {
                template: None,
                published_at: Instant::now(),
                error: None,
            });
            state.error = Some(error);
        });
    }

    /// Subscribes to the templates [`run()`] publishes from now on.
    ///
    /// Subscribe before reading the cache, and keep the subscription across the wait:
    /// `watch::Sender::subscribe()` marks the current template as seen and everything published
    /// after it as unseen, so a template published while the caller decides what to do with the
    /// one it just read still wakes it.
    pub(crate) fn subscribe(&self) -> TemplateChanges {
        TemplateChanges(self.0.subscribe())
    }

    /// Publishes `template` as the precomputed template.
    pub(crate) fn publish(&self, template: BlockTemplateResponse) {
        self.0.send_replace(Some(TemplateState {
            template: Some(Arc::new(template)),
            published_at: Instant::now(),
            error: None,
        }));
    }

    /// Returns a template for `tip_hash`, narrowing its timestamp range to the local-clock bound.
    /// Rejects empty ranges and Testnet work whose time-dependent difficulty may have become easier.
    pub(crate) fn template_for_tip(
        &self,
        tip_hash: block::Hash,
        network: &Network,
        now: DateTime32,
    ) -> Option<Arc<BlockTemplateResponse>> {
        let cached = self.0.borrow();
        let template = cached.as_ref()?.template.as_ref()?;

        // Mining on a template for another tip extends a chain Zebra has already seen a block for.
        if template.previous_block_hash != tip_hash {
            return None;
        }

        // Only an abbreviated standard-difficulty Testnet time range can become unprofitable.
        // At the full 90-minute median-time cap, even a fresh build clamps to the same max_time.
        // Regtest deliberately uses historical chain time rather than the wall clock.
        if now > template.max_time
            && !network.is_regtest()
            && NetworkUpgrade::minimum_difficulty_spacing_for_height(
                network,
                Height(template.height),
            )
            .is_some()
            && template.bits != network.target_difficulty_limit().to_compact()
            && (!network.is_max_block_time_enforced(Height(template.height))
                || template
                    .max_time
                    .saturating_duration_since(template.min_time)
                    .seconds()
                    < Duration32::from_minutes(90).seconds() - 1)
        {
            return None;
        }

        let (cur_time, max_time) = template.clamped_time_range(now)?;
        if max_time == template.max_time {
            return Some(Arc::clone(template));
        }

        // Only copy when rollback narrows the response; keep the cached snapshot and work ID.
        let mut template = (**template).clone();
        template.cur_time = cur_time;
        template.max_time = max_time;
        Some(Arc::new(template))
    }
}

/// Keeps `cache` filled with a block template for the current chain tip.
///
/// Publishes a coinbase-only template as soon as the chain tip changes, then replaces it with a
/// template that contains mempool transactions. Rebuilds after throttled mempool changes, with a
/// backstop for missed events and clock changes after at least [`MEMPOOL_LONG_POLL_INTERVAL`]
/// seconds. Both waits respect the previous build's duration.
///
/// Runs until the task is aborted.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run<Mempool, ReadStateService, Tip, SyncStatus>(
    network: Network,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    cache: TemplateCache,
    mempool: Mempool,
    mut mempool_changes: broadcast::Receiver<MempoolChange>,
    read_state: ReadStateService,
    mut latest_chain_tip: Tip,
    sync_status: SyncStatus,
) where
    Mempool: MempoolService,
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let Some(_updater) = cache.claim_updater() else {
        return;
    };

    // The coinbase transaction for a coinbase-only block at this height, built while we're idle. A
    // shielded coinbase takes seconds to prove, which is too slow to do after the tip changes.
    let mut next_coinbase: Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)> = None;

    // Whether the last build failed, so a failing spell is logged once rather than every second.
    let mut was_failing = false;

    loop {
        // `getblocktemplate` returns an error until Zebra is synced to the tip, so there's nothing
        // to precompute before then.
        if let Err(error) =
            check_synced_to_tip(&network, latest_chain_tip.clone(), sync_status.clone())
        {
            tracing::trace!(
                ?error,
                "waiting to sync to the tip before building templates"
            );
            sleep(RETRY_DELAY).await;
            continue;
        }

        // Mark tip changes up to this point as seen, so a change while we build a template wakes up
        // the wait at the end of this iteration, rather than being missed.
        latest_chain_tip.mark_best_tip_seen();
        // The next snapshot includes earlier events; retain events arriving during the build.
        mempool_changes = mempool_changes.resubscribe();

        // If we have no template for the current tip, publish a coinbase-only one immediately, so
        // miners extend the new tip instead of wasting work on a shorter chain while we select
        // mempool transactions for it.
        if let Some((tip_height, tip_hash)) = latest_chain_tip.best_tip_height_and_hash() {
            if !cache.holds_tip(tip_hash) {
                if let Ok(height) = tip_height.next() {
                    store_precomputed_coinbase(&mut next_coinbase, height, &coinbase_cache).await;
                }

                match build(
                    &network,
                    &miner_params,
                    &coinbase_cache,
                    read_state.clone(),
                    None::<Mempool>,
                    None,
                )
                .await
                {
                    Ok(Some((template, _))) => cache.publish(template),
                    // A coinbase-only template doesn't read the mempool, so it can't be out of
                    // sync with the state.
                    Ok(None) => {}
                    Err(error) => {
                        cache.record_error(error.clone());
                        tracing::debug!(?error, "failed to build a template for the new tip")
                    }
                }
            }
        }

        let build_started = Instant::now();
        // Cancel obsolete snapshots, but retain the owner of every dispatched proof.
        let build_phase = AtomicU8::new(BUILD_FETCHING);
        let mut full_build = Box::pin(build(
            &network,
            &miner_params,
            &coinbase_cache,
            read_state.clone(),
            Some(mempool.clone()),
            Some(&build_phase),
        ));
        let mut tip_change = latest_chain_tip.clone();
        let built = tokio::select! {
            built = &mut full_build => built,
            _ = tip_change.best_tip_changed() => {
                if build_phase.load(Ordering::Acquire) != BUILD_STARTED {
                    continue;
                }
                full_build.await
            }
        };
        let built = match built {
            Ok(Some((template, mempool_tx_ids))) => {
                match state_tip_hash(read_state.clone()).await {
                    Ok(tip_hash) if tip_hash == Some(template.previous_block_hash) => {
                        Ok(Some((template, mempool_tx_ids)))
                    }
                    Ok(_) => {
                        // The state advanced while the template was being built. Retry immediately
                        // rather than waiting for the chain tip notification to catch up.
                        tracing::debug!("discarding a template for a superseded chain tip");
                        continue;
                    }
                    Err(error) => Err((Some(template.previous_block_hash), error)),
                }
            }
            result => result,
        };

        let mempool_tx_ids = match built {
            Ok(Some((template, mempool_tx_ids))) => {
                if was_failing {
                    tracing::info!("block template builds recovered");
                    was_failing = false;
                }

                cache.publish(template);
                mempool_tx_ids
            }
            // The state and the mempool disagreed about the tip, so retry with fresh data.
            Ok(None) => {
                sleep(RETRY_DELAY).await;
                continue;
            }
            Err(error) => {
                cache.record_error(error.clone());
                // While this keeps failing the RPC serves the last template, so miners silently
                // lose the fees of everything that has arrived since. Log the start of a failing
                // spell loudly, then stay quiet: the retry delay is a second, so warning on every
                // attempt would bury the rest of the log.
                if was_failing {
                    tracing::debug!(?error, "failed to build a block template");
                } else {
                    tracing::warn!(
                        ?error,
                        "failed to build a block template, serving the last one until this \
                         recovers"
                    );
                    was_failing = true;
                }

                sleep(RETRY_DELAY).await;
                continue;
            }
        };

        // Pace mempool work by its cost, including proofs, rather than allowing slow builds to
        // run back-to-back. Tip changes still bypass this pause.
        let build_finished = Instant::now();
        let next_mempool_build =
            build_finished + MEMPOOL_THROTTLE.max(build_finished - build_started);

        // Build the coinbase transaction for the block after next while we're idle, so the next tip
        // change doesn't have to wait for a shielded coinbase proof.
        if let Some(height) = latest_chain_tip
            .best_tip_height()
            .and_then(|tip_height| tip_height.next().ok())
            .and_then(|next_height| next_height.next().ok())
        {
            start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, height);
        }

        // Keep the tip and backstop live even while the mempool query is pending.
        let mut tip_change = latest_chain_tip.clone();
        tokio::select! {
            biased;
            tip_changed = tip_change.best_tip_changed() => {
                if tip_changed.is_err() {
                    return;
                }
            }
            _ = sleep_until(
                next_mempool_build.max(
                    build_finished + Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL),
                ),
            ) => {}
            _ = wait_for_mempool_change(
                &mut mempool_changes, mempool.clone(), &mempool_tx_ids, next_mempool_build,
            ) => {}
        }
    }
}

/// Waits for a changed verified set, not merely a rejected transaction or an overflowed channel.
async fn wait_for_mempool_change<Mempool: MempoolService>(
    changes: &mut broadcast::Receiver<MempoolChange>,
    mempool: Mempool,
    template_ids: &HashSet<UnminedTxId>,
    next_build: Instant,
) {
    let mut next_check = next_build;
    loop {
        let change = changes.recv().await;
        if matches!(&change, Err(broadcast::error::RecvError::Closed)) {
            std::future::pending::<()>().await;
        }

        // Rejection-only reads are bounded separately; an addition bypasses that deadline.
        if !change.is_ok_and(|change| change.is_added()) {
            tokio::select! {
                biased;
                _ = sleep_until(next_check) => {}
                _ = async {
                    loop {
                        match changes.recv().await {
                            Ok(change) if change.is_added() => break,
                            Err(broadcast::error::RecvError::Closed) =>
                                std::future::pending::<()>().await,
                            _ => {}
                        }
                    }
                } => {}
            }
        }

        // The deadline includes the previous build's duration, so expensive work gets an equal
        // idle interval even when verified additions arrive continuously.
        sleep_until(next_build).await;

        // Discard the whole burst, including lagged notifications, before taking the snapshot.
        // Comparing all IDs also detects collateral evictions absent from the notification.
        *changes = changes.resubscribe();
        next_check = Instant::now() + MEMPOOL_THROTTLE;
        match mempool
            .clone()
            .oneshot(mempool::Request::TransactionIds)
            .await
        {
            Ok(mempool::Response::TransactionIds(ids)) if ids == *template_ids => {}
            Ok(mempool::Response::TransactionIds(_)) | Err(_) => return,
            Ok(_) => unreachable!("mempool must return transaction IDs for TransactionIds"),
        }
    }
}

/// Returns the hash of the chain tip the state has committed, if any.
pub(crate) async fn state_tip_hash<ReadStateService>(
    read_state: ReadStateService,
) -> RpcResult<Option<block::Hash>>
where
    ReadStateService: ReadState,
{
    let ReadResponse::Tip(tip) = read_state
        .oneshot(ReadRequest::Tip)
        .await
        .map_misc_error()?
    else {
        unreachable!("state service returned the wrong response to a Tip request");
    };

    Ok(tip.map(|(_, tip_hash)| tip_hash))
}

/// Builds a block template for the block after the current chain tip.
///
/// Selects mempool transactions if `mempool` is `Some`, and builds a coinbase-only template
/// otherwise. Returns `None` if the state and the mempool disagree about the chain tip.
async fn build<Mempool, ReadStateService>(
    network: &Network,
    miner_params: &MinerParams,
    coinbase_cache: &CoinbaseCache,
    read_state: ReadStateService,
    mempool: Option<Mempool>,
    phase: Option<&AtomicU8>,
) -> Result<
    Option<(BlockTemplateResponse, HashSet<UnminedTxId>)>,
    (Option<block::Hash>, ErrorObjectOwned),
>
where
    Mempool: MempoolService,
    ReadStateService: ReadState,
{
    let mut error_tip = None;
    let snapshot = tokio::time::timeout(SNAPSHOT_TIMEOUT, async {
        let chain_info = fetch_chain_info(read_state).await?;
        error_tip = Some(chain_info.tip_hash);
        let (mempool_txs, mempool_tx_deps) = match mempool {
            Some(mempool) => {
                let Some(snapshot) =
                    fetch_mempool_transactions(mempool, chain_info.tip_hash).await?
                else {
                    return Ok(None);
                };
                snapshot
            }
            None => Default::default(),
        };
        Ok::<_, ErrorObjectOwned>(Some((chain_info, mempool_txs, mempool_tx_deps)))
    })
    .await
    .map_misc_error()
    .and_then(|result| result)
    .map_err(|error| (error_tip, error))?;
    let Some((chain_info, mempool_txs, mempool_tx_deps)) = snapshot else {
        return Ok(None);
    };
    let mempool_tx_ids = mempool_txs.iter().map(|tx| tx.transaction.id).collect();

    let long_poll_id = LongPollInput::new(
        chain_info.tip_height,
        chain_info.tip_hash,
        chain_info.max_time,
        mempool_txs.iter().map(|tx| tx.transaction.id),
    )
    .generate_id();

    let network = network.clone();
    let miner_params = miner_params.clone();
    let coinbase_cache = coinbase_cache.clone();
    if let Some(phase) = phase {
        phase.store(BUILD_WAITING_FOR_PERMIT, Ordering::Release);
    }
    let build_guard = coinbase_cache.build.clone().lock_owned().await;
    if let Some(phase) = phase {
        phase.store(BUILD_STARTED, Ordering::Release);
    }

    // Transaction selection, the coinbase transaction, and the block roots are all CPU-bound, and
    // a shielded coinbase takes seconds to prove, so keep them off the async executor.
    tokio::task::spawn_blocking(move || {
        let _build_guard = build_guard;
        BlockTemplateResponse::from_mempool(
            &network,
            &coinbase_cache,
            &miner_params,
            &chain_info,
            long_poll_id,
            mempool_txs,
            mempool_tx_deps,
        )
        .map(|template| Some((template, mempool_tx_ids)))
    })
    .await
    .map_misc_error()
    .and_then(|result| result)
    .map_err(|error| (error_tip, error))
}

/// Starts building the coinbase transaction for a coinbase-only block at `height`, unless it is
/// already built or another coinbase is still being built.
fn start_precomputing_coinbase(
    next_coinbase: &mut Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)>,
    network: &Network,
    miner_params: &MinerParams,
    height: Height,
) {
    // The future parent does not exist yet, so its NSM balance is unknown.
    if zebra_chain::parameters::subsidy::nsm_reissuance_is_active(height, network) {
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
        tokio::task::spawn_blocking(move || {
            TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
                .expect("valid coinbase tx")
        }),
    ));
}

/// Moves the precomputed coinbase transaction into `coinbase_cache`, if it was built for `height`.
///
/// A coinbase built for another height has the wrong BIP-34 height and subsidy. Keep tracking it
/// until it finishes, so a later precomputation cannot detach an unfinished proof.
async fn store_precomputed_coinbase(
    next_coinbase: &mut Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)>,
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
        .expect("the precomputed height was checked above");
    coinbase_cache.select(height, None);

    match coinbase.await {
        // A coinbase-only block pays no fees, so this also caches the zero-fee coinbase that
        // ZIP-317 transaction selection needs for its size and sigop limits.
        Ok(coinbase) => coinbase_cache.store(height, Amount::zero(), None, coinbase),
        Err(error) => tracing::warn!(?error, "precomputed coinbase transaction task failed"),
    }
}
