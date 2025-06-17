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
//! The mempool downloader doesn't send verified transactions to the [`Mempool`]
//! service. So Zebra must spawn a task that regularly polls the downloader for
//! ready transactions. (To ensure that transactions propagate across the entire
//! network in each 75s block interval, the polling interval should be around
//! 5-10 seconds.)
//!
//! Polling the downloader from [`Mempool::poll_ready`] is not sufficient.
//! [`Service::poll_ready`] is only called when there is a service request.
//! But we want to download and gossip transactions,
//! even when there are no other service requests.
//!
//! [`Mempool`]: super::Mempool
//! [`Mempool::poll_ready`]: super::Mempool::poll_ready
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
    FutureExt,
};
use pin_project::{pin_project, pinned_drop};
use thiserror::Error;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::Height,
    transaction::{self, UnminedTxId, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::transaction as tx;
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_node_services::mempool::Gossip;
use zebra_state::{self as zs, CloneError};

use crate::components::{
    mempool::crawler::RATE_LIMIT_DELAY,
    sync::{BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT},
};

use super::MempoolError;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Controls how long we wait for a transaction download request to complete.
///
/// This is currently equal to [`BLOCK_DOWNLOAD_TIMEOUT`] for
/// consistency, even though parts of the rationale used for defining the value
/// don't apply here (e.g. we can drop transactions hashes when the queue is full).
pub(crate) const TRANSACTION_DOWNLOAD_TIMEOUT: Duration = BLOCK_DOWNLOAD_TIMEOUT;

/// Controls how long we wait for a transaction verify request to complete.
///
/// This is currently equal to [`BLOCK_VERIFY_TIMEOUT`] for
/// consistency.
///
/// This timeout may lead to denial of service, which will be handled in
/// [#2694](https://github.com/ZcashFoundation/zebra/issues/2694)
pub(crate) const TRANSACTION_VERIFY_TIMEOUT: Duration = BLOCK_VERIFY_TIMEOUT;

/// The maximum number of concurrent inbound download and verify tasks.
///
/// We expect the mempool crawler to download and verify most mempool transactions, so this bound
/// can be small. But it should be at least the default `network.peerset_initial_target_size` config,
/// to avoid disconnecting peers on startup.
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
//
// TODO: replace with the configured value of network.peerset_initial_target_size
pub const MAX_INBOUND_CONCURRENCY: usize = 25;

/// A marker struct for the oneshot channels which cancel a pending download and verify.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct CancelDownloadAndVerify;

/// Errors that can occur while downloading and verifying a transaction.
#[derive(Error, Debug, Clone)]
#[allow(dead_code)]
pub enum TransactionDownloadVerifyError {
    #[error("transaction is already in state")]
    InState,

    #[error("error in state service: {0}")]
    StateError(#[source] CloneError),

    #[error("error downloading transaction: {0}")]
    DownloadFailed(#[source] CloneError),

    #[error("transaction download / verification was cancelled")]
    Cancelled,

    #[error("transaction did not pass consensus validation: {error}")]
    Invalid {
        error: zebra_consensus::error::TransactionError,
        advertiser_addr: Option<PeerSocketAddr>,
    },
}

/// Represents a [`Stream`] of download and verification tasks.
#[pin_project(PinnedDrop)]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
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
        JoinHandle<
            Result<
                Result<
                    (
                        VerifiedUnminedTx,
                        Vec<transparent::OutPoint>,
                        Option<Height>,
                        Option<oneshot::Sender<Result<(), BoxError>>>,
                    ),
                    (TransactionDownloadVerifyError, UnminedTxId),
                >,
                tokio::time::error::Elapsed,
            >,
        >,
    >,

    /// A list of channels that can be used to cancel pending transaction download and
    /// verify tasks. Each channel also has the corresponding request.
    cancel_handles: HashMap<UnminedTxId, (oneshot::Sender<CancelDownloadAndVerify>, Gossip)>,
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
    type Item = Result<
        Result<
            (
                VerifiedUnminedTx,
                Vec<transparent::OutPoint>,
                Option<Height>,
                Option<oneshot::Sender<Result<(), BoxError>>>,
            ),
            (UnminedTxId, TransactionDownloadVerifyError),
        >,
        tokio::time::error::Elapsed,
    >;

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
        let item = if let Some(join_result) = ready!(this.pending.poll_next(cx)) {
            let result = join_result.expect("transaction download and verify tasks must not panic");
            let result = match result {
                Ok(Ok((tx, spent_mempool_outpoints, tip_height, rsp_tx))) => {
                    this.cancel_handles.remove(&tx.transaction.id);
                    Ok(Ok((tx, spent_mempool_outpoints, tip_height, rsp_tx)))
                }
                Ok(Err((e, hash))) => {
                    this.cancel_handles.remove(&hash);
                    Ok(Err((hash, e)))
                }
                Err(elapsed) => Err(elapsed),
            };

            Some(result)
        } else {
            None
        };

        Poll::Ready(item)
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
    #[allow(clippy::unwrap_in_result)]
    pub fn download_if_needed_and_verify(
        &mut self,
        gossiped_tx: Gossip,
        mut rsp_tx: Option<oneshot::Sender<Result<(), BoxError>>>,
    ) -> Result<(), MempoolError> {
        let txid = gossiped_tx.id();

        if self.cancel_handles.contains_key(&txid) {
            debug!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "transaction id already queued for inbound download: ignored transaction"
            );
            metrics::gauge!("mempool.currently.queued.transactions",)
                .set(self.pending.len() as f64);

            return Err(MempoolError::AlreadyQueued);
        }

        if self.pending.len() >= MAX_INBOUND_CONCURRENCY {
            debug!(
                ?txid,
                queue_len = self.pending.len(),
                ?MAX_INBOUND_CONCURRENCY,
                "too many transactions queued for inbound download: ignored transaction"
            );
            metrics::gauge!("mempool.currently.queued.transactions",)
                .set(self.pending.len() as f64);

            return Err(MempoolError::FullQueue);
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<CancelDownloadAndVerify>();

        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let mut state = self.state.clone();

        let gossiped_tx_req = gossiped_tx.clone();

        let fut = async move {
            // Don't download/verify if the transaction is already in the best chain.
            Self::transaction_in_best_chain(&mut state, txid).await?;

            trace!(?txid, "transaction is not in best chain");

            let (tip_height, next_height) = match state.oneshot(zs::Request::Tip).await {
                Ok(zs::Response::Tip(None)) => Ok((None, Height(0))),
                Ok(zs::Response::Tip(Some((height, _hash)))) => {
                    let next_height =
                        (height + 1).expect("valid heights are far below the maximum");
                    Ok((Some(height), next_height))
                }
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(TransactionDownloadVerifyError::StateError(e.into())),
            }?;

            trace!(?txid, ?next_height, "got next height");

            let (tx, advertiser_addr) = match gossiped_tx {
                Gossip::Id(txid) => {
                    let req = zn::Request::TransactionsById(std::iter::once(txid).collect());

                    let tx = match network
                        .oneshot(req)
                        .await
                        .map_err(CloneError::from)
                        .map_err(TransactionDownloadVerifyError::DownloadFailed)?
                    {
                        zn::Response::Transactions(mut txs) => txs.pop().ok_or_else(|| {
                            TransactionDownloadVerifyError::DownloadFailed(
                                BoxError::from("no transactions returned").into(),
                            )
                        })?,
                        _ => unreachable!("wrong response to transaction request"),
                    };

                    let (tx, advertiser_addr) = tx.available().expect(
                        "unexpected missing tx status: single tx failures should be errors",
                    );

                    metrics::counter!(
                        "mempool.downloaded.transactions.total",
                        "version" => format!("{}",tx.transaction.version()),
                    ).increment(1);
                    (tx, advertiser_addr)
                }
                Gossip::Tx(tx) => {
                    metrics::counter!(
                        "mempool.pushed.transactions.total",
                        "version" => format!("{}",tx.transaction.version()),
                    ).increment(1);
                    (tx, None)
                }
            };

            trace!(?txid, "got tx");

            let result = verifier
                .oneshot(tx::Request::Mempool {
                    transaction: tx.clone(),
                    height: next_height,
                })
                .map_ok(|rsp| {
                    let tx::Response::Mempool { transaction, spent_mempool_outpoints, tx_dependencies } = rsp else {
                        panic!("unexpected non-mempool response to mempool request")
                    };

                    (transaction, spent_mempool_outpoints, tip_height, tx_dependencies)
                })
                .await;

            // Hide the transaction data to avoid filling the logs
            trace!(?txid, result = ?result.as_ref().map(|_tx| ()), "verified transaction for the mempool");

            let (transaction, spent_mempool_outpoints, tip_height, tx_dependencies) = result.map_err(|e| TransactionDownloadVerifyError::Invalid { error: e.into(), advertiser_addr } )?;

             zebra_state::verify_tx_in_template(state, block_verifier_router, tx, tx_dependencies).await?;

            
            Ok((transaction, spent_mempool_outpoints, tip_height, tx_dependencies))
        }
        .map_ok(|(tx, spent_mempool_outpoints, tip_height)| {
            metrics::counter!(
                "mempool.verified.transactions.total",
                "version" => format!("{}", tx.transaction.transaction.version()),
            ).increment(1);
            (tx, spent_mempool_outpoints, tip_height)
        })
        // Tack the hash onto the error so we can remove the cancel handle
        // on failure as well as on success.
        .map_err(move |e| (e, txid))
        .inspect(move |result| {
            // Hide the transaction data to avoid filling the logs
            let result = result.as_ref().map(|_tx| txid);
            debug!("mempool transaction result: {result:?}");
        })
        .in_current_span();

        let task = tokio::spawn(async move {
            let fut = tokio::time::timeout(RATE_LIMIT_DELAY, fut);

            // Prefer the cancel handle if both are ready.
            let result = tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    trace!("task cancelled prior to completion");
                    metrics::counter!("mempool.cancelled.verify.tasks.total").increment(1);
                    if let Some(rsp_tx) = rsp_tx.take() {
                        let _ = rsp_tx.send(Err("verification cancelled".into()));
                    }

                    Ok(Err((TransactionDownloadVerifyError::Cancelled, txid)))
                }
                verification = fut => {
                    verification
                        .inspect_err(|_elapsed| {
                            if let Some(rsp_tx) = rsp_tx.take() {
                                let _ = rsp_tx.send(Err("timeout waiting for verification result".into()));
                            }
                        })
                        .map(|inner_result| {
                            match inner_result {
                                Ok((transaction, spent_mempool_outpoints, tip_height)) => Ok((transaction, spent_mempool_outpoints, tip_height, rsp_tx)),
                                Err((tx_verifier_error, tx_id)) => {
                                    if let Some(rsp_tx) = rsp_tx.take() {
                                        let error_msg = format!(
                                            "failed to validate tx: {tx_id}, error: {tx_verifier_error}"
                                        );
                                        let _ = rsp_tx.send(Err(error_msg.into()));
                                    };

                                    Err((tx_verifier_error, tx_id))
                                }
                            }
                        })
                },
            };

            result
        });

        self.pending.push(task);
        assert!(
            self.cancel_handles
                .insert(txid, (cancel_tx, gossiped_tx_req))
                .is_none(),
            "transactions are only queued once"
        );

        debug!(
            ?txid,
            queue_len = self.pending.len(),
            ?MAX_INBOUND_CONCURRENCY,
            "queued transaction hash for download"
        );
        metrics::gauge!("mempool.currently.queued.transactions",).set(self.pending.len() as f64);
        metrics::counter!("mempool.queued.transactions.total").increment(1);

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
                let _ = handle.0.send(CancelDownloadAndVerify);
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
            let _ = cancel.0.send(CancelDownloadAndVerify);
        }
        assert!(self.pending.is_empty());
        assert!(self.cancel_handles.is_empty());
        metrics::gauge!("mempool.currently.queued.transactions",).set(self.pending.len() as f64);
    }

    /// Get the number of currently in-flight download tasks.
    #[allow(dead_code)]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Get a list of the currently pending transaction requests.
    pub fn transaction_requests(&self) -> impl Iterator<Item = &Gossip> {
        self.cancel_handles.iter().map(|(_tx_id, (_handle, tx))| tx)
    }

    /// Check if transaction is already in the best chain.
    async fn transaction_in_best_chain(
        state: &mut ZS,
        txid: UnminedTxId,
    ) -> Result<(), TransactionDownloadVerifyError> {
        match state
            .ready()
            .await
            .map_err(CloneError::from)
            .map_err(TransactionDownloadVerifyError::StateError)?
            .call(zs::Request::Transaction(txid.mined_id()))
            .await
        {
            Ok(zs::Response::Transaction(None)) => Ok(()),
            Ok(zs::Response::Transaction(Some(_))) => Err(TransactionDownloadVerifyError::InState),
            Ok(_) => unreachable!("wrong response"),
            Err(e) => Err(TransactionDownloadVerifyError::StateError(e.into())),
        }?;

        Ok(())
    }
}

#[pinned_drop]
impl<ZN, ZV, ZS> PinnedDrop for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<tx::Request, Response = tx::Response, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    fn drop(mut self: Pin<&mut Self>) {
        self.cancel_all();

        metrics::gauge!("mempool.currently.queued.transactions").set(0 as f64);
    }
}
