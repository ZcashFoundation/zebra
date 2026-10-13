//! Fixed test vectors for peer connections.
//!
//! TODO: add tests for:
//!   - inbound message as request
//!   - inbound message, but not a request (or a response)

use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::Ordering, Arc, Mutex},
    task::Poll,
    time::{Duration, Instant},
};

use futures::{
    channel::{mpsc, oneshot},
    sink::SinkMapErr,
    FutureExt, SinkExt, StreamExt,
};
use metrics::{
    atomics::AtomicU64, Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString,
    Unit,
};
use tower::load_shed::error::Overloaded;
use tracing::Span;

use zebra_chain::{
    block::{self, Block},
    serialization::{SerializationError, ZcashDeserializeInto},
    transaction::UnminedTx,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    constants::{MAX_OVERLOAD_DROP_PROBABILITY, MIN_OVERLOAD_DROP_PROBABILITY, REQUEST_TIMEOUT},
    peer::{
        connection::{overload_drop_connection_probability, Connection, Handler, State},
        ClientRequest, ErrorSlot,
    },
    protocol::{
        external::{InventoryHash, Message},
        internal::InventoryResponse,
    },
    types::Nonce,
    PeerError, Request, Response,
};

#[derive(Default)]
struct ConnectionMetricsRecorder {
    counters: Mutex<HashMap<Key, Arc<AtomicU64>>>,
    gauges: Mutex<HashMap<String, Arc<AtomicU64>>>,
}

impl ConnectionMetricsRecorder {
    fn counter(&self, key: Key) -> u64 {
        self.counters
            .lock()
            .expect("test recorder mutex is unpoisoned")
            .get(&key)
            .map_or(0, |value| value.load(Ordering::Relaxed))
    }

    fn state(&self, command: &str) -> f64 {
        let gauges = self
            .gauges
            .lock()
            .expect("test recorder mutex is unpoisoned");
        f64::from_bits(
            gauges
                .get(command)
                .expect("the connection recorded this state")
                .load(Ordering::Relaxed),
        )
    }
}

