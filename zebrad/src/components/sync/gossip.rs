//! A task that gossips newly verified [`block::Hash`]es to peers.
//!
//! [`block::Hash`]: zebra_chain::block::Hash

use std::time::Duration;

use futures::TryFutureExt;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tower::{timeout::Timeout, Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{block, chain_tip::ChainTip};
use zebra_network as zn;
use zebra_state::ChainTipChange;

use crate::{
    components::sync::{SyncStatus, PEER_GOSSIP_DELAY, TIPS_RESPONSE_TIMEOUT},
    BoxError,
};

use BlockGossipError::*;

/// Errors that can occur when gossiping committed blocks
#[derive(Error, Debug)]
pub enum BlockGossipError {
    #[error("chain tip sender was dropped")]
    TipChange(watch::error::RecvError),

    #[error("sync status sender was dropped")]
    SyncStatus(watch::error::RecvError),

    #[error("permanent peer set failure")]
    PeerSetReadiness(zn::BoxError),
}

/// Run continuously, gossiping newly verified [`block::Hash`]es to peers.
///
/// Once the state has reached the chain tip, broadcast the [`block::Hash`]es
/// of newly verified blocks to all ready peers.
///
/// Blocks are only gossiped if they are:
/// - on the best chain, and
/// - the most recent block verified since the last gossip.
///
/// In particular, if a lot of blocks are committed at the same time,
/// gossips will be disabled or skipped until the state reaches the latest tip.
///
/// [`block::Hash`]: zebra_chain::block::Hash
pub async fn gossip_best_tip_block_hashes<ZN>(
    sync_status: SyncStatus,
    mut chain_state: ChainTipChange,
    broadcast_network: ZN,
    mut mined_block_receiver: Option<mpsc::Receiver<(block::Hash, block::Height)>>,
) -> Result<(), BlockGossipError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    info!("initializing block gossip task");

    // use the same timeout as tips requests,
    // so broadcasts don't delay the syncer too long
    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);

    loop {
        // TODO: Refactor this into a struct and move the contents of this loop into its own method.
        let mut sync_status = sync_status.clone();
        let mut chain_tip = chain_state.clone();

        // TODO: Move the contents of this async block to its own method
        let tip_change_close_to_network_tip_fut = async move {
            /// A brief duration to wait after a tip change for a new message in the mined block channel.
            // TODO: Add a test to check that Zebra does not advertise mined blocks to peers twice.
            const WAIT_FOR_BLOCK_SUBMISSION_DELAY: Duration = Duration::from_micros(100);

            // wait for at least the network timeout between gossips
            //
            // in practice, we expect blocks to arrive approximately every 75 seconds,
            // so waiting 6 seconds won't make much difference
            tokio::time::sleep(PEER_GOSSIP_DELAY).await;

            // wait for at least one tip change, to make sure we have a new block hash to broadcast
            let tip_action = chain_tip.wait_for_tip_change().await.map_err(TipChange)?;

            // wait for block submissions to be received through the `mined_block_receiver` if the tip
            // change is from a block submission.
            tokio::time::sleep(WAIT_FOR_BLOCK_SUBMISSION_DELAY).await;

            // wait until we're close to the tip, because broadcasts are only useful for nodes near the tip
            // (if they're a long way from the tip, they use the syncer and block locators), unless a mined block
            // hash is received before `wait_until_close_to_tip()` is ready.
            sync_status
                .wait_until_close_to_tip()
                .map_err(SyncStatus)
                .await?;

            // get the latest tip change when close to tip - it might be different to the change we awaited,
            // because the syncer might take a long time to reach the tip
            let best_tip = chain_tip
                .last_tip_change()
                .unwrap_or(tip_action)
                .best_tip_hash_and_height();

            Ok((best_tip, "sending committed block broadcast", chain_tip))
        }
        .in_current_span();

        // TODO: Move this logic for selecting the first ready future and updating `chain_state` to its own method.
        let (((mut hash, mut height), log_msg, updated_chain_state), mut is_block_submission) =
            if let Some(mined_block_receiver) = mined_block_receiver.as_mut() {
                tokio::select! {
                    tip_change_close_to_network_tip = tip_change_close_to_network_tip_fut => {
                        (tip_change_close_to_network_tip?, false)
                    },

                    Some(tip_change) = mined_block_receiver.recv() => {
                       ((tip_change, "sending mined block broadcast", chain_state), true)
                    }
                }
            } else {
                (tip_change_close_to_network_tip_fut.await?, false)
            };

        chain_state = updated_chain_state;

        // TODO: Move logic for calling the peer set to its own method.

        info!(?height, ?hash, is_block_submission, log_msg);

        // `tower::timeout::Timeout` only bounds the response future, not `poll_ready()`.
        // If there are no ready peers, only waiting for readiness would stop this task from
        // consuming the mined block channel, and `submitblock` would eventually fail with a full
        // channel even though the block was committed.
        //
        // So we keep receiving mined blocks while we wait, and only broadcast the latest one.
        // The pending broadcast is kept until peers are ready, so the latest block is still
        // advertised after a temporary lack of ready peers.
        //
        // A chain tip broadcast that is replaced by a mined block is sent after the mined block,
        // unless the mined block is the new best tip. Mined blocks can be on side chains, and the
        // tip change for the replaced broadcast has already been consumed from `chain_state`.
        let mut displaced_tip = None;

        loop {
            let mut superseded_blocks: usize = 0;
            let ready_result = loop {
                tokio::select! {
                    biased;

                    // Dropping an unfinished `Ready` future doesn't affect the service's
                    // readiness, and we call the service straight after it becomes ready.
                    ready_result = broadcast_network.ready() => break ready_result.map(|_| ()),

                    Some(mined_block) = recv_mined_block(&mut mined_block_receiver) => {
                        if is_block_submission {
                            superseded_blocks = superseded_blocks.saturating_add(1);
                        } else {
                            displaced_tip = Some((hash, height));
                        }

                        (hash, height) = mined_block;
                        is_block_submission = true;
                    }
                }
            };

            ready_result.map_err(PeerSetReadiness)?;

            if superseded_blocks > 0 {
                info!(
                    ?height,
                    ?hash,
                    superseded_blocks,
                    "peers were not ready for block broadcasts, only sending the latest mined block",
                );
            }

            // block broadcasts inform other nodes about new blocks,
            // so our internal Grow or Reset state doesn't matter to them
            let request = if is_block_submission {
                zn::Request::AdvertiseBlockToAll(hash)
            } else {
                zn::Request::AdvertiseBlock(hash, None)
            };

            let broadcast_fut = broadcast_network.call(request);

            // Await the broadcast future in a spawned task to avoid waiting on
            // `AdvertiseBlockToAll` requests when there are unready peers.
            // Broadcast requests don't return errors, and we'd just want to ignore them anyway.
            tokio::spawn(broadcast_fut);

            // TODO: Move this logic for marking the last change hash as seen to its own method.

            let is_best_tip = chain_state.latest_chain_tip().best_tip_hash() == Some(hash);

            // Mark the last change hash of `chain_state` as the last block submission hash to avoid
            // advertising a block hash to some peers twice.
            if is_block_submission
                && mined_block_receiver
                    .as_ref()
                    .is_some_and(|rx| rx.is_empty())
                && is_best_tip
            {
                chain_state.mark_last_change_hash(hash);
            }

            match displaced_tip.take() {
                Some(tip) if !is_best_tip => {
                    (hash, height) = tip;
                    is_block_submission = false;
                }
                _ => break,
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
