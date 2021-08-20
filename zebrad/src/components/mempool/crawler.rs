//! Zebra Mempool crawler.
//!
//! The crawler periodically requests transactions from peers in order to populate the mempool.

use std::time::Duration;

use futures::{stream, StreamExt, TryStreamExt};
use tokio::{sync::Mutex, time::sleep};
use tower::{BoxError, Service, ServiceExt};

use zebra_network::{Request, Response};

/// The number of peers to request transactions from per crawl event.
const FANOUT: usize = 4;

/// The delay between crawl events.
const RATE_LIMIT_DELAY: Duration = Duration::from_secs(75);

/// The mempool transaction crawler.
pub struct Crawler<S> {
    peer_set: Mutex<S>,
}

impl<S> Crawler<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    /// Spawn an asynchronous task to run the mempool crawler.
    pub fn spawn(peer_set: S) {
        let crawler = Crawler {
            peer_set: Mutex::new(peer_set),
        };

        tokio::spawn(crawler.run());
    }

    /// Periodically crawl peers for transactions to include in the mempool.
    pub async fn run(self) {
        loop {
            self.wait_until_enabled().await;
            self.crawl_transactions().await;
            sleep(RATE_LIMIT_DELAY).await;
        }
    }

    /// Wait until the mempool is enabled.
    async fn wait_until_enabled(&self) {
        // TODO: Check if synchronizing up to chain tip has finished (#).
    }

    /// Crawl peers for transactions.
    ///
    /// Concurrently request [`FANOUT`] peers for transactions to include in the mempool.
    async fn crawl_transactions(&self) {
        let requests = stream::repeat(Request::MempoolTransactionIds).take(FANOUT);
        let peer_set = self.peer_set.lock().await.clone();

        trace!("Crawling for mempool transactions");

        peer_set
            .call_all(requests)
            .unordered()
            .and_then(|response| self.handle_response(response))
            // TODO: Reduce the log level of the errors (#2655).
            .inspect_err(|error| info!("Failed to crawl peer for mempool transactions: {}", error))
            .for_each(|_| async {})
            .await;
    }

    /// Handle a peer's response to the crawler's request for transactions.
    async fn handle_response(&self, response: Response) -> Result<(), BoxError> {
        let transaction_ids = match response {
            Response::TransactionIds(ids) => ids,
            _ => unreachable!("Peer set did not respond with transaction IDs to mempool crawler"),
        };

        trace!(
            "Mempool crawler received {} transaction IDs",
            transaction_ids.len()
        );

        // TODO: Download transactions and send them to the mempool

        Ok(())
    }
}
