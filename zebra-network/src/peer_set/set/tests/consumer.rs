//! Real buffered peer-set and client-channel fixtures for downstream consumer tests.

use std::{net::SocketAddr, time::Duration};

use futures::{stream, StreamExt};
use tower::{discover::Change, ServiceExt};
use zebra_chain::{block, chain_tip::mock::MockChainTip, parameters::Network};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, ConnectedAddr, MinimumPeerVersion},
    peer_set::InventoryChange,
    types::{InventoryHash, PeerServices},
    BoxError, NetworkService, PeerSocketAddr, Request, Response,
};

pub use crate::peer::ReceivedClientRequest;

use super::{PeerSetBuilder, PeerSetGuard};

/// Owns a buffered peer set and the remote endpoints of its real client channels.
pub struct PeerSetHarness {
    /// The service to pass to the production sync or inbound constructor.
    pub network: NetworkService,
    /// The connected addresses, in the same order as the supplied service flags.
    pub addrs: Vec<PeerSocketAddr>,
    peers: Vec<ClientTestHarness>,
    guard: PeerSetGuard,
    busy_requests: Vec<super::super::ResponseFuture>,
}

impl PeerSetHarness {
    /// Constructs real clients, optionally filling one client's two request-channel slots.
    ///
    /// Inventory uses the existing one-entry test channel, so two updates without a poll
    /// deterministically exercise registry lag. No socket or wall-clock timing is involved.
    pub async fn new(services: &[PeerServices], busy_peer: Option<usize>) -> Self {
        let mut guard = PeerSetGuard::new();
        let inventory_receiver = guard.create_inventory_receiver();
        let inventory_sender = guard.inventory_sender().as_ref().unwrap().clone();
        let mut addrs = Vec::new();
        let mut peers = Vec::new();
        let mut changes: Vec<Result<_, BoxError>> = Vec::new();
        for (index, services) in services.iter().enumerate() {
            let host = u8::try_from(index + 1).expect("test peer count fits in an IPv4 octet");
            let addr: PeerSocketAddr = SocketAddr::from(([127, 0, 0, host], 8233)).into();
            let (mut client, handle) = ClientTestHarness::build()
                .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
                .with_services(*services)
                .with_connected_addr(ConnectedAddr::new_inbound_direct(addr))
                .finish();
            client.inv_collector = inventory_sender.clone();
            changes.push(Ok(Change::Insert(addr, client.into())));
            addrs.push(addr);
            peers.push(handle);
        }
        let (tip, _tip_sender) = MockChainTip::new();
        let builder = PeerSetBuilder::new()
            .with_discover(stream::iter(changes).chain(stream::pending()))
            .with_minimum_peer_version(MinimumPeerVersion::new(tip, &Network::Mainnet));
        let (mut peer_set, mut peer_guard) = PeerSetBuilder {
            inv_stream: Some(inventory_receiver),
            ..builder
        }
        .build();
        // Keep the sender for the channel actually used by this peer set.
        *peer_guard.inventory_sender() = guard.inventory_sender().take();
        peer_set.ready().await.expect("test peers are ready");
        let mut busy_requests = Vec::new();
        if let Some(peer) = busy_peer {
            for byte in [0xF0, 0xF1] {
                peer_set.ready().await.expect("test peers remain connected");
                busy_requests.push(peer_set.call_ready_peer(
                    addrs[peer],
                    Request::BlocksByHash([block::Hash([byte; 32])].into_iter().collect()),
                ));
            }
            peer_set
                .ready()
                .await
                .expect("an idle fallback peer remains ready");
            assert!(peer_set.cancel_handles.contains_key(&addrs[peer]));
        }
        let (network, _poll_task) = peer_set.into_buffer(3);
        Self {
            network,
            addrs,
            peers,
            guard: peer_guard,
            busy_requests,
        }
    }

    /// Receives an already-dispatched client request, without advancing the clock.
    pub fn take_request(&mut self, peer: usize) -> Option<ReceivedClientRequest> {
        self.peers[peer].take_request()
    }

    /// Waits at most one virtual second for the specified client to receive a request.
    pub async fn next_request(&mut self, peer: usize) -> ReceivedClientRequest {
        self.next_request_from(&[peer]).await.1
    }

    /// Receives the next request from any of the supplied peers, with a one-second bound.
    pub async fn next_request_from(&mut self, peers: &[usize]) -> (usize, ReceivedClientRequest) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                for &peer in peers {
                    if let Some(request) = self.take_request(peer) {
                        return (peer, request);
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("a selected peer must receive the consumer request")
    }

    /// Publishes an inventory event through the real lossy registry channel.
    pub fn advertise(&mut self, peer: usize, hash: block::Hash) {
        self.guard
            .inventory_sender()
            .as_ref()
            .unwrap()
            .send(InventoryChange::new_available(
                InventoryHash::Block(hash),
                self.addrs[peer],
            ))
            .expect("the real inventory registry is still connected");
    }

    /// Polls the production peer set through its normal buffered maintenance request.
    pub async fn flush(&self) {
        self.network
            .clone()
            .oneshot(Request::PollPeerSet)
            .await
            .expect("the test peer set is healthy");
    }

    /// Frees the two pre-filled client slots and completes their unrelated requests.
    pub async fn release_busy_peer(&mut self, peer: usize) {
        for byte in [0xF0, 0xF1] {
            let request = self.next_request(peer).await;
            assert_eq!(
                request.request(),
                &Request::BlocksByHash([block::Hash([byte; 32])].into_iter().collect()),
            );
            request.respond(Ok(Response::Nil));
        }
        for response in self.busy_requests.drain(..) {
            response.await.expect("priming requests completed");
        }
    }
}
