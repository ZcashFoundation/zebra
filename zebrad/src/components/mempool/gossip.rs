//! A task that gossips any [`zebra_chain::transaction::UnminedTxId`] that enters the mempool to peers.
//!
//! This module is just a function [`gossip_mempool_transaction_id`] that waits for mempool
//! insertion events received in a channel and broadcasts the transactions to peers.

use std::collections::HashSet;

use tokio::sync::broadcast::{
    self,
    error::{RecvError, TryRecvError},
};
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_chain::transaction::UnminedTxId;
use zebra_network::MAX_TX_INV_IN_SENT_MESSAGE;

use zebra_network as zn;

use crate::{
    components::sync::{PEER_GOSSIP_DELAY, TIPS_RESPONSE_TIMEOUT},
    BoxError,
};

/// The maximum number of channel messages we will combine into a single peer broadcast.
pub const MAX_CHANGES_BEFORE_SEND: usize = 10;

/// Runs continuously, gossiping new [`UnminedTxId`] to peers.
///
/// Broadcasts any new [`UnminedTxId`]s that get stored in the mempool to multiple ready peers.
pub async fn gossip_mempool_transaction_id<ZN>(
    mut receiver: broadcast::Receiver<HashSet<UnminedTxId>>,
    broadcast_network: ZN,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    let max_tx_inv_in_message: usize = MAX_TX_INV_IN_SENT_MESSAGE
        .try_into()
        .expect("constant fits in usize");

    info!("initializing transaction gossip task");

    // use the same timeout as tips requests,
    // so broadcasts don't delay the syncer too long
    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);

    loop {
        let mut combined_changes = 1;

        // once we get new data in the channel, broadcast to peers
        //
        // the mempool automatically combines some transaction IDs that arrive close together,
        // and this task also combines the changes that are in the channel before sending
        let mut txs = loop {
            match receiver.recv().await {
                Ok(txs) => break txs,
                Err(RecvError::Lagged(skip_count)) => info!(
                    ?skip_count,
                    "dropped transactions before gossiping due to heavy mempool or network load"
                ),
                Err(closed @ RecvError::Closed) => Err(closed)?,
            }
        };

        // also combine transaction IDs that arrived shortly after this one,
        // but limit the number of changes and the number of transaction IDs
        // (the network layer handles the actual limits, this just makes sure the loop terminates)
        while combined_changes <= MAX_CHANGES_BEFORE_SEND && txs.len() < max_tx_inv_in_message {
            match receiver.try_recv() {
                Ok(extra_txs) => txs.extend(extra_txs.iter()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(skip_count)) => info!(
                    ?skip_count,
                    "dropped transactions before gossiping due to heavy mempool or network load"
                ),
                Err(closed @ TryRecvError::Closed) => Err(closed)?,
            }

            combined_changes += 1;
        }

        let txs_len = txs.len();
        let request = zn::Request::AdvertiseTransactionIds(txs);

        info!(%request, changes = %combined_changes, "sending mempool transaction broadcast");
        debug!(
            ?request,
            changes = ?combined_changes,
            "full list of mempool transactions in broadcast"
        );

        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network.ready().await?.call(request).await;

        metrics::counter!("mempool.gossiped.transactions.total", txs_len as u64);

        // wait for at least the network timeout between gossips
        //
        // in practice, transactions arrive every 1-20 seconds,
        // so waiting 6 seconds can delay transaction propagation, in order to reduce peer load
        tokio::time::sleep(PEER_GOSSIP_DELAY).await;
    }
}
