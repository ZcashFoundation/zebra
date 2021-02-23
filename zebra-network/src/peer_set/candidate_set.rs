use std::sync::{Arc, Mutex};

use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use tower::{Service, ServiceExt};
use tracing::Level;

use crate::{types::MetaAddr, AddressBook, BoxError, Request, Response};

/// The `CandidateSet` maintains a pool of candidate peers.
///
/// It divides the set of all possible candidate peers into three disjoint subsets:
///
/// 1. Disconnected peers, which we previously connected to but are not currently connected to;
/// 2. Gossiped peers, which we learned about from other peers but have never connected to;
/// 3. Failed peers, to whom we attempted to connect but were unable to.
///
/// ```ascii,no_run
///                         ┌─────────────────┐
///                         │     PeerSet     │
///                         │GetPeers Requests│
///                         └─────────────────┘
///                                  │
///                                  │
///                                  │
///                                  │
///                                  ▼
///    ┌─────────────┐   filter by   Λ     filter by
///    │   PeerSet   │!contains_addr╱ ╲ !contains_addr
/// ┌──│ AddressBook │────────────▶▕   ▏◀───────────────────┐
/// │  └─────────────┘              ╲ ╱                     │
/// │         │                      V                      │
/// │         │disconnected_peers    │                      │
/// │         ▼                      │                      │
/// │         Λ     filter by        │                      │
/// │        ╱ ╲ !contains_addr      │                      │
/// │       ▕   ▏◀───────────────────┼──────────────────────┤
/// │        ╲ ╱                     │                      │
/// │         V                      │                      │
/// │         │                      │                      │
/// │┌────────┼──────────────────────┼──────────────────────┼────────┐
/// ││        ▼                      ▼                      │        │
/// ││ ┌─────────────┐        ┌─────────────┐        ┌─────────────┐ │
/// ││ │Disconnected │        │  Gossiped   │        │Failed Peers │ │
/// ││ │    Peers    │        │    Peers    │        │ AddressBook │◀┼┐
/// ││ │ AddressBook │        │ AddressBook │        │             │ ││
/// ││ └─────────────┘        └─────────────┘        └─────────────┘ ││
/// ││        │                      │                      │        ││
/// ││ #1 drain_oldest        #2 drain_newest        #3 drain_oldest ││
/// ││        │                      │                      │        ││
/// ││        ├──────────────────────┴──────────────────────┘        ││
/// ││        │           disjoint candidate sets                    ││
/// │└────────┼──────────────────────────────────────────────────────┘│
/// │         ▼                                                       │
/// │         Λ                                                       │
/// │        ╱ ╲         filter by                                    │
/// └──────▶▕   ▏!is_potentially_connected                            │
///          ╲ ╱                                                      │
///           V                                                       │
///           │                                                       │
///           │                                                       │
///           ▼                                                       │
///           Λ                                                       │
///          ╱ ╲                                                      │
///         ▕   ▏─────────────────────────────────────────────────────┘
///          ╲ ╱   connection failed, update last_seen to now()
///           V
///           │
///           │
///           ▼
///    ┌────────────┐
///    │    send    │
///    │peer::Client│
///    │to Discover │
///    └────────────┘
/// ```
pub(super) struct CandidateSet<S> {
    pub(super) disconnected: AddressBook,
    pub(super) gossiped: AddressBook,
    pub(super) failed: AddressBook,
    pub(super) peer_set: Arc<Mutex<AddressBook>>,
    pub(super) peer_service: S,
}

impl<S> CandidateSet<S>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    pub fn new(peer_set: Arc<Mutex<AddressBook>>, peer_service: S) -> CandidateSet<S> {
        CandidateSet {
            disconnected: AddressBook::new(span!(Level::TRACE, "disconnected peers")),
            gossiped: AddressBook::new(span!(Level::TRACE, "gossiped peers")),
            failed: AddressBook::new(span!(Level::TRACE, "failed peers")),
            peer_set,
            peer_service,
        }
    }

    pub async fn update(&mut self) -> Result<(), BoxError> {
        // Opportunistically crawl the network on every update call to ensure
        // we're actively fetching peers. Continue independently of whether we
        // actually receive any peers, but always ask the network for more.
        // Because requests are load-balanced across existing peers, we can make
        // multiple requests concurrently, which will be randomly assigned to
        // existing peers, but we don't make too many because update may be
        // called while the peer set is already loaded.
        let mut responses = FuturesUnordered::new();
        trace!("sending GetPeers requests");
        // Yes this loops only once (for now), until we add fanout back.
        for _ in 0..1usize {
            self.peer_service.ready_and().await?;
            responses.push(self.peer_service.call(Request::Peers));
        }
        while let Some(rsp) = responses.next().await {
            if let Ok(Response::Peers(addrs)) = rsp {
                let addr_len = addrs.len();
                let prev_len = self.gossiped.len();
                // Filter new addresses to ensure that gossiped addresses are actually new
                let failed = &self.failed;
                let peer_set = &self.peer_set;
                let new_addrs = addrs
                    .into_iter()
                    .filter(|meta| !failed.contains_addr(&meta.addr))
                    .filter(|meta| !peer_set.lock().unwrap().contains_addr(&meta.addr));
                self.gossiped.extend(new_addrs);
                trace!(
                    addr_len,
                    new_addrs = self.gossiped.len() - prev_len,
                    "got response to GetPeers"
                );
            } else {
                trace!("got error in GetPeers request");
            }
        }

        // Determine whether any known peers have recently disconnected.
        let failed = &self.failed;
        let peer_set = &self.peer_set;
        self.disconnected.extend(
            peer_set
                .lock()
                .expect("mutex must be unpoisoned")
                .disconnected_peers()
                .filter(|meta| failed.contains_addr(&meta.addr)),
        );

        Ok(())
    }

    pub fn next(&mut self) -> Option<MetaAddr> {
        metrics::gauge!("candidate_set.disconnected", self.disconnected.len() as f64);
        metrics::gauge!("candidate_set.gossiped", self.gossiped.len() as f64);
        metrics::gauge!("candidate_set.failed", self.failed.len() as f64);
        debug!(
            disconnected_peers = self.disconnected.len(),
            gossiped_peers = self.gossiped.len(),
            failed_peers = self.failed.len()
        );
        let guard = self.peer_set.lock().unwrap();
        self.disconnected
            .drain_oldest()
            .chain(self.gossiped.drain_newest())
            .chain(self.failed.drain_oldest())
            .find(|meta| !guard.is_potentially_connected(&meta.addr))
    }

    pub fn report_failed(&mut self, mut addr: MetaAddr) {
        addr.last_seen = Utc::now();
        self.failed.update(addr);
    }
}