impl Recorder for ConnectionMetricsRecorder {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        Counter::from_arc(
            self.counters
                .lock()
                .expect("test recorder mutex is unpoisoned")
                .entry(key.clone())
                .or_default()
                .clone(),
        )
    }

    fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        if key.name() != "zebra.net.connection.state" {
            return Gauge::noop();
        }

        let mut labels = key.labels();
        let command = labels.next().expect("connection state has a command label");
        assert_eq!(command.key(), "command");
        assert!(
            labels.next().is_none(),
            "state must not have per-peer labels"
        );
        let mut gauges = self
            .gauges
            .lock()
            .expect("test recorder mutex is unpoisoned");
        Gauge::from_arc(
            gauges
                .entry(command.value().to_owned())
                .or_default()
                .clone(),
        )
    }

    fn register_histogram(&self, _key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

/// Aggregate gauges count concurrent peers and return to zero after transitions and disconnects.
#[tokio::test]
async fn connection_state_metrics_return_to_zero() {
    let _init_guard = zebra_test::init();
    let recorder = ConnectionMetricsRecorder::default();
    let mut series_count = None;

    metrics::with_local_recorder(&recorder, || {
        for cycle in 0..8 {
            let (first, mut client_tx, _service, mut outbound_messages, _error_slot) =
                new_test_connection();
            let (second, _idle_client_tx, _idle_service, _idle_messages, _idle_error_slot) =
                new_test_connection();
            let (mut peer_tx, peer_rx) = mpsc::channel(1);
            let (mut idle_peer_tx, idle_peer_rx) = mpsc::channel(1);
            let first = first.run(peer_rx).shared();
            let second = second.run(idle_peer_rx).shared();

            assert_eq!(first.clone().now_or_never(), None);
            assert_eq!(second.clone().now_or_never(), None);
            assert_eq!(recorder.state("AwaitingRequest"), 2.0);

            // Polling an unchanged state must not count the same connection twice.
            assert_eq!(first.clone().now_or_never(), None);
            assert_eq!(recorder.state("AwaitingRequest"), 2.0);

            let (response_tx, mut response_rx) = oneshot::channel();
            client_tx
                .try_send(ClientRequest {
                    request: Request::Peers,
                    tx: response_tx,
                    inv_collector: None,
                    transient_addr: None,
                    span: Span::none(),
                })
                .expect("the connection is listening for client requests");
            assert_eq!(first.clone().now_or_never(), None);
            assert_eq!(
                outbound_messages.try_recv().expect("the request was sent"),
                Message::GetAddr,
            );
            assert_eq!(first.clone().now_or_never(), None);
            assert_eq!(recorder.state("AwaitingRequest"), 1.0);
            assert_eq!(recorder.state("AwaitingResponse(Peers)"), 1.0);

            peer_tx
                .try_send(Ok(Message::Addr(Vec::new())))
                .expect("the connection is listening for peer messages");
            assert_eq!(first.clone().now_or_never(), None);
            assert_eq!(
                response_rx
                    .try_recv()
                    .expect("the response channel is open")
                    .expect("the response was sent")
                    .expect("the peer returned a valid response"),
                Response::Peers(Vec::new()),
            );
            assert_eq!(recorder.state("AwaitingRequest"), 2.0);
            assert_eq!(recorder.state("AwaitingResponse(Peers)"), 0.0);

            // Exercise both normal disconnects and cancellation of a live connection.
            if cycle % 2 == 0 {
                peer_tx.close_channel();
                assert_eq!(first.clone().now_or_never(), Some(()));
            }
            drop(first);
            assert_eq!(recorder.state("AwaitingRequest"), 1.0);

            // Distinct transport error messages must not create distinct state series.
            idle_peer_tx
                .try_send(Err(
                    std::io::Error::other(format!("peer failure {cycle}")).into()
                ))
                .expect("the idle connection is listening for peer messages");
            assert_eq!(second.clone().now_or_never(), Some(()));
            drop(second);
            drop(new_test_connection().0);

            let gauges = recorder
                .gauges
                .lock()
                .expect("test recorder mutex is unpoisoned");
            assert!(gauges
                .values()
                .all(|value| f64::from_bits(value.load(Ordering::Relaxed)) == 0.0));
            assert_eq!(*series_count.get_or_insert(gauges.len()), gauges.len());
        }
    });
}

#[test]
fn connection_state_commands_omit_serialization_details() {
    for detail in ["first peer failure", "second peer failure"] {
        let handler = Handler::Finished(Err(PeerError::Serialization(
            std::io::Error::other(detail).into(),
        )));
        assert_eq!(handler.command(), "Finished(Serialization)");
        assert!(
            handler.to_string().contains(detail),
            "logs retain error details",
        );
    }
}

/// Test that the connection run loop works as a future
#[tokio::test]
async fn connection_run_loop_ok() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // The run loop will wait forever for a request from Zebra or the peer,
    // without any errors, channel closes, or bytes written.
    //
    // But the connection closes if we drop the future, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop works as a spawned task
#[tokio::test]
async fn connection_run_loop_spawn_ok() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop works as a spawned task with messages in and out
#[tokio::test]
async fn connection_run_loop_message_ok() {
    let _init_guard = zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send and receive
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx
        .try_send(request)
        .expect("internal request channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    peer_tx
        .try_send(Ok(Message::Addr(Vec::new())))
        .expect("peer inbound response channel is valid");

    // give the event loop time to run
    tokio::task::yield_now().await;
    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect("response is a message (not an error)"),
        Response::Peers(Vec::new()),
    );

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Public clients can send maintenance requests without panicking or writing to the peer.
#[tokio::test(start_paused = true)]
async fn client_poll_peer_set_returns_nil() {
    use tower::{Service, ServiceExt};

    let _init_guard = zebra_test::init();
    let (connection, client_tx, _inbound_service, mut outbound, error_slot) = new_test_connection();
    let (mut client, _harness) = crate::peer::ClientTestHarness::build().finish();
    client.server_tx = client_tx;
    client.error_slot = error_slot.clone();
    let connection_task = tokio::spawn(connection.run(futures::stream::pending()));

    let response = tokio::time::timeout(Duration::from_secs(1), async {
        client
            .ready()
            .await
            .unwrap()
            .call(Request::PollPeerSet)
            .await
    })
    .await
    .expect("maintenance requests must finish without a peer response")
    .expect("maintenance requests must succeed");
    assert_eq!(response, Response::Nil);
    assert!(outbound.next().now_or_never().is_none());
    assert!(error_slot.try_get_error().is_none());

    connection_task.abort();
    assert!(connection_task.await.unwrap_err().is_cancelled());
}

/// An empty mempool reply releases the connection before the request timeout.
#[tokio::test(start_paused = true)]
async fn empty_mempool_reply_unblocks_queued_block_request() {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (connection, mut client_tx, _inbound_service, mut outbound, error_slot) =
        new_test_connection();
    let connection_task = tokio::spawn(connection.run(peer_rx));

    let (mempool_tx, mempool_rx) = oneshot::channel();
    client_tx
        .try_send(ClientRequest {
            request: Request::MempoolTransactionIds,
            tx: mempool_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::Mempool),
    );

    let hash = zebra_chain::block::Hash([1; 32]);
    let (block_tx, block_rx) = oneshot::channel();
    client_tx
        .try_send(ClientRequest {
            request: Request::BlockDownload {
                hash,
                advertiser: None,
            },
            tx: block_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .unwrap();
    assert!(outbound.next().now_or_never().is_none());

    peer_tx.send(Ok(Message::Inv(Vec::new()))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        assert_eq!(
            mempool_rx.await.unwrap().unwrap(),
            Response::TransactionIds(Vec::new()),
        );
        assert_eq!(
            outbound.next().await,
            Some(Message::GetData(vec![InventoryHash::Block(hash)])),
        );
    })
    .await
    .expect("an empty mempool reply must release the queued block request without a timeout");

    assert!(error_slot.try_get_error().is_none());
    drop(block_rx);
    connection_task.abort();
    assert!(connection_task.await.unwrap_err().is_cancelled());
}

/// A late empty mempool reply must not finish a later block-locator request.
#[tokio::test(start_paused = true)]
async fn cancelled_mempool_reply_does_not_complete_find_blocks() {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (connection, mut client_tx, _inbound_service, mut outbound, error_slot) =
        new_test_connection();
    let connection_task = tokio::spawn(connection.run(peer_rx));

    let (mempool_tx, mempool_rx) = oneshot::channel();
    client_tx
        .try_send(ClientRequest {
            request: Request::MempoolTransactionIds,
            tx: mempool_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::Mempool),
    );
    drop(mempool_rx);

    let (find_tx, mut find_rx) = oneshot::channel();
    client_tx
        .send(ClientRequest {
            request: Request::FindBlocks {
                known_blocks: Vec::new(),
                stop: None,
            },
            tx: find_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::GetBlocks {
            known_blocks: Vec::new(),
            stop: None,
        }),
    );

    peer_tx.send(Ok(Message::Inv(Vec::new()))).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut find_rx)
            .await
            .is_err(),
        "the late empty mempool reply must leave the block-locator request pending",
    );

    let hashes = vec![
        zebra_chain::block::Hash([1; 32]),
        zebra_chain::block::Hash([2; 32]),
    ];
    peer_tx
        .send(Ok(Message::Inv(
            hashes.iter().copied().map(InventoryHash::Block).collect(),
        )))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), find_rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Response::BlockHashes(hashes),
    );

    assert!(error_slot.try_get_error().is_none());
    connection_task.abort();
    assert!(connection_task.await.unwrap_err().is_cancelled());
}

