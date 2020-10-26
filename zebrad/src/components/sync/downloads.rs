use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::{self, Block};
use zebra_network as zn;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Represents a [`Stream`] of download and verification tasks during chain sync.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    network: ZN,
    verifier: ZV,
    #[pin]
    pending: FuturesUnordered<JoinHandle<Result<block::Hash, (BoxError, block::Hash)>>>,
    cancel_handles: HashMap<block::Hash, oneshot::Sender<()>>,
}

impl<ZN, ZV> Stream for Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    type Item = Result<block::Hash, BoxError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = self.project();
        // This would be cleaner with poll_map #63514, but that's nightly only.
        if let Some(join_result) = ready!(this.pending.poll_next(cx)) {
            match join_result.expect("block download and verify tasks must not panic") {
                Ok(hash) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Ok(hash)))
                }
                Err((e, hash)) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Err(e)))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.pending.size_hint()
    }
}

impl<ZN, ZV> Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Initialize a new download stream with the provided `network` and
    /// `verifier` services.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(network: ZN, verifier: ZV) -> Self {
        Self {
            network,
            verifier,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// This method waits for the network to become ready, and returns an error
    /// only if the network service fails. It returns immediately after queuing
    /// the request.
    #[instrument(skip(self))]
    pub async fn queue_download(&mut self, hash: block::Hash) -> Result<(), BoxError> {
        // We construct the block requests sequentially, waiting for the peer
        // set to be ready to process each request. This ensures that we start
        // block downloads in the order we want them (though they may resolve
        // out of order), and it means that we respect backpressure. Otherwise,
        // if we waited for readiness and did the service call in the spawned
        // tasks, all of the spawned tasks would race each other waiting for the
        // network to become ready.
        tracing::debug!("waiting to request block");
        let block_req = self
            .network
            .ready_and()
            .await?
            .call(zn::Request::BlocksByHash(std::iter::once(hash).collect()));
        tracing::debug!("requested block");

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let span = tracing::warn_span!("block_fetch_verify", ?hash);
        let mut verifier = self.verifier.clone();
        let task = tokio::spawn(
            async move {
                let rsp = tokio::select! {
                    _ = &mut cancel_rx => {
                        metrics::counter!("sync.cancelled.download.count", 1);
                        return Err("canceled block_fetch_verify".into())
                    }
                    rsp = block_req => rsp?,
                };

                let block = if let zn::Response::Blocks(blocks) = rsp {
                    blocks
                        .into_iter()
                        .next()
                        .expect("successful response has the block in it")
                } else {
                    unreachable!("wrong response to block request");
                };
                metrics::counter!("sync.downloaded.block.count", 1);

                let rsp = verifier.ready_and().await?.call(block);
                let verification = tokio::select! {
                    _ = &mut cancel_rx => {
                        metrics::counter!("sync.cancelled.verify.count", 1);
                        return Err("canceled block_fetch_verify".into())
                    }
                    verification = rsp => verification,
                };
                if verification.is_ok() {
                    metrics::counter!("sync.verified.block.count", 1);
                }

                verification
            }
            .instrument(span)
            // Tack the hash onto the error so we can remove the cancel handle
            // on failure as well as on success.
            .map_err(move |e| (e, hash)),
        );

        self.cancel_handles.insert(hash, cancel_tx);
        self.pending.push(task);

        Ok(())
    }

    /// Cancel all running tasks and reset the downloader state.
    pub fn cancel_all(&mut self) {
        // Signal cancellation to all running tasks.
        for (_hash, cancel) in self.cancel_handles.drain() {
            let _ = cancel.send(());
        }
        // Replace the pending task list with an empty one and drop it.
        let _ = std::mem::take(&mut self.pending);
    }

    /// Get the number of currently in-flight download tasks.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }
}
