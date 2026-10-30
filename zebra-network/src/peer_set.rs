pub(crate) mod candidate_set;
mod initialize;
mod inventory_registry;
mod limit;
mod service;
pub(crate) mod set;
mod stall_tracker;
mod unready_service;

pub(crate) use candidate_set::{
    crawl_once, crawler_services, next_reconnect_peer, ready_peer_count, CrawlService,
    NextPeerService,
};
pub(crate) use inventory_registry::InventoryChange;
pub(crate) use limit::{ActiveConnectionCounter, ConnectionTracker};

use inventory_registry::InventoryRegistry;
pub(crate) use set::PeerSet;

pub use initialize::{init, init_with_block_gossip_peer_ips};
pub use service::{NetworkResponseFuture, NetworkService};

use std::{
    collections::HashMap,
    sync::Arc,
    task::{Context, Poll, Wake},
};

use futures::{
    future::BoxFuture, stream::FuturesUnordered, task::AtomicWaker, FutureExt, StreamExt,
};
use rand::seq::SliceRandom;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::PollSender;
use tower::{limit::ConcurrencyLimit, Service};
use zebra_chain::{block, chain_tip::ChainTip, parameters::Network};

use crate::{
    constants::REQUEST_TIMEOUT, peer::MinimumPeerVersion, protocol::external::types::Version,
    BanList, BoxError, PeerSocketAddr, Request, Response,
};

/// A queued advert acknowledges a completed local transport write, not remote receipt.
#[derive(Debug)]
pub(crate) struct BlockAdvert {
    pub(crate) hash: block::Hash,
    pub(crate) compact_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) result: oneshot::Sender<Result<(), BoxError>>,
}

type AdvertPeers = HashMap<PeerSocketAddr, (Version, bool, bool, mpsc::Sender<BlockAdvert>)>;

/// Discovery and maintenance requests wake both an active worker and the idle-buffer self-poller.
#[derive(Debug, Default)]
struct PeerSetWaker {
    notify: tokio::sync::Notify,
    worker: AtomicWaker,
}

impl Wake for PeerSetWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify.notify_one();
        self.worker.wake();
    }
}

/// Bounded block fanout independent of ordinary peer request readiness.
///
/// Accepts `AdvertiseBlock` (sampled peers plus sidecars), `AdvertiseBlockToAll`, and
/// `AdvertiseEarlyBlock` (the same sample, restricted to bilaterally negotiated connections).
/// Readiness reconciles pending discovery and waits for an eligible connected peer, even if busy.
/// Success requires every selected connection to complete its local write. Full queues, revoked
/// connections, write failures, and losing all eligible peers after readiness are errors.
/// Keep an owning [`NetworkService`] alive; advertiser handles do not own background tasks.
#[derive(Clone, Debug)]
pub struct BlockAdvertiser<C> {
    inner: ConcurrencyLimit<AdvertisePeers<C>>,
}

#[derive(Debug)]
struct AdvertisePeers<C> {
    peers: watch::Receiver<AdvertPeers>,
    bans: watch::Receiver<BanList>,
    minimum_version: MinimumPeerVersion<C>,
    network: Network,
    maintenance: PollSender<oneshot::Sender<()>>,
    wake: Arc<PeerSetWaker>,
    completion: Option<oneshot::Receiver<()>>,
    maintained: bool,
}

impl<C: Clone> Clone for AdvertisePeers<C> {
    fn clone(&self) -> Self {
        Self {
            peers: self.peers.clone(),
            bans: self.bans.clone(),
            minimum_version: self.minimum_version.clone(),
            network: self.network.clone(),
            maintenance: self.maintenance.clone(),
            wake: self.wake.clone(),
            completion: None,
            maintained: false,
        }
    }
}

impl<C: ChainTip + Clone + Send + 'static> Service<Request> for BlockAdvertiser<C> {
    type Response = Response;
    type Error = BoxError;
    type Future = tower::limit::concurrency::future::ResponseFuture<
        BoxFuture<'static, Result<Response, BoxError>>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.inner.call(request)
    }
}

impl<C: ChainTip + Clone + Send + 'static> Service<Request> for AdvertisePeers<C> {
    type Response = Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        if !self.maintained {
            if self.completion.is_none() {
                futures::ready!(self.maintenance.poll_reserve(cx))
                    .map_err(|_| "peer set maintenance stopped")?;
                let (completed, completion) = oneshot::channel();
                self.maintenance
                    .send_item(completed)
                    .map_err(|_| "peer set maintenance stopped")?;
                self.completion = Some(completion);
                self.wake.wake_by_ref();
            }
            let result = futures::ready!(self
                .completion
                .as_mut()
                .expect("a maintenance request was submitted")
                .poll_unpin(cx));
            self.completion = None;
            result?;
            self.maintained = true;
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.maintained = false;
        let (hash, all, early) = match request {
            Request::AdvertiseBlock(hash, _) => (hash, false, false),
            Request::AdvertiseBlockToAll(hash) => (hash, true, false),
            Request::AdvertiseEarlyBlock(hash) => (hash, false, true),
            _ => return futures::future::ready(Err("not a block advertisement".into())).boxed(),
        };
        let minimum = self.minimum_version.current();
        let bans = self.bans.borrow();
        let peers = self.peers.borrow();
        let mut selected: Vec<_> = peers
            .iter()
            .filter(|(addr, (version, _, early_block_relay, sender))| {
                *version >= minimum
                    && (!early || *early_block_relay)
                    && !bans.is_banned(addr.ip())
                    && !sender.is_closed()
            })
            .map(|(_, entry)| entry)
            .collect();
        selected.shuffle(&mut rand::rng());
        let sample = broadcast_peer_count(&self.network, selected.len());
        let mut writes = FuturesUnordered::new();
        let mut sampled = 0;
        let compact_slots = Arc::new(tokio::sync::Semaphore::new(3));
        for (_, sidecar, _, sender) in selected {
            if !all && !*sidecar {
                if sampled >= sample {
                    continue;
                }
                sampled += 1;
            }
            let (result, receiver) = oneshot::channel();
            let admission = sender
                .try_send(BlockAdvert {
                    hash,
                    compact_slots: compact_slots.clone(),
                    result,
                })
                .map_err(|error| -> BoxError { error.to_string().into() });
            writes.push(async move {
                admission?;
                tokio::time::timeout(REQUEST_TIMEOUT, receiver).await??
            });
        }
        async move {
            if writes.is_empty() {
                return Err("no eligible peers for block advertisement".into());
            }
            let mut failure = None;
            while let Some(result) = writes.next().await {
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
            }
            match failure {
                Some(error) => Err(error),
                None => Ok(Response::Nil),
            }
        }
        .boxed()
    }
}

fn broadcast_peer_count(network: &Network, peers: usize) -> usize {
    if network.is_regtest() {
        peers
    } else {
        peers.div_ceil(3)
    }
}
