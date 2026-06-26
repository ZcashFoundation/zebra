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
    net::SocketAddr,
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
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::Height,
    transaction::{self, UnminedTxId, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::transaction as tx;
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_node_services::mempool::{Gossip, MempoolBatch, MempoolChange, QueuedStage};
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
pub const MAX_INBOUND_CONCURRENCY: usize = 500;

/// The maximum number of concurrent inbound download tasks attributable to a
/// single advertising peer.
///
/// Caps how many slots of [`MAX_INBOUND_CONCURRENCY`] one peer's `Inv`
/// advertisements can occupy, so a single peer cannot saturate the global
/// queue with fake txids and deny gossip-path mempool admission for honest
/// peers. See `GHSA-4fc2-h7jh-287c`. Crawler-driven and locally-pushed
/// transactions have no source peer and are not counted against the cap.
pub const MAX_INBOUND_CONCURRENCY_PER_PEER: usize = 5;

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
                    Box<(TransactionDownloadVerifyError, UnminedTxId)>,
                >,
                (UnminedTxId, tokio::time::error::Elapsed),
            >,
        >,
    >,

    /// A list of channels that can be used to cancel pending transaction
    /// download and verify tasks. Each entry also stores the corresponding
    /// gossip request and the announcing peer (when known), so completion can
    /// release the per-peer slot by `UnminedTxId` lookup.
    ///
    /// The final tuple element tracks the transaction's current [`QueuedStage`].
    /// It is owned and mutated **only** by the single mempool task (via
    /// [`Downloads::drain_awaiting_verification`]), never by the spawned download
    /// tasks, so [`Downloads::queued_stages`] reads a consistent point-in-time
    /// value ordered with the `Queued` events the same task emits (design
    /// §3a-1, §6, §9.8).
    cancel_handles: HashMap<
        UnminedTxId,
        (
            oneshot::Sender<CancelDownloadAndVerify>,
            Gossip,
            Option<SocketAddr>,
            QueuedStage,
        ),
    >,

    /// The number of currently in-flight download tasks per advertising peer.
    ///
    /// Invariant: a peer is present here iff some entry in [`Self::cancel_handles`]
    /// has it as the third tuple element. Enforces
    /// [`MAX_INBOUND_CONCURRENCY_PER_PEER`]. See `GHSA-4fc2-h7jh-287c`.
    pending_per_peer: HashMap<SocketAddr, usize>,

    /// Broadcasts transaction lifecycle changes observed in the download/verify
    /// pipeline (`Queued{AwaitingDownload}`, `Queued{AwaitingVerification}`, and
    /// `Removed{FailedDownload}`) to mempool change subscribers.
    ///
    /// These are mid-cycle observations, so they are broadcast as checksum-less
    /// [`MempoolBatch`]es (design §9.8); the settled per-cycle checksum is
    /// emitted by `Mempool::poll_ready`.
    change_sender: broadcast::Sender<MempoolBatch>,

    /// Signals from spawned download tasks that a transaction has finished
    /// downloading and is awaiting verification.
    ///
    /// The download task only *reports* the transition over this channel; the
    /// authoritative `AwaitingVerification` stage update and its
    /// `Queued{AwaitingVerification}` event are both applied by the single
    /// mempool task in [`Downloads::drain_awaiting_verification`], keeping the
    /// stage reflected in the digest consistent with the events ordered before
    /// the settled batch (design §3a-1, §9.8). At most one message per in-flight
    /// task, so the channel depth is bounded by [`MAX_INBOUND_CONCURRENCY`].
    awaiting_verification_tx: mpsc::UnboundedSender<UnminedTxId>,

    /// The receiver drained by [`Downloads::drain_awaiting_verification`].
    awaiting_verification_rx: mpsc::UnboundedReceiver<UnminedTxId>,
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
            Box<(UnminedTxId, TransactionDownloadVerifyError)>,
        >,
        (UnminedTxId, tokio::time::error::Elapsed),
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
            let (result, completed_txid) = match result {
                Ok(Ok((tx, spent_mempool_outpoints, tip_height, rsp_tx))) => {
                    let hash = tx.transaction.id;
                    (
                        Ok(Ok((tx, spent_mempool_outpoints, tip_height, rsp_tx))),
                        Some(hash),
                    )
                }
                Ok(Err(boxed_err)) => {
                    let (e, hash) = *boxed_err;
                    (Ok(Err(Box::new((hash, e)))), Some(hash))
                }
                Err((txid, elapsed)) => {
                    // Remove the cancel handle so the spawned task's queued `Gossip`
                    // doesn't stay resident in `cancel_handles` after a verification
                    // timeout. Without this, a peer that gets each transaction to
                    // hit `RATE_LIMIT_DELAY` can leak ~2 MB per tx until OOM.
                    this.cancel_handles.remove(&txid);
                    (Err((txid, elapsed)), None)
                }
            };

            if let Some(hash) = completed_txid {
                if let Some((_, _gossip, Some(source), _stage)) = this.cancel_handles.remove(&hash) {
                    Self::release_peer_slot(this.pending_per_peer, source);
                }
            }

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
    /// `change_sender` broadcasts transaction lifecycle changes to subscribers.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(
        network: ZN,
        verifier: ZV,
        state: ZS,
        change_sender: broadcast::Sender<MempoolBatch>,
    ) -> Self {
        let (awaiting_verification_tx, awaiting_verification_rx) = mpsc::unbounded_channel();

        Self {
            network,
            verifier,
            state,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
            pending_per_peer: HashMap::new(),
            change_sender,
            awaiting_verification_tx,
            awaiting_verification_rx,
        }
    }

    /// Queue a transaction for download (if needed) and verification.
    ///
    /// Returns the action taken in response to the queue request.
    ///
    /// When `source` is `Some`, the per-peer cap
    /// [`MAX_INBOUND_CONCURRENCY_PER_PEER`] is enforced; crawler-driven and
    /// locally-pushed transactions pass `None` and are not capped per peer.
    #[instrument(skip(self, gossiped_tx), fields(txid = %gossiped_tx.id()))]
    #[allow(clippy::unwrap_in_result)]
    pub fn download_if_needed_and_verify(
        &mut self,
        gossiped_tx: Gossip,
        source: Option<SocketAddr>,
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

        // Per-peer cap: a single advertising peer cannot saturate the queue
        // with attacker-supplied fake txids. See `GHSA-4fc2-h7jh-287c`.
        if let Some(source) = source {
            let count = self.pending_per_peer.get(&source).copied().unwrap_or(0);
            if count >= MAX_INBOUND_CONCURRENCY_PER_PEER {
                debug!(
                    ?txid,
                    peer_queue_len = count,
                    ?MAX_INBOUND_CONCURRENCY_PER_PEER,
                    "too many transactions queued for this peer: ignored transaction"
                );
                metrics::counter!("mempool.full_queue.per_peer.total").increment(1);
                return Err(MempoolError::FullQueue);
            }
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<CancelDownloadAndVerify>();

        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let mut state = self.state.clone();
        let change_sender = self.change_sender.clone();
        let awaiting_verification_tx = self.awaiting_verification_tx.clone();

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
                        .map_err(TransactionDownloadVerifyError::DownloadFailed)
                        .inspect_err(|_| {
                            let _ = change_sender.send(MempoolBatch::event(
                                MempoolChange::removed_failed_download(
                                    std::iter::once(txid).collect(),
                                ),
                            ));
                        })?
                    {
                        zn::Response::Transactions(mut txs) => txs
                            .pop()
                            .ok_or_else(|| {
                                TransactionDownloadVerifyError::DownloadFailed(
                                    BoxError::from("no transactions returned").into(),
                                )
                            })
                            .inspect_err(|_| {
                                let _ = change_sender.send(MempoolBatch::event(
                                    MempoolChange::removed_failed_download(
                                        std::iter::once(txid).collect(),
                                    ),
                                ));
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

            // Both gossip paths converge here with the tx content in hand. Report
            // the download→verification transition to the single mempool task,
            // which applies the authoritative stage update and emits the matching
            // `Queued{AwaitingVerification}` event in order. Doing both on one
            // task keeps the stage reflected in the per-cycle digest consistent
            // with the events ordered before the settled batch (design §9.8).
            let _ = awaiting_verification_tx.send(txid);

            let result = verifier
                .oneshot(tx::Request::Mempool {
                    transaction: tx.clone(),
                    height: next_height,
                })
                .map_ok(|rsp| {
                    let tx::Response::Mempool { transaction, spent_mempool_outpoints } = rsp else {
                        panic!("unexpected non-mempool response to mempool request")
                    };

                    (transaction, spent_mempool_outpoints, tip_height)
                })
                .await;

            // Hide the transaction data to avoid filling the logs
            trace!(?txid, result = ?result.as_ref().map(|_tx| ()), "verified transaction for the mempool");

            result.map_err(|e| TransactionDownloadVerifyError::Invalid { error: e.into(), advertiser_addr } )
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
        .map_err(move |e| Box::new((e, txid)))
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

                    Ok(Err(Box::new((TransactionDownloadVerifyError::Cancelled, txid))))
                }
                verification = fut => {
                    verification
                        .inspect_err(|_elapsed| {
                            if let Some(rsp_tx) = rsp_tx.take() {
                                let _ = rsp_tx.send(Err("timeout waiting for verification result".into()));
                            }
                        })
                        .map_err(|elapsed| (txid, elapsed))
                        .map(|inner_result| {
                            match inner_result {
                                Ok((transaction, spent_mempool_outpoints, tip_height)) => Ok((transaction, spent_mempool_outpoints, tip_height, rsp_tx)),
                                Err(boxed_err) => {
                                    let (tx_verifier_error, tx_id) = *boxed_err;
                                    if let Some(rsp_tx) = rsp_tx.take() {
                                        let error_msg = format!(
                                            "failed to validate tx: {tx_id}, error: {tx_verifier_error}"
                                        );
                                        let _ = rsp_tx.send(Err(error_msg.into()));
                                    };

                                    Err(Box::new((tx_verifier_error, tx_id)))
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
                .insert(
                    txid,
                    (
                        cancel_tx,
                        gossiped_tx_req,
                        source,
                        QueuedStage::AwaitingDownload,
                    ),
                )
                .is_none(),
            "transactions are only queued once"
        );
        if let Some(source) = source {
            // The per-peer cap check above ensures this can't exceed
            // `MAX_INBOUND_CONCURRENCY_PER_PEER`.
            *self.pending_per_peer.entry(source).or_insert(0) += 1;
        }

        debug!(
            ?txid,
            queue_len = self.pending.len(),
            ?MAX_INBOUND_CONCURRENCY,
            "queued transaction hash for download"
        );
        metrics::gauge!("mempool.currently.queued.transactions",).set(self.pending.len() as f64);
        metrics::counter!("mempool.queued.transactions.total").increment(1);

        // The transaction is now newly queued (not a duplicate or queue-full
        // rejection, which returned early above), so emit the entry event.
        let _ = self.change_sender.send(MempoolBatch::event(
            MempoolChange::queued_awaiting_download(std::iter::once(txid).collect()),
        ));

        Ok(())
    }

    /// Cancel download/verification tasks of transactions with the
    /// given transaction hash (see [`UnminedTxId::mined_id`]).
    ///
    /// Returns the [`UnminedTxId`]s that were still queued and are now removed.
    /// They leave the queued set synchronously (so the next digest excludes
    /// them), but the spawned task only emits its `Removed` event on a later
    /// cycle, so the caller must emit a matching `Removed` event in the *same*
    /// cycle to keep the digest and event stream consistent (design §3a-1).
    pub fn cancel(&mut self, mined_ids: &HashSet<transaction::Hash>) -> Vec<UnminedTxId> {
        // TODO: this can be simplified with [`HashMap::drain_filter`] which
        // is currently nightly-only experimental API.
        let removed_txids: Vec<UnminedTxId> = self
            .cancel_handles
            .keys()
            .filter(|txid| mined_ids.contains(&txid.mined_id()))
            .cloned()
            .collect();

        for txid in &removed_txids {
            if let Some((cancel_tx, _gossip, source, _stage)) = self.cancel_handles.remove(txid) {
                let _ = cancel_tx.send(CancelDownloadAndVerify);
                if let Some(source) = source {
                    Self::release_peer_slot(&mut self.pending_per_peer, source);
                }
            }
        }

        removed_txids
    }

    /// Cancel all running tasks and reset the downloader state.
    // Note: copied from zebrad/src/components/sync/downloads.rs
    pub fn cancel_all(&mut self) {
        // Replace the pending task list with an empty one and drop it.
        let _ = std::mem::take(&mut self.pending);
        // Signal cancellation to all running tasks.
        // Since we already dropped the JoinHandles above, they should
        // fail silently.
        for (_hash, (cancel_tx, _gossip, _source, _stage)) in self.cancel_handles.drain() {
            let _ = cancel_tx.send(CancelDownloadAndVerify);
        }
        self.pending_per_peer.clear();
        assert!(self.pending.is_empty());
        assert!(self.cancel_handles.is_empty());
        metrics::gauge!("mempool.currently.queued.transactions",).set(self.pending.len() as f64);
    }

    /// Decrement the per-peer pending count for `source`, removing the entry
    /// when it reaches zero.
    fn release_peer_slot(pending_per_peer: &mut HashMap<SocketAddr, usize>, source: SocketAddr) {
        if let Some(count) = pending_per_peer.get_mut(&source) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending_per_peer.remove(&source);
            }
        }
    }

    /// Get the number of currently in-flight download tasks.
    #[allow(dead_code)]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Get a list of the currently pending transaction requests.
    pub fn transaction_requests(&self) -> impl Iterator<Item = &Gossip> {
        self.cancel_handles
            .iter()
            .map(|(_tx_id, (_handle, tx, _source, _stage))| tx)
    }

    /// Returns the currently queued set, mapping each transaction's
    /// [`UnminedTxId`] to its [`QueuedStage`] (design §3a-1, §6).
    ///
    /// The stage is owned by the single mempool task and only advanced by
    /// [`Downloads::drain_awaiting_verification`], so it matches the stage a
    /// follower derives from the `Queued` lifecycle event stream.
    pub fn queued_stages(&self) -> HashMap<UnminedTxId, QueuedStage> {
        self.cancel_handles
            .iter()
            .map(|(txid, (_handle, _tx, _source, stage))| (*txid, *stage))
            .collect()
    }

    /// Applies the `AwaitingDownload → AwaitingVerification` stage transitions
    /// reported by the spawned download tasks, and returns the [`UnminedTxId`]s
    /// that transitioned and are still queued.
    ///
    /// The caller (the single mempool task) emits a `Queued{AwaitingVerification}`
    /// event for the returned ids in the same change cycle, so the stage update
    /// reflected in that cycle's digest is ordered consistently with the event
    /// the follower applies (design §3a-1, §9.8). Signals for transactions that
    /// have already left the queue (mined, cancelled, completed) are dropped.
    pub fn drain_awaiting_verification(&mut self) -> HashSet<UnminedTxId> {
        let mut transitioned = HashSet::new();

        while let Ok(txid) = self.awaiting_verification_rx.try_recv() {
            if let Some((_cancel_tx, _gossip, _source, stage)) = self.cancel_handles.get_mut(&txid) {
                *stage = QueuedStage::AwaitingVerification;
                transitioned.insert(txid);
            }
        }

        transitioned
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
