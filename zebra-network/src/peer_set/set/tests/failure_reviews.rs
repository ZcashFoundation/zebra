//! Regression checkpoints for inventory failure eligibility, attribution, and wakeups.

use std::{future::Future, sync::Arc};

use futures::{channel::mpsc, SinkExt};
use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    constants::REQUEST_TIMEOUT,
    peer::{Connection, ErrorSlot},
    peer_set::ActiveConnectionCounter,
    protocol::{external::Message, internal::InventoryResponse},
};

use super::*;

/// A real block and matching download request, so successful routing also completes the download.
fn block_download() -> (Request, InventoryHash, Response) {
    let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .expect("the historical block vector is valid");
    let hash = block::Hash::from(&block);
    (
        Request::BlocksByHash(iter::once(hash).collect()),
        InventoryHash::Block(hash),
        Response::Blocks(vec![InventoryResponse::Available((Arc::new(block), None))]),
    )
}

/// Complete an already dispatched download through the real client response channel.
async fn finish_download(
    handle: &mut ClientTestHarness,
    request: Request,
    response: Response,
    response_fut: impl Future<Output = Result<Response, BoxError>>,
) {
    let client_request = handle
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the eligible peer must receive the download before its queue deadline");
    assert_eq!(client_request.request, request);
    client_request
        .tx
        .send(Ok(response.clone()))
        .expect("the download is still waiting for its response");
    assert_eq!(
        timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, response_fut)
            .await
            .expect("a dispatched download must not hang")
            .expect("the peer returned the requested block"),
        response,
    );
}

/// Carried provenance cannot override explicit missing inventory or a completed peer failure.
#[tokio::test(start_paused = true)]
async fn carried_advertiser_respects_missing_and_failed_candidates() {
    let _init_guard = zebra_test::init();
    for failed in [false, true] {
        let (mut peer_set, mut guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);
        let (_, inventory, response) = block_download();
        let InventoryHash::Block(hash) = inventory else {
            unreachable!("the helper constructs block inventory");
        };
        let request = Request::BlockDownload {
            hash,
            advertiser: Some(addrs[1]),
        };

        if failed {
            assert!(respond_to_request(
                &mut peer_set,
                &mut handles[1],
                request.clone(),
                Err(PeerError::ConnectionReceiveTimeout.into()),
            )
            .await
            .is_err());
        } else {
            send_inventory(
                &mut guard,
                InventoryStatus::new_missing(inventory, addrs[1]),
            );
        }

        let download = peer_set
            .ready()
            .await
            .expect("the healthy serving connection remains ready")
            .call(request.clone());
        assert_eq!(
            received_request(&mut handles[1]),
            None,
            "known provenance must not restore an ineligible source",
        );
        finish_download(&mut handles[0], request, response, download).await;
    }
}

/// N7: a failed connected advertisement must not redirect historic work to unrelated peers.
#[tokio::test(start_paused = true)]
async fn failed_advertiser_does_not_enable_nonserving_fallback() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, mut guard, addrs, mut handles, _best_tip) = peer_set_with_services(&[
        PeerServices::NODE_NETWORK,
        PeerServices::empty(),
        PeerServices::empty(),
    ]);
    let (request, hash, response) = block_download();
    send_inventory(&mut guard, InventoryStatus::new_available(hash, addrs[1]));
    assert!(respond_to_request(
        &mut peer_set,
        &mut handles[1],
        request.clone(),
        Err(PeerError::ConnectionReceiveTimeout.into()),
    )
    .await
    .is_err());

    let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
    assert!(peer_set.has_peer_with_addr(addrs[1]));
    assert!(peer_set
        .inventory_registry
        .failed_peers(hash)
        .any(|p| *p == addrs[1]));
    let mut queued = peer_set
        .ready()
        .await
        .expect("fallback peers are ready")
        .call(request.clone());
    assert!(timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, &mut queued)
        .await
        .is_err());
    assert_eq!(received_request(&mut handles[1]), None);
    assert_eq!(
        received_request(&mut handles[2]),
        None,
        "the failed advertisement must not enable a non-serving fallback",
    );

    assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
    assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
    timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, peer_set.ready())
        .await
        .expect("released client capacity must become ready")
        .expect("the peer set remains healthy");
    finish_download(&mut handles[0], request, response, queued).await;
}

#[derive(Clone, Copy)]
enum CandidateChange {
    Failure,
    Removal,
    Advertisement,
}

