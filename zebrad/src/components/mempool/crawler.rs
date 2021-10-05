//! Zebra Mempool crawler.
//!
//! The crawler periodically requests transactions from peers in order to populate the mempool.

use std::time::Duration;

use futures::{stream::FuturesUnordered, StreamExt};
use tokio::{task::JoinHandle, time::sleep};
use tower::{timeout::Timeout, BoxError, Service, ServiceExt};

use zebra_network as zn;

use super::{
    super::{mempool, sync::SyncStatus},
    downloads::Gossip,
};

#[cfg(test)]
mod tests;

/// The number of peers to request transactions from per crawl event.
const FANOUT: usize = 4;

/// The delay between crawl events.
const RATE_LIMIT_DELAY: Duration = Duration::from_secs(75);

/// The time to wait for a peer response.
///
/// # Correctness
///
/// If this timeout is removed or set too high, the crawler may hang waiting for a peer to respond.
///
/// If this timeout is set too low, the crawler may fail to populate the mempool.
const PEER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

/// The mempool transaction crawler.
pub struct Crawler<PeerSet, Mempool> {
    peer_set: Timeout<PeerSet>,
    mempool: Mempool,
    status: SyncStatus,
}

impl<PeerSet, Mempool> Crawler<PeerSet, Mempool>
where
    PeerSet:
        Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone + Send + 'static,
    PeerSet::Future: Send,
    Mempool:
        Service<mempool::Request, Response = mempool::Response, Error = BoxError> + Send + 'static,
    Mempool::Future: Send,
{
    /// Spawn an asynchronous task to run the mempool crawler.
    pub fn spawn(
        peer_set: PeerSet,
        mempool: Mempool,
        status: SyncStatus,
    ) -> JoinHandle<Result<(), BoxError>> {
        let crawler = Crawler {
            peer_set: Timeout::new(peer_set, PEER_RESPONSE_TIMEOUT),
            mempool,
            status,
        };

        tokio::spawn(crawler.run())
    }

    /// Periodically crawl peers for transactions to include in the mempool.
    ///
    /// Runs until the [`SyncStatus`] loses its connection to the chain syncer, which happens when
    /// Zebra is shutting down.
    pub async fn run(mut self) -> Result<(), BoxError> {
        while self.status.wait_until_close_to_tip().await.is_ok() {
            self.crawl_transactions().await?;
            sleep(RATE_LIMIT_DELAY).await;
        }

        Ok(())
    }

    /// Crawl peers for transactions.
    ///
    /// Concurrently request [`FANOUT`] peers for transactions to include in the mempool.
    async fn crawl_transactions(&mut self) -> Result<(), BoxError> {
        let peer_set = self.peer_set.clone();

        trace!("Crawling for mempool transactions");

        let mut requests = FuturesUnordered::new();
        // get readiness for one peer at a time, to avoid peer set contention
        for _ in 0..FANOUT {
            let mut peer_set = peer_set.clone();
            // end the task on permanent peer set errors
            let peer_set = peer_set.ready_and().await?;

            requests.push(peer_set.call(zn::Request::MempoolTransactionIds));
        }

        while let Some(result) = requests.next().await {
            // log individual response errors
            match result {
                Ok(response) => self.handle_response(response).await?,
                // TODO: Reduce the log level of the errors (#2655).
                Err(error) => info!("Failed to crawl peer for mempool transactions: {}", error),
            }
        }

        Ok(())
    }

    /// Handle a peer's response to the crawler's request for transactions.
    async fn handle_response(&mut self, response: zn::Response) -> Result<(), BoxError> {
        let transaction_ids: Vec<_> = match response {
            zn::Response::TransactionIds(ids) => ids.into_iter().map(Gossip::Id).collect(),
            _ => unreachable!("Peer set did not respond with transaction IDs to mempool crawler"),
        };

        trace!(
            "Mempool crawler received {} transaction IDs",
            transaction_ids.len()
        );

        if !transaction_ids.is_empty() {
            self.queue_transactions(transaction_ids).await?;
        }

        Ok(())
    }

    /// Forward the crawled transactions IDs to the mempool transaction downloader.
    async fn queue_transactions(&mut self, transaction_ids: Vec<Gossip>) -> Result<(), BoxError> {
        let call_result = self
            .mempool
            .ready_and()
            .await?
            .call(mempool::Request::Queue(transaction_ids))
            .await;

        let queue_errors = match call_result {
            Ok(mempool::Response::Queued(queue_results)) => {
                queue_results.into_iter().filter_map(Result::err)
            }
            Ok(_) => unreachable!("Mempool did not respond with queue results to mempool crawler"),
            Err(call_error) => {
                debug!("Ignoring unexpected peer behavior: {}", call_error);
                return Ok(());
            }
        };

        for error in queue_errors {
            debug!("Failed to download a crawled transaction: {}", error);
        }

        Ok(())
    }
}
