//! Loopback tests for the version 2 handshake over QUIC.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use quinn::Connection;
use tokio::time::Instant;

use zebra_chain::{block, chain_tip::NoChainTip, parameters::Network};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{
        v2::{
            handshake::{handshake, HandshakeError, LocalIdentity, Role},
            nonces::NonceRegistry,
            tests::{connected_pair, identity, run_handshake},
        },
        ConnectedAddr, MinimumPeerVersion,
    },
    protocol::{
        external::types::{Nonce, PeerServices, Version},
        v2::{
            handshake::{InitRecord, INIT_RECORD_KIND},
            record::RecordCodec,
            types::{ErrorCode, StreamType},
        },
    },
    quic::app_error_code,
};

/// Every loopback handshake finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A valid `init` record for a raw test peer.
fn raw_init(nonce: u64, version: Version) -> InitRecord {
    InitRecord {
        version,
        services: PeerServices::NODE_NETWORK,
        nonce: Nonce(nonce),
        user_agent: "/raw-peer/".to_string(),
        start_height: block::Height(5),
        relay: false,
        announce: false,
        full_ids: false,
    }
}

/// Frames `payload` as one record.
fn record(payload: &[u8]) -> Bytes {
    RecordCodec::encode_record(payload).expect("small payloads are within the limit")
}

/// The bytes a raw peer writes on a handshake stream: the type byte, then
/// `rest`.
fn typed(rest: &[u8]) -> Vec<u8> {
    let mut bytes = vec![StreamType::Handshake.code()];
    bytes.extend_from_slice(rest);
    bytes
}

/// The application error code a peer observed when `connection` closed.
async fn closed_with(connection: &Connection) -> Option<u64> {
    match connection.closed().await {
        quinn::ConnectionError::ApplicationClosed(close) => Some(close.error_code.into_inner()),
        _ => None,
    }
}