/// Keep both serving-channel slots full across the event: no readiness transition can help.
async fn queued_block_reroutes_on_candidate_change(change: CandidateChange) {
    let _init_guard = zebra_test::init();
    let (initial_peers, addrs, mut handles) = mock_peers_with_services(&SERVING_AND_NON_SERVING);
    let (mut discovery_tx, discovery_rx) = mpsc::channel(2);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, mut guard) = PeerSetBuilder::new()
        .with_discover(stream::select(initial_peers, discovery_rx))
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(SERVING_AND_NON_SERVING.len(), DEFAULT_MAX_CONNS_PER_IP))
        .build();
    let (request, hash, response) = block_download();

    let first_response = peer_set
        .ready()
        .await
        .expect("serving peer is ready")
        .call(request.clone());
    let first_request = handles[0]
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the serving peer receives the original download");
    assert_eq!(first_request.request, request);
    let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
    let (mut peer_set, poll_task) = peer_set.into_buffer(10);
    let mut queued = peer_set
        .ready()
        .await
        .expect("the buffer has room")
        .call(request.clone());
    assert!(timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, &mut queued)
        .await
        .is_err());
    assert_eq!(received_request(&mut handles[1]), None);

    match change {
        CandidateChange::Failure => {
            first_request
                .tx
                .send(Err(PeerError::ConnectionReceiveTimeout.into()))
                .expect("the original download is still waiting");
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, first_response)
                .await
                .expect("the real client must deliver its error")
                .expect_err("the original download failed");
        }
        CandidateChange::Removal => {
            discovery_tx
                .try_send(Ok(Change::Remove(addrs[0])))
                .expect("the discovery channel has room");
        }
        CandidateChange::Advertisement => {
            send_inventory(&mut guard, InventoryStatus::new_available(hash, addrs[1]));
        }
    }

    // No request or readiness poll is sent to Buffer after the candidate change. Only the
    // production notification task can wake it and dispatch the already queued download.
    tokio::time::sleep(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4).await;
    finish_download(&mut handles[1], request, response, queued).await;
    poll_task.abort();
}

/// N9: an actual response error must wake and reroute queued work behind Buffer.
#[tokio::test(start_paused = true)]
async fn queued_block_reroutes_on_failure_behind_buffer() {
    queued_block_reroutes_on_candidate_change(CandidateChange::Failure).await;
}

/// N9: removing the only busy serving peer must immediately enable the ready fallback.
#[tokio::test(start_paused = true)]
async fn queued_block_reroutes_on_removal_behind_buffer() {
    queued_block_reroutes_on_candidate_change(CandidateChange::Removal).await;
}

/// N9: a useful advertisement must wake the idle Buffer without any peer becoming ready.
#[tokio::test(start_paused = true)]
async fn queued_block_reroutes_on_advertisement_behind_buffer() {
    queued_block_reroutes_on_candidate_change(CandidateChange::Advertisement).await;
}

/// N13: dropping a pending caller is not evidence that its connected peer failed.
#[tokio::test(start_paused = true)]
async fn caller_drop_does_not_record_block_failure() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, _guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&SERVING_AND_NON_SERVING);
    let (request, hash, response) = block_download();
    let mut pending = peer_set
        .ready()
        .await
        .expect("serving peer is ready")
        .call(request.clone());
    let in_flight = handles[0]
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the serving peer received the download");
    assert!(pending.as_mut().now_or_never().is_none());
    drop(pending);
    assert!(in_flight.tx.is_canceled());

    peer_set
        .ready()
        .await
        .expect("caller cancellation leaves the peer usable");
    assert_eq!(peer_set.inventory_registry.failed_peers(hash).count(), 0);
    assert_eq!(peer_set.inventory_registry.missing_peers(hash).count(), 0);
    assert!(peer_set.has_peer_with_addr(addrs[0]));
    let retry = peer_set.call(request.clone());
    assert_eq!(received_request(&mut handles[1]), None);
    finish_download(&mut handles[0], request, response, retry).await;
}

