use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use color_eyre::eyre::eyre;
use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::transaction::{UnminedTx, UnminedTxId};
use zebra_consensus::transaction as tx;
use zebra_network as zn;
use zebra_state as zs;

use crate::components::sync::{BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Controls how long we wait for a transaction download request to complete.
///
/// This is currently equal to [`crate::components::sync::BLOCK_DOWNLOAD_TIMEOUT`] for
/// consistency, even though parts of the rationale used for defining the value
/// don't apply here (e.g. we can drop transactions hashes when the queue is full).
pub(crate) const TRANSACTION_DOWNLOAD_TIMEOUT: Duration = BLOCK_DOWNLOAD_TIMEOUT;

/// Controls how long we wait for a transaction verify request to complete.
///
/// This is currently equal to [`crate::components::sync::BLOCK_VERIFY_TIMEOUT`] for
/// consistency.
///
/// This timeout may lead to denial of service, which will be handled in
/// https://github.com/ZcashFoundation/zebra/issues/2694
pub(crate) const TRANSACTION_VERIFY_TIMEOUT: Duration = BLOCK_VERIFY_TIMEOUT;

/// The maximum number of concurrent inbound download and verify tasks.
///
/// We expect the mempool crawler to download and verify most mempool transactions, so this bound
/// can be small.
///
/// ## Security
///
/// We use a small concurrency limit, to prevent memory denial-of-service
/// attacks.
///
/// The maximum transaction size is 2 million bytes. A deserialized malicious
/// transaction with ~225_000 transparent outputs can take up 9MB of RAM.
/// (See #1880 for more details.)
///
/// Malicious transactions will eventually timeout or fail validation.
/// Once validation fails, the transaction is dropped, and its memory is deallocated.
///
/// Since Zebra keeps an `inv` index, inbound downloads for malicious transactions
/// will be directed to the malicious node that originally gossiped the hash.
/// Therefore, this attack can be carried out by a single malicious node.
const MAX_INBOUND_CONCURRENCY: usize = 10;

/// The action taken in response to a peer's gossiped transaction hash.
#[derive(Debug)]
pub enum DownloadAction {
    /// The transaction hash was successfully queued for download and verification.
    AddedToQueue,

    /// The transaction hash is already queued, so this request was ignored.
    ///
    /// Another peer has already gossiped the same hash to us, or the mempool crawler has fetched it.
    AlreadyQueued,

    /// The queue is at capacity, so this request was ignored.
    ///
    /// The mempool crawler should discover this transaction later.
    /// If it is mined into a block, it will be downloaded by the syncer, or the inbound block downloader.
    ///
    /// The queue's capacity is [`MAX_INBOUND_CONCURRENCY`].
    FullQueue,
}

/// A gossiped transaction, which can be the transaction itself or just its ID.
#[derive(Debug)]
pub enum GossipedTx {
    Id(UnminedTxId),
    Tx(UnminedTx),
}

impl GossipedTx {
    /// Return the [`UnminedTxId`] of a gossiped transaction.
    pub fn id(&self) -> UnminedTxId {
        match self {
            GossipedTx::Id(txid) => *txid,
            GossipedTx::Tx(tx) => tx.id,
        }
    }
}

impl From<UnminedTxId> for GossipedTx {
    fn from(txid: UnminedTxId) -> Self {
        GossipedTx::Id(txid)
    }
}

impl From<UnminedTx> for GossipedTx {
    fn from(tx: UnminedTx) -> Self {
        GossipedTx::Tx(tx)
    }
}

/// Represents a [`Stream`] of download and verification tasks.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    // Services
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded transactions.
    verifier: ZV,

    /// A service that manages cached blockchain state.
    state: ZS,

    // Internal downloads state
    /// A list of pending transaction download and verify tasks.
    #[pin]
    pending: FuturesUnordered<JoinHandle<Result<UnminedTx, (BoxError, UnminedTxId)>>>,

    /// A list of channels that can be used to cancel pending transaction download and
    /// verify tasks.
    cancel_handles: HashMap<UnminedTxId, oneshot::Sender<()>>,
}