/// Two nodes complete the handshake in both roles, see each other's
/// records, and negotiate the lower version.
#[tokio::test]
async fn handshake_completes_in_both_roles() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (outbound, inbound, _client, _server) = connected_pair(&network).await;
        let initiator = identity("/initiator/");
        let responder = identity("/responder/");
        let initiator_nonces = NonceRegistry::spawn(8);
        let responder_nonces = NonceRegistry::spawn(8);

        let (from_initiator, from_responder) = tokio::join!(
            run_handshake(
                &outbound,
                Role::Initiator,
                &network,
                &initiator,
                &initiator_nonces
            ),
            run_handshake(
                &inbound,
                Role::Responder,
                &network,
                &responder,
                &responder_nonces
            ),
        );
        let from_initiator = from_initiator.expect("the initiator completes");
        let from_responder = from_responder.expect("the responder completes");

        let seen_by_initiator = &from_initiator.connection_info;
        let seen_by_responder = &from_responder.connection_info;
        assert_eq!(seen_by_initiator.remote.user_agent(), "/responder/");
        assert_eq!(seen_by_responder.remote.user_agent(), "/initiator/");
        assert_eq!(
            seen_by_initiator.remote.version(),
            CURRENT_NETWORK_PROTOCOL_VERSION
        );
        assert_eq!(
            seen_by_initiator.negotiated_version,
            CURRENT_NETWORK_PROTOCOL_VERSION
        );
        assert!(seen_by_initiator.remote.relay());
        assert_eq!(
            seen_by_initiator.remote.services(),
            PeerServices::NODE_NETWORK
        );
        assert!(!seen_by_initiator.connected_addr.is_inbound());
        assert!(seen_by_responder.connected_addr.is_inbound());

        let responder_init = seen_by_initiator
            .remote
            .as_init_record()
            .expect("a v2 peer");
        let initiator_init = seen_by_responder
            .remote
            .as_init_record()
            .expect("a v2 peer");
        assert_ne!(initiator_init.nonce, responder_init.nonce);
        assert!(
            !initiator_init.announce && !initiator_init.full_ids,
            "compact blocks are not requested yet"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A node that connects to itself detects its own nonce and closes the
/// connection with `SELF_CONNECTION`.
#[tokio::test]
async fn self_connection_is_detected() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (outbound, inbound, _client, _server) = connected_pair(&network).await;
        let me = identity("/me/");
        let shared_nonces = NonceRegistry::spawn(8);

        let (from_initiator, from_responder) = tokio::join!(
            run_handshake(&outbound, Role::Initiator, &network, &me, &shared_nonces),
            run_handshake(&inbound, Role::Responder, &network, &me, &shared_nonces),
        );

        // Each side registers its nonce before sending it and nothing un-registers, so a side
        // that reads the other's init sees its own nonce. The first side to detect it closes
        // the connection, which can cut off its own init in flight, so the other side may see
        // the connection end instead.
        let errors = [from_initiator, from_responder]
            .map(|result| result.err().expect("a self-connection never completes"));
        let detected = errors
            .iter()
            .filter(|error| matches!(error, HandshakeError::SelfConnection))
            .count();
        assert!(detected >= 1, "got {errors:?}");
        assert_eq!(
            HandshakeError::SelfConnection.error_code(),
            ErrorCode::SelfConnection,
            "the code the detecting side closes with"
        );

        // A side that detected the self-connection closed the connection itself, unless the other
        // side's close arrived first; a side that did not was closed by the other, with the code.
        // Both sides can detect it, so `raw_peer_close_codes` checks the code on the wire too.
        for (connection, error) in [(&outbound, &errors[0]), (&inbound, &errors[1])] {
            let close = connection.closed().await;
            let by_peer = match &close {
                quinn::ConnectionError::ApplicationClosed(close) => {
                    Some(close.error_code.into_inner())
                }
                _ => None,
            };
            if matches!(error, HandshakeError::SelfConnection) {
                assert!(
                    matches!(close, quinn::ConnectionError::LocallyClosed)
                        || by_peer == Some(ErrorCode::SelfConnection.code()),
                    "got {close:?}"
                );
            } else {
                assert_eq!(by_peer, Some(ErrorCode::SelfConnection.code()), "{error:?}");
            }
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Unknown record kinds before the `init` record are ignored, and a stream
/// of another type opened before the handshake is refused without ending
/// the connection.
#[tokio::test]
async fn unknown_kinds_are_ignored_and_early_streams_refused() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (raw_peer, inbound, _client, _server) = connected_pair(&network).await;
        let responder = identity("/responder/");
        let nonces = NonceRegistry::spawn(8);

        let raw = async {
            // A request stream before the handshake: refused with REFUSED.
            let (mut early_send, mut early_recv) = raw_peer.open_bi().await.expect("opens");
            early_send
                .write_all(&[StreamType::GetAddr.code()])
                .await
                .expect("writes");

            let (mut send, recv) = raw_peer.open_bi().await.expect("opens");
            send.write_all(&[StreamType::Handshake.code()])
                .await
                .expect("writes");
            send.write_all(&record(&[0x7F, 0xAA]))
                .await
                .expect("writes");
            let init = raw_init(2, CURRENT_NETWORK_PROTOCOL_VERSION).encode();
            send.write_all(&record(&init)).await.expect("writes");

            let early_outcome = early_recv.read_to_end(16).await;
            let error = early_outcome.expect_err("the early stream was refused");
            assert!(
                matches!(
                    error,
                    quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
                        if code == app_error_code(ErrorCode::Refused)
                ),
                "got {error:?}"
            );

            // The responder's init arrives on the handshake stream.
            let mut framed = tokio_util::codec::FramedRead::new(recv, RecordCodec);
            let payload = framed.next().await.expect("a record").expect("well-formed");
            assert_eq!(payload[0], INIT_RECORD_KIND);
        };

        let (_, result) = tokio::join!(
            raw,
            run_handshake(&inbound, Role::Responder, &network, &responder, &nonces)
        );
        let completed = result.expect("the handshake completes despite the noise");
        assert_eq!(completed.connection_info.remote.user_agent(), "/raw-peer/");
        assert!(!completed.connection_info.remote.relay());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A peer that connects and sends nothing is closed when the deadline
/// passes.
#[tokio::test]
async fn silent_peer_times_out() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (raw_peer, inbound, _client, _server) = connected_pair(&network).await;
        let responder = identity("/responder/");
        let nonces = NonceRegistry::spawn(8);
        let mut minimum_peer_version = MinimumPeerVersion::new(NoChainTip, &network);

        let result = handshake(
            &inbound,
            ConnectedAddr::new_inbound_direct(inbound.remote_address().into()),
            Role::Responder,
            &responder,
            &mut minimum_peer_version,
            &nonces,
            Instant::now() + Duration::from_millis(200),
        )
        .await;
        let error = result.err().expect("a silent peer times out");
        assert!(matches!(error, HandshakeError::Timeout), "got {error:?}");
        assert_eq!(
            closed_with(&raw_peer).await,
            Some(ErrorCode::NoError.code())
        );

        // The initiator sends its init and waits for the responder's; a silent responder times
        // it out the same way.
        let (outbound, silent_responder, _client, _server) = connected_pair(&network).await;
        let initiator = identity("/initiator/");
        let result = handshake(
            &outbound,
            ConnectedAddr::new_outbound_direct(outbound.remote_address().into()),
            Role::Initiator,
            &initiator,
            &mut minimum_peer_version,
            &nonces,
            Instant::now() + Duration::from_millis(200),
        )
        .await;
        let error = result.err().expect("a silent responder times out");
        assert!(matches!(error, HandshakeError::Timeout), "got {error:?}");
        assert_eq!(
            closed_with(&silent_responder).await,
            Some(ErrorCode::NoError.code())
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// How a raw peer ends its handshake stream after writing its bytes.
enum End {
    /// Leaves the stream open.
    Open,
    /// Finishes its sending side.
    Finish,
    /// Resets its sending side with `CANCELLED`.
    Reset,
}

/// A raw peer's bytes on the handshake stream decide the error the
/// responder reports and the code it closes the connection with.
#[tokio::test]
async fn raw_peer_close_codes() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // A nonce the responder registered as its own, as if the raw peer were the responder itself.
    const OWN_NONCE: u64 = 0x5E1F;

    let cases: [(&str, Vec<u8>, End, fn(&HandshakeError) -> bool, ErrorCode); 7] = [
        (
            "obsolete init",
            typed(&record(&raw_init(1, Version(1)).encode())),
            End::Open,
            |error| matches!(error, HandshakeError::Obsolete { .. }),
            ErrorCode::Obsolete,
        ),
        (
            "own nonce",
            typed(&record(
                &raw_init(OWN_NONCE, CURRENT_NETWORK_PROTOCOL_VERSION).encode(),
            )),
            End::Open,
            |error| matches!(error, HandshakeError::SelfConnection),
            ErrorCode::SelfConnection,
        ),
        (
            "malformed init",
            typed(&record(&[INIT_RECORD_KIND, 1, 2, 3])),
            End::Open,
            |error| matches!(error, HandshakeError::Init(_)),
            ErrorCode::ProtocolError,
        ),
        // Intent to disconnect: nothing to blame.
        (
            "finished after the type byte",
            typed(&[]),
            End::Finish,
            |error| matches!(error, HandshakeError::StreamFinished),
            ErrorCode::NoError,
        ),
        (
            "finished before the type byte",
            Vec::new(),
            End::Finish,
            |error| matches!(error, HandshakeError::FinishedBeforeStreamType),
            ErrorCode::ProtocolError,
        ),
        // FLOOD, from the length prefix alone.
        (
            "oversized record prefix",
            typed(&u32::MAX.to_le_bytes()),
            End::Open,
            |error| matches!(error, HandshakeError::Record(_)),
            ErrorCode::Flood,
        ),
        // The peer abandoned the stream: a transport-level end.
        (
            "reset before the type byte",
            Vec::new(),
            End::Reset,
            |error| matches!(error, HandshakeError::Read(_)),
            ErrorCode::NoError,
        ),
    ];

    tokio::time::timeout(TEST_TIMEOUT, async {
        let responder = identity("/responder/");
        let nonces = NonceRegistry::spawn(8);
        nonces.register(Nonce(OWN_NONCE)).await.expect("registers");

        for (name, bytes, end, matches, code) in cases {
            let (raw_peer, inbound, _client, _server) = connected_pair(&network).await;
            let raw = async {
                let (mut send, _recv) = raw_peer.open_bi().await.expect("opens");
                send.write_all(&bytes).await.expect("writes");
                match end {
                    End::Open => {}
                    End::Finish => send.finish().expect("finishes"),
                    End::Reset => send
                        .reset(app_error_code(ErrorCode::Cancelled))
                        .expect("resets"),
                }
                closed_with(&raw_peer).await
            };

            let (close_code, result) = tokio::join!(
                raw,
                run_handshake(&inbound, Role::Responder, &network, &responder, &nonces)
            );
            let error = result
                .err()
                .unwrap_or_else(|| panic!("{name}: the handshake fails"));
            assert!(matches(&error), "{name}: got {error:?}");
            assert_eq!(close_code, Some(code.code()), "{name}");
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// While syncing, an outbound peer that does not advertise `NODE_NETWORK` is
/// declined with `NO_ERROR`; the same peer is accepted inbound.
#[tokio::test]
async fn non_serving_outbound_peers_are_declined_while_syncing() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // `NoChainTip` reports no tip, so the node counts as syncing.
        let us = identity("/us/");
        let non_serving = LocalIdentity {
            user_agent: "/light/".to_string(),
            services: PeerServices::empty(),
            relay: true,
        };

        let our_nonces = NonceRegistry::spawn(8);
        let their_nonces = NonceRegistry::spawn(8);

        // Outbound (we are the initiator): declined.
        let (outbound, inbound, _client, _server) = connected_pair(&network).await;
        let (ours, theirs) = tokio::join!(
            run_handshake(&outbound, Role::Initiator, &network, &us, &our_nonces),
            run_handshake(&inbound, Role::Responder, &network, &non_serving, &their_nonces),
        );
        let error = ours.err().expect("a non-serving outbound peer is declined");
        assert!(
            matches!(error, HandshakeError::MissingRequiredServices { services } if services.is_empty()),
            "got {error:?}"
        );
        assert_eq!(error.error_code(), ErrorCode::NoError);
        // The other side's handshake may complete before the close arrives; either way the
        // connection ends with NO_ERROR.
        drop(theirs);
        assert_eq!(closed_with(&inbound).await, Some(ErrorCode::NoError.code()));

        // Inbound (they are the initiator): accepted.
        let (outbound, inbound, _client, _server) = connected_pair(&network).await;
        let (theirs, ours) = tokio::join!(
            run_handshake(&outbound, Role::Initiator, &network, &non_serving, &their_nonces),
            run_handshake(&inbound, Role::Responder, &network, &us, &our_nonces),
        );
        theirs.expect("the initiator completes");
        let ours = ours.expect("a non-serving inbound peer is accepted");
        assert_eq!(ours.connection_info.remote.services(), PeerServices::empty());
    })
    .await
    .expect("the test completes well within the timeout");
}
