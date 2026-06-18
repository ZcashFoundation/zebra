//! A download stream that handles gossiped blocks from peers.

use std::{
    collections::HashMap,
    net::IpAddr,
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
use tokio::{
    sync::{oneshot, Mutex},
    task::JoinHandle,
};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::components::sync::MIN_CONCURRENCY_LIMIT;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Source key used for inbound block download ordering.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AdvertiserSource {
    /// Legacy TCP peers are ordered per IP address, preserving existing source attribution.
    LegacyIp(IpAddr),

    /// Zakura peers are ordered per authenticated peer id.
    Zakura(zn::zakura::ZakuraPeerId),
}

impl From<zn::PeerSource> for AdvertiserSource {
    fn from(source: zn::PeerSource) -> Self {
        match source {
            zn::PeerSource::LegacySocket(addr) => Self::LegacyIp(addr.ip()),
            zn::PeerSource::Zakura(peer_id) => Self::Zakura(peer_id),
        }
    }
}

impl AdvertiserSource {
    fn max_in_flight(&self, global_limit: usize) -> usize {
        match self {
            Self::LegacyIp(_) => 1,
            // Authenticated Zakura peers get a per-peer slice of the global queue
            // rather than the whole thing, so one peer cannot monopolize inbound
            // block gossip admission. Never exceed the global limit on small queues.
            Self::Zakura(_) => MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER.min(global_limit),
        }
    }
}

#[derive(Clone, Debug)]
struct DownloadTask {
    hash: block::Hash,
    download_source: Option<zn::PeerSource>,
    advertiser: Option<AdvertiserSource>,
}

/// The maximum number of concurrent inbound download and verify tasks.
/// Also used as the maximum lookahead limit, before block verification.
///
/// We expect the syncer to download and verify checkpoints, so this bound
/// can be small.
///
/// ## Security
///
/// The maximum block size is 2 million bytes. A deserialized malicious
/// block with ~225_000 transparent outputs can take up 9MB of RAM.
/// The total queue bound is `MAX_INBOUND_CONCURRENCY * 9 MB`. Admission is
/// bounded globally by [`Downloads::full_verify_concurrency_limit`], deduped by
/// block hash, and bounded per advertiser source. Legacy TCP sources keep the
/// historical one-in-flight per-IP bound; authenticated Zakura sources are bounded
/// per peer id by [`MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER`], so a single Zakura
/// peer cannot fill the global queue with distinct gossiped hashes and deny
/// admission to honest peers (the block-gossip analogue of the mempool fix for
/// `GHSA-4fc2-h7jh-287c`). Admitted same-source downloads fetch block bodies promptly, then
/// wait on a fair source-local gate before verification. This preserves
/// source-local commit order without needing a later inbound request to drain a
/// passive queue.
/// (See #1880 for more details.)
///
/// Malicious blocks will eventually timeout or fail contextual validation.
/// Once validation fails, the block is dropped, and its memory is deallocated.
pub const MAX_INBOUND_CONCURRENCY: usize = 200;

/// The maximum number of concurrent inbound block download tasks attributable to
/// a single authenticated Zakura advertiser.
///
/// Caps how many slots of [`MAX_INBOUND_CONCURRENCY`] (or the configured
/// `full_verify_concurrency_limit`) one Zakura peer's gossiped block hashes can
/// occupy, so a single peer cannot saturate the global queue with distinct hashes
/// and deny gossip-path block admission to honest peers. This mirrors the mempool
/// per-peer cap added for `GHSA-4fc2-h7jh-287c`. Legacy TCP sources keep their
/// historical one-in-flight per-IP bound; crawler/sync-driven downloads have no
/// advertiser source and are not counted against this cap.
pub const MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER: usize = 5;

/// The action taken in response to a peer's gossiped block hash.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DownloadAction {
    /// The block hash was successfully queued for download and verification.
    AddedToQueue,

    /// The block hash is already queued, so this request was ignored.
    ///
    /// Another peer has already gossiped the same hash to us.
    AlreadyQueued,

    /// The queue is at capacity, so this request was ignored.
    ///
    /// The sync service should discover this block later, when we are closer
    /// to the tip. The queue's capacity is [`Downloads::full_verify_concurrency_limit`].
    FullQueue,
}

