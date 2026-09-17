//! A precomputed block template for the `getblocktemplate` RPC.
//!
//! Miners call `getblocktemplate` far more often than the chain tip or the mempool change, and
//! building a template needs a state read, a mempool read, ZIP-317 transaction selection, and a
//! coinbase transaction, which re-runs a shielded proof when the miner address has a shielded
//! component. So [`run()`] keeps a template for the current chain tip ready in a [`TemplateCache`],
//! and the RPC only has to check that the template still extends the tip.
//!
//! The precomputed template can be a few seconds behind the mempool, which costs the miner the fees
//! of the transactions that arrived in the meantime, until the next refresh. But it is never behind
//! the chain: the RPC ignores a template whose previous block hash isn't the current tip, and
//! [`run()`] publishes a coinbase-only template for a new tip as soon as it sees one.

use std::{sync::Arc, time::Duration};

use jsonrpsee::core::RpcResult;
use tokio::{sync::watch, task::JoinHandle, time::sleep};

use tower::ServiceExt;

use zebra_chain::{
    amount::{Amount, NegativeOrZero},
    block::{self, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    work::difficulty::ParameterDifficulty,
};
use zebra_node_services::mempool::MempoolService;
use zebra_state::{ReadRequest, ReadResponse, ReadState};

use crate::{
    methods::types::{long_poll::LongPollInput, transaction::TransactionTemplate},
    server::error::MapError,
};

use super::{
    check_synced_to_tip, constants::MEMPOOL_LONG_POLL_INTERVAL, fetch_chain_info,
    fetch_mempool_transactions, zip317::select_mempool_transactions, BlockTemplateResponse,
    CoinbaseCache, MinerParams,
};

#[cfg(test)]
mod tests;

/// How long `getblocktemplate` waits for [`run()`] to publish a template for the current chain
/// tip, before building a template itself.
///
/// [`run()`] publishes a coinbase-only template as soon as it sees a tip change, so this timeout
/// only expires if that task is busy building a template, or isn't running at all.
pub(crate) const NEW_TIP_TIMEOUT: Duration = Duration::from_secs(1);

/// The same wait, for a miner address with a shielded component.
///
/// Building a template for a shielded address runs a coinbase proof, which takes seconds. So
/// giving up on the updater is the expensive option, not the cheap one: every waiting request
/// starts a proof of its own, alongside the one the updater is already running, which is the
/// per-request proving this cache exists to avoid.
///
/// Wait long enough to cover a proof and the rest of the build, but not forever, so a wedged
/// updater still can't stall `getblocktemplate` indefinitely.
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
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// A block template for the block after the current chain tip, shared between [`run()`] and the
/// `getblocktemplate` RPC.
#[derive(Clone)]
pub(crate) struct TemplateCache(Arc<watch::Sender<Option<Arc<BlockTemplateResponse>>>>);

impl Default for TemplateCache {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(None)))
    }
}

/// A subscription to the templates [`run()`] publishes.
pub(crate) struct TemplateChanges(watch::Receiver<Option<Arc<BlockTemplateResponse>>>);

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
}

impl TemplateCache {
    /// Returns `true` if the precomputed template extends `tip_hash`.
    fn holds_tip(&self, tip_hash: block::Hash) -> bool {
        self.0
            .borrow()
            .as_ref()
            .is_some_and(|template| template.previous_block_hash == tip_hash)
    }

    /// Returns `true` if no [`run()`] task has published a template yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.borrow().is_none()
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
        self.0.send_replace(Some(Arc::new(template)));
    }

    /// Returns a template for `tip_hash`, unless Testnet's time-dependent difficulty may have
    /// become easier since it was built.
    pub(crate) fn template_for_tip(
        &self,
        tip_hash: block::Hash,
        network: &Network,
        now: DateTime32,
    ) -> Option<Arc<BlockTemplateResponse>> {
        let cached = self.0.borrow();
        let template = cached.as_ref()?;

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
                Height(template.height.saturating_sub(1)),
            )
            .is_some()
            && template.bits != network.target_difficulty_limit().to_compact()
            && template
                .max_time
                .saturating_duration_since(template.min_time)
                .seconds()
                < Duration32::from_minutes(90).seconds() - 1
        {
            return None;
        }

        Some(Arc::clone(template))
    }
}

