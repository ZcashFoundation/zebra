use tower::{Service, ServiceExt};
use tracing::warn;

use zebra_chain::{block, chain_tip::ChainTip};
use zebra_network::zakura::BlockSyncFrontiers;

use super::ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT;

pub(crate) fn verified_block_tip_from_state(
    finalized_tip: Option<(block::Height, block::Hash)>,
    tip: Option<(block::Height, block::Hash)>,
    empty_state_tip: (block::Height, block::Hash),
) -> (block::Height, block::Hash) {
    let finalized_tip = finalized_tip.unwrap_or(empty_state_tip);
    let tip = tip.unwrap_or(empty_state_tip);

    if finalized_tip.0 > tip.0 {
        finalized_tip
    } else {
        tip
    }
}

pub(crate) async fn query_block_sync_frontiers<ReadState>(
    read_state: ReadState,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
) -> Option<BlockSyncFrontiers>
where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let latest_tip = latest_chain_tip.best_tip_height_and_hash();
    let finalized_tip = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::FinalizedTip),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::FinalizedTip(tip))) => tip,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected FinalizedTip response");
            None
        }
        Ok(Err(error)) => {
            warn!(
                ?error,
                "failed to refresh Zakura block-sync finalized frontier"
            );
            None
        }
        Err(_elapsed) => {
            warn!("timed out refreshing Zakura block-sync finalized frontier");
            None
        }
    };

    match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state.oneshot(zebra_state::ReadRequest::Tip),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::Tip(tip))) => {
            let state_tip = (finalized_tip.is_some() || tip.is_some()).then(|| {
                verified_block_tip_from_state(
                    finalized_tip,
                    tip,
                    latest_tip.unwrap_or((block::Height(0), block::Hash([0; 32]))),
                )
            });
            let (height, hash) = match (state_tip, latest_tip) {
                (Some(state_tip), latest_tip) => {
                    verified_block_tip_from_state(Some(state_tip), latest_tip, state_tip)
                }
                (None, Some(latest_tip)) => latest_tip,
                (None, None) => return None,
            };
            let finalized_height = finalized_tip.map_or(block::Height(0), |(height, _)| height);
            Some(BlockSyncFrontiers {
                finalized_height,
                verified_block_tip: height,
                verified_block_hash: hash,
            })
        }
        Ok(Ok(response)) => {
            warn!(?response, "unexpected Tip response");
            None
        }
        Ok(Err(error)) => {
            warn!(?error, "failed to refresh Zakura block-sync body frontier");
            None
        }
        Err(_elapsed) => {
            warn!("timed out refreshing Zakura block-sync body frontier");
            None
        }
    }
}
