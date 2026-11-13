//! Address relay between version 2 peers (draft, "Address Broadcasting"),
//! owned by one actor task and reached over a channel.
//!
//! Every version 2 connection registers a bounded channel with the hub. The
//! hub forwards each fresh address a peer announced to two other registered
//! peers, trickled out after a random delay, and advertises this node's own
//! version 2 address to each registered peer soon after it registers, then
//! about once a day. A connection writes what it receives from the hub on its
//! address announcement stream.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use indexmap::IndexSet;
use rand::RngExt;
use sha2::{Digest, Sha256};
use tokio::{sync::mpsc, time::Instant};

use zebra_chain::{
    parameters::Network,
    serialization::{DateTime32, Duration32},
};

use crate::{
    meta_addr::MetaAddr,
    peer::address_is_valid_for_outbound_connections,
    protocol::{
        external::{canonical_peer_addr, types::PeerServices},
        v2::announcement::clamp_relayed_time,
    },
    PeerSocketAddr,
};

#[cfg(test)]
mod tests;

/// How many batches of addresses may wait for a connection to write them
/// before the hub drops new ones (best effort).
pub const RELAY_CHANNEL_SIZE: usize = 16;

/// How many messages may wait for the hub before connections' offers are
/// dropped. Offers are rate-limited per connection, so this only fills
/// briefly.
const INBOX_SIZE: usize = 1024;

/// How long a registration waits for room in the hub's queue before it is
/// given up.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);

/// The most addresses sent to one peer per trickle; the rest stay queued for
/// the next one.
///
/// Bitcoin Core relays only `addr` messages of at most ten records, since a
/// larger one answers a `getaddr`. A version 2 address announcement stream
/// carries only unsolicited records (`get-addr` answers have their own
/// stream) and has no message boundaries, so a received batch's size says
/// nothing about the sender. Instead this node sends small batches, so a
/// peer's address budget is spent gradually.
pub const MAX_RELAY_BATCH: usize = 10;

/// The oldest a relayed record's time may be, before now, to be relayed
/// (Bitcoin Core).
const MAX_RELAY_AGE: Duration32 = Duration32::from_minutes(10);

/// How many peers each relayed address goes to (Bitcoin Core: 2 for
/// reachable addresses).
pub const RELAY_FANOUT: usize = 2;

/// How many addresses the hub remembers each peer knows, to suppress echoes;
/// the oldest is forgotten first (Bitcoin Core).
pub const MAX_KNOWN_ADDRS: usize = 5000;

/// The most addresses queued for one peer between trickles; more are
/// dropped.
const MAX_QUEUED_ADDRS: usize = 1000;

/// The timings the hub uses: production values in [`Timings::default`],
/// short ones in tests.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timings {
    /// The mean of the exponential delay before a peer's queued addresses
    /// are sent (Bitcoin Core: 30 s).
    pub trickle_mean: Duration,
    /// The most a self-advertisement waits after a peer registers.
    pub first_self_advert_max_delay: Duration,
    /// The mean interval between later self-advertisements.
    pub self_advert_interval: Duration,
    /// How far each later self-advertisement is from the mean interval, at
    /// most, either way.
    pub self_advert_jitter: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            trickle_mean: Duration::from_secs(30),
            first_self_advert_max_delay: Duration::from_secs(60),
            self_advert_interval: Duration::from_secs(24 * 60 * 60),
            self_advert_jitter: Duration::from_secs(60 * 60),
        }
    }
}

/// A message to the hub.
enum Message {
    /// A connection to `peer` takes addresses on `tx`.
    Register {
        peer: PeerSocketAddr,
        id: u64,
        tx: mpsc::Sender<Vec<MetaAddr>>,
    },
    /// The connection registered as `id` has ended.
    Deregister { peer: PeerSocketAddr, id: u64 },
    /// `source` announced `addrs` in one batch.
    Offer {
        source: PeerSocketAddr,
        addrs: Vec<MetaAddr>,
    },
}

/// A connection's registration with the hub, for deregistering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration {
    peer: PeerSocketAddr,
    id: u64,
}

