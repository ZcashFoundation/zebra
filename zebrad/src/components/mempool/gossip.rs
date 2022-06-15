//! A task that gossips any [`zebra_chain::transaction::UnminedTxId`] that enters the mempool to peers.
//!
//! This module is just a function [`gossip_mempool_transaction_id`] that waits for mempool
//! insertion events received in a channel and broadcasts the transactions to peers.

use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_network as zn;

use tokio::sync::watch;
use zebra_chain::transaction::UnminedTxId;

use std::collections::HashSet;

use crate::BoxError;

use crate::components::sync::TIPS_RESPONSE_TIMEOUT;

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
        // once we get new data in the channel, broadcast to peers
        receiver.changed().await?;

        let txs = receiver.borrow().clone();
        let txs_len = txs.len();
        let request = zn::Request::AdvertiseTransactionIds(txs);

        info!(?request, "sending mempool transaction broadcast");

        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network.ready().await?.call(request).await;

        metrics::counter!("mempool.gossiped.transactions.total", txs_len as u64);
    }
}
