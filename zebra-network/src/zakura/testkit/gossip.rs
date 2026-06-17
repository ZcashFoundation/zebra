//! Lightweight in-process gossip mesh for multi-node Zakura tests.
//!
//! This is deliberately a *toy* flood-sub protocol layered directly on the
//! relay-free loopback Iroh endpoints from [`LocalEndpointFactory`]. It reuses
//! the production [`Frame`] wire type, but not the production handler or
//! supervisor, so it can prove multi-node mesh formation and message
//! propagation cheaply and deterministically while the real P2P-v2 gossip path
//! does not yet exist.
//!
//! Each node runs a small accept loop and keeps every connection it dials or
//! accepts. When a node first sees a message it records it and forwards it to
//! every other connection (flood-sub); a per-node seen-set makes the flood
//! terminate. Because both ends of a single QUIC connection run the same read
//! loop and can open streams, one connection per pair is enough for
//! bidirectional gossip — no mutual dials, so the test never depends on
//! connection de-duplication.

use std::{collections::HashSet, sync::Arc};

use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
    NodeAddr,
};
use tokio::sync::Mutex;

use super::{InboundRecorder, LocalEndpointFactory};
use crate::{
    zakura::{Frame, ZakuraPeerId},
    BoxError,
};

/// ALPN for the testkit gossip protocol.
const GOSSIP_ALPN: &[u8] = b"/zakura/testkit/gossip/0";
/// Frame cap for testkit gossip messages.
const GOSSIP_MAX_FRAME: u32 = 64 * 1024;
/// Stream kind recorded for received gossip frames.
const GOSSIP_STREAM_KIND: u16 = 2;

/// Shared gossip state for one node.
#[derive(Debug)]
struct GossipCore {
    recorder: InboundRecorder,
    seen: Mutex<HashSet<Vec<u8>>>,
    conns: Mutex<Vec<Connection>>,
}

impl GossipCore {
    fn new(recorder: InboundRecorder) -> Self {
        Self {
            recorder,
            seen: Mutex::new(HashSet::new()),
            conns: Mutex::new(Vec::new()),
        }
    }

    /// Keep a connection so this node can both receive on it and flood to it.
    async fn register_conn(&self, conn: Connection) {
        self.conns.lock().await.push(conn);
    }

    /// Record a payload as seen; returns true if it was new to this node.
    async fn mark_new(&self, payload: &[u8]) -> bool {
        self.seen.lock().await.insert(payload.to_vec())
    }

    /// Observe a frame in this node's bounded recorder.
    fn record(&self, frame: &Frame) {
        let _ = self.recorder.deliver(
            ZakuraPeerId::new(vec![0; 32]).expect("test peer id is within bounds"),
            GOSSIP_STREAM_KIND,
            frame.clone(),
        );
    }

    /// Forward `frame` to every connection this node holds.
    async fn broadcast(&self, frame: &Frame) {
        let Ok(encoded) = frame.encode(GOSSIP_MAX_FRAME) else {
            return;
        };
        let conns = self.conns.lock().await.clone();
        for conn in conns {
            if let Ok((mut send, _recv)) = conn.open_bi().await {
                let _ = send.write_all(&encoded).await;
                let _ = send.finish();
            }
        }
    }

    /// Handle a freshly received frame: record and flood it exactly once.
    async fn on_received(self: &Arc<Self>, frame: Frame) {
        if !self.mark_new(&frame.payload).await {
            return;
        }
        self.record(&frame);
        self.broadcast(&frame).await;
    }

    /// Read inbound streams on `conn` until it closes.
    async fn serve(self: Arc<Self>, conn: Connection) {
        while let Ok((_send, mut recv)) = conn.accept_bi().await {
            let core = self.clone();
            tokio::spawn(async move {
                if let Ok(bytes) = recv.read_to_end(GOSSIP_MAX_FRAME as usize).await {
                    if let Ok(frame) = Frame::decode(&bytes, GOSSIP_MAX_FRAME) {
                        core.on_received(frame).await;
                    }
                }
            });
        }
    }
}

/// Test-only gossip protocol handler that records and floods inbound frames.
#[derive(Clone, Debug)]
struct GossipHandler {
    core: Arc<GossipCore>,
}