/// An empty `getblocks` result sends nothing, so peers can't take it for an empty mempool reply,
/// while a nonempty result is still sent as an inventory.
#[tokio::test(start_paused = true)]
async fn empty_find_blocks_result_sends_no_inventory() {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (connection, _client_tx, mut inbound_service, mut outbound, error_slot) =
        new_test_connection();
    let connection_task = tokio::spawn(connection.run(peer_rx));
    let get_blocks = Message::GetBlocks {
        known_blocks: Vec::new(),
        stop: None,
    };
    let find_blocks = Request::FindBlocks {
        known_blocks: Vec::new(),
        stop: None,
    };

    peer_tx.send(Ok(get_blocks.clone())).await.unwrap();
    inbound_service
        .expect_request(find_blocks.clone())
        .await
        .respond(Response::BlockHashes(Vec::new()));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .is_err(),
        "an empty getblocks result must not be sent as an empty inventory",
    );

    let hash = zebra_chain::block::Hash([1; 32]);
    peer_tx.send(Ok(get_blocks)).await.unwrap();
    inbound_service
        .expect_request(find_blocks)
        .await
        .respond(Response::BlockHashes(vec![hash]));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::Inv(vec![InventoryHash::Block(hash)])),
    );

    assert!(error_slot.try_get_error().is_none());
    connection_task.abort();
    assert!(connection_task.await.unwrap_err().is_cancelled());
}

