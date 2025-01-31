//! A task that gossips newly verified [`block::Hash`]es to peers.
//!
//! [`block::Hash`]: zebra_chain::block::Hash

use futures::TryFutureExt;
use thiserror::Error;
use tokio::sync::watch;
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_chain::block;
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
    mut sync_status: SyncStatus,
    mut chain_state: ChainTipChange,
    broadcast_network: ZN,
    mut mined_block_receiver: Option<watch::Receiver<(block::Hash, block::Height)>>,
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
        // wait for at least one tip change, to make sure we have a new block hash to broadcast
        let tip_action = chain_state.wait_for_tip_change().await.map_err(TipChange)?;
        // wait until we're close to the tip, because broadcasts are only useful for nodes near the tip
        // (if they're a long way from the tip, they use the syncer and block locators), unless a mined block
        // hash is received before `wait_until_close_to_tip()` is ready.
        let close_to_tip_fut = sync_status.wait_until_close_to_tip().map_err(SyncStatus);
        let (hash, height) = if let Some(mined_block_receiver) = &mut mined_block_receiver {
            tokio::select! {
                close_to_tip = close_to_tip_fut => {
                    close_to_tip?;
                    // get the latest tip change - it might be different to the change we awaited,
                    // because the syncer might take a long time to reach the tip
                    chain_state.last_tip_change().unwrap_or(tip_action).best_tip_hash_and_height()
                },

                recv_result = mined_block_receiver.changed() => {
                    recv_result.map_err(SyncStatus)?;
                    // we have a new block to broadcast from the `submitblock `RPC method, get block data and release the channel.
                    *mined_block_receiver.borrow_and_update()
                }
            }
        } else {
            close_to_tip_fut.await?;
            chain_state
                .last_tip_change()
                .unwrap_or(tip_action)
                .best_tip_hash_and_height()
        };

        // block broadcasts inform other nodes about new blocks,
        // so our internal Grow or Reset state doesn't matter to them
        let request = zn::Request::AdvertiseBlock(hash);

        debug!(?height, ?request, "sending committed block broadcast");
        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network
            .ready()
            .await
            .map_err(PeerSetReadiness)?
            .call(request)
            .await;

        // wait for at least the network timeout between gossips
        //
        // in practice, we expect blocks to arrive approximately every 75 seconds,
        // so waiting 6 seconds won't make much difference
        tokio::time::sleep(PEER_GOSSIP_DELAY).await;
    }
}