impl ProtocolHandler for GossipHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.core.register_conn(connection.clone()).await;
        self.core.clone().serve(connection).await;
        Ok(())
    }
}

/// A lightweight in-process gossip node over a relay-free loopback endpoint.
#[derive(Debug)]
pub struct GossipNode {
    router: Router,
    core: Arc<GossipCore>,
}

impl GossipNode {
    /// Spawn a gossip node with the deterministic identity derived from `seed`.
    pub async fn spawn(seed: u64) -> Result<Self, BoxError> {
        let endpoint = LocalEndpointFactory::new().endpoint(seed).await?;
        let core = Arc::new(GossipCore::new(InboundRecorder::new(1024)));
        let router = Router::builder(endpoint)
            .accept(GOSSIP_ALPN, GossipHandler { core: core.clone() })
            .spawn();
        Ok(Self { router, core })
    }

    /// Current Iroh node address.
    pub async fn node_addr(&self) -> NodeAddr {
        LocalEndpointFactory::node_addr(self.router.endpoint()).await
    }

    /// Bounded inbound recorder of received gossip frames.
    pub fn recorder(&self) -> InboundRecorder {
        self.core.recorder.clone()
    }

    /// Dial `peer` and keep the connection for sending and receiving gossip.
    pub async fn connect(&self, peer: &GossipNode) -> Result<(), BoxError> {
        let peer_addr = peer.node_addr().await;
        let endpoint = self.router.endpoint();
        endpoint.add_node_addr(peer_addr.clone())?;
        let conn = endpoint.connect(peer_addr, GOSSIP_ALPN).await?;
        self.core.register_conn(conn.clone()).await;
        let core = self.core.clone();
        tokio::spawn(async move { core.serve(conn).await });
        Ok(())
    }

    /// Originate a gossip message: record it locally and flood it to peers.
    pub async fn broadcast(&self, payload: Vec<u8>) -> Result<(), BoxError> {
        let frame = Frame {
            message_type: 1,
            flags: 0,
            payload: payload.clone(),
        };
        // Mark our own message seen so echoes are ignored, and record locally so
        // the origin also ends with the payload.
        self.core.mark_new(&payload).await;
        self.core.record(&frame);
        self.core.broadcast(&frame).await;
        Ok(())
    }

    /// Shut the node down.
    pub async fn shutdown(&self) {
        let _ = self.router.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::await_until;
    use super::*;

    #[tokio::test]
    async fn gossip_floods_to_every_node_across_a_line() -> Result<(), BoxError> {
        let _guard = zebra_test::init();

        // Spawn a 5-node line: 1 - 2 - 3 - 4 - 5. Only adjacent nodes are
        // directly connected, so reaching the far end *requires* flood-sub
        // forwarding through the intermediate nodes.
        let mut nodes = Vec::new();
        for seed in 1..=5u64 {
            nodes.push(GossipNode::spawn(seed).await?);
        }
        for pair in nodes.windows(2) {
            pair[0].connect(&pair[1]).await?;
        }

        // Let every accept side register its inbound connection before flooding.
        await_until(
            "every node wired into the line",
            Duration::from_secs(5),
            || {
                // Endpoints 0 and 4 have one neighbour; the rest have two.
                nodes.iter().enumerate().all(|(index, _)| {
                    let expected = if index == 0 || index == nodes.len() - 1 {
                        1
                    } else {
                        2
                    };
                    conn_count(&nodes[index]) >= expected
                })
            },
        )
        .await?;

        let payload = b"flood-sub-hello".to_vec();
        nodes[0].broadcast(payload.clone()).await?;

        for (index, node) in nodes.iter().enumerate() {
            let recorder = node.recorder();
            let payload = payload.clone();
            await_until(
                format!("gossip reaches node {index}"),
                Duration::from_secs(5),
                || recorder.contains_payload(GOSSIP_STREAM_KIND, &payload),
            )
            .await?;
        }

        for node in &nodes {
            node.shutdown().await;
        }
        Ok(())
    }

    fn conn_count(node: &GossipNode) -> usize {
        node.core
            .conns
            .try_lock()
            .map(|conns| conns.len())
            .unwrap_or(0)
    }
}