/// A handle to the address relay hub.
///
/// No method waits. Offers and deregistrations are best effort: one that
/// does not fit in the hub's queue is dropped. A registration that does not
/// fit is delivered once there is room. Cloning the handle shares the hub,
/// which runs until every handle has been dropped.
#[derive(Clone, Debug)]
pub struct AddrRelayHandle {
    inbox: mpsc::Sender<Message>,
    next_id: Arc<AtomicU64>,
}

impl AddrRelayHandle {
    /// Registers the connection to `peer`, whose addresses to announce arrive
    /// on `tx`. While another open connection to `peer` is registered, the
    /// hub keeps that one and drops `tx`.
    ///
    /// A registration is never dropped for a full queue: the connection
    /// would then get no relayed address for as long as it lasted. It is
    /// delivered by a task instead, which waits for room for up to
    /// [`REGISTER_TIMEOUT`], or until the connection ends, so the caller
    /// never waits. A deregistration sent in the meantime can reach the hub
    /// first: the hub drops the closed destination at the next registration.
    ///
    /// # Panics
    ///
    /// If the queue is full and this is called outside a Tokio runtime.
    pub fn register(&self, peer: PeerSocketAddr, tx: mpsc::Sender<Vec<MetaAddr>>) -> Registration {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let registration = Registration { peer, id };
        let message = Message::Register {
            peer,
            id,
            tx: tx.clone(),
        };
        let Err(mpsc::error::TrySendError::Full(message)) = self.inbox.try_send(message) else {
            return registration;
        };

        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::select! {
                // The connection ended, so there is nothing left to register.
                () = tx.closed() => {}
                sent = tokio::time::timeout(REGISTER_TIMEOUT, inbox.send(message)) => {
                    if sent.is_err() {
                        metrics::counter!("zcash.net.v2.addresses.relay.unregistered")
                            .increment(1);
                        tracing::debug!(
                            ?peer,
                            "address relay hub stayed behind, so this peer gets no relayed \
                             addresses"
                        );
                    }
                }
            }
        });
        registration
    }

    /// Deregisters a connection that has ended. The hub also drops a
    /// connection whose channel it finds closed.
    pub fn deregister(&self, registration: Registration) {
        let Registration { peer, id } = registration;
        self.send(Message::Deregister { peer, id });
    }

    /// Offers a batch of addresses `source` announced, already within its
    /// budget and with their times clamped, for relay.
    pub fn offer(&self, source: PeerSocketAddr, addrs: Vec<MetaAddr>) {
        self.send(Message::Offer { source, addrs });
    }

    fn send(&self, message: Message) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.inbox.try_send(message) {
            tracing::debug!("address relay hub is behind, dropping a message");
        }
    }
}

/// The address relay hub.
pub struct AddrRelay;

impl AddrRelay {
    /// Spawns the hub. It advertises `advertised` to every registered peer
    /// if it is valid for outbound connections on `network`, and nothing
    /// without it.
    pub fn spawn(advertised: Option<SocketAddr>, network: Network) -> AddrRelayHandle {
        Self::spawn_with(advertised, network, rand::random(), Timings::default())
    }

