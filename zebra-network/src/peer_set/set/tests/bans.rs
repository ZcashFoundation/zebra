//! Ban publication must take effect before ordinary requests start their wire writes.

use std::time::Duration;

use futures::{channel::mpsc, stream, FutureExt, Sink, SinkExt, StreamExt};
use tokio::{sync::watch, time::timeout};
use tower::{discover::Change, Service, ServiceExt};
use zebra_chain::{parameters::Network, serialization::SerializationError};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, ConnectedAddr, Connection, MinimumPeerVersion},
    peer_set::ActiveConnectionCounter,
    protocol::external::Message,
    BanList, BoxError, PeerSocketAddr, Request, Response, SharedPeerError,
};

use super::PeerSetBuilder;

/// Exercise the real peer set, client, connection loop, and bounded outbound sink.
async fn reject_banned_request(delayed_sink: bool) {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(1), async {
        let addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        let (mut client, _harness) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_connected_addr(ConnectedAddr::new_outbound_direct(addr))
            .finish();
        let (client_tx, client_rx) = mpsc::channel(1);
        client.server_tx = client_tx;
        let (_advert_tx, advert_rx) = tokio::sync::mpsc::channel(1);
        let (bans_tx, bans_rx) = watch::channel(BanList::default());
        let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
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
            // Publish after peer-set readiness, before dispatch, without another readiness poll.
            bans_tx.send_modify(|bans| bans.ban(addr.ip()));
        }
        let response = peers.call(Request::Peers);
        if delayed_sink {
            // Dispatch while eligible, then ban while the real connection waits for sink readiness.
            assert!(connection.as_mut().now_or_never().is_none());
            bans_tx.send_modify(|bans| bans.ban(addr.ip()));
            assert_eq!(outbound_rx.next().await, Some(earlier_message));
        }

        let completed = connection.as_mut().now_or_never();
        assert_eq!(
            outbound_rx.next().now_or_never(),
            Some(None),
            "a newly banned peer must receive no ordinary request message",
        );
        assert_eq!(completed, Some(()), "the banned connection must shut down");
        let error = response.await.expect_err("the banned request must fail");
        assert_eq!(
            error
                .downcast_ref::<SharedPeerError>()
                .unwrap()
                .inner_debug(),
            "ConnectionDropped",
        );
    })
    .await
    .expect("ban rejection must not wait for the wire response timeout");
}

#[tokio::test(start_paused = true)]
async fn ban_after_peer_set_readiness_rejects_request_before_write() {
    reject_banned_request(false).await;
}

#[tokio::test(start_paused = true)]
async fn ban_during_sink_readiness_rejects_request_before_write() {
    reject_banned_request(true).await;
}
