use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use color_eyre::eyre::{eyre, Report};
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
use zebra_state as zs;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Manages download and verification of blocks gossiped to this peer.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + 'static,
    ZS::Future: Send,
{
    network: ZN,
    verifier: ZV,
    state: ZS,
    #[pin]
    pending: FuturesUnordered<JoinHandle<Result<block::Hash, (BoxError, block::Hash)>>>,
    cancel_handles: HashMap<block::Hash, oneshot::Sender<()>>,
}

impl<ZN, ZV, ZS> Stream for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + 'static,
    ZS::Future: Send,
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

impl<ZN, ZV, ZS> Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + 'static,
    ZS::Future: Send,
{
    /// Initialize a new download stream with the provided `network` and
    /// `verifier` services.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(network: ZN, verifier: ZV, state: ZS) -> Self {
        Self {
            network,
            verifier,
            state,
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
    pub async fn download_and_verify(&mut self, hash: block::Hash) -> Result<bool, Report> {
        if self.cancel_handles.contains_key(&hash) {
            return Ok(false);
        }

        // Check if the block is already in the state.
        let block_state_req = self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zs::Request::Block(zs::HashOrHeight::from(hash)))
            .await;

        let block_in_chain = match block_state_req {
            Ok(zs::Response::Block(block)) => block,
            _ => None,
        };

        // Block already in state, get out.
        if block_in_chain.is_some() {
            return Ok(false);
        }

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
            .await
            .map_err(|e| eyre!(e))?
            .call(zn::Request::BlocksByHash(std::iter::once(hash).collect()));
        tracing::debug!("requested block");

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let mut verifier = self.verifier.clone();
        let task = tokio::spawn(
            async move {
                let rsp = tokio::select! {
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to download completion");
                        metrics::counter!("gossip.cancelled.download.count", 1);
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
                metrics::counter!("gossip.downloaded.block.count", 1);

                let rsp = verifier.ready_and().await?.call(block);
                let verification = tokio::select! {
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to verification");
                        metrics::counter!("gossip.cancelled.verify.count", 1);
                        return Err("canceled block_fetch_verify".into())
                    }
                    verification = rsp => verification,
                };
                if verification.is_ok() {
                    metrics::counter!("sync.verified.block.count", 1);
                }

                verification
            }
            .in_current_span()
            // Tack the hash onto the error so we can remove the cancel handle
            // on failure as well as on success.
            .map_err(move |e| (e, hash)),
        );

        self.pending.push(task);
        // XXX replace with expect_none when stable
        assert!(
            self.cancel_handles.insert(hash, cancel_tx).is_none(),
            "blocks are only queued once"
        );

        Ok(true)
    }
}