    /// Spawns the hub with a relay `secret` and `timings`.
    pub(crate) fn spawn_with(
        advertised: Option<SocketAddr>,
        network: Network,
        secret: [u8; 32],
        timings: Timings,
    ) -> AddrRelayHandle {
        let advertised = advertised.map(canonical_peer_addr).filter(|addr| {
            address_is_valid_for_outbound_connections(*addr, network.clone()).is_ok()
        });
        let (inbox, receiver) = mpsc::channel(INBOX_SIZE);
        let hub = Hub {
            secret,
            advertised,
            network,
            timings,
            peers: HashMap::new(),
        };
        tokio::spawn(hub.run(receiver));
        AddrRelayHandle {
            inbox,
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// The peers `addr` is relayed to on unix day `day`: the [`RELAY_FANOUT`]
/// `candidates` with the lowest `SHA-256(secret || addr || day || candidate)`.
///
/// The choice is the same all day for each address, so a peer that announces
/// an address repeatedly can't spread it further, and it can't be predicted
/// without `secret`.
pub(crate) fn relay_targets(
    secret: &[u8; 32],
    addr: PeerSocketAddr,
    day: u32,
    candidates: impl IntoIterator<Item = PeerSocketAddr>,
) -> Vec<PeerSocketAddr> {
    let mut prefix = Sha256::new();
    prefix.update(secret);
    prefix.update(addr_bytes(addr));
    prefix.update(day.to_le_bytes());

    let mut ranked: Vec<([u8; 32], PeerSocketAddr)> = candidates
        .into_iter()
        .map(|candidate| {
            let mut hasher = prefix.clone();
            hasher.update(addr_bytes(candidate));
            (hasher.finalize().into(), candidate)
        })
        .collect();
    ranked.sort_unstable();
    ranked
        .into_iter()
        .take(RELAY_FANOUT)
        .map(|(_, candidate)| candidate)
        .collect()
}

/// The unix day of `time`.
pub(crate) fn unix_day(time: DateTime32) -> u32 {
    time.timestamp() / (24 * 60 * 60)
}

/// The 16-byte IPv6 (or IPv4-mapped) address and big-endian port of `addr`.
fn addr_bytes(addr: PeerSocketAddr) -> [u8; 18] {
    let ip = match addr.ip() {
        IpAddr::V4(ip) => ip.to_ipv6_mapped(),
        IpAddr::V6(ip) => ip,
    };
    let mut bytes = [0; 18];
    bytes[..16].copy_from_slice(&ip.octets());
    bytes[16..].copy_from_slice(&addr.port().to_be_bytes());
    bytes
}

/// A registered peer.
struct Destination {
    id: u64,
    tx: mpsc::Sender<Vec<MetaAddr>>,
    /// Addresses waiting for the next trickle.
    queue: Vec<MetaAddr>,
    /// Addresses the peer announced or was sent, oldest first.
    known: IndexSet<PeerSocketAddr>,
    /// When `queue` is sent; `None` while it is empty.
    flush_at: Option<Instant>,
    /// When this node's address is next advertised; `None` without one.
    self_advert_at: Option<Instant>,
}

impl Destination {
    /// Remembers that the peer knows `addr`, returning whether it was new.
    fn learn(&mut self, addr: PeerSocketAddr) -> bool {
        if !self.known.insert(addr) {
            return false;
        }
        if self.known.len() > MAX_KNOWN_ADDRS {
            // Shifting keeps the order, so the oldest is always first.
            self.known.shift_remove_index(0);
        }
        true
    }

    /// The next time this destination needs the hub.
    fn deadline(&self) -> Option<Instant> {
        match (self.flush_at, self.self_advert_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// The hub's state, owned by its task.
struct Hub {
    secret: [u8; 32],
    advertised: Option<PeerSocketAddr>,
    network: Network,
    timings: Timings,
    peers: HashMap<PeerSocketAddr, Destination>,
}

impl Hub {
    async fn run(mut self, mut receiver: mpsc::Receiver<Message>) {
        loop {
            let deadline = self.peers.values().filter_map(Destination::deadline).min();
            tokio::select! {
                message = receiver.recv() => match message {
                    Some(message) => self.handle(message, Instant::now()),
                    // Every handle was dropped.
                    None => return,
                },
                () = sleep_until_some(deadline) => self.fire(Instant::now()),
            }
        }
    }

    fn handle(&mut self, message: Message, now: Instant) {
        match message {
            Message::Register { peer, id, tx } => {
                // A connection task that was aborted, or whose deregistration was dropped,
                // never deregisters, so this keeps the map to live connections and this one.
                self.peers.retain(|_, dest| !dest.tx.is_closed());
                if self.peers.contains_key(&peer) {
                    // The peer set keeps the older of two connections to one address too.
                    return;
                }
                let self_advert_at = self
                    .advertised
                    .map(|_| now + random_up_to(self.timings.first_self_advert_max_delay));
                self.peers.insert(
                    peer,
                    Destination {
                        id,
                        tx,
                        queue: Vec::new(),
                        known: IndexSet::new(),
                        flush_at: None,
                        self_advert_at,
                    },
                );
            }
            Message::Deregister { peer, id } => {
                if self.peers.get(&peer).is_some_and(|dest| dest.id == id) {
                    self.peers.remove(&peer);
                }
            }
            Message::Offer { source, addrs } => self.offer(source, addrs, now),
        }
    }

    /// Queues each fresh full-node address for [`RELAY_FANOUT`] peers other
    /// than `source`.
    fn offer(&mut self, source: PeerSocketAddr, addrs: Vec<MetaAddr>, now: Instant) {
        // The source knows what it announced, whether or not it is relayed.
        if let Some(dest) = self.peers.get_mut(&source) {
            for addr in &addrs {
                dest.learn(addr.addr());
            }
        }

        let wall_now = DateTime32::now();
        let oldest = wall_now.saturating_sub(MAX_RELAY_AGE);
        let day = unix_day(wall_now);
        for addr in addrs {
            let Some(time) = addr.last_seen() else {
                continue;
            };
            if clamp_relayed_time(time, wall_now) < oldest
                || Some(addr.addr()) == self.advertised
                // The address book doesn't keep records that don't serve the chain.
                || !addr
                    .services()
                    .is_some_and(|services| services.contains(PeerServices::NODE_NETWORK))
                || address_is_valid_for_outbound_connections(addr.addr(), self.network.clone())
                    .is_err()
            {
                continue;
            }
            let candidates = self
                .peers
                .iter()
                .filter(|(peer, dest)| **peer != source && !dest.tx.is_closed())
                .map(|(peer, _)| *peer);
            for target in relay_targets(&self.secret, addr.addr(), day, candidates) {
                let trickle_mean = self.timings.trickle_mean;
                let dest = self
                    .peers
                    .get_mut(&target)
                    .expect("targets are chosen from the registered peers");
                // A full queue drops the address before it is learned, so it can be sent later.
                if dest.queue.len() >= MAX_QUEUED_ADDRS || !dest.learn(addr.addr()) {
                    continue;
                }
                dest.queue.push(addr.clone());
                dest.flush_at
                    .get_or_insert_with(|| now + exponential_delay(trickle_mean));
            }
        }
    }

    /// Sends what is due at `now`.
    fn fire(&mut self, now: Instant) {
        let trickle_mean = self.timings.trickle_mean;
        let mut closed = Vec::new();
        for (peer, dest) in &mut self.peers {
            if dest.flush_at.is_some_and(|at| at <= now) {
                let count = dest.queue.len().min(MAX_RELAY_BATCH);
                let addrs: Vec<MetaAddr> = dest.queue.drain(..count).collect();
                dest.flush_at =
                    (!dest.queue.is_empty()).then(|| now + exponential_delay(trickle_mean));
                match dest.tx.try_send(addrs) {
                    Ok(()) => {
                        metrics::counter!("zcash.net.v2.addresses.relayed", "kind" => "relay")
                            // A queue holds at most `MAX_QUEUED_ADDRS`, which fits in a `u64`.
                            .increment(count as u64);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!(%peer, count, "peer is behind, dropping relayed addresses");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => closed.push(*peer),
                }
            }

            if let (Some(at), Some(advertised)) = (dest.self_advert_at, self.advertised) {
                if at <= now {
                    let jitter = random_up_to(self.timings.self_advert_jitter * 2);
                    dest.self_advert_at = Some(
                        now + self.timings.self_advert_interval + jitter
                            - self.timings.self_advert_jitter,
                    );
                    let ours = MetaAddr::new_gossiped_meta_addr(
                        advertised,
                        PeerServices::NODE_NETWORK,
                        DateTime32::now(),
                    );
                    match dest.tx.try_send(vec![ours]) {
                        Ok(()) => {
                            metrics::counter!("zcash.net.v2.addresses.relayed", "kind" => "self")
                                .increment(1);
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::debug!(%peer, "peer is behind, dropping self-advertisement");
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => closed.push(*peer),
                    }
                }
            }
        }
        for peer in closed {
            self.peers.remove(&peer);
        }
    }
}

/// Sleeps until `deadline`, or forever without one.
async fn sleep_until_some(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// A uniformly random duration from zero to `max`.
fn random_up_to(max: Duration) -> Duration {
    max.mul_f64(rand::rng().random::<f64>())
}

/// An exponentially distributed delay with mean `mean`, capped at ten times
/// the mean so one unlucky draw cannot hold a queue back for long.
fn exponential_delay(mean: Duration) -> Duration {
    let uniform: f64 = rand::rng().random::<f64>();
    // `uniform` is in [0, 1), so the logarithm is finite and at most zero.
    let factor = -(1.0 - uniform).ln();
    mean.mul_f64(factor.min(10.0))
}
