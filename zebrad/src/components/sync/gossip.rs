//! A task that gossips committed tips and validated submitted blocks to peers.
//!
//! [`block::Hash`]: zebra_chain::block::Hash

use std::{collections::HashSet, time::Duration};

use futures::{stream::FuturesUnordered, StreamExt};
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_chain::{block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip};
use zebra_network as zn;
use zebra_state::ChainTipChange;

use crate::{
    components::sync::{SyncStatus, TIPS_RESPONSE_TIMEOUT},
    BoxError,
};

use BlockGossipError::*;

/// Bound concurrent broadcasts while continuing to coalesce incoming block notifications.
const MAX_IN_FLIGHT_BROADCASTS: usize = 16;

/// Allow the committing RPC task to enqueue its mined-block notification before tip fallback.
const WAIT_FOR_BLOCK_SUBMISSION_DELAY: Duration = Duration::from_micros(100);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BroadcastKind {
    Mined,
    Early,
    Tip,
}

/// Errors that can occur when gossiping blocks.
#[derive(Error, Debug)]
pub enum BlockGossipError {
    #[error("chain tip sender was dropped")]
    TipChange(watch::error::RecvError),

    #[error("sync status sender was dropped")]
    SyncStatus(watch::error::RecvError),

    #[error("permanent peer set failure")]
    PeerSetReadiness(zn::BoxError),
}