/// Dedicated block adverts preserve the ordinary request counter and cancellation labels.
#[test]
fn block_advert_request_metrics_include_cancellation() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let recorder = ConnectionMetricsRecorder::default();

    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            tokio::time::pause();
            tokio::time::timeout(Duration::from_secs(1), async {
                let (mut connection, _client_tx, _service, mut outbound_messages, _error_slot) =
                    new_test_connection();
                let hash = block::Hash([1; 32]);
                let (result, delivery) = tokio::sync::oneshot::channel();
                let (_, message) = tokio::join!(
                    connection.send_block_advert(crate::peer_set::BlockAdvert { hash, result }),
                    outbound_messages.next(),
                );
                assert_eq!(
                    message,
                    Some(Message::Inv(vec![InventoryHash::Block(hash)]))
                );
                delivery
                    .await
                    .expect("the completed write sends an acknowledgement")
                    .expect("the connected test transport accepts the advert");

                let (result, delivery) = tokio::sync::oneshot::channel();
                drop(delivery);
                connection
                    .send_block_advert(crate::peer_set::BlockAdvert { hash, result })
                    .await;
                assert!(outbound_messages.try_recv().is_err());

                assert_eq!(
                    recorder.counter(Key::from_parts(
                        "zebra.net.out.requests",
                        &[("command", Request::AdvertiseBlock(hash, None).command())],
                    )),
                    1,
                );
                assert_eq!(
                    recorder.counter(Key::from_parts(
                        "zebra.net.out.requests.canceled",
                        &[("command", Request::AdvertiseBlockToAll(hash).command())],
                    )),
                    1,
                );
                assert_eq!(recorder.counter(Key::from_name("peer.canceled")), 1);
            })
            .await
            .expect("the local transport and canceled advert complete without remote responses");
        });
    });
}
/// Block adverts bypass pending responses without replacing their handlers or closing the client.
#[tokio::test]
async fn block_advert_while_awaiting_response() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    tokio::time::timeout(Duration::from_secs(1), async {
        let (mut peer_tx, peer_rx) = mpsc::channel(1);
        let (
            mut connection,
            mut client_tx,
            _inbound_service,
            mut peer_outbound_messages,
            shared_error_slot,
        ) = new_test_connection();
        let (block_advert_tx, block_advert_rx) = tokio::sync::mpsc::channel(4);
        connection.block_advert_rx = block_advert_rx;
        let connection_task = tokio::spawn(connection.run(peer_rx));

        // Exercise the idle path before leaving an address request awaiting its response.
        let idle_hash = block::Hash([1; 32]);
        let (result, idle_delivery) = tokio::sync::oneshot::channel();
        block_advert_tx
            .try_send(crate::peer_set::BlockAdvert {
                hash: idle_hash,
                result,
            })
            .unwrap();
        assert_eq!(
            peer_outbound_messages.next().await,
            Some(Message::Inv(vec![InventoryHash::Block(idle_hash)]))
        );
        idle_delivery.await.unwrap().unwrap();

        let (request_tx, mut request_rx) = oneshot::channel();
        client_tx
            .try_send(ClientRequest {
                request: Request::Peers,
                tx: request_tx,
                inv_collector: None,
                transient_addr: None,
                span: Span::current(),
            })
            .unwrap();
        assert_eq!(peer_outbound_messages.next().await, Some(Message::GetAddr));

        let busy_hash = block::Hash([2; 32]);
        let (result, busy_delivery) = tokio::sync::oneshot::channel();
        block_advert_tx
            .try_send(crate::peer_set::BlockAdvert {
                hash: busy_hash,
                result,
            })
            .unwrap();
        assert_eq!(
            peer_outbound_messages.next().await,
            Some(Message::Inv(vec![InventoryHash::Block(busy_hash)]))
        );
        busy_delivery.await.unwrap().unwrap();
        assert!(request_rx.try_recv().unwrap().is_none());

        // Closing only the advert channel must leave the original request alive.
        drop(block_advert_tx);
        peer_tx.try_send(Ok(Message::Addr(Vec::new()))).unwrap();
        assert_eq!(
            request_rx.await.unwrap().unwrap(),
            Response::Peers(Vec::new())
        );
        assert!(shared_error_slot.try_get_error().is_none());

        drop(client_tx);
        connection_task.await.unwrap();
    })
    .await
    .expect("adverts and the original response finish before the request timeout");
}

