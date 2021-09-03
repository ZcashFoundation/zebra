//! A task that gossips newly verified [`block::Hash`]es to peers.

use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_network as zn;
use zebra_state::ChainTipChange;

use crate::BoxError;

use super::{SyncStatus, TIPS_RESPONSE_TIMEOUT};

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
pub async fn gossip_best_tip_block_hashes<ZN>(
    mut sync_status: SyncStatus,
    mut chain_state: ChainTipChange,
    broadcast_network: ZN,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    info!("initializing block gossip task");

    // use the same timeout as tips requests,
    // so broadcasts don't delay the syncer too long
    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);

    // TODO:
    //
    // Optional Cleanup
    // - wrap returned errors in a newtype, so we can distinguish sync and tip watch errors
    // - refactor to avoid checking SyncStatus twice, maybe by passing a closure?
    loop {
        sync_status.wait_until_close_to_tip().await?;

        let tip_action = chain_state.wait_for_tip_change().await?;
        // check sync status again, because it might be a long time between blocks
        if sync_status.is_close_to_tip() {
            // unlike the mempool, we don't care if the change is a reset
            let request = zn::Request::AdvertiseBlock(tip_action.best_tip_hash());

            let height = tip_action.best_tip_height();
            info!(?height, ?request, "sending verified block broadcast");

            // broadcast requests don't return errors, and we'd just want to ignore them anyway
            let _ = broadcast_network.ready_and().await?.call(request).await;
        }
    }
}
