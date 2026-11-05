//! Bans and minimum-version upgrades published before an ordinary request's authorization check,
//! which runs once the outbound sink is ready, must stop its wire write.

use std::time::Duration;

use futures::{channel::mpsc, stream, FutureExt, Sink, SinkExt, StreamExt};
use tokio::{sync::watch, time::timeout};
use tower::{discover::Change, Service, ServiceExt};
use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    serialization::SerializationError,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    peer::{ClientTestHarness, ConnectedAddr, Connection, MinimumPeerVersion},
    peer_set::ActiveConnectionCounter,
    protocol::external::{types::Version, Message},
    BanList, BoxError, PeerSocketAddr, Request, Response, SharedPeerError,
};

use super::PeerSetBuilder;

/// Exercise the real peer set, client, connection loop, and bounded outbound sink.
///
/// Revokes the peer with a ban, or with an upgrade that raises the minimum peer version above it.
async fn reject_revoked_request(upgrade: bool, delayed_sink: bool) {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(1), async {
        let network = Network::Mainnet;
        let addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        let (mut client, _harness) = ClientTestHarness::build()
            .with_version(Version::min_specified_for_upgrade(
                &network,
                NetworkUpgrade::Nu6_2,
            ))
            .with_connected_addr(ConnectedAddr::new_outbound_direct(addr))
            .finish();
        let (client_tx, client_rx) = mpsc::channel(1);
        client.server_tx = client_tx;
        let (_advert_tx, advert_rx) = tokio::sync::mpsc::channel(1);
        let (bans_tx, bans_rx) = watch::channel(BanList::default());
        let (minimum, tip) = MinimumPeerVersion::with_mock_chain_tip(&network);
        tip.send_best_tip_height(NetworkUpgrade::Nu6_2.activation_height(&network));
        let revoke = || {
            if upgrade {
                tip.send_best_tip_height(NetworkUpgrade::Nu6_3.activation_height(&network));
            } else {
                bans_tx.send_modify(|bans| bans.ban(addr.ip()));
            }
        };
        let (mut outbound_tx, mut outbound_rx) = mpsc::channel(0);
        let earlier_message = Message::Pong(crate::types::Nonce::default());
        if delayed_sink {
            // Fill the sender's implicit slot without flushing it. The request must wait for room.
            outbound_tx.feed(earlier_message.clone()).await.unwrap();
            assert!(futures::future::poll_fn(|cx| {
                std::pin::Pin::new(&mut outbound_tx).poll_ready(cx)
            })
            .now_or_never()
            .is_none());
        }
        let inbound: MockService<Request, Response, PanicAssertion> = MockService::build().finish();
        let connection = Connection::new(
            inbound,
            (client_rx, advert_rx, minimum.clone(), bans_rx.clone()),
            client.error_slot.clone(),
            outbound_tx.sink_map_err(|_| {
                SerializationError::from(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "test peer message receiver closed",
                ))
            }),
            ActiveConnectionCounter::new_counter().track_connection(),
            client.connection_info.clone(),
            Vec::new(),
        );
        let connection = connection.run(stream::pending());
        tokio::pin!(connection);
        let discovery = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
            .chain(stream::pending());
        let (mut peers, _guard) = PeerSetBuilder::new()
            .with_discover(discovery)
            .with_minimum_peer_version(minimum)
            .build();
        peers.bans_receiver = bans_rx;
        peers.ready().await.unwrap();

        if !delayed_sink {
            // Revoke after peer-set readiness, before dispatch, without another readiness poll.
            revoke();
        }
        let response = peers.call(Request::Peers);
        if delayed_sink {
            // Dispatch while eligible, then revoke while the connection waits for sink readiness.
            assert!(connection.as_mut().now_or_never().is_none());
            revoke();
            assert_eq!(outbound_rx.next().await, Some(earlier_message));
        }

        let completed = connection.as_mut().now_or_never();
        assert_eq!(
            outbound_rx.next().now_or_never(),
            Some(None),
            "a revoked peer must receive no ordinary request message",
        );
        assert_eq!(completed, Some(()), "the revoked connection must shut down");
        let error = response.await.expect_err("the revoked request must fail");
        assert_eq!(
            error
                .downcast_ref::<SharedPeerError>()
                .unwrap()
                .inner_debug(),
            "ConnectionDropped",
        );
    })
    .await
    .expect("revocation must not wait for the wire response timeout");
}

#[tokio::test(start_paused = true)]
async fn ban_after_peer_set_readiness_rejects_request_before_write() {
    reject_revoked_request(false, false).await;
}

#[tokio::test(start_paused = true)]
async fn ban_during_sink_readiness_rejects_request_before_write() {
    reject_revoked_request(false, true).await;
}

#[tokio::test(start_paused = true)]
async fn upgrade_during_sink_readiness_rejects_obsolete_request_before_write() {
    reject_revoked_request(true, true).await;
}