/// Manages download and verification of blocks gossiped to this peer.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    // Configuration
    //
    /// The configured full verification concurrency limit, after applying the minimum limit.
    full_verify_concurrency_limit: usize,

    // Services
    //
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded blocks.
    verifier: ZV,

    /// A service that manages cached blockchain state.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: zs::LatestChainTip,

    // Internal downloads state
    //
    /// Active block download and verify tasks.
    #[pin]
    pending: FuturesUnordered<
        JoinHandle<Result<block::Hash, (BoxError, block::Hash, Option<PeerSocketAddr>)>>,
    >,

    /// Cancellation handles for active tasks in [`Self::pending`], keyed by block
    /// hash. The optional source is recorded so completion can clear
    /// [`Self::source_counts`] and [`Self::source_locks`].
    cancel_handles: HashMap<block::Hash, (oneshot::Sender<()>, Option<AdvertiserSource>)>,

    /// Fair source-local verification gates for admitted downloads.
    ///
    /// Tasks are spawned immediately, but same-source tasks must acquire this gate
    /// before committing a downloaded block to the verifier.
    source_locks: HashMap<AdvertiserSource, Arc<Mutex<()>>>,

    /// Number of admitted tasks, active or waiting, per source.
    source_counts: HashMap<AdvertiserSource, usize>,
}

