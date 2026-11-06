//! Loopback tests for the `get-hashes` client: a [`SyncPeer`] over a real
//! version 2 connection task, and a raw peer that answers its requests by hand.

use std::time::Duration;

use quinn::{Connection, ConnectionError};

use zebra_chain::block::{self, SyncHashEntry};

use crate::{
    protocol::v2::{response::hashes::encode_hashes_response, types::ErrorCode},
    quic::app_error_code,
    sync_peer::{tests::fixture, SyncPeerError},
};

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An arbitrary entry for `height`.
fn entry(height: u32) -> SyncHashEntry {
    SyncHashEntry {
        hash: block::Hash([u8::try_from(height % 256).expect("in range"); 32]),
        size: 1_000 + u64::from(height),
        txs: 1,
        txouts: 2,
        notes: 0,
    }
}

/// Serves one request on `server`: checks the request bytes, then answers
/// with the body `respond` returns, or with nothing if it is empty (after
/// `respond` reset the stream).
async fn serve_one(
    server: &Connection,
    start: u32,
    count: u32,
    respond: impl FnOnce(&mut quinn::SendStream) -> Vec<u8>,
) {
    let (mut send, mut recv) = server.accept_bi().await.expect("a request stream");
    let request = recv.read_to_end(64).await.expect("the request is complete");
    let mut expected = vec![0x06];
    expected.extend(start.to_le_bytes());
    expected.extend(count.to_le_bytes());
    assert_eq!(request, expected, "the request is 0x06 ‖ start ‖ count");

    let body = respond(&mut send);
    if !body.is_empty() {
        // The client stops reading an overlong response, so the write may fail.
        let _ = send.write_all(&body).await;
        let _ = send.finish();
        // Wait for the client to read the response or stop the stream.
        let _ = send.stopped().await;
    }
}

/// A conforming response, including a prefix, is returned as is.
#[tokio::test]
async fn conforming_prefix_is_returned() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let pair = fixture().await;
        let entries: Vec<_> = (100..103).map(entry).collect();
        let body = encode_hashes_response(&entries)
            .expect("encodable")
            .to_vec();

        let (fetched, ()) = tokio::join!(
            pair.peer.get_hashes(block::Height(100), 10),
            serve_one(&pair.raw, 100, 10, |_| body),
        );
        assert_eq!(fetched, Ok(entries));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `get-hashes` does not wait for the peer's bulk permits: it is served while
/// block-range streams hold every one of them.
#[tokio::test]
async fn get_hashes_does_not_wait_for_bulk_permits() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let pair = fixture().await;
        let _bulk = pair
            .peer
            .bulk_permits
            .clone()
            .acquire_many_owned(
                u32::try_from(crate::sync_peer::BULK_STREAMS_PER_PEER).expect("small"),
            )
            .await
            .expect("the semaphore is open");

        let entries: Vec<_> = (0..2).map(entry).collect();
        let body = encode_hashes_response(&entries)
            .expect("encodable")
            .to_vec();
        let (fetched, ()) = tokio::join!(
            pair.peer.get_hashes(block::Height(0), 2),
            serve_one(&pair.raw, 0, 2, |_| body),
        );
        assert_eq!(fetched, Ok(entries));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `REFUSED` reset is reported as a refusal, and the connection stays open.
#[tokio::test]
async fn refusal_is_reported() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let pair = fixture().await;
        let (fetched, ()) = tokio::join!(
            pair.peer.get_hashes(block::Height(0), 1),
            serve_one(&pair.raw, 0, 1, |send| {
                send.reset(app_error_code(ErrorCode::Refused))
                    .expect("the stream is open");
                Vec::new()
            }),
        );
        assert_eq!(fetched, Err(SyncPeerError::Refused));
        assert!(pair.raw.close_reason().is_none(), "the connection is open");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// More entries than requested is a violation, which closes the connection
/// with `PROTOCOL_ERROR`.
#[tokio::test]
async fn too_many_entries_close_the_connection() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let pair = fixture().await;
        let entries: Vec<_> = (0..3).map(entry).collect();
        let body = encode_hashes_response(&entries)
            .expect("encodable")
            .to_vec();

        let (fetched, ()) = tokio::join!(
            pair.peer.get_hashes(block::Height(0), 2),
            serve_one(&pair.raw, 0, 2, |_| body),
        );
        assert!(
            matches!(
                fetched,
                Err(SyncPeerError::Violation {
                    code: ErrorCode::ProtocolError,
                    ..
                })
            ),
            "got {fetched:?}"
        );
        assert_closed_with_protocol_error(&pair.raw).await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A response longer than any conforming response is a violation, read no
/// further than the bound.
#[tokio::test]
async fn overlong_response_closes_the_connection() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let pair = fixture().await;
        let body = vec![0; crate::protocol::v2::response::hashes::MAX_HASHES_RESPONSE_LEN + 1];

        let (fetched, ()) = tokio::join!(
            pair.peer.get_hashes(block::Height(0), 1),
            serve_one(&pair.raw, 0, 1, |_| body),
        );
        assert!(
            matches!(fetched, Err(SyncPeerError::Violation { .. })),
            "got {fetched:?}"
        );
        assert_closed_with_protocol_error(&pair.raw).await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Waits for the node to close `server`'s connection with `PROTOCOL_ERROR`.
async fn assert_closed_with_protocol_error(server: &Connection) {
    match server.closed().await {
        ConnectionError::ApplicationClosed(close) => {
            assert_eq!(close.error_code, app_error_code(ErrorCode::ProtocolError));
        }
        error => panic!("expected an application close, got {error:?}"),
    }
}
