//! Transaction downloader and verifier.
//!
//! The main struct [`Downloads`] allows downloading and verifying transactions.
//! It is used by the mempool to get transactions into it. It is also able to
//! just verify transactions that were directly pushed.
//!
//! The verification itself is done by the [`zebra_consensus`] crate.
//!
//! Verified transactions are returned to the caller in [`Downloads::poll_next`].
//! This is in contrast to the block downloader and verifiers which don't
//! return anything and forward the verified blocks to the state themselves.
//!
//! # Correctness
//!
//! The mempool downloader doesn't send verified transactions to the [`Mempool`] service.
//! So Zebra must spawn a task that regularly polls the downloader for ready transactions.
//! (To ensure that transactions propagate across the entire network in each 75s block interval,
//! the polling interval should be around 5-10 seconds.)
//!
//! Polling the downloader from [`Mempool::poll_ready`] is not sufficient.
//! [`Service::poll_ready`] is only called when there is a service request.
//! But we want to download and gossip transactions,
//! even when there are no other service requests.
use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::{pin_project, pinned_drop};
use thiserror::Error;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::transaction::{self, UnminedTx, UnminedTxId, VerifiedUnminedTx};
use zebra_consensus::transaction as tx;
use zebra_network as zn;
use zebra_state as zs;

use crate::components::sync::{BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT};

use super::MempoolError;

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
/// [#2694](https://github.com/ZcashFoundation/zebra/issues/2694)
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
pub(crate) const MAX_INBOUND_CONCURRENCY: usize = 10;

