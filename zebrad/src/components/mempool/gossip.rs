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

/// The maximum number of times we will delay sending because there is a new change.
pub const MAX_CHANGES_BEFORE_SEND: usize = 10;

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
        let mut combined_changes = 1;

        // once we get new data in the channel, broadcast to peers,
        // the mempool automatically combines some transactions that arrive close together
        receiver.changed().await?;
        let mut txs = receiver.borrow().clone();
        tokio::task::yield_now().await;

        // also combine transactions that arrived shortly after this one
        while receiver.has_changed()? && combined_changes < MAX_CHANGES_BEFORE_SEND {
            // Correctness
            // - set the has_changed() flag to false using borrow_and_update()
            // - clone() so we don't hold the watch channel lock while modifying txs
            let extra_txs = receiver.borrow_and_update().clone();
            txs.extend(extra_txs.iter());

            combined_changes += 1;

            tokio::task::yield_now().await;
        }

        let txs_len = txs.len();
        let request = zn::Request::AdvertiseTransactionIds(txs);

        // TODO: rate-limit this info level log?
        info!(%request, changes = %combined_changes, "sending mempool transaction broadcast");
        debug!(
            ?request,
            changes = ?combined_changes,
            "full list of mempool transactions in broadcast"
        );

        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network.ready().await?.call(request).await;

        metrics::counter!("mempool.gossiped.transactions.total", txs_len as u64);
    }
}