impl<ZN, ZV, ZS> Stream for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    type Item = Result<block::Hash, (BoxError, Option<PeerSocketAddr>)>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // If no download and verify tasks have exited since the last poll, this
        // task is scheduled for wakeup when the next task becomes ready.
        //
        // TODO: this would be cleaner with poll_map (#2693)
        if let Some(join_result) = ready!(this.pending.as_mut().poll_next(cx)) {
            let (result, hash) =
                match join_result.expect("block download and verify tasks must not panic") {
                    Ok(hash) => (Ok(hash), hash),
                    Err((e, hash, advertiser_addr)) => (Err((e, advertiser_addr)), hash),
                };
            if let Some((_, Some(source))) = this.cancel_handles.remove(&hash) {
                let source_count = this
                    .source_counts
                    .get_mut(&source)
                    .expect("source count is inserted when a download task is admitted");
                *source_count = source_count
                    .checked_sub(1)
                    .expect("source count is positive while a download task is admitted");

                if *source_count == 0 {
                    this.source_counts.remove(&source);
                    this.source_locks.remove(&source);
                }
            }
            Poll::Ready(Some(result))
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
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    /// Initialize a new download stream with the provided `network`, `verifier`, and `state` services.
    /// The `latest_chain_tip` must be linked to the provided `state` service.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(
        full_verify_concurrency_limit: usize,
        network: ZN,
        verifier: ZV,
        state: ZS,
        latest_chain_tip: zs::LatestChainTip,
    ) -> Self {
        // The syncer already warns about the minimum.
        let full_verify_concurrency_limit =
            full_verify_concurrency_limit.clamp(MIN_CONCURRENCY_LIMIT, MAX_INBOUND_CONCURRENCY);

        Self {
            full_verify_concurrency_limit,
            network,
            verifier,
            state,
            latest_chain_tip,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
            source_locks: HashMap::new(),
            source_counts: HashMap::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// When `download_source` is `Some`, the block request is directed to that
    /// source. Admission is still controlled by the global queue bound and
    /// per-hash dedupe, so valid consecutive blocks from one source are not
    /// dropped solely because an earlier block is still being verified.
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(
        &mut self,
        hash: block::Hash,
        download_source: Option<zn::PeerSource>,
    ) -> DownloadAction {
        if self.cancel_handles.contains_key(&hash) {
            debug!(
                ?hash,
                queue_len = self.queue_len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "block hash already queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.queue_len() as f64);
            metrics::counter!("gossip.already.queued.dropped.block.hash.count").increment(1);

            return DownloadAction::AlreadyQueued;
        }

        if self.queue_len() >= self.full_verify_concurrency_limit {
            debug!(
                ?hash,
                queue_len = self.queue_len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "too many blocks queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.queue_len() as f64);
            metrics::counter!("gossip.full.queue.dropped.block.hash.count").increment(1);

            return DownloadAction::FullQueue;
        }

        let advertiser = download_source.clone().map(AdvertiserSource::from);
        if let Some(source) = &advertiser {
            let source_count = self.source_counts.get(source).copied().unwrap_or_default();
            let source_limit = source.max_in_flight(self.full_verify_concurrency_limit);
            if source_count >= source_limit {
                debug!(
                    ?hash,
                    ?source,
                    source_count,
                    source_limit,
                    queue_len = self.queue_len(),
                    concurrency_limit = self.full_verify_concurrency_limit,
                    "too many blocks queued for inbound download from one source: ignored block",
                );

                metrics::gauge!("gossip.queued.block.count").set(self.queue_len() as f64);
                metrics::counter!("gossip.source.queue.dropped.block.hash.count").increment(1);

                return DownloadAction::FullQueue;
            }
        }

        let download = DownloadTask {
            hash,
            advertiser,
            download_source,
        };

        self.spawn_download_task(download);

        debug!(
            ?hash,
            queue_len = self.queue_len(),
            concurrency_limit = self.full_verify_concurrency_limit,
            "queued hash for download",
        );
        metrics::gauge!("gossip.queued.block.count").set(self.queue_len() as f64);

        DownloadAction::AddedToQueue
    }

    fn queue_len(&self) -> usize {
        self.pending.len()
    }

    fn spawn_download_task(&mut self, download: DownloadTask) {
        let DownloadTask {
            hash,
            download_source,
            advertiser,
        } = download;

        let source_lock = advertiser.as_ref().map(|source| {
            *self.source_counts.entry(source.clone()).or_default() += 1;
            self.source_locks
                .entry(source.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        });

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let state = self.state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let full_verify_concurrency_limit = self.full_verify_concurrency_limit;

        let fut = async move {
            // Check if the full block body is already in the state. `KnownBlock`
            // can be true for header-only Zakura sync state, but inbound gossip
            // still needs to fetch and verify the block body in that case.
            match state.oneshot(zs::Request::AnyChainBlock(hash.into())).await {
                Ok(zs::Response::Block(None)) => Ok(()),
                Ok(zs::Response::Block(Some(_))) => Err("already present".into()),
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(e),
            }
            .map_err(|e| (e, None))?;

            let request_hashes = std::iter::once(hash).collect();
            let request = match download_source {
                Some(source) => zn::Request::BlocksByHashFrom {
                    hashes: request_hashes,
                    source,
                },
                None => zn::Request::BlocksByHash(request_hashes),
            };

            let (block, advertiser_addr) = if let zn::Response::Blocks(blocks) =
                network.oneshot(request).await.map_err(|e| (e, None))?
            {
                // A peer must answer a single-hash block request with exactly one
                // block entry. A malformed or empty response is a peer fault (e.g.
                // a Zakura peer that tore its connection down mid-response), not a
                // local invariant, so fail the download gracefully rather than
                // panicking the task.
                if blocks.len() != 1 {
                    return Err((
                        format!(
                            "wrong number of blocks in response to a single hash: got {}",
                            blocks.len()
                        )
                        .into(),
                        None,
                    ));
                }

                let Some(available) = blocks.first().expect("just checked length").available()
                else {
                    return Err((
                        "unexpected missing block status: single block failures should be errors"
                            .into(),
                        None,
                    ));
                };
                available
            } else {
                unreachable!("wrong response to block request");
            };

            // Bind the delivered block to the hash we requested. A peer that
            // substitutes a different (even valid) block must not be able to
            // corrupt our hash/source accounting: the verifier returns the
            // committed block's hash, and queue cleanup keys on it, so a
            // mismatch would leave the originally requested entry stale.
            if block.hash() != hash {
                return Err((
                    format!(
                        "peer returned block {} in response to a request for {hash}",
                        block.hash()
                    )
                    .into(),
                    advertiser_addr,
                ));
            }
            metrics::counter!("gossip.downloaded.block.count").increment(1);

            // # Security & Performance
            //
            // Reject blocks that are too far ahead of our tip,
            // and blocks that are behind the finalized tip.
            //
            // Avoids denial of service attacks. Also reduces wasted work on high blocks
            // that will timeout before being verified, and low blocks that can never be finalized.
            let tip_height = latest_chain_tip.best_tip_height();

            let max_lookahead_height = if let Some(tip_height) = tip_height {
                let lookahead = HeightDiff::try_from(full_verify_concurrency_limit)
                    .expect("fits in HeightDiff");
                (tip_height + lookahead).expect("tip is much lower than Height::MAX")
            } else {
                let genesis_lookahead =
                    u32::try_from(full_verify_concurrency_limit - 1).expect("fits in u32");
                block::Height(genesis_lookahead)
            };

            // Get the finalized tip height, assuming we're using the non-finalized state.
            //
            // It doesn't matter if we're a few blocks off here, because blocks this low
            // are part of a fork with much less work. So they would be rejected anyway.
            //
            // And if we're still checkpointing, the checkpointer will reject blocks behind
            // the finalized tip anyway.
            //
            // TODO: get the actual finalized tip height
            let min_accepted_height = tip_height
                .map(|tip_height| {
                    block::Height(tip_height.0.saturating_sub(zs::MAX_BLOCK_REORG_HEIGHT))
                })
                .unwrap_or(block::Height(0));

            let block_height = block
                .coinbase_height()
                .ok_or_else(|| {
                    debug!(
                        ?hash,
                        "gossiped block with no height: dropped downloaded block"
                    );
                    metrics::counter!("gossip.no.height.dropped.block.count").increment(1);

                    BoxError::from("gossiped block with no height")
                })
                .map_err(|e| (e, None))?;

            if block_height > max_lookahead_height {
                debug!(
                    ?hash,
                    ?block_height,
                    ?tip_height,
                    ?max_lookahead_height,
                    lookahead_limit = full_verify_concurrency_limit,
                    "gossiped block height too far ahead of the tip: dropped downloaded block",
                );
                metrics::counter!("gossip.max.height.limit.dropped.block.count").increment(1);

                Err("gossiped block height too far ahead").map_err(|e| (e.into(), None))?;
            } else if block_height < min_accepted_height {
                debug!(
                    ?hash,
                    ?block_height,
                    ?tip_height,
                    ?min_accepted_height,
                    behind_tip_limit = ?zs::MAX_BLOCK_REORG_HEIGHT,
                    "gossiped block height behind the finalized tip: dropped downloaded block",
                );
                metrics::counter!("gossip.min.height.limit.dropped.block.count").increment(1);

                Err("gossiped block height behind the finalized tip")
                    .map_err(|e| (e.into(), None))?;
            }

            let _source_guard = match source_lock {
                Some(source_lock) => Some(source_lock.lock_owned().await),
                None => None,
            };

            verifier
                .oneshot(zebra_consensus::Request::Commit(block))
                .await
                .map(|hash| (hash, block_height))
                .map_err(|e| (e, advertiser_addr))
        }
        .map_ok(|(hash, height)| {
            info!(?height, "downloaded and verified gossiped block");
            metrics::counter!("gossip.verified.block.count").increment(1);
            hash
        })
        // Tack the hash onto the error so poll_next can look up the cancel
        // handle and advertising IP on failure as well as success.
        .map_err(move |(e, advertiser_addr)| (e, hash, advertiser_addr))
        .in_current_span();

        let task = tokio::spawn(async move {
            // Prefer the cancel handle if both are ready.
            tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    trace!("task cancelled prior to completion");
                    metrics::counter!("gossip.cancelled.count").increment(1);
                    Err(("canceled".into(), hash, None))
                }
                verification = fut => verification,
            }
        });

        self.pending.push(task);
        assert!(
            self.cancel_handles
                .insert(hash, (cancel_tx, advertiser))
                .is_none(),
            "blocks are only queued once"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use std::{collections::HashSet, future, time::Duration};
    use tower::{service_fn, util::BoxCloneService};
    use zebra_chain::{parameters::Network, serialization::ZcashDeserializeInto};
    use zebra_network::InventoryResponse::Available;

    type PendingNetwork = BoxCloneService<zn::Request, zn::Response, BoxError>;
    type PendingVerifier = BoxCloneService<zebra_consensus::Request, block::Hash, BoxError>;
    type PendingState = BoxCloneService<zs::Request, zs::Response, BoxError>;

    fn hash(byte: u8) -> block::Hash {
        block::Hash([byte; 32])
    }

    fn pending_downloads() -> Downloads<PendingNetwork, PendingVerifier, PendingState> {
        let (_tip_sender, latest_chain_tip, _tip_change) =
            zs::ChainTipSender::new(None, &Network::Mainnet);
        Downloads::new(
            MAX_INBOUND_CONCURRENCY,
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<zn::Response, BoxError>>()
            })),
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<block::Hash, BoxError>>()
            })),
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<zs::Response, BoxError>>()
            })),
            latest_chain_tip,
        )
    }

    #[tokio::test]
    async fn source_admission_limits_preserve_legacy_bound_and_allow_zakura_burst() {
        let mut downloads = pending_downloads();
        let legacy_a = zn::PeerSource::LegacySocket(([127, 0, 0, 1], 8233).into());
        let legacy_same_ip = zn::PeerSource::LegacySocket(([127, 0, 0, 1], 18233).into());
        let zakura_a = zn::PeerSource::Zakura(
            zn::zakura::ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds"),
        );
        let zakura_b = zn::PeerSource::Zakura(
            zn::zakura::ZakuraPeerId::new(vec![8; 32]).expect("test peer id is within bounds"),
        );

        assert_eq!(
            downloads.download_and_verify(hash(1), Some(legacy_a)),
            DownloadAction::AddedToQueue
        );
        assert_eq!(downloads.pending.len(), 1);
        assert_eq!(
            downloads.download_and_verify(hash(2), Some(legacy_same_ip)),
            DownloadAction::FullQueue
        );
        assert_eq!(downloads.pending.len(), 1);
        assert_eq!(
            downloads.download_and_verify(hash(3), Some(zakura_a.clone())),
            DownloadAction::AddedToQueue
        );
        assert_eq!(downloads.pending.len(), 2);
        assert_eq!(
            downloads.download_and_verify(hash(4), Some(zakura_a.clone())),
            DownloadAction::AddedToQueue
        );
        assert_eq!(downloads.pending.len(), 3);
        assert_eq!(
            downloads.download_and_verify(hash(5), Some(zakura_a)),
            DownloadAction::AddedToQueue
        );
        assert_eq!(downloads.pending.len(), 4);
        assert_eq!(
            downloads.download_and_verify(hash(6), Some(zakura_b)),
            DownloadAction::AddedToQueue
        );
        assert_eq!(downloads.pending.len(), 5);
        assert_eq!(downloads.queue_len(), 5);
        assert_eq!(
            downloads.download_and_verify(hash(6), None),
            DownloadAction::AlreadyQueued
        );
    }

    #[tokio::test]
    async fn single_zakura_source_cannot_monopolize_inbound_block_queue() {
        let mut downloads = pending_downloads();
        let zakura_a = zn::PeerSource::Zakura(
            zn::zakura::ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds"),
        );
        let zakura_b = zn::PeerSource::Zakura(
            zn::zakura::ZakuraPeerId::new(vec![8; 32]).expect("test peer id is within bounds"),
        );

        // A single Zakura source may occupy at most
        // `MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER` slots of the global inbound
        // queue, even though the global queue still has plenty of room.
        for index in 0..MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER {
            let byte = u8::try_from(index).expect("per-peer cap fits in u8");
            assert_eq!(
                downloads.download_and_verify(hash(byte), Some(zakura_a.clone())),
                DownloadAction::AddedToQueue
            );
        }
        assert_eq!(
            downloads.pending.len(),
            MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER
        );

        // The next distinct hash from the same source is rejected: one Zakura peer
        // cannot keep advertising until it owns the whole global gossip queue.
        let over_cap = hash(
            u8::try_from(MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER).expect("per-peer cap fits in u8"),
        );
        assert_eq!(
            downloads.download_and_verify(over_cap, Some(zakura_a)),
            DownloadAction::FullQueue
        );
        assert_eq!(
            downloads.pending.len(),
            MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER
        );

        // A different honest Zakura peer is still admitted while the first is at
        // its cap, so no single peer monopolizes gossip admission.
        assert_eq!(
            downloads.download_and_verify(hash(200), Some(zakura_b)),
            DownloadAction::AddedToQueue
        );
        assert_eq!(
            downloads.pending.len(),
            MAX_INBOUND_BLOCK_CONCURRENCY_PER_PEER + 1
        );
    }

    #[tokio::test]
    async fn global_inbound_download_queue_bounds_admission() {
        let (_tip_sender, latest_chain_tip, _tip_change) =
            zs::ChainTipSender::new(None, &Network::Mainnet);
        let mut downloads = Downloads::new(
            MIN_CONCURRENCY_LIMIT,
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<zn::Response, BoxError>>()
            })),
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<block::Hash, BoxError>>()
            })),
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<zs::Response, BoxError>>()
            })),
            latest_chain_tip,
        );

        for index in 0..MIN_CONCURRENCY_LIMIT {
            let byte = u8::try_from(index).expect("minimum concurrency limit fits in u8");
            assert_eq!(
                downloads.download_and_verify(hash(byte), None),
                DownloadAction::AddedToQueue
            );
        }
        let overflow_hash = hash(
            u8::try_from(MIN_CONCURRENCY_LIMIT).expect("minimum concurrency limit fits in u8"),
        );
        assert_eq!(
            downloads.download_and_verify(overflow_hash, None),
            DownloadAction::FullQueue
        );
    }

    #[tokio::test]
    async fn same_source_downloads_fetch_promptly_and_verify_in_order() -> Result<(), BoxError> {
        let block_one: Arc<block::Block> =
            zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
        let block_two: Arc<block::Block> =
            zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
        let hash_one = block_one.hash();
        let hash_two = block_two.hash();
        let peer_id =
            zn::zakura::ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds");
        let source = zn::PeerSource::Zakura(peer_id.clone());

        let blocks = Arc::new(HashMap::from([
            (hash_one, block_one.clone()),
            (hash_two, block_two.clone()),
        ]));
        let (network_tx, mut network_rx) = tokio::sync::mpsc::unbounded_channel::<zn::Request>();
        let network = BoxCloneService::new(service_fn(move |request: zn::Request| {
            let blocks = blocks.clone();
            let network_tx = network_tx.clone();

            async move {
                network_tx.send(request.clone())?;

                let zn::Request::BlocksByHashFrom { hashes, .. } = request else {
                    return Err("unexpected network request".into());
                };
                let hash = hashes
                    .iter()
                    .next()
                    .copied()
                    .expect("download requests contain one hash");
                let block = blocks
                    .get(&hash)
                    .cloned()
                    .expect("test network has a block for the requested hash");

                Ok(zn::Response::Blocks(vec![Available((block, None))]))
            }
        }));

        let (commit_tx, mut commit_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = oneshot::channel();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let verifier =
            BoxCloneService::new(service_fn(move |request: zebra_consensus::Request| {
                let commit_tx = commit_tx.clone();
                let release_rx = release_rx.clone();

                async move {
                    let zebra_consensus::Request::Commit(block) = request else {
                        return Err("unexpected verifier request".into());
                    };
                    let hash = block.hash();
                    commit_tx.send(hash)?;

                    if hash == hash_one {
                        let release_rx = release_rx
                            .lock()
                            .await
                            .take()
                            .expect("first block verifier is released once");
                        release_rx.await?;
                    }

                    Ok(hash)
                }
            }));

        let (_tip_sender, latest_chain_tip, _tip_change) =
            zs::ChainTipSender::new(None, &Network::Mainnet);
        let mut downloads = Downloads::new(
            MAX_INBOUND_CONCURRENCY,
            network,
            verifier,
            BoxCloneService::new(service_fn(|request| async move {
                match request {
                    zs::Request::AnyChainBlock(_) => Ok(zs::Response::Block(None)),
                    request => Err(format!("unexpected state request: {request:?}").into()),
                }
            })),
            latest_chain_tip,
        );

        assert_eq!(
            downloads.download_and_verify(hash_one, Some(source.clone())),
            DownloadAction::AddedToQueue
        );
        assert_eq!(
            downloads.download_and_verify(hash_two, Some(source)),
            DownloadAction::AddedToQueue
        );

        let first_request = tokio::time::timeout(Duration::from_secs(1), network_rx.recv())
            .await
            .expect("first same-source download starts")
            .expect("network request channel is open");
        assert_eq!(
            first_request,
            zn::Request::BlocksByHashFrom {
                hashes: HashSet::from([hash_one]),
                source: zn::PeerSource::Zakura(peer_id.clone()),
            }
        );

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first block reaches verifier"),
            Some(hash_one)
        );

        let second_request = tokio::time::timeout(Duration::from_secs(1), network_rx.recv())
            .await
            .expect("second same-source download starts without waiting for verifier cleanup")
            .expect("network request channel is open");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), commit_rx.recv())
                .await
                .is_err(),
            "second same-source verifier commit must wait until the first verifier finishes",
        );

        release_tx
            .send(())
            .expect("first verifier task is waiting for release");

        assert_eq!(
            second_request,
            zn::Request::BlocksByHashFrom {
                hashes: HashSet::from([hash_two]),
                source: zn::PeerSource::Zakura(peer_id),
            }
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second block reaches verifier after first release"),
            Some(hash_two)
        );

        let first_result = tokio::time::timeout(Duration::from_secs(1), downloads.next())
            .await
            .expect("first completed download is yielded")
            .expect("downloads stream is open")
            .expect("first download succeeds");
        let second_result = tokio::time::timeout(Duration::from_secs(1), downloads.next())
            .await
            .expect("second completed download is yielded")
            .expect("downloads stream is open")
            .expect("second download succeeds");
        assert_eq!(
            HashSet::from([first_result, second_result]),
            HashSet::from([hash_one, hash_two])
        );
        assert_eq!(downloads.queue_len(), 0);
        assert!(downloads.source_locks.is_empty());
        assert!(downloads.source_counts.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn zakura_advertiser_source_is_sent_to_network_download_request() {
        let hash = hash(9);
        let peer_id =
            zn::zakura::ZakuraPeerId::new(vec![9; 32]).expect("test peer id is within bounds");
        let (network_tx, mut network_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_tip_sender, latest_chain_tip, _tip_change) =
            zs::ChainTipSender::new(None, &Network::Mainnet);
        let mut downloads = Downloads::new(
            MAX_INBOUND_CONCURRENCY,
            BoxCloneService::new(service_fn(move |request| {
                let network_tx = network_tx.clone();
                async move {
                    network_tx.send(request)?;
                    future::pending::<Result<zn::Response, BoxError>>().await
                }
            })),
            BoxCloneService::new(service_fn(|_request| {
                future::pending::<Result<block::Hash, BoxError>>()
            })),
            BoxCloneService::new(service_fn(|request| async move {
                match request {
                    zs::Request::AnyChainBlock(_) => Ok(zs::Response::Block(None)),
                    request => Err(format!("unexpected state request: {request:?}").into()),
                }
            })),
            latest_chain_tip,
        );

        assert_eq!(
            downloads.download_and_verify(hash, Some(zn::PeerSource::Zakura(peer_id.clone())),),
            DownloadAction::AddedToQueue
        );
        let poll_task = tokio::spawn(async move {
            let _ = downloads.next().await;
        });
        let request = tokio::time::timeout(Duration::from_secs(1), network_rx.recv())
            .await
            .expect("network request is sent")
            .expect("network request channel is open");
        assert_eq!(
            request,
            zn::Request::BlocksByHashFrom {
                hashes: HashSet::from([hash]),
                source: zn::PeerSource::Zakura(peer_id),
            }
        );
        poll_task.abort();
    }

    #[tokio::test]
    async fn substituted_block_response_is_rejected_and_cleans_up_requested_hash(
    ) -> Result<(), BoxError> {
        let requested_block: Arc<block::Block> =
            zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
        let substitute_block: Arc<block::Block> =
            zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
        let requested_hash = requested_block.hash();
        assert_ne!(requested_hash, substitute_block.hash());

        let peer_id =
            zn::zakura::ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds");
        let source = zn::PeerSource::Zakura(peer_id);

        // The peer answers a single-hash request with a *different* valid block.
        let substitute = substitute_block.clone();
        let network = BoxCloneService::new(service_fn(move |request: zn::Request| {
            let substitute = substitute.clone();
            async move {
                let (zn::Request::BlocksByHashFrom { .. } | zn::Request::BlocksByHash(_)) = request
                else {
                    return Err("unexpected network request".into());
                };
                Ok(zn::Response::Blocks(vec![Available((substitute, None))]))
            }
        }));

        // The verifier would happily commit whatever block it is handed.
        let verifier =
            BoxCloneService::new(service_fn(|request: zebra_consensus::Request| async move {
                let zebra_consensus::Request::Commit(block) = request else {
                    return Err("unexpected verifier request".into());
                };
                Ok(block.hash())
            }));

        let (_tip_sender, latest_chain_tip, _tip_change) =
            zs::ChainTipSender::new(None, &Network::Mainnet);
        let mut downloads = Downloads::new(
            MAX_INBOUND_CONCURRENCY,
            network,
            verifier,
            BoxCloneService::new(service_fn(|request| async move {
                match request {
                    zs::Request::AnyChainBlock(_) => Ok(zs::Response::Block(None)),
                    request => Err(format!("unexpected state request: {request:?}").into()),
                }
            })),
            latest_chain_tip,
        );

        assert_eq!(
            downloads.download_and_verify(requested_hash, Some(source)),
            DownloadAction::AddedToQueue
        );

        // The download must fail rather than accept the substituted block.
        let result = tokio::time::timeout(Duration::from_secs(1), downloads.next())
            .await
            .expect("download completes")
            .expect("downloads stream is open");
        assert!(
            result.is_err(),
            "a substituted block must be rejected, not accepted as {result:?}",
        );

        // Cleanup must key on the requested hash: no stale queue or source-count
        // state may remain under the originally requested hash.
        assert!(
            downloads.cancel_handles.is_empty(),
            "queue entry under the requested hash must be removed",
        );
        assert!(
            downloads.source_counts.is_empty(),
            "source accounting must be decremented for the requested hash",
        );
        assert!(downloads.source_locks.is_empty());
        assert_eq!(downloads.queue_len(), 0);

        Ok(())
    }
}
