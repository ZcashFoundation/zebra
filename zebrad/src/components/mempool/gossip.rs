//! Gossip accepted transaction IDs, treating the change channel as a wakeup rather than storage.

use std::time::Duration;

use tokio::sync::broadcast::{
    self,
    error::{RecvError, TryRecvError},
};
use tower::{Service, ServiceExt};

use zebra_network as zn;
use zebra_node_services::mempool::{MempoolChange, Request, Response};

use crate::{components::sync::TIPS_RESPONSE_TIMEOUT, BoxError};

/// Maximum number of change notifications consumed between bounded pending-set drains.
pub const MAX_CHANGES_BEFORE_SEND: usize = 10;

/// Minimum interval between transaction inventory batches, independent of block gossip.
pub const TRANSACTION_GOSSIP_DELAY: Duration = Duration::from_secs(2);

/// Gossips still-live accepted IDs from the mempool's bounded pending set.
///
/// Lagged notifications cannot lose IDs. Full batches are drained without another wakeup,
/// and a failed network send retains its batch for retry.
pub async fn gossip_mempool_transaction_id<ZN, ZM>(
    mut receiver: broadcast::Receiver<MempoolChange>,
    broadcast_network: ZN,
    mempool: ZM,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZM: Service<Request, Response = Response, Error = BoxError> + Send + Clone + 'static,
    ZM::Future: Send,
{
    let limit = usize::try_from(zn::MAX_TX_INV_IN_SENT_MESSAGE)
        .expect("the network transaction inventory limit fits in usize");
    // Drain at startup as well: there may have been no subscriber when IDs were accepted.
    let mut drain_without_wakeup = true;

    loop {
        if !drain_without_wakeup {
            loop {
                match receiver.recv().await {
                    Ok(change) if change.is_added() => break,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(count)) => {
                        metrics::counter!("mempool.gossip.lagged.events.total").increment(count);
                        break;
                    }
                    Err(closed @ RecvError::Closed) => return Err(closed.into()),
                }
            }
            for _ in 0..MAX_CHANGES_BEFORE_SEND {
                match receiver.try_recv() {
                    Ok(_) => {}
                    Err(TryRecvError::Lagged(count)) => {
                        metrics::counter!("mempool.gossip.lagged.events.total").increment(count);
                    }
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                }
            }
        }

        let Response::TransactionIds(ids) = tokio::time::timeout(
            TIPS_RESPONSE_TIMEOUT,
            mempool
                .clone()
                .oneshot(Request::TakePendingGossipTransactionIds { limit }),
        )
        .await??
        else {
            return Err("pending gossip drain must return transaction IDs".into());
        };
        drain_without_wakeup = !ids.is_empty();
        if ids.is_empty() {
            continue;
        }

        let count = u64::try_from(ids.len()).expect("a bounded inventory count fits in u64");
        let request = zn::Request::AdvertiseTransactionIds(ids, None);
        loop {
            match tokio::time::timeout(
                TIPS_RESPONSE_TIMEOUT,
                broadcast_network.clone().oneshot(request.clone()),
            )
            .await
            {
                Ok(Ok(_)) => break,
                result => {
                    debug!(
                        ?result,
                        "transaction gossip failed, retaining batch for retry"
                    );
                    tokio::time::sleep(TRANSACTION_GOSSIP_DELAY).await;
                }
            }
        }
        metrics::counter!("mempool.gossiped.transactions.total").increment(count);
        tokio::time::sleep(TRANSACTION_GOSSIP_DELAY).await;
    }
}

#[cfg(test)]
mod tests;
