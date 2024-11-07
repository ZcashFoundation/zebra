//! Pending [`transparent::Output`] tracker for [`AwaitOutput` requests](zebra_node_services::mempool::Request::AwaitOutput).

use std::{collections::HashMap, future::Future};

use tokio::sync::broadcast;

use tower::BoxError;
use zebra_chain::transparent;

use zebra_node_services::mempool::Response;

/// Pending [`transparent::Output`] tracker for handling the mempool's
/// [`AwaitOutput` requests](zebra_node_services::mempool::Request::AwaitOutput).
#[derive(Debug, Default)]
pub struct PendingOutputs(HashMap<transparent::OutPoint, broadcast::Sender<transparent::Output>>);

impl PendingOutputs {
    /// Returns a future that will resolve to the `transparent::Output` pointed
    /// to by the given `transparent::OutPoint` when it is available.
    pub fn queue(
        &mut self,
        outpoint: transparent::OutPoint,
    ) -> impl Future<Output = Result<Response, BoxError>> {
        let mut receiver = self
            .0
            .entry(outpoint)
            .or_insert_with(|| {
                let (sender, _) = broadcast::channel(1);
                sender
            })
            .subscribe();

        async move {
            receiver
                .recv()
                .await
                .map(Response::UnspentOutput)
                .map_err(BoxError::from)
        }
    }

    /// Notify all requests waiting for the [`transparent::Output`] pointed to by
    /// the given [`transparent::OutPoint`] that the [`transparent::Output`] has
    /// arrived.
    #[inline]
    pub fn respond(&mut self, outpoint: &transparent::OutPoint, output: transparent::Output) {
        if let Some(sender) = self.0.remove(outpoint) {
            // Adding the outpoint as a field lets us cross-reference
            // with the trace of the verification that made the request.
            tracing::trace!(?outpoint, "found pending mempool output");
            let _ = sender.send(output);
        }
    }

    /// Scan the set of waiting Output requests for channels where all receivers
    /// have been dropped and remove the corresponding sender.
    pub fn prune(&mut self) {
        self.0.retain(|_, chan| chan.receiver_count() > 0);
    }

    /// Clears the inner [`HashMap`] of queued pending output requests.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
