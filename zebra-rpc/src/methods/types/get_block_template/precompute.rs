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

use std::{collections::HashSet, sync::Arc, time::Duration};

use jsonrpsee::core::RpcResult;
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

use tower::ServiceExt;
use zebra_chain::{
    amount::{Amount, NegativeOrZero},
    block::{self, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
    transaction::UnminedTxId,
};
use zebra_node_services::mempool::{self, MempoolChange, MempoolChangeKind, MempoolService};
use zebra_state::ReadState;

use crate::{
    methods::types::{long_poll::LongPollInput, transaction::TransactionTemplate},
    server::error::MapError,
};

use super::{
    check_synced_to_tip, fetch_chain_info, fetch_mempool_transactions,
    zip317::select_mempool_transactions, BlockTemplateResponse, CoinbaseCache, MinerParams,
};

#[cfg(test)]
mod tests;

/// How long `getblocktemplate` waits for [`run()`] to publish a template for a new chain tip,
/// before building a template itself.
///
/// [`run()`] publishes a coinbase-only template as soon as it sees a tip change, so this timeout
/// only expires if that task is busy building a template, or isn't running at all.
const NEW_TIP_TIMEOUT: Duration = Duration::from_secs(1);

/// How long [`run()`] waits before retrying, when Zebra isn't synced to the chain tip, or the state
/// and the mempool disagree about the tip.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// How long [`run()`] waits after a mempool change before rebuilding, so a burst of changes costs
/// one rebuild rather than one per transaction.
///
/// Rebuilding reads the whole mempool, re-runs ZIP-317 selection, and rebuilds the coinbase, so it
/// is far more expensive than the change notification that triggers it.
const MEMPOOL_DEBOUNCE: Duration = Duration::from_millis(500);

/// How long [`run()`] waits before rebuilding when nothing has changed.
///
/// Mempool and chain tip changes drive rebuilds, so this only has to keep `cur_time` from ageing:
/// on Testnet the difficulty depends on it through the minimum-difficulty rule. It also bounds how
/// long a lost notification can stall the template.
const BACKSTOP_REFRESH: Duration = Duration::from_secs(30);

/// A precomputed template, with the mempool it was built from.
///
/// The template's long poll ID is derived from every ID in the mempool, not just the transactions
/// ZIP-317 selected, so deciding whether a mempool change invalidates the template needs the whole
/// set.
struct Precomputed {
    template: Arc<BlockTemplateResponse>,
    mempool_tx_ids: Arc<HashSet<UnminedTxId>>,
}

/// A block template for the block after the current chain tip, shared between [`run()`] and the
/// `getblocktemplate` RPC.
#[derive(Clone)]
pub(crate) struct TemplateCache(Arc<watch::Sender<Option<Precomputed>>>);

impl Default for TemplateCache {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(None)))
    }
}

/// A subscription to the templates [`run()`] publishes.
pub(crate) struct TemplateChanges(watch::Receiver<Option<Precomputed>>);

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
            .is_some_and(|precomputed| precomputed.template.previous_block_hash == tip_hash)
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

    /// Returns the mempool IDs the precomputed template was built from, if there is one.
    fn mempool_tx_ids(&self) -> Option<Arc<HashSet<UnminedTxId>>> {
        self.0
            .borrow()
            .as_ref()
            .map(|precomputed| precomputed.mempool_tx_ids.clone())
    }

    /// Returns `true` if any of `tx_ids` was in the mempool the template was built from.
    ///
    /// This is the set behind the template's long poll ID, so it includes transactions ZIP-317 left
    /// out: removing one of those still has to produce a new long poll ID, or a long polling miner
    /// waits on work whose mempool no longer exists.
    fn built_from_any(&self, tx_ids: &HashSet<UnminedTxId>) -> bool {
        let Some(mempool_tx_ids) = self.mempool_tx_ids() else {
            return false;
        };

        tx_ids.iter().any(|tx_id| mempool_tx_ids.contains(tx_id))
    }

    /// Publishes `template`, built from the mempool holding `mempool_tx_ids`.
    fn publish(&self, template: BlockTemplateResponse, mempool_tx_ids: HashSet<UnminedTxId>) {
        self.0.send_replace(Some(Precomputed {
            template: Arc::new(template),
            mempool_tx_ids: Arc::new(mempool_tx_ids),
        }));
    }

    /// Returns the precomputed template if it extends `tip_hash`, waiting up to
    /// [`NEW_TIP_TIMEOUT`] for [`run()`] to catch up with a recent tip change.
    ///
    /// Returns `None` if no [`run()`] task has published a template yet, or if it didn't catch up
    /// in time. Never returns a template for a different tip: mining on it would extend a chain
    /// that Zebra has already seen a block for.
    pub(crate) async fn wait_for_tip(
        &self,
        tip_hash: block::Hash,
    ) -> Option<Arc<BlockTemplateResponse>> {
        let mut receiver = self.0.subscribe();

        // An empty cache means no `run()` task has published a template, so there's nothing to wait
        // for.
        let mut template = receiver.borrow_and_update().as_ref()?.template.clone();

        timeout(NEW_TIP_TIMEOUT, async move {
            loop {
                if template.previous_block_hash == tip_hash {
                    return Some(template);
                }

                receiver.changed().await.ok()?;
                template = receiver.borrow_and_update().as_ref()?.template.clone();
            }
        })
        .await
        .ok()
        .flatten()
    }
}