/// Feed a real peer response (or silence) through Connection, Client, and PeerSet accounting.
async fn connection_block_error_attribution(not_found: bool) {
    let _init_guard = zebra_test::init();
    let (mut peer_set, mut guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&SERVING_AND_NON_SERVING);
    let (request, hash, response) = block_download();
    peer_set.ready().await.expect("serving peer is ready");
    let connection_info = peer_set.ready_services[&addrs[0]].connection_info().clone();
    let pending = peer_set.call(request.clone());
    let mut client_request = handles[0]
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the serving client received the download");

    // The channel harness normally has an isolated collector. Connect the production response
    // sender to this PeerSet's collector instead of fabricating a missing-inventory event.
    client_request.inv_collector = guard.inventory_sender().as_ref().cloned();
    client_request.transient_addr = Some(addrs[0]);
    let (mut client_tx, client_rx) = mpsc::channel(1);
    let (mut remote_tx, remote_rx) = mpsc::channel(1);
    let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
    let inbound: MockService<Request, Response, PanicAssertion> = MockService::build().finish();
    let connection = Connection::new(
        inbound,
        client_rx,
        ErrorSlot::default(),
        outbound_tx.sink_map_err(|_| {
            SerializationError::from(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test peer message receiver closed",
            ))
        }),
        ActiveConnectionCounter::new_counter().track_connection(),
        connection_info,
        Vec::new(),
    );
    let connection_task = tokio::spawn(connection.run(remote_rx));
    client_tx
        .try_send(client_request)
        .expect("the client channel has room");
    assert_eq!(
        timeout(REQUEST_TIMEOUT / 4, outbound_rx.next())
            .await
            .expect("the connection must send getdata before its response timeout"),
        Some(Message::GetData(vec![hash])),
    );
    if not_found {
        remote_tx
            .try_send(Ok(Message::NotFound(vec![hash])))
            .expect("the peer response channel has room");
    }
    let error = timeout(REQUEST_TIMEOUT + Duration::from_secs(1), pending)
        .await
        .expect("the production response deadline must complete the request")
        .expect_err("the peer either refused the block or timed out");
    let error = error
        .downcast_ref::<SharedPeerError>()
        .expect("client errors retain their type");
    if not_found {
        assert!(error.inner_debug().contains("NotFoundResponse"));
    } else {
        assert_eq!(error.inner_debug(), "ConnectionReceiveTimeout");
    }

    peer_set
        .ready()
        .await
        .expect("the fallback peer remains ready");
    assert_eq!(
        peer_set
            .inventory_registry
            .missing_peers(hash)
            .copied()
            .collect::<Vec<_>>(),
        if not_found { vec![addrs[0]] } else { vec![] },
    );
    assert_eq!(
        peer_set
            .inventory_registry
            .failed_peers(hash)
            .copied()
            .collect::<Vec<_>>(),
        if not_found { vec![] } else { vec![addrs[0]] },
    );
    let retry = peer_set.call(request.clone());
    assert_eq!(received_request(&mut handles[0]), None);
    finish_download(&mut handles[1], request, response, retry).await;
    connection_task.abort();
}

/// Preservation: an explicit wire notfound records missing inventory, never a peer failure.
#[tokio::test(start_paused = true)]
async fn explicit_notfound_records_missing_without_block_failure() {
    connection_block_error_attribution(true).await;
}

/// Preservation/N13 control: real response-deadline expiry still records a genuine failure.
#[tokio::test(start_paused = true)]
async fn connection_timeout_records_block_failure_without_missing() {
    connection_block_error_attribution(false).await;
}

/// N14: busy non-serving failed peers have the same last-resort eligibility as ready ones.
#[tokio::test(start_paused = true)]
async fn busy_failed_nonserving_peer_remains_last_resort() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, mut guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&[PeerServices::empty(), PeerServices::empty()]);
    let (request, hash, response) = block_download();
    send_inventory(&mut guard, InventoryStatus::new_missing(hash, addrs[1]));
    assert!(respond_to_request(
        &mut peer_set,
        &mut handles[0],
        request.clone(),
        Err(PeerError::ConnectionReceiveTimeout.into()),
    )
    .await
    .is_err());

    // Advertise only the unrelated requests used to fill this client's channel. The target
    // block has no advertisement and its only other ready peer explicitly lacks it.
    let mut busy = Vec::new();
    for byte in [1, 9] {
        send_inventory(
            &mut guard,
            InventoryStatus::new_available(InventoryHash::Block(block::Hash([byte; 32])), addrs[0]),
        );
        busy.push(
            peer_set
                .ready()
                .await
                .expect("a peer is ready")
                .call(block_request(byte)),
        );
    }
    peer_set
        .ready()
        .await
        .expect("the missing peer is still ready");
    assert!(peer_set.cancel_handles.contains_key(&addrs[0]));
    let mut queued = peer_set.call(request.clone());
    assert!(
        timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, &mut queued)
            .await
            .is_err(),
        "the only non-missing peer must be awaited rather than immediately refused",
    );
    assert_eq!(received_request(&mut handles[1]), None);
    assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
    assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
    timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, peer_set.ready())
        .await
        .expect("released client capacity must become ready")
        .expect("the peer set remains healthy");
    finish_download(&mut handles[0], request, response, queued).await;
}