/// Run continuously, gossiping committed tips and validated submitted blocks.
///
/// Once the state has reached the chain tip, broadcast the [`block::Hash`]es
/// of newly verified blocks to eligible connected peers, including busy peers.
///
/// Blocks are only gossiped if they are:
/// - on the best chain, and
/// - the most recent block verified since the last gossip.
///
/// In particular, if a lot of blocks are committed at the same time,
/// gossips will be disabled or skipped until the state reaches the latest tip.
///
/// Pending submissions use ordinary gossip fanout before verification completes. Only committed
/// mined blocks use all-peer delivery, so early adverts cannot replace their deferred advertisements.
///
/// [`block::Hash`]: zebra_chain::block::Hash
pub async fn gossip_best_tip_block_hashes<ZN>(
    mut sync_status: SyncStatus,
    mut chain_state: ChainTipChange,
    broadcast_network: ZN,
    mut mined_block_receiver: Option<mpsc::Receiver<(block::Hash, block::Height)>>,
    submitted_blocks: zebra_rpc::SubmittedBlockCache,
) -> Result<(), BlockGossipError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    info!("initializing block gossip task");

    // use the same timeout as tips requests,
    // so broadcasts don't delay the syncer too long
    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);
    let mut submitted_receiver = submitted_blocks.subscribe();

    let mut broadcasts = FuturesUnordered::new();
    let mut mined_in_flight = HashSet::new();
    let mut pending_mined = None;
    let mut pending_early = None;
    let mut pending_tip = None;
    let mut tip_ready_at = tokio::time::Instant::now();

    loop {
        // A rejection or expiry can withdraw a pending body while peers are unready.
        if pending_early.is_some_and(|(hash, _)| submitted_blocks.get(&hash).is_none()) {
            pending_early = None;
        }

        let tip = pending_tip
            .filter(|(hash, _)| !mined_in_flight.contains(hash))
            .map(|block| (block, BroadcastKind::Tip));
        let next = pending_mined
            .map(|block| (block, BroadcastKind::Mined))
            // Early submissions must not postpone a committed tip that is ready to gossip.
            .or_else(|| {
                tip.filter(|_| {
                    tokio::time::Instant::now() >= tip_ready_at && sync_status.is_close_to_tip()
                })
            })
            .or_else(|| pending_early.map(|block| (block, BroadcastKind::Early)))
            .or(tip);

        tokio::select! {
            biased;

            // Complete mined broadcasts before considering submissions or their tip fallback.
            Some((hash, kind, result)) = broadcasts.next(), if !broadcasts.is_empty() => {
                let result: Result<zn::Response, BoxError> = result;
                if kind == BroadcastKind::Mined {
                    mined_in_flight.remove(&hash);
                    if result.is_ok() {
                        if pending_tip.is_some_and(|(tip, _)| tip == hash) {
                            pending_tip = None;
                        }
                        // A stale completion must not overwrite bookkeeping for a newer tip.
                        if chain_state.latest_chain_tip().best_tip_hash() == Some(hash) {
                            chain_state.mark_last_change_hash(hash);
                        }
                    }
                    // On failure, leave the committed tip pending for the ordinary broadcast.
                }
            }

            Some(block) = recv_mined_block(&mut mined_block_receiver) => {
                debug!(hash = ?block.0, height = ?block.1, "received mined block for broadcast");
                if !mined_in_flight.contains(&block.0) {
                    pending_mined = Some(block);
                }
                // Do not let a superseded early advert replace mined inventory for busy peers.
                pending_early = None;
            }

            tip_change = chain_state.wait_for_tip_change() => {
                pending_tip = Some(tip_change.map_err(TipChange)?.best_tip_hash_and_height());
                tip_ready_at = tokio::time::Instant::now() + WAIT_FOR_BLOCK_SUBMISSION_DELAY;
            }

            ready = async {
                if matches!(next, Some((_, BroadcastKind::Tip))) {
                    tokio::time::sleep_until(tip_ready_at).await;
                    sync_status.wait_until_close_to_tip().await.map_err(SyncStatus)?;
                }
                broadcast_network.ready().await.map(|_| ()).map_err(PeerSetReadiness)
            }, if next.is_some() && broadcasts.len() < MAX_IN_FLIGHT_BROADCASTS => {
                ready?;
                let ((hash, height), kind) = next.expect("readiness is polled with a pending broadcast");
                let is_block_submission = kind != BroadcastKind::Tip;
                let log_msg = match kind {
                    BroadcastKind::Mined => "sending mined block broadcast",
                    BroadcastKind::Early => "sending pending block broadcast",
                    BroadcastKind::Tip => "sending committed block broadcast",
                };
                // Early adverts precede the same block's mined broadcast; keep them out of INFO.
                if kind == BroadcastKind::Early {
                    debug!(?height, ?hash, is_block_submission, log_msg);
                } else {
                    info!(?height, ?hash, is_block_submission, log_msg);
                }

                match kind {
                    BroadcastKind::Mined => {
                        pending_mined = None;
                        mined_in_flight.insert(hash);
                    }
                    BroadcastKind::Early => {
                        pending_early = None;
                        if submitted_blocks.get(&hash).is_none() {
                            continue;
                        }
                    }
                    BroadcastKind::Tip => pending_tip = None,
                }

                let request = match kind {
                    BroadcastKind::Mined => zn::Request::AdvertiseBlockToAll(hash),
                    BroadcastKind::Early | BroadcastKind::Tip => {
                        zn::Request::AdvertiseBlock(hash, None)
                    }
                };
                let broadcast = broadcast_network.call(request);
                // Poll broadcasts concurrently with readiness and incoming notifications.
                broadcasts.push(async move { (hash, kind, broadcast.await) });
            }

            early = recv_submitted_block(&mut submitted_receiver), if pending_mined.is_none() => {
                pending_early = Some(early);
            }
        }
    }
}

/// Receives the next mined block from `mined_block_receiver`.
///
/// Never resolves if there is no receiver, so it can be used in a `select!` with other futures.
async fn recv_mined_block(
    mined_block_receiver: &mut Option<mpsc::Receiver<(block::Hash, block::Height)>>,
) -> Option<(block::Hash, block::Height)> {
    match mined_block_receiver {
        Some(mined_block_receiver) => mined_block_receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Receives the latest pending submission, coalescing updates while committed adverts wait.
async fn recv_submitted_block(
    receiver: &mut watch::Receiver<Option<(block::Hash, block::Height)>>,
) -> (block::Hash, block::Height) {
    while receiver.changed().await.is_ok() {
        if let Some(block) = *receiver.borrow_and_update() {
            return block;
        }
    }
    std::future::pending().await
}