/// Keeps `cache` filled with a block template for the current chain tip.
///
/// Publishes a coinbase-only template as soon as the chain tip changes, then replaces it with a
/// template that contains mempool transactions. Rebuilds when the chain tip changes, when the
/// mempool changes in a way that affects the template (debounced by [`MEMPOOL_DEBOUNCE`]), and
/// every [`BACKSTOP_REFRESH`] otherwise, which keeps `cur_time` current.
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
                    Ok(Some((template, mempool_tx_ids))) => cache.publish(template, mempool_tx_ids),
                    // A coinbase-only template doesn't read the mempool, so it can't be out of
                    // sync with the state.
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(?error, "failed to build a template for the new tip")
                    }
                }
            }
        }

        // Build a template with mempool transactions, and give up if the tip changes while we're
        // building it: that template is already stale, and the next iteration replaces it.
        let mut tip_change = latest_chain_tip.clone();
        let build_with_mempool = build(
            &network,
            &miner_params,
            &coinbase_cache,
            read_state.clone(),
            Some(mempool.clone()),
        );

        let built = tokio::select! {
            biased;
            tip_changed = tip_change.best_tip_changed() => {
                // A closed channel means the state service has shut down, so there are no more
                // templates to build.
                if tip_changed.is_err() {
                    return;
                }

                continue;
            }
            built = build_with_mempool => built,
        };

        match built {
            Ok(Some((template, mempool_tx_ids))) => {
                if was_failing {
                    tracing::info!("block template builds recovered");
                    was_failing = false;
                }

                cache.publish(template, mempool_tx_ids)
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

        // Wait for something that changes the template.
        if !wait_for_change(&latest_chain_tip, &mut mempool_changes, &cache, &mempool).await {
            // The chain tip channel or the mempool change channel closed: Zebra is shutting down.
            return;
        }
    }
}

/// Waits until the precomputed template is worth rebuilding: the chain tip changed, the mempool
/// changed in a way that affects the template, or [`BACKSTOP_REFRESH`] elapsed.
///
/// Returns `false` when a channel closes, which means Zebra is shutting down.
async fn wait_for_change<Mempool, Tip>(
    latest_chain_tip: &Tip,
    mempool_changes: &mut broadcast::Receiver<MempoolChange>,
    cache: &TemplateCache,
    mempool: &Mempool,
) -> bool
where
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let mut tip_change = latest_chain_tip.clone();
    let backstop = sleep(BACKSTOP_REFRESH);
    tokio::pin!(backstop);

    loop {
        tokio::select! {
            biased;

            tip_changed = tip_change.best_tip_changed() => {
                return tip_changed.is_ok();
            }

            change = mempool_changes.recv() => {
                match change {
                    Ok(change) if affects_template(&change, cache) => {
                        // Collapse the rest of the burst into this rebuild: a busy mempool
                        // notifies far more often than a template is worth rebuilding.
                        sleep(MEMPOOL_DEBOUNCE).await;
                        while mempool_changes.try_recv().is_ok() {}
                        return true;
                    }
                    // A change that can't affect the template. Keep waiting, so a peer spraying
                    // rejected transactions can't make us rebuild.
                    Ok(_) => continue,
                    // Changes were dropped, so we don't know what they were.
                    //
                    // # Security
                    //
                    // Rebuilding unconditionally here would undo the filter above: a peer sending
                    // invalid transactions fast enough overflows this channel, and every overflow
                    // would buy the rebuild its rejected transactions could not. So compare the
                    // mempool with the set the template was built from instead.
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::debug!(?dropped, "mempool change channel lagged");
                        while mempool_changes.try_recv().is_ok() {}

                        match current_mempool_tx_ids(mempool.clone()).await {
                            Some(current)
                                if Some(&current) != cache.mempool_tx_ids().as_deref() =>
                            {
                                sleep(MEMPOOL_DEBOUNCE).await;
                                while mempool_changes.try_recv().is_ok() {}
                                return true;
                            }
                            // The same mempool, or the mempool didn't answer: nothing to do, and
                            // the backstop still bounds how long the template can go unrefreshed.
                            _ => continue,
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return false,
                }
            }

            _ = &mut backstop => return true,
        }
    }
}