#[derive(Clone, Copy)]
enum ReplacementFailure {
    Current,
    Previous,
    Delayed,
}

async fn replacement_connection_ignores_old_failure(failure: ReplacementFailure) {
    let _init_guard = zebra_test::init();
    let (initial_peers, addrs, mut handles) = mock_peers_with_services(&SERVING_AND_NON_SERVING);
    let (mut discovery_tx, discovery_rx) = mpsc::channel(2);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(stream::select(initial_peers, discovery_rx))
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(SERVING_AND_NON_SERVING.len(), DEFAULT_MAX_CONNS_PER_IP))
        .build();
    let (request, hash, response) = block_download();
    let old_response = peer_set
        .ready()
        .await
        .expect("old connection is ready")
        .call(request.clone());
    let old_request = handles[0]
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the old connection receives the download");
    let delayed = if matches!(failure, ReplacementFailure::Delayed) {
        Some((old_response, old_request))
    } else {
        old_request
            .tx
            .send(Err(PeerError::ConnectionReceiveTimeout.into()))
            .expect("the old request is waiting for its response");
        timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, old_response)
            .await
            .expect("the old response must not hang")
            .expect_err("the old connection failed");
        peer_set.ready().await.expect("fallback remains ready");
        assert_eq!(
            peer_set
                .inventory_registry
                .failed_peers(hash)
                .copied()
                .collect::<Vec<_>>(),
            vec![addrs[0]]
        );
        if matches!(failure, ReplacementFailure::Previous) {
            tokio::time::advance(crate::constants::INVENTORY_ROTATION_INTERVAL).await;
            peer_set
                .ready()
                .await
                .expect("rotation leaves connected peers usable");
        }
        None
    };

    let (replacement, mut replacement_handle) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_services(PeerServices::NODE_NETWORK)
        .finish();
    discovery_tx
        .try_send(Ok(Change::Remove(addrs[0])))
        .expect("the discovery channel has room");
    discovery_tx
        .try_send(Ok(Change::Insert(addrs[0], replacement.into())))
        .expect("the discovery channel has room for the replacement");
    timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, peer_set.ready())
        .await
        .expect("the replacement must become ready")
        .expect("the peer set remains healthy");
    assert!(!handles[0].wants_connection_heartbeats());

    if let Some((old_response, old_request)) = delayed {
        old_request
            .tx
            .send(Err(PeerError::ConnectionReceiveTimeout.into()))
            .expect("the old request still owns its response receiver");
        timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, old_response)
            .await
            .expect("the delayed response must not hang")
            .expect_err("the old request failed");
        peer_set
            .ready()
            .await
            .expect("the replacement remains ready");
    }
    let retry = peer_set.call(request.clone());
    assert_eq!(
        received_request(&mut handles[1]),
        None,
        "the replacement must not lose serving preference to its predecessor's failure",
    );
    finish_download(&mut replacement_handle, request, response, retry).await;
    assert_eq!(peer_set.inventory_registry.failed_peers(hash).count(), 0);
}

/// N15: neither failure-cache generation may carry an old connection's failures forward.
#[tokio::test(start_paused = true)]
async fn replacement_connection_does_not_inherit_cached_block_failure() {
    for failure in [ReplacementFailure::Current, ReplacementFailure::Previous] {
        replacement_connection_ignores_old_failure(failure).await;
    }
}

/// N15: clearing the cache on replacement is insufficient if delayed events lack identity.
#[tokio::test(start_paused = true)]
async fn replacement_connection_ignores_delayed_old_block_failure() {
    replacement_connection_ignores_old_failure(ReplacementFailure::Delayed).await;
}

/// Missed rotations must not immediately age out a newly registered block failure.
#[tokio::test(start_paused = true)]
async fn overdue_inventory_rotations_preserve_new_block_failure() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, _guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&SERVING_AND_NON_SERVING);
    let (request, hash, _) = block_download();
    respond_to_request(
        &mut peer_set,
        &mut handles[0],
        request,
        Err(PeerError::ConnectionReceiveTimeout.into()),
    )
    .await
    .expect_err("the serving peer failed the block request");

    // Leave the completed failure queued while three rotation ticks become overdue.
    tokio::time::advance(3 * crate::constants::INVENTORY_ROTATION_INTERVAL).await;
    for _ in 0..3 {
        peer_set.ready().await.expect("fallback remains ready");
    }
    assert!(
        peer_set
            .inventory_registry
            .failed_peers(hash)
            .any(|peer| *peer == addrs[0]),
        "missed ticks must not expire a failure registered on the first poll",
    );
}
