//! A task that gossips newly verified [`block::Hash`]es to peers.
//!
//! [`block::Hash`]: zebra_chain::block::Hash

use std::{collections::VecDeque, time::Duration};

use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{block, chain_tip::ChainTip};
use zebra_network as zn;
use zebra_state::ChainTipChange;

use crate::{
    components::sync::{SyncStatus, TIPS_RESPONSE_TIMEOUT},
    BoxError,
};

use BlockGossipError::*;

#[cfg(test)]
mod tests;

/// Errors that can occur when gossiping committed blocks.
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
/// Committed-tip gossip waits until sync is close to the network tip, and coalesces multiple
/// commits to the latest best-chain block. Successful mined-block commits may bypass the sync gate.
///
/// Broadcasts run one at a time. Readiness and responses have deadlines; notifications keep
/// draining while either is pending, retaining the latest mined block and the best tip independently.
pub async fn gossip_best_tip_block_hashes<ZN>(
    sync_status: SyncStatus,
    mut chain_state: ChainTipChange,
    mut broadcast_network: ZN,
    mut mined_block_receiver: Option<mpsc::Receiver<(block::Hash, block::Height)>>,
) -> Result<(), BlockGossipError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    info!("initializing block gossip task");

    // ponytail: scan at most 128 recent hashes; use an indexed cache if this bound grows.
    // Ordinary broadcasts must not suppress a later all-peer submission.
    const RECENT_BROADCAST_LIMIT: usize = 128;
    let mut recent_broadcasts: VecDeque<(block::Hash, bool)> = VecDeque::new();

    // A mined side-chain block must not displace an unsent best tip.
    let mut pending = [None; 2];
    loop {
        tokio::select! {
            biased;
            readiness = tokio::time::timeout(
                TIPS_RESPONSE_TIMEOUT,
                broadcast_network.ready(),
            ), if pending.iter().any(Option::is_some) => {
                match readiness {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => return Err(PeerSetReadiness(error)),
                    Err(_) => continue,
                }
                let slot = [0, 1]
                    .into_iter()
                    .find(|slot| pending[*slot].is_some())
                    .expect("readiness is polled only while a broadcast is pending");
                let (hash, height) = pending[slot]
                    .take()
                    .expect("the selected broadcast slot is populated");
                let is_block_submission = slot == 0;
                let request = if is_block_submission {
                    zn::Request::AdvertiseBlockToAll(hash)
                } else {
                    zn::Request::AdvertiseBlock(hash, None)
                };
                info!(?height, ?request, "sending block broadcast");
                let response = tokio::time::timeout(
                    TIPS_RESPONSE_TIMEOUT,
                    broadcast_network.call(request),
                );
                tokio::pin!(response);
                let succeeded = loop {
                    tokio::select! {
                        biased;
                        result = &mut response => break matches!(result, Ok(Ok(_))),
                        next = next_broadcast(
                            &sync_status,
                            &mut chain_state,
                            &mut mined_block_receiver,
                        ) => {
                            let (next_slot, block) = next?;
                            pending[next_slot] = Some(block);
                        }
                    }
                };
                if succeeded {
                    if let Some((_, all_peers)) = recent_broadcasts
                        .iter_mut()
                        .find(|(seen, _)| *seen == hash)
                    {
                        *all_peers |= is_block_submission;
                    } else {
                        if recent_broadcasts.len() == RECENT_BROADCAST_LIMIT {
                            recent_broadcasts.pop_front();
                        }
                        recent_broadcasts.push_back((hash, is_block_submission));
                    }
                    if is_block_submission
                        && chain_state.latest_chain_tip().best_tip_hash() == Some(hash)
                    {
                        chain_state.mark_last_change_hash(hash);
                    }
                }
            }
            next = next_broadcast(
                &sync_status,
                &mut chain_state,
                &mut mined_block_receiver,
            ) => {
                let (slot, block) = next?;
                pending[slot] = Some(block);
            }
        }
        for (slot, candidate) in pending.iter_mut().enumerate() {
            let Some((hash, _)) = candidate else {
                continue;
            };
            let is_block_submission = slot == 0;
            if recent_broadcasts
                .iter()
                .any(|(seen, all_peers)| *seen == *hash && (!is_block_submission || *all_peers))
            {
                *candidate = None;
            }
        }
    }
}

/// Waits for committed inventory, keeping mined blocks and best-tip inventory distinct.
async fn next_broadcast(
    sync_status: &SyncStatus,
    chain_state: &mut ChainTipChange,
    mined_block_receiver: &mut Option<mpsc::Receiver<(block::Hash, block::Height)>>,
) -> Result<(usize, (block::Hash, block::Height)), BlockGossipError> {
    let mut sync_status = sync_status.clone();
    let mut chain_tip = chain_state.clone();
    let has_mined_receiver = mined_block_receiver.is_some();
    let committed_tip = async move {
        let tip_action = chain_tip.wait_for_tip_change().await.map_err(TipChange)?;

        // A commit publishes the tip just before submitblock sends its notification. Give that
        // notification the same short race window as the mined-block propagation path.
        if has_mined_receiver {
            tokio::time::sleep(Duration::from_micros(100)).await;
        }
        sync_status
            .wait_until_close_to_tip()
            .await
            .map_err(SyncStatus)?;
        let best_tip = chain_tip
            .last_tip_change()
            .unwrap_or(tip_action)
            .best_tip_hash_and_height();
        Ok::<_, BlockGossipError>((best_tip, chain_tip))
    }
    .in_current_span();
    let submitted = async {
        match mined_block_receiver {
            Some(receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        biased;
        Some(block) = submitted => Ok((0, block)),
        tip = committed_tip => {
            let (tip, updated_chain_state) = tip?;
            *chain_state = updated_chain_state;
            Ok((1, tip))
        }
    }
}
