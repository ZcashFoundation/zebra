//! The registry of connected [`SyncPeer`]s: an actor task that
//! owns the map, removes closed connections, and publishes the current set
//! through a `watch` channel. No locks.
//!
//! Every version 2 connection whose handshake completes is registered,
//! whatever dialled it (`initial_v2_peers`, inbound, or address-book
//! discovery), so peer discovery needs no change here.

use std::{fmt, sync::Arc};

use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use indexmap::IndexMap;
use tokio::sync::{mpsc, watch};

use super::{ConnectionId, SyncPeer};

/// The receiving end of the registry: the connected version 2 peers, in
/// connection order. Each update replaces the whole slice.
///
/// A peer stays listed until its connection has closed, so a listed peer
/// may be closing: callers skip those with [`SyncPeer::is_closed`].
pub type SyncPeers = watch::Receiver<Arc<[SyncPeer]>>;

/// A handle that registers connections with the registry actor.
///
/// `zebrad` spawns the registry with [`SyncPeersSender::spawn`] and passes
/// the sender to the network in the runtime-only
/// [`Config::v2_sync_peers`](crate::Config::v2_sync_peers) field. Two
/// senders are equal when they reach the same actor, so the network
/// `Config` keeps deriving equality.
#[derive(Clone)]
pub struct SyncPeersSender {
    /// Unbounded so that registering never waits or drops a connection: one
    /// message is sent per completed handshake, which the connection limits
    /// already bound, and the actor handles each at once.
    connected: mpsc::UnboundedSender<SyncPeer>,
}

impl SyncPeersSender {
    /// Spawns the registry actor, and returns the sender for the network
    /// `Config` and the receiver for the sync engine. The actor runs until
    /// every sender has been dropped.
    pub fn spawn() -> (SyncPeersSender, SyncPeers) {
        let (connected, connected_rx) = mpsc::unbounded_channel();
        let (peers_tx, peers) = watch::channel(Arc::from(Vec::new()));
        tokio::spawn(run(connected_rx, peers_tx));
        (SyncPeersSender { connected }, peers)
    }

    /// Registers a connection whose handshake just completed, without
    /// waiting. The actor removes it when `connection.closed()` resolves, or
    /// when its connection task ends.
    pub(crate) fn connected(&self, peer: SyncPeer) {
        // The actor only exits once every sender is gone, and this is one.
        let _ = self.connected.send(peer);
    }
}

/// The registry actor: owns the connected peers, and publishes the whole set
/// to `peers_tx` on every change. Exits when every [`SyncPeersSender`] has
/// been dropped.
async fn run(
    mut connected_rx: mpsc::UnboundedReceiver<SyncPeer>,
    peers_tx: watch::Sender<Arc<[SyncPeer]>>,
) {
    let mut peers: IndexMap<ConnectionId, SyncPeer> = IndexMap::new();
    let mut closing: FuturesUnordered<BoxFuture<'static, ConnectionId>> = FuturesUnordered::new();

    loop {
        tokio::select! {
            peer = connected_rx.recv() => {
                let Some(peer) = peer else { break };
                let id = peer.id();
                let watched = peer.clone();
                closing.push(async move {
                    watched.closed().await;
                    // An aborted connection task leaves its connection open while handles to it
                    // remain; close it, as dropping the last handle would.
                    watched.close_if_open();
                    id
                }.boxed());
                peers.insert(id, peer);
            }
            Some(id) = closing.next(), if !closing.is_empty() => {
                peers.shift_remove(&id);
            }
        }
        peers_tx.send_replace(peers.values().cloned().collect());
    }
}

impl fmt::Debug for SyncPeersSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncPeersSender").finish_non_exhaustive()
    }
}

impl PartialEq for SyncPeersSender {
    fn eq(&self, other: &Self) -> bool {
        self.connected.same_channel(&other.connected)
    }
}

impl Eq for SyncPeersSender {}
