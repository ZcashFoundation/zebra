//! The version 2 address segment: the addresses of version 2 (QUIC) peers.
//!
//! Version 2 address records name QUIC endpoints (draft, "Network IDs"), so they are kept apart
//! from the legacy address book, in a second, plain [`AddressBook`](crate::AddressBook) that one
//! actor task owns outright. Its entries come from:
//! - the configured `initial_v2_peers`, the DNS seeders on the default port, and the version 2
//!   peer cache on disk, which are *trusted*: never in the gossip buckets, so relay cannot evict
//!   them. The cache only holds peers this node connected to,
//! - the records of version 2 `get-addr` answers and address announcements, which are *relayed*:
//!   admitted through the secret-keyed [`buckets`], by address and source group, and
//! - this node's own outbound version 2 connections, which make an entry *tried*.
//!
//! Inbound version 2 peers are not recorded: their remote port may not be a listener.
//!
//! Connections, the dialer, and the cache task talk to the actor through a cloneable
//! [`V2BookHandle`] over one bounded channel, with oneshot replies, and the dialer watches its
//! [`V2BookStats`]. Nothing here is shared behind a lock.

use std::{collections::HashSet, net::SocketAddr};

use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tracing::Instrument;

use zebra_chain::parameters::Network;

use crate::{
    address_book_updater::MIN_CHANNEL_SIZE,
    meta_addr::{MetaAddr, MetaAddrChange},
    BanList, Config, PeerSocketAddr,
};

// The version 2 connections and dialer use the segment from the next commit.
#[allow(dead_code)]
mod actor;
#[allow(dead_code)]
mod buckets;
#[allow(dead_code)]
mod cache;
#[allow(dead_code)]
mod intake;

#[cfg(test)]
mod tests;

pub(crate) use buckets::Group;
#[allow(unused_imports)]
pub(crate) use cache::{load_v2_peer_cache, v2_peer_cache_updater};

/// How long a failed entry stays out of reach of a new seed: a seed for an address the segment
/// already has is skipped, unless the entry failed this long ago.
///
/// Re-seeding a failed entry makes it eligible for another attempt
/// (`MetaAddrChange::apply_to_meta_addr` treats it as an initial peer retry), so without this,
/// every DNS reseed would re-probe every seed result that does not accept QUIC.
pub(crate) const V2_DNS_RETRY_FAILED_AFTER: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// Where a trusted entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Provenance {
    /// The operator configured it in `initial_v2_peers`.
    Configured,

    /// A DNS seeder returned it, and its port was set to the network's default port.
    DnsSeed,

    /// It was in the version 2 peer cache on disk.
    Cache,
}

/// How a version 2 peer relayed addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Via {
    /// In its answer to this node's `get-addr` request.
    GetAddr,

    /// On its address announcement stream.
    Announcement,
}

impl Via {
    /// The metric label for this way of relaying addresses.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Via::GetAddr => "get_addr",
            Via::Announcement => "announcement",
        }
    }
}

/// The sizes of the segment's populations, published after every change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct V2BookStats {
    /// Entries this node attempted or connected to, that are not trusted.
    pub tried: usize,

    /// Relayed entries that were never attempted.
    pub relayed: usize,

    /// Configured, DNS seed, and cached entries.
    pub trusted: usize,

    /// Entries that have not failed and are not banned: candidates and live connections.
    pub dialable: usize,
}

/// A message to the segment actor.
#[derive(Debug)]
pub(crate) enum Message {
    /// A connection status change of an outbound version 2 connection.
    Change(MetaAddrChange),

    /// Address records a version 2 peer relayed.
    Intake {
        /// The peer that relayed them.
        source: PeerSocketAddr,
        /// The records, as decoded.
        addrs: Vec<MetaAddr>,
        /// How the peer relayed them.
        via: Via,
    },

    /// Trusted addresses.
    Seed {
        /// The addresses.
        addrs: Vec<PeerSocketAddr>,
        /// Where they came from.
        provenance: Provenance,
    },

    /// Asks for the next discovered address to dial. The book is left as it is: the dialer
    /// sends the attempt as a [`Message::Change`] when it dials the address.
    NextCandidate {
        /// Addresses that must not be returned: configured peers and open connections.
        exclude: HashSet<PeerSocketAddr>,
        /// Groups that already have a discovered outbound connection.
        busy_groups: HashSet<Group>,
        /// Whether the endpoint can only reach IPv4 addresses.
        ipv4_only: bool,
        /// Where the candidate goes.
        reply: oneshot::Sender<Option<MetaAddr>>,
    },