impl<ZN, ZV, ZS> Stream for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    type Item = Result<UnminedTx, BoxError>;

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
        // TODO: this would be cleaner with poll_map (#2693)
        if let Some(join_result) = ready!(this.pending.poll_next(cx)) {
            match join_result.expect("transaction download and verify tasks must not panic") {
                Ok(tx) => {
                    this.cancel_handles.remove(&tx.id);
                    Poll::Ready(Some(Ok(tx)))
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
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
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

    /// Queue a transaction for download (if needed) and verification.
    ///
    /// Returns the action taken in response to the queue request.
    #[instrument(skip(self, gossiped_tx), fields(txid = %gossiped_tx.id()))]
    pub fn download_if_needed_and_verify(&mut self, gossiped_tx: GossipedTx) -> DownloadAction {
        let txid = gossiped_tx.id();

        if self.cancel_handles.contains_key(&txid) {
            tracing::debug!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "transaction id already queued for inbound download: ignored transaction"
            );
            return DownloadAction::AlreadyQueued;
        }

        if self.pending.len() >= MAX_INBOUND_CONCURRENCY {
            tracing::info!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "too many transactions queued for inbound download: ignored transaction"
            );
            return DownloadAction::FullQueue;
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let mut state = self.state.clone();

        let fut = async move {
            // Don't download/verify if the transaction is already in the state.
            Self::transaction_in_state(&mut state, txid).await?;

            let height = match state.oneshot(zs::Request::Tip).await {
                Ok(zs::Response::Tip(None)) => Err("no block at the tip".into()),
                Ok(zs::Response::Tip(Some((height, _hash)))) => Ok(height),
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(e),
            }?;
            let height = (height + 1).ok_or_else(|| eyre!("no next height"))?;

            let tx = match gossiped_tx {
                GossipedTx::Id(txid) => {
                    let req = zn::Request::TransactionsById(std::iter::once(txid).collect());

                    let tx = match network.oneshot(req).await? {
                        zn::Response::Transactions(mut txs) => txs
                            .pop()
                            .expect("successful response has the transaction in it"),
                        _ => unreachable!("wrong response to transaction request"),
                    };

                    metrics::counter!("gossip.downloaded.transaction.count", 1);
                    tx
                }
                GossipedTx::Tx(tx) => {
                    metrics::counter!("gossip.pushed.transaction.count", 1);
                    tx
                }
            };

            let result = verifier
                .oneshot(tx::Request::Mempool {
                    transaction: tx.clone(),
                    height,
                })
                .map_ok(|_hash| tx)
                .await;

            tracing::debug!(?txid, ?result, "verified transaction for the mempool");

            result
        }
        .map_ok(|tx| {
            metrics::counter!("gossip.verified.transaction.count", 1);
            tx
        })
        // Tack the hash onto the error so we can remove the cancel handle
        // on failure as well as on success.
        .map_err(move |e| (e, txid))
        .in_current_span();

        let task = tokio::spawn(async move {
            // TODO: if the verifier and cancel are both ready, which should we
            //       prefer? (Currently, select! chooses one at random.)
            tokio::select! {
                _ = &mut cancel_rx => {
                    tracing::trace!("task cancelled prior to completion");
                    metrics::counter!("gossip.cancelled.count", 1);
                    Err(("canceled".into(), txid))
                }
                verification = fut => verification,
            }
        });

        self.pending.push(task);
        assert!(
            self.cancel_handles.insert(txid, cancel_tx).is_none(),
            "transactions are only queued once"
        );

        tracing::debug!(
            ?txid,
            queue_len = self.pending.len(),
            ?MAX_INBOUND_CONCURRENCY,
            "queued transaction hash for download"
        );
        metrics::gauge!("gossip.queued.transaction.count", self.pending.len() as _);

        DownloadAction::AddedToQueue
    }

    /// Check if transaction is already in the state.
    async fn transaction_in_state(state: &mut ZS, txid: UnminedTxId) -> Result<(), BoxError> {
        // Check if the transaction is already in the state.
        match state
            .ready_and()
            .await?
            .call(zs::Request::Transaction(txid.mined_id()))
            .await
        {
            Ok(zs::Response::Transaction(None)) => Ok(()),
            Ok(zs::Response::Transaction(Some(_))) => Err("already present in state".into()),
            Ok(_) => unreachable!("wrong response"),
            Err(e) => Err(e),
        }?;

        Ok(())
    }
}