/// Errors that can occur while downloading and verifying a transaction.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TransactionDownloadVerifyError {
    #[error("transaction is already in state")]
    InState,

    #[error("error in state service")]
    StateError(#[source] BoxError),

    #[error("transaction not validated because the tip is empty")]
    NoTip,

    #[error("error downloading transaction")]
    DownloadFailed(#[source] BoxError),

    #[error("transaction download / verification was cancelled")]
    Cancelled,

    #[error("transaction did not pass consensus validation")]
    Invalid(#[from] zebra_consensus::error::TransactionError),
}

/// A gossiped transaction, which can be the transaction itself or just its ID.
#[derive(Debug, Eq, PartialEq)]
pub enum Gossip {
    Id(UnminedTxId),
    Tx(UnminedTx),
}

impl Gossip {
    /// Return the [`UnminedTxId`] of a gossiped transaction.
    pub fn id(&self) -> UnminedTxId {
        match self {
            Gossip::Id(txid) => *txid,
            Gossip::Tx(tx) => tx.id,
        }
    }
}

impl From<UnminedTxId> for Gossip {
    fn from(txid: UnminedTxId) -> Self {
        Gossip::Id(txid)
    }
}

impl From<UnminedTx> for Gossip {
    fn from(tx: UnminedTx) -> Self {
        Gossip::Tx(tx)
    }
}

/// Represents a [`Stream`] of download and verification tasks.
#[pin_project(PinnedDrop)]
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
    pending: FuturesUnordered<
        JoinHandle<Result<VerifiedUnminedTx, (TransactionDownloadVerifyError, UnminedTxId)>>,
    >,

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
    type Item = Result<VerifiedUnminedTx, (UnminedTxId, TransactionDownloadVerifyError)>;

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
                    this.cancel_handles.remove(&tx.transaction.id);
                    Poll::Ready(Some(Ok(tx)))
                }
                Err((e, hash)) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Err((hash, e))))
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
    /// Initialize a new download stream with the provided services.
    ///
    /// `network` is used to download transactions.
    /// `verifier` is used to verify transactions.
    /// `state` is used to check if transactions are already in the state.
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
    pub fn download_if_needed_and_verify(
        &mut self,
        gossiped_tx: Gossip,
    ) -> Result<(), MempoolError> {
        let txid = gossiped_tx.id();

        if self.cancel_handles.contains_key(&txid) {
            tracing::debug!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "transaction id already queued for inbound download: ignored transaction"
            );
            return Err(MempoolError::AlreadyQueued);
        }

        if self.pending.len() >= MAX_INBOUND_CONCURRENCY {
            tracing::info!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "too many transactions queued for inbound download: ignored transaction"
            );
            return Err(MempoolError::FullQueue);
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
                Ok(zs::Response::Tip(None)) => Err(TransactionDownloadVerifyError::NoTip),
                Ok(zs::Response::Tip(Some((height, _hash)))) => Ok(height),
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(TransactionDownloadVerifyError::StateError(e)),
            }?;
            let height = (height + 1).expect("must have next height");

            let tx = match gossiped_tx {
                Gossip::Id(txid) => {
                    let req = zn::Request::TransactionsById(std::iter::once(txid).collect());

                    let tx = match network
                        .oneshot(req)
                        .await
                        .map_err(|e| TransactionDownloadVerifyError::DownloadFailed(e))?
                    {
                        zn::Response::Transactions(mut txs) => txs.pop().ok_or_else(|| {
                            TransactionDownloadVerifyError::DownloadFailed(
                                "no transactions returned".into(),
                            )
                        })?,
                        _ => unreachable!("wrong response to transaction request"),
                    };

                    metrics::counter!(
                        "mempool.downloaded.transactions.total",
                        1,
                        "version" => format!("{}",tx.transaction.version()),
                    );
                    tx
                }
                Gossip::Tx(tx) => {
                    metrics::counter!(
                        "mempool.pushed.transactions.total",
                        1,
                        "version" => format!("{}",tx.transaction.version()),
                    );
                    tx
                }
            };

            let result = verifier
                .oneshot(tx::Request::Mempool {
                    transaction: tx.clone(),
                    height,
                })
                .map_ok(|rsp| {
                    rsp.into_mempool_transaction()
                        .expect("unexpected non-mempool response to mempool request")
                })
                .await;

            tracing::debug!(?txid, ?result, "verified transaction for the mempool");

            result.map_err(|e| TransactionDownloadVerifyError::Invalid(e.into()))
        }
        .map_ok(|tx| {
            metrics::counter!(
                "mempool.verified.transactions.total",
                1,
                "version" => format!("{}", tx.transaction.transaction.version()),
            );
            tx
        })
        // Tack the hash onto the error so we can remove the cancel handle
        // on failure as well as on success.
        .map_err(move |e| (e, txid))
        .in_current_span();

        let task = tokio::spawn(async move {
            // Prefer the cancel handle if both are ready.
            tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    tracing::trace!("task cancelled prior to completion");
                    metrics::counter!("mempool.cancelled.verify.tasks.total", 1);
                    Err((TransactionDownloadVerifyError::Cancelled, txid))
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
        metrics::gauge!(
            "mempool.currently.queued.transactions",
            self.pending.len() as _
        );
        metrics::counter!("mempool.queued.transactions.total", 1);

        Ok(())
    }

    /// Cancel download/verification tasks of transactions with the
    /// given transaction hash (see [`UnminedTxId::mined_id`]).
    pub fn cancel(&mut self, mined_ids: &HashSet<transaction::Hash>) {
        // TODO: this can be simplified with [`HashMap::drain_filter`] which
        // is currently nightly-only experimental API.
        let removed_txids: Vec<UnminedTxId> = self
            .cancel_handles
            .keys()
            .filter(|txid| mined_ids.contains(&txid.mined_id()))
            .cloned()
            .collect();

        for txid in removed_txids {
            if let Some(handle) = self.cancel_handles.remove(&txid) {
                let _ = handle.send(());
            }
        }
    }

    /// Cancel all running tasks and reset the downloader state.
    // Note: copied from zebrad/src/components/sync/downloads.rs
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
        metrics::gauge!(
            "mempool.currently.queued.transactions",
            self.pending.len() as _
        );
    }

    /// Get the number of currently in-flight download tasks.
    // Note: copied from zebrad/src/components/sync/downloads.rs
    #[allow(dead_code)]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Check if transaction is already in the state.
    async fn transaction_in_state(
        state: &mut ZS,
        txid: UnminedTxId,
    ) -> Result<(), TransactionDownloadVerifyError> {
        // Check if the transaction is already in the state.
        match state
            .ready()
            .await
            .map_err(|e| TransactionDownloadVerifyError::StateError(e))?
            .call(zs::Request::Transaction(txid.mined_id()))
            .await
        {
            Ok(zs::Response::Transaction(None)) => Ok(()),
            Ok(zs::Response::Transaction(Some(_))) => Err(TransactionDownloadVerifyError::InState),
            Ok(_) => unreachable!("wrong response"),
            Err(e) => Err(TransactionDownloadVerifyError::StateError(e)),
        }?;

        Ok(())
    }
}

#[pinned_drop]
impl<ZN, ZV, ZS> PinnedDrop for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    fn drop(self: Pin<&mut Self>) {
        metrics::gauge!("mempool.currently.queued.transactions", 0 as _);
    }
}