    /// Asks for the addresses to answer a version 2 `get-addr` request with.
    GetAddrSample {
        /// Where the addresses go.
        reply: oneshot::Sender<Vec<MetaAddr>>,
    },

    /// Asks for the addresses to write to the version 2 peer cache.
    Cacheable {
        /// Where the addresses go.
        reply: oneshot::Sender<Vec<MetaAddr>>,
    },
}

/// A cloneable handle to the segment actor.
#[derive(Clone, Debug)]
pub(crate) struct V2BookHandle {
    tx: mpsc::Sender<Message>,
}

impl V2BookHandle {
    /// Returns a handle that sends to `tx`. Tests use it to see the messages a component sends.
    pub(crate) fn new(tx: mpsc::Sender<Message>) -> Self {
        V2BookHandle { tx }
    }

    /// Sends a connection status change, waiting for room: changes are never dropped, so the
    /// segment's connection records stay accurate. Does nothing if the actor has stopped.
    pub(crate) async fn change(&self, change: MetaAddrChange) {
        let _ = self.tx.send(Message::Change(change)).await;
    }

    /// Hands relayed records to the actor without waiting: when its queue is full they are
    /// dropped and counted, so a peer cannot slow its connection task by relaying.
    pub(crate) fn intake(&self, source: PeerSocketAddr, addrs: Vec<MetaAddr>, via: Via) {
        if addrs.is_empty() {
            return;
        }
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.tx.try_send(Message::Intake { source, addrs, via })
        {
            metrics::counter!("zcash.net.v2.addresses.intake.dropped").increment(1);
        }
    }

    /// Sends trusted addresses, waiting for room.
    pub(crate) async fn seed(&self, addrs: Vec<PeerSocketAddr>, provenance: Provenance) {
        if addrs.is_empty() {
            return;
        }
        let _ = self.tx.send(Message::Seed { addrs, provenance }).await;
    }

    /// Returns the next discovered address to dial, or `None` if there is none, or the actor has
    /// stopped. The address is not marked as attempted: the caller sends that change when it
    /// dials it.
    pub(crate) async fn next_candidate(
        &self,
        exclude: HashSet<PeerSocketAddr>,
        busy_groups: HashSet<Group>,
        ipv4_only: bool,
    ) -> Option<MetaAddr> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(Message::NextCandidate {
                exclude,
                busy_groups,
                ipv4_only,
                reply,
            })
            .await
            .ok()?;
        reply_rx.await.ok().flatten()
    }

    /// Returns the addresses to answer a version 2 `get-addr` request with, or none if the actor
    /// has stopped.
    pub(crate) async fn get_addr_sample(&self) -> Vec<MetaAddr> {
        let (reply, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(Message::GetAddrSample { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Returns the addresses to write to the version 2 peer cache, or none if the actor has
    /// stopped.
    pub(crate) async fn cacheable(&self) -> Vec<MetaAddr> {
        let (reply, reply_rx) = oneshot::channel();
        if self.tx.send(Message::Cacheable { reply }).await.is_err() {
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }
}

/// Spawns the segment actor for `config` on `network`, advertising `local_listener` as this
/// node's own version 2 address when it is valid for outbound connections, and removing the
/// entries that `bans_receiver` bans.
///
/// Returns its handle, the watch of its statistics, and its task, which ends when every handle
/// is dropped.
pub(crate) fn spawn(
    config: &Config,
    network: &Network,
    local_listener: SocketAddr,
    bans_receiver: watch::Receiver<BanList>,
) -> (V2BookHandle, watch::Receiver<V2BookStats>, JoinHandle<()>) {
    let channel_size = config
        .peerset_total_connection_limit()
        .saturating_add(MIN_CHANNEL_SIZE);
    let (tx, rx) = mpsc::channel(channel_size);
    let (stats_tx, stats_rx) = watch::channel(V2BookStats::default());

    let book = actor::V2Book::new(
        network,
        local_listener,
        config.max_connections_per_ip,
        bans_receiver.borrow().clone(),
        stats_tx,
    );
    let task = tokio::spawn(actor::run(book, rx, bans_receiver).in_current_span());

    (V2BookHandle::new(tx), stats_rx, task)
}