/// A pending GetBlocks response must not be displaced by a simultaneous unsolicited advert.
#[tokio::test]
async fn getblocks_response_precedes_ready_block_advert() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    tokio::time::timeout(Duration::from_secs(1), async {
        let (mut peer_tx, peer_rx) = mpsc::channel(1);
        let (
            mut connection,
            client_tx,
            mut inbound_service,
            mut peer_outbound_messages,
            shared_error_slot,
        ) = new_test_connection();
        let (block_advert_tx, block_advert_rx) = tokio::sync::mpsc::channel(4);
        connection.block_advert_rx = block_advert_rx;
        let response_hash = block::Hash([3; 32]);
        let advert_hash = block::Hash([4; 32]);

        // Queue both inputs before starting the loop, making selection order deterministic.
        peer_tx
            .try_send(Ok(Message::GetBlocks {
                known_blocks: Vec::new(),
                stop: None,
            }))
            .unwrap();
        let (result, _delivery) = tokio::sync::oneshot::channel();
        block_advert_tx
            .try_send(crate::peer_set::BlockAdvert {
                hash: advert_hash,
                result,
            })
            .unwrap();
        let connection_task = tokio::spawn(connection.run(peer_rx));

        tokio::join!(
            async {
                inbound_service
                    .expect_request(Request::FindBlocks {
                        known_blocks: Vec::new(),
                        stop: None,
                    })
                    .await
                    .respond(Response::BlockHashes(vec![response_hash]));
            },
            async {
                assert_eq!(
                    peer_outbound_messages.next().await,
                    Some(Message::Inv(vec![InventoryHash::Block(response_hash)]))
                );
                assert_eq!(
                    peer_outbound_messages.next().await,
                    Some(Message::Inv(vec![InventoryHash::Block(advert_hash)]))
                );
            }
        );
        assert!(shared_error_slot.try_get_error().is_none());

        drop(client_tx);
        connection_task.await.unwrap();
    })
    .await
    .expect("the response and advert are both delivered");
}

/// A singleton reaches inbound without also scheduling a sync download.
#[tokio::test]
async fn connection_find_blocks_singleton_inv_is_only_advertised() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        _shared_error_slot,
    ) = new_test_connection();
    let connection = tokio::spawn(connection.run(peer_rx));

    let (request_tx, request_rx) = oneshot::channel();
    client_tx
        .try_send(ClientRequest {
            request: Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            tx: request_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .expect("internal request channel is valid");
    assert_eq!(
        peer_outbound_messages.next().await,
        Some(Message::GetBlocks {
            known_blocks: vec![],
            stop: None,
        }),
    );

    let hash = block::Hash([1; 32]);
    peer_tx
        .try_send(Ok(Message::Inv(vec![hash.into()])))
        .expect("peer inbound response channel is valid");

    inbound_service
        .expect_request(Request::AdvertiseBlock(hash, None))
        .await
        .respond(Response::Nil);
    assert_eq!(
        request_rx
            .await
            .expect("peer internal response channel is valid")
            .expect("singleton inventory completes FindBlocks"),
        Response::BlockHashes(Vec::new()),
    );

    connection.abort();
}

/// Multi-block inventories complete FindBlocks without being advertised to inbound.
#[tokio::test]
async fn connection_find_blocks_multi_inv_is_not_advertised() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        _shared_error_slot,
    ) = new_test_connection();
    let connection = tokio::spawn(connection.run(peer_rx));

    let (request_tx, request_rx) = oneshot::channel();
    client_tx
        .try_send(ClientRequest {
            request: Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            tx: request_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::current(),
        })
        .expect("internal request channel is valid");
    assert_eq!(
        peer_outbound_messages.next().await,
        Some(Message::GetBlocks {
            known_blocks: vec![],
            stop: None,
        }),
    );

    let hashes = vec![block::Hash([1; 32]), block::Hash([2; 32])];
    peer_tx
        .try_send(Ok(Message::Inv(
            hashes.iter().copied().map(Into::into).collect(),
        )))
        .expect("peer inbound response channel is valid");
    assert_eq!(
        request_rx
            .await
            .expect("peer internal response channel is valid")
            .expect("multi-block inventory completes FindBlocks"),
        Response::BlockHashes(hashes),
    );
    inbound_service.expect_no_requests().await;

    connection.abort();
}

/// Test that the connection run loop fails correctly when dropped
#[tokio::test]
async fn connection_run_loop_future_drop() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // now_or_never implicitly drops the connection future.
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionDropped",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the internal client closes the connection channel
#[tokio::test]
async fn connection_run_loop_client_close() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    let connection = connection.run(peer_rx);

    // Explicitly close the client channel.
    client_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ClientDropped",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the internal client drops the connection channel
#[tokio::test]
async fn connection_run_loop_client_drop() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Drop the client channel.
    std::mem::drop(client_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ClientDropped",
    );

    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the peer channel is closed.
/// (We're not sure if tokio closes or drops the TcpStream when the TCP connection closes.)
#[tokio::test]
async fn connection_run_loop_inbound_close() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Explicitly close the inbound peer channel.
    peer_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionClosed",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the peer channel is dropped