/// Returns the IDs currently in the mempool, or `None` if it didn't answer.
///
/// `TransactionIds` is much cheaper than the `FullTransactions` a rebuild needs: it copies IDs
/// rather than whole transactions.
async fn current_mempool_tx_ids<Mempool>(mempool: Mempool) -> Option<HashSet<UnminedTxId>>
where
    Mempool: MempoolService,
{
    let response = mempool
        .oneshot(mempool::Request::TransactionIds)
        .await
        .ok()?;

    match response {
        mempool::Response::TransactionIds(tx_ids) => Some(tx_ids),
        _ => None,
    }
}

/// Returns `true` if `change` can change what the next template should contain.
fn affects_template(change: &MempoolChange, cache: &TemplateCache) -> bool {
    match change.kind() {
        // New transactions are candidates for the next template.
        MempoolChangeKind::Added => true,

        // A transaction leaving the mempool only matters if the template names it: a template that
        // still contains it would produce a block that can't be mined.
        //
        // # Security
        //
        // This variant also fires for transactions that failed verification and were never in the
        // mempool, so rebuilding for every one of them would let a peer force sustained rebuilds
        // by sending invalid transactions. Debouncing alone doesn't fix that, since the peer can
        // simply keep sending.
        MempoolChangeKind::Invalidated => cache.built_from_any(change.tx_ids()),

        // Mined transactions arrive with the chain tip change that mined them, which rebuilds the
        // template anyway.
        MempoolChangeKind::Mined => false,
    }
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
) -> RpcResult<Option<(BlockTemplateResponse, HashSet<UnminedTxId>)>>
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

    let mempool_tx_ids: HashSet<UnminedTxId> =
        mempool_txs.iter().map(|tx| tx.transaction.id).collect();

    let long_poll_id = LongPollInput::new(
        chain_info.tip_height,
        chain_info.tip_hash,
        chain_info.max_time,
        mempool_tx_ids.iter().copied(),
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
        Some((
            BlockTemplateResponse::new_internal(
                &network,
                &coinbase_cache,
                &miner_params,
                &chain_info,
                long_poll_id,
                mempool_txs,
                None,
            ),
            mempool_tx_ids,
        ))
    })
    .await
    .map_misc_error()
}

/// Starts building the coinbase transaction for a coinbase-only block at `height`, unless it is
/// already built or being built.
fn start_precomputing_coinbase(
    next_coinbase: &mut Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)>,
    network: &Network,
    miner_params: &MinerParams,
    height: Height,
) {
    if next_coinbase
        .as_ref()
        .is_some_and(|(precomputed_height, _)| *precomputed_height == height)
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
/// A coinbase built for another height has the wrong BIP-34 height and subsidy, so it is discarded:
/// the chain advanced by more than one block, or there was a reorg.
async fn store_precomputed_coinbase(
    next_coinbase: &mut Option<(Height, JoinHandle<TransactionTemplate<NegativeOrZero>>)>,
    height: Height,
    coinbase_cache: &CoinbaseCache,
) {
    let Some((_, coinbase)) = next_coinbase
        .take()
        .filter(|(precomputed_height, _)| *precomputed_height == height)
    else {
        return;
    };

    match coinbase.await {
        // A coinbase-only block pays no fees, so this also caches the zero-fee coinbase that
        // ZIP-317 transaction selection needs for its size and sigop limits.
        Ok(coinbase) => coinbase_cache.store(height, Amount::zero(), coinbase),
        Err(error) => tracing::warn!(?error, "precomputed coinbase transaction task failed"),
    }
}
