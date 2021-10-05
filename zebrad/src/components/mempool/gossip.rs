//! A task that gossips [`transaction::UnminedTxId`] that enter the mempool to peers.

use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_network as zn;

use tokio::sync::watch;
use zebra_chain::transaction::UnminedTxId;

use std::collections::HashSet;

use crate::BoxError;

use crate::components::sync::TIPS_RESPONSE_TIMEOUT;

/// Run continuously, gossiping new [`transaction::UnminedTxId`] to peers.
///
/// Broadcast any [`transaction::UnminedTxId`] that gets stored in the mempool to all ready peers.
pub async fn gossip_mempool_transaction_id<ZN>(
    mut receiver: watch::Receiver<Option<UnminedTxId>>,
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

        let tx = receiver.borrow().unwrap();
        let mut hs = HashSet::new();
        hs.insert(tx);

        let request = zn::Request::AdvertiseTransactionIds(hs);

        info!(?request, "sending mempool transaction broadcast");

        // broadcast requests don't return errors, and we'd just want to ignore them anyway
        let _ = broadcast_network.ready_and().await?.call(request).await;
    }
}
