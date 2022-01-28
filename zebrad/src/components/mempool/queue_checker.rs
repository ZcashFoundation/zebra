//! Zebra Mempool queue checker.
//!
//! The queue checker periodically sends a request to the mempool,
//! so that newly verified transactions are added to the mempool,
//! and gossiped to peers.
//!
//! The mempool performs these actions on every request,
//! but we can't guarantee that requests will arrive from peers
//! on a regular basis.
//!
//! Crawler queue requests are also too infrequent,
//! and they only happen if peers respond within the timeout.

use std::time::Duration;

use tokio::{task::JoinHandle, time::sleep};
use tower::{BoxError, Service, ServiceExt};
use tracing_futures::Instrument;

use crate::components::mempool;

/// The delay between queue check events.
///
/// This interval is chosen so that there are a significant number of
/// queue checks in each target block interval.
///
/// This allows transactions to propagate across the network for each block,
/// even if some peers are poorly connected.
const RATE_LIMIT_DELAY: Duration = Duration::from_secs(5);

/// The mempool queue checker.
///
/// The queue checker relies on the mempool to ignore requests when the mempool is inactive.
pub struct QueueChecker<Mempool> {
    /// The mempool service that receives crawled transaction IDs.
    mempool: Mempool,
}

impl<Mempool> QueueChecker<Mempool>
where
    Mempool:
        Service<mempool::Request, Response = mempool::Response, Error = BoxError> + Send + 'static,
    Mempool::Future: Send,
{
    /// Spawn an asynchronous task to run the mempool queue checker.
    pub fn spawn(mempool: Mempool) -> JoinHandle<Result<(), BoxError>> {
        let queue_checker = QueueChecker { mempool };

        tokio::spawn(queue_checker.run().in_current_span())
    }

    /// Periodically check if the mempool has newly verified transactions.
    ///
    /// Runs until the mempool returns an error,
    /// which happens when Zebra is shutting down.
    pub async fn run(mut self) -> Result<(), BoxError> {
        info!("initializing mempool queue checker task");

        loop {
            sleep(RATE_LIMIT_DELAY).await;
            self.check_queue().await?;
        }
    }

    /// Check if the mempool has newly verified transactions.
    async fn check_queue(&mut self) -> Result<(), BoxError> {
        debug!("checking for newly verified mempool transactions");

        // Since this is an internal request, we don't expect any errors.
        // So we propagate any unexpected errors to the task that spawned us.
        let response = self
            .mempool
            .ready()
            .await?
            .call(mempool::Request::CheckForVerifiedTransactions)
            .await?;

        match response {
            mempool::Response::CheckedForVerifiedTransactions => {}
            _ => {
                unreachable!("mempool did not respond with checked queue to mempool queue checker")
            }
        };

        Ok(())
    }
}
