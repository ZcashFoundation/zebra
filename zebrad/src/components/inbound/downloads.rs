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
use zebra_state as zs;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Manages download and verification of blocks gossiped to this peer.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
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
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
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
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
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
    /// Returns true if the block was newly queued, and false if it was already queued.
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(&mut self, hash: block::Hash) -> bool {
        if self.cancel_handles.contains_key(&hash) {
            tracing::debug!("hash already queued for download");
            return false;
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let state = self.state.clone();
        let network = self.network.clone();
        let verifier = self.verifier.clone();

        let fut = async move {
            // Check if the block is already in the state.
            match state.oneshot(zs::Request::Depth(hash.into())).await {
                Ok(zs::Response::Depth(None)) => Ok(()),
                Ok(zs::Response::Depth(Some(_))) => Err("already present".into()),
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(e),
            }?;

            let block = if let zn::Response::Blocks(blocks) = network
                .oneshot(zn::Request::BlocksByHash(std::iter::once(hash).collect()))
                .await?
            {
                blocks
                    .into_iter()
                    .next()
                    .expect("successful response has the block in it")
            } else {
                unreachable!("wrong response to block request");
            };
            metrics::counter!("gossip.downloaded.block.count", 1);

            verifier.oneshot(block).await
        }
        .map_ok(|hash| {
            metrics::counter!("gossip.verified.block.count", 1);
            hash
        })
        // Tack the hash onto the error so we can remove the cancel handle
        // on failure as well as on success.
        .map_err(move |e| (e, hash))
        .in_current_span();

        let task = tokio::spawn(async move {
            tokio::select! {
                _ = &mut cancel_rx => {
                    tracing::trace!("task cancelled prior to completion");
                    metrics::counter!("gossip.cancelled.count", 1);
                    Err(("canceled".into(), hash))
                }
                verification = fut => verification,
            }
        });

        self.pending.push(task);
        // XXX replace with expect_none when stable
        assert!(
            self.cancel_handles.insert(hash, cancel_tx).is_none(),
            "blocks are only queued once"
        );

        tracing::debug!("queued hash for download");
        true
    }
}