/// (We're not sure if tokio closes or drops the TcpStream when the TCP connection closes.)
#[tokio::test]
async fn connection_run_loop_inbound_drop() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Drop the inbound peer channel.
    std::mem::drop(peer_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionClosed",
    );

    assert!(client_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly on internal connection errors.
#[tokio::test]
async fn connection_run_loop_failed() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        mut connection,
        client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Simulate an internal connection error.
    connection.state = State::Failed;
    shared_error_slot
        .try_update_error(PeerError::Overloaded.into())
        .expect("unexpected previous error in tests");

    let connection = connection.run(peer_rx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    // Because the peer error mutex is a sync mutex,
    // the connection can't exit until it reaches the outer async loop.
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "Overloaded",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when sending a message to a peer times out,
/// but we are not expecting a response message from the peer.
#[tokio::test]
async fn connection_run_loop_send_timeout_nil_response() {
    let _init_guard = zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::AdvertiseTransactionIds(HashSet::new(), None),
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // Make the send timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Send timeouts close the connection
    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionSendTimeout",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::Inv(Vec::new())));

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionSendTimeout",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection finished, but did not panic.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Ready(Ok(()))),
        "expected run loop termination, but run loop continued: {connection_result:?}",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when sending a message to a peer times out,
/// and we are expecting a response message from the peer.
#[tokio::test]
async fn connection_run_loop_send_timeout_expect_response() {
    let _init_guard = zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // Make the send timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Send timeouts close the connection
    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionSendTimeout",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionSendTimeout",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection finished, but did not panic.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Ready(Ok(()))),
        "expected run loop termination, but run loop continued: {connection_result:?}",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection returns an error to the client, rather than panicking,
/// when the connection task is cancelled while it is sending a message to a peer.
///
/// Regression test for [GHSA-6v36-c42v-v98p].
///
/// [GHSA-6v36-c42v-v98p]: https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-6v36-c42v-v98p
#[tokio::test]
async fn connection_run_loop_cancelled_during_send() {
    let _init_guard = zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (_peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let connection_join_handle = tokio::spawn(connection.run(peer_rx));

    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::AdvertiseTransactionIds(HashSet::new(), None),
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // The peer isn't reading messages, so the send blocks.
    // Let the connection start sending, but stop before the send times out.
    tokio::time::sleep(REQUEST_TIMEOUT / 2).await;

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");
    assert!(
        matches!(request_rx.try_recv(), Ok(None)),
        "the request should still be sending",
    );

    // Simulate `Client::drop()`, which sets the error slot, then aborts the connection task.
    shared_error_slot
        .try_update_error(PeerError::ClientDropped.into())
        .expect("the connection hasn't errored yet");
    connection_join_handle.abort();

    let connection_result = connection_join_handle.await;
    assert!(
        connection_result
            .as_ref()
            .is_err_and(|error| error.is_cancelled()),
        "expected cancelled run loop, but it panicked or finished: {connection_result:?}",
    );

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("response sender was used before it was dropped")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ClientDropped",
    );

    assert!(client_tx.is_closed());

    inbound_service.expect_no_requests().await;

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::Inv(Vec::new())));

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop continues but returns an error to the client,
/// when a peer accepts a message, but does not send an expected response.
#[tokio::test]
async fn connection_run_loop_receive_timeout() {
    let _init_guard = zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message receive timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    // Make the receive timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Receive timeouts don't close the connection
    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionReceiveTimeout",
    );

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Check basic properties of overload probabilities
#[test]
fn overload_probability_reduces_over_time() {
    let now = Instant::now();

    // Edge case: previous is in the future due to OS monotonic clock bugs
    let prev = now + Duration::from_secs(1);
    assert_eq!(
        overload_drop_connection_probability(now, Some(prev)),
        MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overload time is in the future (OS bugs?), it should have maximum drop probability",
    );

    // Overload/DoS case/edge case: rapidly repeated overloads
    let prev = now;
    assert_eq!(
        overload_drop_connection_probability(now, Some(prev)),
        MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overload times are the same, overloads should have maximum drop probability",
    );

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_micros(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability <= MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overloads are very close together, drops can optionally decrease: {drop_probability} <= {MAX_OVERLOAD_DROP_PROBABILITY}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_millis(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_millis(10);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: frequent overloads
    let prev = now - Duration::from_millis(100);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.01,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: occasional but repeated overloads
    let prev = now - Duration::from_secs(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability > 0.4,
        "if the overloads are distant, drops should decrease a lot: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: occasional overloads
    let prev = now - Duration::from_secs(5);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if overloads are far apart, drops should have minimum drop probability: {drop_probability}",
    );
    let _last_probability = drop_probability;

    // Base case: infrequent overloads
    let prev = now - Duration::from_secs(10);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if overloads are far apart, drops should have minimum drop probability: {drop_probability}",
    );

    // Base case: no previous overload
    let drop_probability = overload_drop_connection_probability(now, None);
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if there is no previous overload time, overloads should have minimum drop probability: {drop_probability}",
    );
}

/// Test that connections are randomly terminated in response to `Overloaded` errors.
///
/// TODO: do a similar test on the real service stack created in the `start` command.
#[tokio::test(flavor = "multi_thread")]
async fn connection_is_randomly_disconnected_on_overload() {
    let _init_guard = zebra_test::init();

    // The number of times we repeat the test
    const TEST_RUNS: usize = 220;
    // The expected number of tests before a test failure due to random chance.
    // Based on 10 tests per PR, 100 PR pushes per week, 50 weeks per year.
    const TESTS_BEFORE_FAILURE: f32 = 50_000.0;

    let test_runs = TEST_RUNS.try_into().expect("constant fits in i32");
    // The probability of random test failure is:
    // MIN_OVERLOAD_DROP_PROBABILITY^TEST_RUNS + MAX_OVERLOAD_DROP_PROBABILITY^TEST_RUNS
    assert!(
        1.0 / MIN_OVERLOAD_DROP_PROBABILITY.powi(test_runs) > TESTS_BEFORE_FAILURE,
        "not enough test runs: failures must be frequent enough to happen in almost all tests"
    );
    assert!(
        1.0 / MAX_OVERLOAD_DROP_PROBABILITY.powi(test_runs) > TESTS_BEFORE_FAILURE,
        "not enough test runs: successes must be frequent enough to happen in almost all tests"
    );

    let mut connection_continues = 0;
    let mut connection_closes = 0;

    for _ in 0..TEST_RUNS {
        // The real stream and sink are from a split TCP connection,
        // but that doesn't change how the state machine behaves.
        let (mut peer_tx, peer_rx) = mpsc::channel(1);

        let (
            connection,
            _client_tx,
            mut inbound_service,
            mut peer_outbound_messages,
            shared_error_slot,
        ) = new_test_connection();

        // The connection hasn't run so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before starting the connection event loop: {error:?}",
        );

        // Start the connection run loop future in a spawned task
        let connection_handle = tokio::spawn(connection.run(peer_rx));
        tokio::time::sleep(Duration::from_millis(1)).await;

        // The connection hasn't received any messages, so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before sending messages to the connection event loop: {error:?}",
        );

        // Simulate an overloaded connection error in response to an inbound request.
        let inbound_req = Message::GetAddr;
        peer_tx
            .send(Ok(inbound_req))
            .await
            .expect("send to channel always succeeds");
        tokio::time::sleep(Duration::from_millis(1)).await;

        // The connection hasn't got a response, so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before sending responses to the connection event loop: {error:?}",
        );

        inbound_service
            .expect_request(Request::Peers)
            .await
            .respond_error(Overloaded::new().into());
        tokio::time::sleep(Duration::from_millis(1)).await;

        let outbound_result = peer_outbound_messages.try_recv();
        assert!(
            outbound_result.is_err(),
            "unexpected outbound message after Overloaded error:\n\
             {outbound_result:?}\n\
             note: Err(TryRecvError::Empty) means no messages, Err(TryRecvError::Closed) means the channel is closed"
        );

        let error = shared_error_slot.try_get_error();
        if error.is_some() {
            connection_closes += 1;
        } else {
            connection_continues += 1;
        }

        // We need to terminate the spawned task
        connection_handle.abort();
    }

    assert!(
        connection_closes > 0,
        "some overloaded connections must be closed at random"
    );
    assert!(
        connection_continues > 0,
        "some overloaded errors must be ignored at random"
    );
}

