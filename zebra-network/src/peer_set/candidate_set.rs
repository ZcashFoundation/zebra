use std::{
    mem,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::{sleep, sleep_until, Instant, Sleep};
use tower::{Service, ServiceExt};

use crate::{types::MetaAddr, AddressBook, BoxError, PeerAddrState, Request, Response};

/// The `CandidateSet` manages the `PeerSet`'s peer reconnection attempts.
///
/// It divides the set of all possible candidate peers into disjoint subsets,
/// using the `PeerAddrState`:
///
/// 1. `Responded` peers, which we previously connected to. If we have not received
///    any messages from a `Responded` peer within a cutoff time, we assume that it
///    has disconnected or hung, and attempt reconnection;
/// 2. `NeverAttempted` peers, which we learned about from other peers or a DNS
///    seeder, but have never connected to;
/// 3. `Failed` peers, to whom we attempted to connect but were unable to;
/// 4. `AttemptPending` peers, which we've recently queued for reconnection.
///
/// ```ascii,no_run
///                         ┌──────────────────┐
///                         │     PeerSet      │
///                         │GetPeers Responses│
///                         └──────────────────┘
///                                  │
///                                  │
///                                  │
///                                  │
///                                  ▼
///             filter by            Λ
///          !contains_addr         ╱ ╲
///  ┌────────────────────────────▶▕   ▏
///  │                              ╲ ╱
///  │                               V
///  │                               │
///  │                               │
///  │                               │
///  │                               │
///  │                               │
///  │                               │
///  │                               │
///  │                               │
///  ├───────────────────────────────┼───────────────────────────────┐
///  │ PeerSet AddressBook           ▼                               │
///  │ ┌─────────────┐       ┌────────────────┐      ┌─────────────┐ │
///  │ │  Possibly   │       │`NeverAttempted`│      │  `Failed`   │ │
///  │ │Disconnected │       │     Peers      │      │   Peers     │◀┼┐
///  │ │ `Responded` │       │                │      │             │ ││
///  │ │    Peers    │       │                │      │             │ ││
///  │ └─────────────┘       └────────────────┘      └─────────────┘ ││
///  │        │                      │                      │        ││
///  │ #1 oldest_first        #2 newest_first        #3 oldest_first ││
///  │        │                      │                      │        ││
///  │        ├──────────────────────┴──────────────────────┘        ││
///  │        │         disjoint `PeerAddrState`s                    ││
///  ├────────┼──────────────────────────────────────────────────────┘│
///  │        ▼                                                       │
///  │        Λ                                                       │
///  │       ╱ ╲         filter by                                    │
///  └─────▶▕   ▏!is_potentially_connected                            │
///          ╲ ╱      to remove live                                  │
///           V      `Responded` peers                                │
///           │                                                       │
///           │                                                       │
///           ▼                                                       │
///    ┌────────────────┐                                             │
///    │`AttemptPending`│                                             │
///    │     Peers      │                                             │
///    │                │                                             │
///    └────────────────┘                                             │
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
///           │
///           │
///           ▼
///  ┌───────────────────────────────────────┐
///  │ every time we receive a peer message: │
///  │  * update state to `Responded`        │
///  │  * update last_seen to now()          │
///  └───────────────────────────────────────┘
///
/// ```
// TODO:
//   * draw arrow from the "peer message" box into the `Responded` state box
//   * make the "disjoint states" box include `AttemptPending`
pub(super) struct CandidateSet<S> {
    pub(super) peer_set: Arc<Mutex<AddressBook>>,
    pub(super) peer_service: S,
    sleep: Sleep,
}

impl<S> CandidateSet<S>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    /// The minimum time between successive calls to `CandidateSet::next()`.
    ///
    /// ## Security
    ///
    /// Zebra resists distributed denial of service attacks by making sure that new peer connections
    /// are initiated at least `MIN_PEER_CONNECTION_INTERVAL` apart.
    const MIN_PEER_CONNECTION_INTERVAL: Duration = Duration::from_millis(100);

    /// Uses `peer_set` and `peer_service` to manage a [`CandidateSet`] of peers.
    pub fn new(peer_set: Arc<Mutex<AddressBook>>, peer_service: S) -> CandidateSet<S> {
        CandidateSet {
            peer_set,
            peer_service,
            sleep: sleep(Duration::from_secs(0)),
        }
    }

    /// Update the peer set from the network.
    ///
    /// - Ask a few live `Responded` peers to send us more peers.
    /// - Process all completed peer responses, adding new peers in the
    ///   `NeverAttempted` state.
    ///
    /// ## Correctness
    ///
    /// The handshaker sets up the peer message receiver so it also sends a
    /// `Responded` peer address update.
    ///
    /// `report_failed` puts peers into the `Failed` state.
    ///
    /// `next` puts peers into the `AttemptPending` state.
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
            if let Ok(Response::Peers(rsp_addrs)) = rsp {
                // Filter new addresses to ensure that gossiped addresses are actually new
                let peer_set = &self.peer_set;
                let new_addrs = rsp_addrs
                    .iter()
                    .filter(|meta| !peer_set.lock().unwrap().contains_addr(&meta.addr))
                    .collect::<Vec<_>>();
                trace!(
                    ?rsp_addrs,
                    new_addr_count = ?new_addrs.len(),
                    "got response to GetPeers"
                );
                // New addresses are deserialized in the `NeverAttempted` state
                peer_set
                    .lock()
                    .unwrap()
                    .extend(new_addrs.into_iter().cloned());
            } else {
                trace!("got error in GetPeers request");
            }
        }

        Ok(())
    }

    /// Returns the next candidate for a connection attempt, if any are available.
    ///
    /// Returns peers in this order:
    /// - oldest `Responded` that are not live
    /// - newest `NeverAttempted`
    /// - oldest `Failed`
    ///
    /// Skips `AttemptPending` peers and live `Responded` peers.
    ///
    /// ## Correctness
    ///
    /// `AttemptPending` peers will become `Responded` if they respond, or
    /// become `Failed` if they time out or provide a bad response.
    ///
    /// Live `Responded` peers will stay live if they keep responding, or
    /// become a reconnection candidate if they stop responding.
    pub async fn next(&mut self) -> Option<MetaAddr> {
        let now = Instant::now();
        let mut sleep = sleep_until(now + Self::MIN_PEER_CONNECTION_INTERVAL);
        mem::swap(&mut self.sleep, &mut sleep);

        let reconnect = {
            let mut peer_set_guard = self.peer_set.lock().unwrap();
            let mut reconnect = peer_set_guard.reconnection_peers().next()?;

            reconnect.last_seen = Utc::now();
            reconnect.last_connection_state = PeerAddrState::AttemptPending;
            peer_set_guard.update(reconnect);
            reconnect
        };

        sleep.await;

        Some(reconnect)
    }

    /// Mark `addr` as a failed peer.
    pub fn report_failed(&mut self, mut addr: MetaAddr) {
        addr.last_seen = Utc::now();
        addr.last_connection_state = PeerAddrState::Failed;
        self.peer_set.lock().unwrap().update(addr);
    }
}
