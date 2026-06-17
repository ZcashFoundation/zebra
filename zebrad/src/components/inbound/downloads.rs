//! A download stream that handles gossiped blocks from peers.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    pin::Pin,
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

use zebra_chain::{
    block::{self, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::components::sync::MIN_CONCURRENCY_LIMIT;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Source key used for inbound block download accounting.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AdvertiserSource {
    /// Legacy TCP peers are capped per IP address, preserving the existing policy.
    LegacyIp(IpAddr),

    /// Zakura peers are capped per authenticated peer id.
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
/// The total queue bound is `MAX_INBOUND_CONCURRENCY * 9 MB`. Each legacy peer IP
/// or authenticated Zakura peer is limited to one in-flight download (9 MB) by
/// the source cap enforced in [`Downloads::download_and_verify`], so a sybil or
/// IPv6-range attacker still needs many distinct source IPs or authenticated
/// Zakura identities to approach the total bound.
/// (See #1880 for more details.)
///
/// Malicious blocks will eventually timeout or fail contextual validation.
/// Once validation fails, the block is dropped, and its memory is deallocated.
pub const MAX_INBOUND_CONCURRENCY: usize = 200;

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

    /// The advertising peer's IP already has an in-flight download, so
    /// this request was ignored. Zcash's post-Blossom target block spacing
    /// is 75 seconds, so honest peers rarely gossip more than one block
    /// before the first is verified; during reorgs or recovery the same
    /// hash also arrives from other peers or via the syncer.
    TooManyFromPeer,
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
    /// A list of pending block download and verify tasks.
    #[pin]
    pending: FuturesUnordered<
        JoinHandle<Result<block::Hash, (BoxError, block::Hash, Option<PeerSocketAddr>)>>,
    >,

    /// Cancellation handles for tasks in [`Self::pending`], keyed by block
    /// hash. The optional source is recorded in [`Self::in_flight_sources`],
    /// so completion can remove it by hash lookup.
    cancel_handles: HashMap<block::Hash, (oneshot::Sender<()>, Option<AdvertiserSource>)>,

    /// Advertiser sources with an in-flight download and verify task.
    ///
    /// Invariant: a source is present iff some entry in [`Self::cancel_handles`]
    /// has value `(_, Some(source))`. Enforces one in-flight download per
    /// legacy IP or authenticated Zakura peer.
    ///
    /// Size-bounded by `full_verify_concurrency_limit` (≤ [`MAX_INBOUND_CONCURRENCY`]),
    /// inherited from the [`DownloadAction::FullQueue`] check on
    /// [`Self::pending`].
    in_flight_sources: HashSet<AdvertiserSource>,
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
            let (result, hash) =
                match join_result.expect("block download and verify tasks must not panic") {
                    Ok(hash) => (Ok(hash), hash),
                    Err((e, hash, advertiser_addr)) => (Err((e, advertiser_addr)), hash),
                };
            if let Some((_, Some(source))) = this.cancel_handles.remove(&hash) {
                assert!(
                    this.in_flight_sources.remove(&source),
                    "every tracked source was inserted when its download was queued",
                );
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
            in_flight_sources: HashSet::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// When `advertiser` is `Some`, it is tracked in
    /// [`Self::in_flight_sources`] and used to enforce one in-flight download
    /// per legacy IP or authenticated Zakura peer; `None` bypasses source
    /// accounting (for example when Zebra triggers the download internally).
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(
        &mut self,
        hash: block::Hash,
        download_source: Option<zn::PeerSource>,
    ) -> DownloadAction {
        let advertiser = download_source.clone().map(AdvertiserSource::from);

        if self.cancel_handles.contains_key(&hash) {
            debug!(
                ?hash,
                queue_len = self.pending.len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "block hash already queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);
            metrics::counter!("gossip.already.queued.dropped.block.hash.count").increment(1);

            return DownloadAction::AlreadyQueued;
        }

        if self.pending.len() >= self.full_verify_concurrency_limit {
            debug!(
                ?hash,
                queue_len = self.pending.len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "too many blocks queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);
            metrics::counter!("gossip.full.queue.dropped.block.hash.count").increment(1);

            return DownloadAction::FullQueue;
        }

        if let Some(source) = &advertiser {
            if self.in_flight_sources.contains(source) {
                debug!(
                    ?hash,
                    ?advertiser,
                    "already have an in-flight inbound download from peer source: ignored block",
                );

                metrics::counter!("gossip.peer.limit.dropped.block.hash.count").increment(1);

                return DownloadAction::TooManyFromPeer;
            }
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let state = self.state.clone();
        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let full_verify_concurrency_limit = self.full_verify_concurrency_limit;

        let fut = async move {
            // Check if the block is already in the state.
            match state.oneshot(zs::Request::KnownBlock(hash)).await {
                Ok(zs::Response::KnownBlock(None)) => Ok(()),
                Ok(zs::Response::KnownBlock(Some(_))) => Err("already present".into()),
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
        if let Some(source) = advertiser.clone() {
            assert!(
                self.in_flight_sources.insert(source),
                "the per-source cap check above rejects any source already in flight",
            );
        }
        assert!(
            self.cancel_handles
                .insert(hash, (cancel_tx, advertiser))
                .is_none(),
            "blocks are only queued once"
        );

        debug!(
            ?hash,
            queue_len = self.pending.len(),
            concurrency_limit = self.full_verify_concurrency_limit,
            "queued hash for download",
        );
        metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);

        DownloadAction::AddedToQueue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use std::{future, time::Duration};
    use tower::{service_fn, util::BoxCloneService};
    use zebra_chain::parameters::Network;

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
    async fn advertiser_sources_enforce_legacy_ip_and_zakura_peer_caps() {
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
        assert_eq!(
            downloads.download_and_verify(hash(2), Some(legacy_same_ip)),
            DownloadAction::TooManyFromPeer
        );
        assert_eq!(
            downloads.download_and_verify(hash(3), Some(zakura_a.clone())),
            DownloadAction::AddedToQueue
        );
        assert_eq!(
            downloads.download_and_verify(hash(4), Some(zakura_a)),
            DownloadAction::TooManyFromPeer
        );
        assert_eq!(
            downloads.download_and_verify(hash(5), Some(zakura_b)),
            DownloadAction::AddedToQueue
        );
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
                    zs::Request::KnownBlock(_) => Ok(zs::Response::KnownBlock(None)),
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
}
