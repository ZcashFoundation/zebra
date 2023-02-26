//! A task that gossips any [`zebra_chain::transaction::UnminedTxId`] that enters the mempool to peers.
//!
//! This module is just a function [`gossip_mempool_transaction_id`] that waits for mempool
//! insertion events received in a channel and broadcasts the transactions to peers.

use std::collections::HashSet;

use tokio::sync::watch;
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_chain::transaction::UnminedTxId;
use zebra_network as zn;

use crate::{components::sync::TIPS_RESPONSE_TIMEOUT, BoxError};

/// Runs continuously, gossiping new [`UnminedTxId`] to peers.
///
/// Broadcasts any [`UnminedTxId`] that gets stored in the mempool to all ready
/// peers.
///
/// [`UnminedTxId`]: zebra_chain::transaction::UnminedTxId
pub async fn gossip_mempool_transaction_id<ZN>(
    mut receiver: watch::Receiver<HashSet<UnminedTxId>>,
    broadcast_network: ZN,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    info!("initializing transaction gossip task");

    // use the same timeout as tips requests,
    // so broadcasts don't delay the syncer too long
    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);

    loop {
        // once we get new data in the channel, broadcast to peers,
        // the mempool automatically combines some transactions that arrive close together
        receiver.changed().await?;
        let mut txs = receiver.borrow().clone();
        tokio::task::yield_now().await;

        // also combine transactions that arrived shortly after this one
        while receiver.has_changed()? {
            // Correctness: reset has_changed(), don't hold the lock
            let extra_txs = receiver.borrow_and_update().clone();
            txs.extend(extra_txs.iter());
            tokio::task::yield_now().await;
        }

        let txs_len = txs.len();
        let request = zn::Request::AdvertiseTransactionIds(txs);

        info!(?request, "sending mempool transaction broadcast");

        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network.ready().await?.call(request).await;

        metrics::counter!("mempool.gossiped.transactions.total", txs_len as u64);
    }
}
