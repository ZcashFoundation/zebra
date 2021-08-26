use std::{
    collections::HashMap,
    pin::Pin,
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
use tower::{hedge, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{block::Height, transaction::UnminedTxId};
use zebra_consensus::transaction as tx;
use zebra_network as zn;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Copy, Clone, Debug)]
pub(crate) struct AlwaysHedge;

impl<Request: Clone> hedge::Policy<Request> for AlwaysHedge {
    fn can_retry(&self, _req: &Request) -> bool {
        true
    }
    fn clone_request(&self, req: &Request) -> Option<Request> {
        Some(req.clone())
    }
}

/// Represents a [`Stream`] of download and verification tasks.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    // Services
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded transactions.
    verifier: ZV,

    // Internal downloads state
    /// A list of pending transaction download and verify tasks.
    #[pin]
    pending: FuturesUnordered<JoinHandle<Result<UnminedTxId, (BoxError, UnminedTxId)>>>,

    /// A list of channels that can be used to cancel pending transaction download and
    /// verify tasks.
    cancel_handles: HashMap<UnminedTxId, oneshot::Sender<()>>,
}

impl<ZN, ZV> Stream for Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    type Item = Result<UnminedTxId, BoxError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = self.project();
        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // If no download and verify tasks have exited since the last poll, this
        // task is scheduled for wakeup when the next task becomes ready.
        //
        // TODO:
        // This would be cleaner with poll_map #63514, but that's nightly only.
        if let Some(join_result) = ready!(this.pending.poll_next(cx)) {
            match join_result.expect("transaction download and verify tasks must not panic") {
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
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
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

    /// Queue a transaction for download and verification.
    ///
    /// This method waits for the network to become ready, and returns an error
    /// only if the network service fails. It returns immediately after queuing
    /// the request.
    #[instrument(skip(self, txid), fields(txid = %txid))]
    pub async fn download_and_verify(&mut self, txid: UnminedTxId) -> Result<(), Report> {
        if self.cancel_handles.contains_key(&txid) {
            return Err(eyre!("duplicate txid queued for download: {:?}", txid));
        }

        // We construct the transaction requests sequentially, waiting for the peer
        // set to be ready to process each request. This ensures that we start
        // transaction downloads in the order we want them (though they may resolve
        // out of order), and it means that we respect backpressure. Otherwise,
        // if we waited for readiness and did the service call in the spawned
        // tasks, all of the spawned tasks would race each other waiting for the
        // network to become ready.
        tracing::debug!("waiting to request transaction");
        let tx_req = self.network.ready_and().await.map_err(|e| eyre!(e))?.call(
            zn::Request::TransactionsById(std::iter::once(txid).collect()),
        );
        tracing::debug!("requested transaction");

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let mut verifier = self.verifier.clone();
        let task = tokio::spawn(
            async move {
                // TODO: if the verifier and cancel are both ready, which should
                //       we prefer? (Currently, select! chooses one at random.)
                let rsp = tokio::select! {
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to download completion");
                        metrics::counter!("mempool.cancelled.transaction.count", 1);
                        return Err("canceled trasaction download".into())
                    }
                    rsp = tx_req => rsp?,
                };

                let tx = if let zn::Response::Transactions(txs) = rsp {
                    txs.into_iter()
                        .next()
                        .expect("successful response has the transaction in it")
                } else {
                    unreachable!("wrong response to transaction request");
                };
                metrics::counter!("mempoool.downloaded.transaction.count", 1);

                let rsp = verifier.ready_and().await?.call(tx::Request::Mempool {
                    transaction: tx,
                    // TODO: which height to use?
                    height: Height(0),
                });
                // TODO: if the verifier and cancel are both ready, which should
                //       we prefer? (Currently, select! chooses one at random.)
                let verification = tokio::select! {
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to verification");
                        metrics::counter!("mempool.cancelled.verify.count", 1);
                        return Err("canceled transaction verification".into())
                    }
                    verification = rsp => verification,
                };
                if verification.is_ok() {
                    metrics::counter!("mempool.verified.transaction.count", 1);
                }

                verification
            }
            .in_current_span()
            // Tack the hash onto the error so we can remove the cancel handle
            // on failure as well as on success.
            .map_err(move |e| (e, txid)),
        );

        self.pending.push(task);
        // XXX replace with expect_none when stable
        assert!(
            self.cancel_handles.insert(txid, cancel_tx).is_none(),
            "transactions are only queued once"
        );

        Ok(())
    }

    /// Cancel all running tasks and reset the downloader state.
    pub fn cancel_all(&mut self) {
        // Replace the pending task list with an empty one and drop it.
        let _ = std::mem::take(&mut self.pending);
        // Signal cancellation to all running tasks.
        // Since we already dropped the JoinHandles above, they should
        // fail silently.
        for (_hash, cancel) in self.cancel_handles.drain() {
            let _ = cancel.send(());
        }
        assert!(self.pending.is_empty());
        assert!(self.cancel_handles.is_empty());
    }

    /// Get the number of currently in-flight download tasks.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }
}