#[tokio::test]
async fn connection_ping_pong_round_trip() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        _inbound_service,
        mut peer_outbound_messages,
        _shared_error_slot,
    ) = new_test_connection();

    let connection = tokio::spawn(connection.run(peer_rx));

    // === Client sends Ping request ===
    let (response_tx, response_rx) = oneshot::channel();
    let nonce = Nonce::default();

    client_tx
        .send(ClientRequest {
            request: Request::Ping(nonce),
            tx: response_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::none(),
        })
        .await
        .expect("send to connection should succeed");

    // === Peer receives Ping message ===
    let outbound_msg = peer_outbound_messages
        .next()
        .await
        .expect("expected outbound Ping message");

    let ping_nonce = match outbound_msg {
        Message::Ping(nonce) => nonce,
        msg => panic!("expected Ping message, but got: {msg:?}"),
    };

    assert_eq!(
        nonce, ping_nonce,
        "Ping nonce in request must match message sent to peer"
    );

    // === Peer sends matching Pong ===
    let pong_rtt = Duration::from_millis(42);
    tokio::time::sleep(pong_rtt).await;

    peer_tx
        .clone()
        .send(Ok(Message::Pong(ping_nonce)))
        .await
        .expect("sending Pong to connection should succeed");

    // === Client receives Pong response and verifies RTT ===
    match response_rx.await.expect("response channel must succeed") {
        Ok(Response::Pong(rtt)) => {
            assert!(
                rtt >= pong_rtt,
                "measured RTT {rtt:?} must be >= simulated RTT {pong_rtt:?}"
            );
        }
        Ok(resp) => panic!("unexpected response: {resp:?}"),
        Err(err) => panic!("unexpected error: {err:?}"),
    }

    drop(peer_tx);
    let _ = connection.await;
}

