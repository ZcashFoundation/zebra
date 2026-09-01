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
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

use zebra_chain::{
    amount::{Amount, NegativeOrZero},
    block::{self, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
};
use zebra_node_services::mempool::MempoolService;
use zebra_state::ReadState;

use crate::{
    methods::types::{long_poll::LongPollInput, transaction::TransactionTemplate},
    server::error::MapError,
};

use super::{
    check_synced_to_tip, constants::MEMPOOL_LONG_POLL_INTERVAL, fetch_chain_info,
    fetch_mempool_transactions, zip317::select_mempool_transactions, BlockTemplateResponse,
    CoinbaseCache, MinerParams,
};

/// How long `getblocktemplate` waits for [`run()`] to publish a template for a new chain tip,
/// before building a template itself.
///
/// [`run()`] publishes a coinbase-only template as soon as it sees a tip change, so this timeout
/// only expires if that task is busy building a template, or isn't running at all.
const NEW_TIP_TIMEOUT: Duration = Duration::from_secs(1);

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

impl TemplateCache {
    /// Returns `true` if the precomputed template extends `tip_hash`.
    fn holds_tip(&self, tip_hash: block::Hash) -> bool {
        self.0
            .borrow()
            .as_ref()
            .is_some_and(|template| template.previous_block_hash == tip_hash)
    }

    /// Publishes `template` as the precomputed template.
    fn publish(&self, template: BlockTemplateResponse) {
        self.0.send_replace(Some(Arc::new(template)));
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
        let mut template = receiver.borrow_and_update().clone()?;

        timeout(NEW_TIP_TIMEOUT, async move {
            loop {
                if template.previous_block_hash == tip_hash {
                    return Some(template);
                }

                receiver.changed().await.ok()?;
                template = receiver.borrow_and_update().clone()?;
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
            Ok(Some(template)) => cache.publish(template),
            // The state and the mempool disagreed about the tip, so retry with fresh data.
            Ok(None) => {
                sleep(RETRY_DELAY).await;
                continue;
            }
            Err(error) => {
                tracing::debug!(?error, "failed to build a block template");
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
            None,
            Some(coinbase_cache),
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