/// Keeps `cache` filled with a block template for the current chain tip.
///
/// Publishes a coinbase-only template as soon as the chain tip changes, then replaces it with a
/// template that contains mempool transactions. Refreshes that template every
/// [`MEMPOOL_LONG_POLL_INTERVAL`] seconds, so it picks up new mempool transactions and a recent
/// `cur_time`.
///
/// Runs until the task is aborted.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run<Mempool, ReadStateService, Tip, SyncStatus>(
    network: Network,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    cache: TemplateCache,
    mempool: Mempool,
    read_state: ReadStateService,
    mut latest_chain_tip: Tip,
    sync_status: SyncStatus,
) where
    Mempool: MempoolService,
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
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
                )
                .await
                {
                    Ok(Some(template)) => cache.publish(template),
                    // A coinbase-only template doesn't read the mempool, so it can't be out of
                    // sync with the state.
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(?error, "failed to build a template for the new tip")
                    }
                }
            }
        }

        // Await the full build even if the tip changes: dropping it would detach its
        // `spawn_blocking` proof, letting repeated tip changes accumulate CPU-heavy work.
        let built = match build(
            &network,
            &miner_params,
            &coinbase_cache,
            read_state.clone(),
            Some(mempool.clone()),
        )
        .await
        {
            Ok(Some(template)) => match state_tip_hash(read_state.clone()).await {
                Ok(tip_hash) if tip_hash == Some(template.previous_block_hash) => {
                    Ok(Some(template))
                }
                Ok(_) => {
                    // The state advanced while the template was being built. Retry immediately
                    // rather than waiting for the chain tip notification to catch up.
                    tracing::debug!("discarding a template for a superseded chain tip");
                    continue;
                }
                Err(error) => Err(error),
            },
            result => result,
        };

        match built {
            Ok(Some(template)) => {
                if was_failing {
                    tracing::info!("block template builds recovered");
                    was_failing = false;
                }

                cache.publish(template)
            }
            // The state and the mempool disagreed about the tip, so retry with fresh data.
            Ok(None) => {
                sleep(RETRY_DELAY).await;
                continue;
            }
            Err(error) => {
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
        }

        // Build the coinbase transaction for the block after next while we're idle, so the next tip
        // change doesn't have to wait for a shielded coinbase proof.
        if let Some(height) = latest_chain_tip
            .best_tip_height()
            .and_then(|tip_height| tip_height.next().ok())
            .and_then(|next_height| next_height.next().ok())
        {
            start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, height);
        }

        // Refresh the template when the chain tip changes, or when the mempool has had time to
        // change. Miners can keep working on an old set of transactions, so they don't need to know
        // about new mempool transactions immediately.
        let mut tip_change = latest_chain_tip.clone();
        tokio::select! {
            biased;
            tip_changed = tip_change.best_tip_changed() => {
                if tip_changed.is_err() {
                    return;
                }
            }
            _ = sleep(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)) => {}
        }
    }
}

/// Returns the hash of the chain tip the state has committed, if any.
async fn state_tip_hash<ReadStateService>(
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
) -> RpcResult<Option<BlockTemplateResponse>>
where
    Mempool: MempoolService,
    ReadStateService: ReadState,
{
    let chain_info = fetch_chain_info(read_state).await?;
    let height = chain_info.tip_height.next().map_misc_error()?;

    let (mempool_txs, mempool_tx_deps) = match mempool {
        Some(mempool) => {
            match fetch_mempool_transactions(mempool, chain_info.tip_hash).await? {
                Some(mempool_data) => mempool_data,
                // The state and the mempool were out of sync, so a template built from this data
                // could contain transactions that are already mined.
                None => return Ok(None),
            }
        }
        None => Default::default(),
    };

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

    // Transaction selection, the coinbase transaction, and the block roots are all CPU-bound, and
    // a shielded coinbase takes seconds to prove, so keep them off the async executor.
    tokio::task::spawn_blocking(move || {
        let mempool_txs = select_mempool_transactions(
            &network,
            height,
            &miner_params,
            mempool_txs,
            mempool_tx_deps,
            Some(&coinbase_cache),
        );

        // `submit_old` depends on the long poll ID the client sent, so the RPC sets it.
        Some(BlockTemplateResponse::new_internal(
            &network,
            &coinbase_cache,
            &miner_params,
            &chain_info,
            long_poll_id,
            mempool_txs,
            None,
        ))
    })
    .await
    .map_misc_error()
}

/// Starts building the coinbase transaction for a coinbase-only block at `height`, unless it is
/// already built or another coinbase is still being built.
fn start_precomputing_coinbase(
    next_coinbase: &mut Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)>,
    network: &Network,
    miner_params: &MinerParams,
    height: Height,
) {
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

    match coinbase.await {
        // A coinbase-only block pays no fees, so this also caches the zero-fee coinbase that
        // ZIP-317 transaction selection needs for its size and sigop limits.
        Ok(coinbase) => coinbase_cache.store(height, Amount::zero(), coinbase),
        Err(error) => tracing::warn!(?error, "precomputed coinbase transaction task failed"),
    }
}