/// Test that a `notfound` for a cancelled request doesn't end a later request for other inventory.
#[tokio::test]
async fn connection_ignores_notfound_for_cancelled_requests() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = [
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_slice(),
        zebra_test::vectors::BLOCK_MAINNET_2_BYTES.as_slice(),
    ]
    .into_iter()
    .map(|bytes| {
        bytes
            .zcash_deserialize_into()
            .expect("block vectors are valid")
    })
    .collect();
    let transactions: Vec<UnminedTx> = blocks
        .iter()
        .map(|block| UnminedTx::from(&block.transactions[0]))
        .collect();
    let block_request = |block: &Arc<Block>| Request::BlocksByHash(HashSet::from([block.hash()]));
    let transaction_request = |tx: &UnminedTx| Request::TransactionsById(HashSet::from([tx.id]));

    // A cancelled transaction request, then a block request.
    let response = cancelled_notfound_then_response(
        transaction_request(&transactions[0]),
        transactions[0].id.into(),
        block_request(&blocks[1]),
        Message::Block(blocks[1].clone()),
    )
    .await;
    assert_eq!(
        response,
        Response::Blocks(vec![InventoryResponse::Available((
            blocks[1].clone(),
            None
        ))])
    );

    // A cancelled block request, then a transaction request.
    let response = cancelled_notfound_then_response(
        block_request(&blocks[0]),
        blocks[0].hash().into(),
        transaction_request(&transactions[1]),
        Message::Tx(transactions[1].clone()),
    )
    .await;
    assert_eq!(
        response,
        Response::Transactions(vec![InventoryResponse::Available((
            transactions[1].clone(),
            None
        ))])
    );

    // A cancelled block request, then a request for a different block.
    let response = cancelled_notfound_then_response(
        block_request(&blocks[0]),
        blocks[0].hash().into(),
        block_request(&blocks[1]),
        Message::Block(blocks[1].clone()),
    )
    .await;
    assert_eq!(
        response,
        Response::Blocks(vec![InventoryResponse::Available((
            blocks[1].clone(),
            None
        ))])
    );
}

/// Cancels `cancelled`, sends `request`, then delivers the peer's `notfound` for `missing` from
/// the cancelled request before `reply`. Returns the response to `request`.
async fn cancelled_notfound_then_response(
    cancelled: Request,
    missing: InventoryHash,
    request: Request,
    reply: Message,
) -> Response {
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (connection, mut client_tx, mut inbound_service, mut peer_outbound_messages, error_slot) =
        new_test_connection();
    let connection = tokio::spawn(connection.run(peer_rx));

    let mut response_rx = None;
    for request in [cancelled, request] {
        // Dropping the previous receiver cancels its request.
        drop(response_rx.take());
        let (tx, rx) = oneshot::channel();
        client_tx
            .send(ClientRequest {
                request,
                tx,
                inv_collector: None,
                transient_addr: None,
                span: Span::none(),
            })
            .await
            .expect("connection accepts requests");
        assert!(matches!(
            peer_outbound_messages.next().await,
            Some(Message::GetData(_))
        ));
        response_rx = Some(rx);
    }

    for message in [Message::NotFound(vec![missing]), reply] {
        peer_tx
            .send(Ok(message))
            .await
            .expect("connection is running");
    }
    let response = response_rx
        .expect("the loop sends the request")
        .await
        .expect("connection responds")
        .expect("the notfound for the cancelled request is ignored");

    assert!(error_slot.try_get_error().is_none());
    inbound_service.expect_no_requests().await;

    drop(peer_tx);
    let _ = connection.await;
    response
}

/// Creates a new [`Connection`] instance for unit tests.
fn new_test_connection() -> (
    Connection<
        MockService<Request, Response, PanicAssertion>,
        SinkMapErr<mpsc::Sender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, PanicAssertion>,
    mpsc::Receiver<Message>,
    ErrorSlot,
) {
    super::new_test_connection()
}
