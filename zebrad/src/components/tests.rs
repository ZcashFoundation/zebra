use tokio::sync::mpsc::{self, UnboundedReceiver};
use tower::{buffer::Buffer, util::BoxService};
use zebra_network::{BoxError, Request, Response};

/// Create a mock service to represent a [`PeerSet`][zebra_network::PeerSet] and intercept the
/// requests it receives.
///
/// The intercepted requests are sent through an unbounded channel to the receiver that's also
/// returned from this function.
pub(crate) fn mock_peer_set() -> (
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
