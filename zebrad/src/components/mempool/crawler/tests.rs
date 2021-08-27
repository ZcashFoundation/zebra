use std::time::Duration;

use tokio::{
    sync::mpsc::{self, UnboundedReceiver},
    time::{self, timeout},
};
use tower::{buffer::Buffer, util::BoxService, BoxError};

use zebra_network::{Request, Response};

use super::{Crawler, SyncStatus, FANOUT, RATE_LIMIT_DELAY};

/// The number of iterations to crawl while testing.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// See more information in [`MAX_REQUEST_DELAY`].
const CRAWL_ITERATIONS: usize = 4;

/// The maximum time to wait for a request to arrive before considering it won't arrive.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// There are [`CRAWL_ITERATIONS`] requests that are expected to not be sent, so the test runs for
/// at least `MAX_REQUEST_DELAY * CRAWL_ITERATIONS`.
const MAX_REQUEST_DELAY: Duration = Duration::from_millis(250);

/// The amount of time to advance beyond the expected instant that the crawler wakes up.
const ERROR_MARGIN: Duration = Duration::from_millis(100);

#[tokio::test]
async fn crawler_requests_for_transaction_ids() {
    let (peer_set, mut requests) = mock_peer_set();

    // Mock the latest sync length in a state that enables the mempool.
    let (sync_status, mut recent_sync_lengths) = SyncStatus::new();
    for _ in 0..5 {
        recent_sync_lengths.push_extend_tips_length(0);
    }

    Crawler::spawn(peer_set, sync_status);

    time::pause();

    for _ in 0..CRAWL_ITERATIONS {
        for _ in 0..FANOUT {
            let request = timeout(MAX_REQUEST_DELAY, requests.recv()).await;

            assert!(matches!(request, Ok(Some(Request::MempoolTransactionIds))));
        }

        let extra_request = timeout(MAX_REQUEST_DELAY, requests.recv()).await;

        assert!(extra_request.is_err());

        time::advance(RATE_LIMIT_DELAY + ERROR_MARGIN).await;
    }
}

/// Create a mock service to represent a [`PeerSet`][zebra_network::PeerSet] and intercept the
/// requests it receives.
///
/// The intercepted requests are sent through an unbounded channel to the receiver that's also
/// returned from this function.
fn mock_peer_set() -> (
    Buffer<BoxService<Request, Response, BoxError>, Request>,
    UnboundedReceiver<Request>,
) {
    let (sender, receiver) = mpsc::unbounded_channel();

    let proxy_service = tower::service_fn(move |request| {
        let sender = sender.clone();

        async move {
            let _ = sender.send(request);

            Ok(Response::TransactionIds(vec![]))
        }
    });

    let service = Buffer::new(BoxService::new(proxy_service), 10);

    (service, receiver)
}
