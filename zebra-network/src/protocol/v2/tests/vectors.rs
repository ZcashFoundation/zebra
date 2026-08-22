//! Fixed test vectors for the version 2 protocol wire formats.

use std::{sync::Arc, time::Duration};

use zebra_chain::{
    block::{self, Block},
    serialization::{DateTime32, ZcashDeserializeInto, ZcashSerialize},
    transaction::{AuthDigest, Transaction, UnminedTxId, WtxId},
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::types::{Nonce, PeerServices, Version},
};

use super::{assert_flood_error, assert_protocol_error};

use super::super::{
    compact_block::{
        self, full_transaction_id, short_id_keys, short_transaction_id, CompactBlock,
        CompactBlockIds, ShortTxId,
    },
    constants::{
        MAX_GET_BLOCKS_HASHES, MAX_GET_BLOCK_RANGE_BYTES, MAX_GET_BLOCK_RANGE_COUNT,
        MAX_GET_HASHES_COUNT, MAX_GET_OBJECT_LENGTH, MAX_GET_TREE_ROOTS_COUNT, MAX_LOCATOR_HASHES,
        MAX_MEMPOOL_RESPONSE_REFS, MAX_PREFILLED_TX_INDEX, MAX_RECORD_PAYLOAD_LEN,
    },
    init::{HandshakeRecord, InitRecord},
    record,
    request::Request,
    response::{
        encode_result_entry, read_result_entry, AddrResponse, HeadersResponse, MempoolResponse,
    },
    txref::TransactionReference,
    types::{ErrorCode, ObjectHash, StreamType, WireError},
};

#[test]
fn stream_type_round_trip() {
    let _init_guard = zebra_test::init();

    for byte in 0..=u8::MAX {
        if let Some(stream_type) = StreamType::from_byte(byte) {
            assert_eq!(stream_type.byte(), byte);
            let is_bidirectional = stream_type == StreamType::Handshake || stream_type.is_request();
            assert_eq!(is_bidirectional, !stream_type.is_announcement());
        }
    }

    assert_eq!(StreamType::from_byte(0x0A), None);
    assert_eq!(StreamType::from_byte(0x13), None);
    assert_eq!(StreamType::from_byte(0xFF), None);
}

#[test]
fn error_code_unknown_is_internal_error() {
    let _init_guard = zebra_test::init();

    for code in 0x00..=0x09u64 {
        assert_eq!(ErrorCode::from_wire(code).wire_code(), code);
    }

    assert_eq!(ErrorCode::from_wire(0x0A), ErrorCode::InternalError);
    assert_eq!(ErrorCode::from_wire(u64::MAX), ErrorCode::InternalError);
}

#[tokio::test]
async fn compact_size_round_trip() {
    let _init_guard = zebra_test::init();

    for value in [
        0u64,
        1,
        0xFC,
        0xFD,
        0xFFFF,
        0x1_0000,
        0xFFFF_FFFF,
        0x1_0000_0000,
        u64::MAX,
    ] {
        let mut buf = Vec::new();
        record::write_compact_size(&mut buf, value).expect("write to Vec succeeds");

        let mut reader = buf.as_slice();
        let read = record::read_compact_size(&mut reader)
            .await
            .expect("canonical encoding parses");
        assert_eq!(read, value);
        assert!(reader.is_empty(), "no trailing bytes for {value}");
    }
}

#[tokio::test]
async fn compact_size_rejects_non_canonical() {
    let _init_guard = zebra_test::init();

    // 0xFC encoded with a 0xFD prefix, 0xFFFF with a 0xFE prefix,
    // and 0xFFFF_FFFF with a 0xFF prefix.
    for bytes in [
        &[0xFDu8, 0xFC, 0x00][..],
        &[0xFE, 0xFF, 0xFF, 0x00, 0x00][..],
        &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00][..],
    ] {
        let mut reader = bytes;
        let result = record::read_compact_size(&mut reader).await;
        assert_protocol_error(&result, "non-canonical encoding {bytes:?} must be rejected");
    }
}

#[tokio::test]
async fn record_round_trip() {
    let _init_guard = zebra_test::init();

    for payload in [&[][..], &[0x42][..], &[0xAB; 300][..]] {
        let mut buf = Vec::new();
        record::write_record(&mut buf, payload).expect("write to Vec succeeds");

        let mut reader = buf.as_slice();
        let read = record::read_record(&mut reader)
            .await
            .expect("record parses")
            .expect("record is present");
        assert_eq!(read, payload);
    }
}

#[tokio::test(start_paused = true)]
async fn record_read_times_out_on_a_partial_record() {
    use tokio::io::AsyncWriteExt;

    let _init_guard = zebra_test::init();

    let (mut peer, mut reader) = tokio::io::duplex(64);

    // A length prefix with no payload: the peer started a record, then
    // stalled. The stream stays open, as it would with transport keep-alives.
    peer.write_all(&[0x04, 0x00]).await.expect("write succeeds");

    let result = record::read_record_timeout(&mut reader, Duration::from_secs(30)).await;
    assert!(
        matches!(result, Err(WireError::Timeout(_))),
        "got: {result:?}",
    );
}

#[tokio::test(start_paused = true)]
async fn record_read_waits_for_an_idle_stream() {
    let _init_guard = zebra_test::init();

    // Announcement and handshake streams idle between records, so a stream
    // that has not started a record must not time out.
    let (_peer, mut reader) = tokio::io::duplex(64);

    let result = tokio::time::timeout(
        Duration::from_secs(3600),
        record::read_record_timeout(&mut reader, Duration::from_secs(30)),
    )
    .await;
    assert!(result.is_err(), "an idle stream must not time out");
}

#[tokio::test]
async fn record_stream_end_and_errors() {
    let _init_guard = zebra_test::init();

    // A finished stream at a record boundary is a clean end.
    let mut reader = &[][..];
    assert!(record::read_record(&mut reader)
        .await
        .expect("clean end of stream")
        .is_none());

    // A length prefix over the payload limit is a FLOOD error.
    let mut over_limit = Vec::new();
    record::write_compact_size(&mut over_limit, MAX_RECORD_PAYLOAD_LEN as u64 + 1)
        .expect("write to Vec succeeds");
    let mut reader = over_limit.as_slice();
    let result = record::read_record(&mut reader).await;
    assert_flood_error(&result, "the encoding");
    assert_eq!(
        result.unwrap_err().connection_error_code(),
        Some(ErrorCode::Flood),
    );

    // A stream finished in the middle of a record is a PROTOCOL_ERROR.
    let mut truncated = Vec::new();
    record::write_record(&mut truncated, &[0xAB; 100]).expect("write to Vec succeeds");
    truncated.truncate(50);
    let mut reader = truncated.as_slice();
    let result = record::read_record(&mut reader).await;
    assert_protocol_error(&result, "the encoding");

    // Trailing data after a complete element is rejected by the
    // end-of-stream check.
    let mut reader = &[0x00][..];
    let result = record::expect_end_of_stream(&mut reader).await;
    assert_protocol_error(&result, "the encoding");
    let mut reader = &[][..];
    record::expect_end_of_stream(&mut reader)
        .await
        .expect("empty stream passes the end-of-stream check");
}

fn test_init_record() -> InitRecord {
    InitRecord {
        version: Version(170_160),
        services: PeerServices::NODE_NETWORK,
        nonce: Nonce(0x0123_4567_89AB_CDEF),
        user_agent: "/ZebraV2:1.0.0/".to_string(),
        start_height: block::Height(2_000_000),
        relay: true,
        announce: false,
        full_ids: false,
    }
}

#[tokio::test]
async fn init_record_round_trip() {
    let _init_guard = zebra_test::init();

    let init = test_init_record();

    let mut buf = Vec::new();
    init.write(&mut buf).await.expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = InitRecord::read(&mut reader)
        .await
        .expect("record parses")
        .expect("record is present");
    assert_eq!(read, init);
}

#[tokio::test]
async fn init_record_skips_unknown_kinds() {
    let _init_guard = zebra_test::init();

    let init = test_init_record();

    // An unknown record kind before the init record is ignored.
    let mut buf = Vec::new();
    record::write_record(&mut buf, &[0x7F, 0xAA, 0xBB]).expect("write to Vec succeeds");
    init.write(&mut buf).await.expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = InitRecord::read(&mut reader)
        .await
        .expect("record parses")
        .expect("record is present");
    assert_eq!(read, init);

    // A stream finished before any init record is a clean end.
    let mut buf = Vec::new();
    record::write_record(&mut buf, &[0x7F]).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    assert!(InitRecord::read(&mut reader)
        .await
        .expect("records parse")
        .is_none());
}

#[test]
fn init_record_rejects_invalid_fields() {
    let _init_guard = zebra_test::init();

    let init = test_init_record();

    // A flag field that is not 0 or 1 is rejected.
    let mut payload = init.to_record_payload();
    *payload.last_mut().expect("payload is not empty") = 2;
    let result = HandshakeRecord::parse(&payload);
    assert_protocol_error(&result, "the encoding");

    // Trailing data after the init fields is rejected.
    let mut payload = init.to_record_payload();
    payload.push(0x00);
    let result = HandshakeRecord::parse(&payload);
    assert_protocol_error(&result, "the encoding");

    // An empty handshake record is rejected.
    let result = HandshakeRecord::parse(&[]);
    assert_protocol_error(&result, "the encoding");

    // An over-long user agent is rejected.
    let long_agent = InitRecord {
        user_agent: "x".repeat(257),
        ..init
    };
    let result = HandshakeRecord::parse(&long_agent.to_record_payload());
    assert_protocol_error(&result, "the encoding");
}

fn test_txrefs() -> Vec<TransactionReference> {
    vec![
        TransactionReference::Txid(zebra_chain::transaction::Hash([0x11; 32])),
        TransactionReference::Wtxid(WtxId {
            id: zebra_chain::transaction::Hash([0x22; 32]),
            auth_digest: AuthDigest([0x33; 32]),
        }),
        TransactionReference::ShortId {
            block_hash: block::Hash([0x44; 32]),
            short_id: ShortTxId([1, 2, 3, 4, 5, 6]),
        },
    ]
}

#[tokio::test]
async fn transaction_reference_round_trip() {
    let _init_guard = zebra_test::init();

    for txref in test_txrefs() {
        let mut buf = Vec::new();
        txref.encode(&mut buf).expect("write to Vec succeeds");

        let read = TransactionReference::parse_exact(&buf)
            .await
            .expect("reference parses");
        assert_eq!(read, txref);
    }

    // An unrecognized reference type is rejected.
    let result = TransactionReference::parse_exact(&[0x04; 33]).await;
    assert_protocol_error(&result, "the encoding");

    // Trailing data is rejected.
    let mut buf = Vec::new();
    test_txrefs()[0]
        .encode(&mut buf)
        .expect("write to Vec succeeds");
    buf.push(0x00);
    let result = TransactionReference::parse_exact(&buf).await;
    assert_protocol_error(&result, "the encoding");
}

#[test]
fn full_transaction_id_construction() {
    let _init_guard = zebra_test::init();

    let txid = zebra_chain::transaction::Hash([0xAA; 32]);
    let legacy = UnminedTxId::Legacy(txid);
    let full = full_transaction_id(&legacy);
    assert_eq!(full.id, txid);
    assert_eq!(
        full.auth_digest,
        zebra_chain::block::merkle::AUTH_DIGEST_PLACEHOLDER,
    );

    let wtxid = WtxId {
        id: zebra_chain::transaction::Hash([0xBB; 32]),
        auth_digest: AuthDigest([0xCC; 32]),
    };
    let full = full_transaction_id(&UnminedTxId::Witnessed(wtxid));
    assert_eq!(full, wtxid);
}

/// Checks the short transaction ID computation against a manual
/// reimplementation of the specified steps.
#[test]
fn short_transaction_id_computation() {
    let _init_guard = zebra_test::init();

    use sha2::{Digest, Sha256};
    use std::hash::Hasher;

    let header_bytes = [0x5A; 100];
    let nonce = 0xDEAD_BEEF_0BAD_F00Du64;

    // SHA-256 of the header bytes followed by the little-endian nonce.
    let mut hasher = Sha256::new();
    hasher.update(header_bytes);
    hasher.update(nonce.to_le_bytes());
    let digest = hasher.finalize();
    let expected_k0 = u64::from_le_bytes(digest[0..8].try_into().unwrap());
    let expected_k1 = u64::from_le_bytes(digest[8..16].try_into().unwrap());

    let (k0, k1) = short_id_keys(&header_bytes, nonce);
    assert_eq!((k0, k1), (expected_k0, expected_k1));

    // SipHash-2-4 of the wtxid, truncated to the low 6 bytes, little-endian.
    let wtxid = WtxId {
        id: zebra_chain::transaction::Hash([0x77; 32]),
        auth_digest: AuthDigest([0x88; 32]),
    };
    let mut hasher = siphasher::sip::SipHasher24::new_with_keys(k0, k1);
    hasher.write(&wtxid.as_bytes());
    let expected = hasher.finish().to_le_bytes();

    let short_id = short_transaction_id(k0, k1, &UnminedTxId::Witnessed(wtxid));
    assert_eq!(short_id.0, expected[0..6]);

    // A legacy transaction hashes its 32-byte txid instead.
    let txid = zebra_chain::transaction::Hash([0x99; 32]);
    let mut hasher = siphasher::sip::SipHasher24::new_with_keys(k0, k1);
    hasher.write(&txid.0);
    let expected = hasher.finish().to_le_bytes();

    let short_id = short_transaction_id(k0, k1, &UnminedTxId::Legacy(txid));
    assert_eq!(short_id.0, expected[0..6]);
}

/// Returns a parsed mainnet block test vector with at least 3 transactions.
fn test_block() -> Arc<Block> {
    zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .map(|bytes| {
            bytes
                .zcash_deserialize_into::<Block>()
                .expect("block test vectors parse")
        })
        .find(|block| block.transactions.len() >= 3)
        .expect("some mainnet block test vector has at least 3 transactions")
        .into()
}

#[tokio::test]
async fn compact_block_round_trip_short_ids() {
    let _init_guard = zebra_test::init();

    let block = test_block();
    let compact = CompactBlock::from_block(&block, 0x1122_3344_5566_7788, false, &[])
        .expect("compact block builds");

    // Only the coinbase is prefilled.
    assert_eq!(compact.prefilled.len(), 1);
    assert_eq!(compact.prefilled[0].index, 0);
    let expected_ids = block.transactions.len() - 1;
    assert!(matches!(&compact.ids, CompactBlockIds::Short(ids) if ids.len() == expected_ids));

    let mut buf = Vec::new();
    compact.encode(&mut buf).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = CompactBlock::read(&mut reader)
        .await
        .expect("compact block parses");
    assert_eq!(read, compact);
    assert!(reader.is_empty());
}

#[tokio::test]
async fn compact_block_round_trip_full_ids() {
    let _init_guard = zebra_test::init();

    let block = test_block();
    // Prefill the second transaction as well as the coinbase.
    let compact = CompactBlock::from_block(&block, 0, true, &[1]).expect("compact block builds");

    assert_eq!(compact.prefilled.len(), 2);
    assert_eq!(compact.nonce, 0);
    let expected_ids = block.transactions.len() - 2;
    assert!(matches!(&compact.ids, CompactBlockIds::Full(ids) if ids.len() == expected_ids));

    let mut buf = Vec::new();
    compact.encode(&mut buf).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = CompactBlock::read(&mut reader)
        .await
        .expect("compact block parses");
    assert_eq!(read, compact);
}

#[tokio::test]
async fn compact_block_rejects_invalid() {
    let _init_guard = zebra_test::init();

    let block = test_block();
    let compact = CompactBlock::from_block(&block, 1, false, &[]).expect("compact block builds");

    let mut buf = Vec::new();
    compact.encode(&mut buf).expect("write to Vec succeeds");

    // Corrupt the ids_kind byte: header record || nonce (8) || ids_kind.
    let header_len = {
        let header_bytes = compact
            .header
            .zcash_serialize_to_vec()
            .expect("serializing a header to a Vec never fails");
        // 3-byte CompactSize prefix for a header-sized record.
        3 + header_bytes.len()
    };
    let ids_kind_offset = header_len + 8;

    let mut bad_kind = buf.clone();
    bad_kind[ids_kind_offset] = 0x02;
    let mut reader = bad_kind.as_slice();
    let result = CompactBlock::read(&mut reader).await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn compact_block_rejects_bad_prefilled_indexes() {
    let _init_guard = zebra_test::init();

    let block = test_block();

    // Duplicate prefilled indexes cannot be encoded, and are rejected when
    // decoding: encode a compact block, then check that decoding validates
    // the differential encoding by constructing one with an index over the
    // limit.
    let mut compact =
        CompactBlock::from_block(&block, 1, false, &[1]).expect("compact block builds");
    compact.prefilled[1].index = MAX_PREFILLED_TX_INDEX + 1;

    let mut buf = Vec::new();
    compact.encode(&mut buf).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let result = CompactBlock::read(&mut reader).await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn compact_block_rejects_overflowing_transaction_counts() {
    let _init_guard = zebra_test::init();

    let block = test_block();
    let compact = CompactBlock::from_block(&block, 1, false, &[]).expect("compact block builds");
    let header_bytes = compact
        .header
        .zcash_serialize_to_vec()
        .expect("serializing a header to a Vec never fails");

    // A `prefilled_count` that overflows `ids_count + prefilled_count` must be
    // rejected as a flood, rather than wrapping past the limit check and
    // reaching the prefilled transaction preallocation.
    let mut buf = Vec::new();
    record::write_record(&mut buf, &header_bytes).expect("write to Vec succeeds");
    buf.extend_from_slice(&0u64.to_le_bytes());
    buf.push(compact_block::IDS_KIND_SHORT);
    record::write_compact_size(&mut buf, 1).expect("write to Vec succeeds");
    buf.extend_from_slice(&[0u8; 6]);
    record::write_compact_size(&mut buf, u64::MAX).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let result = CompactBlock::read(&mut reader).await;
    assert_flood_error(&result, "the encoding");
}

#[tokio::test]
async fn request_round_trips() {
    let _init_guard = zebra_test::init();

    let requests = vec![
        Request::GetHeaders {
            known_blocks: vec![block::Hash([0x01; 32]), block::Hash([0x02; 32])],
            stop: Some(block::Hash([0x03; 32])),
            tx_ids: false,
        },
        Request::GetHeaders {
            known_blocks: vec![],
            stop: None,
            tx_ids: true,
        },
        Request::GetBlocks {
            hashes: vec![block::Hash([0x04; 32]), block::Hash([0x05; 32])],
        },
        Request::GetTx {
            refs: test_txrefs(),
        },
        Request::GetAddr,
        Request::GetMempool,
        Request::GetHashes {
            start_height: 0,
            stride: 1,
            count: 50_000,
        },
        Request::GetHashes {
            start_height: u32::MAX,
            stride: u32::MAX,
            count: 1,
        },
        Request::GetHashes {
            start_height: 419_200,
            stride: 400,
            count: 0,
        },
        Request::GetBlockRange {
            final_hash: block::Hash([0x06; 32]),
            count: 65_536,
            max_bytes: 67_108_864,
        },
        Request::GetTreeRoots {
            start_height: 1_046_400,
            final_hash: block::Hash([0x07; 32]),
            count: 4_000,
        },
        Request::GetObject {
            hash: ObjectHash([0x08; 32]),
            offset: u64::MAX,
            length: 33_554_432,
        },
    ];

    for request in requests {
        let mut buf = Vec::new();
        request.encode(&mut buf).expect("write to Vec succeeds");

        let mut reader = buf.as_slice();
        let read = Request::read(request.stream_type(), &mut reader)
            .await
            .expect("request parses");
        assert_eq!(read, request);

        record::expect_end_of_stream(&mut reader)
            .await
            .expect("request reads consume the whole request");
    }
}

#[tokio::test]
async fn request_rejects_over_limit() {
    let _init_guard = zebra_test::init();

    // A locator with more than the maximum hashes is rejected on encode
    // and on decode.
    let over_limit = Request::GetHeaders {
        known_blocks: vec![block::Hash([0; 32]); MAX_LOCATOR_HASHES as usize + 1],
        stop: None,
        tx_ids: false,
    };
    // Encoding fails locally, so it must not be classified as a peer
    // violation: it would close a healthy connection.
    let mut buf = Vec::new();
    let result = over_limit.encode(&mut buf);
    assert!(
        matches!(result, Err(WireError::Local(_))),
        "got: {result:?}"
    );

    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, MAX_LOCATOR_HASHES + 1).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let result = Request::read(StreamType::GetHeaders, &mut reader).await;
    assert_protocol_error(&result, "the encoding");

    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, MAX_GET_BLOCKS_HASHES + 1).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let result = Request::read(StreamType::GetBlocks, &mut reader).await;
    assert_protocol_error(&result, "the encoding");
}

/// The synchronization request encodings are pinned to the draft's field
/// tables: heights and strides are little-endian `u32`s, hashes are raw
/// 32-byte values, and counts and sizes are CompactSizes.
#[tokio::test]
async fn sync_request_encodings_match_the_draft() {
    let _init_guard = zebra_test::init();

    let cases: Vec<(Request, Vec<u8>)> = vec![
        (
            Request::GetHashes {
                start_height: 419_200,
                stride: 400,
                count: 3,
            },
            [
                &419_200u32.to_le_bytes()[..],
                &400u32.to_le_bytes()[..],
                &[0x03][..],
            ]
            .concat(),
        ),
        (
            Request::GetBlockRange {
                final_hash: block::Hash([0xCD; 32]),
                count: 300,
                max_bytes: 16_777_216,
            },
            [
                &[0xCD; 32][..],
                &[0xFD, 0x2C, 0x01][..],
                &[0xFE, 0x00, 0x00, 0x00, 0x01][..],
            ]
            .concat(),
        ),
        (
            Request::GetTreeRoots {
                start_height: 500_000,
                final_hash: block::Hash([0xAB; 32]),
                count: 2,
            },
            [&500_000u32.to_le_bytes()[..], &[0xAB; 32][..], &[0x02][..]].concat(),
        ),
        (
            Request::GetObject {
                hash: ObjectHash([0xEF; 32]),
                offset: 253,
                length: 1_000,
            },
            [
                &[0xEF; 32][..],
                &[0xFD, 0xFD, 0x00][..],
                &[0xFD, 0xE8, 0x03][..],
            ]
            .concat(),
        ),
    ];

    for (request, expected) in cases {
        let mut buf = Vec::new();
        request.encode(&mut buf).expect("write to Vec succeeds");
        assert_eq!(buf, expected, "encoding of {request:?}");

        let mut reader = buf.as_slice();
        let read = Request::read(request.stream_type(), &mut reader)
            .await
            .expect("request parses");
        assert_eq!(read, request);
        record::expect_end_of_stream(&mut reader)
            .await
            .expect("request reads consume the whole request");
    }
}

/// Requests violating the synchronization stream types' bounds are local
/// errors on the send side and connection errors on the receive side.
#[tokio::test]
async fn sync_request_bounds_are_enforced() {
    let _init_guard = zebra_test::init();

    let over_limit = vec![
        // A stride of 0 is invalid.
        Request::GetHashes {
            start_height: 0,
            stride: 0,
            count: 1,
        },
        Request::GetHashes {
            start_height: 0,
            stride: 1,
            count: MAX_GET_HASHES_COUNT + 1,
        },
        // The greatest requested height must not exceed `u32::MAX`:
        // this request's second hash would be above it.
        Request::GetHashes {
            start_height: u32::MAX,
            stride: 1,
            count: 2,
        },
        Request::GetBlockRange {
            final_hash: block::Hash([0; 32]),
            count: MAX_GET_BLOCK_RANGE_COUNT + 1,
            max_bytes: 0,
        },
        Request::GetBlockRange {
            final_hash: block::Hash([0; 32]),
            count: 0,
            max_bytes: MAX_GET_BLOCK_RANGE_BYTES + 1,
        },
        Request::GetTreeRoots {
            start_height: 0,
            final_hash: block::Hash([0; 32]),
            count: MAX_GET_TREE_ROOTS_COUNT + 1,
        },
        Request::GetObject {
            hash: ObjectHash([0; 32]),
            offset: 0,
            length: MAX_GET_OBJECT_LENGTH + 1,
        },
    ];

    for request in over_limit {
        // Encoding fails locally, so it must not be classified as a peer
        // violation: it would close a healthy connection.
        let mut buf = Vec::new();
        let result = request.encode(&mut buf);
        assert!(
            matches!(result, Err(WireError::Local(_))),
            "encoding {request:?} got: {result:?}",
        );

        // Reading the same request from a peer is a connection error of
        // type `PROTOCOL_ERROR`. The invalid fields must be encoded
        // directly, since `encode` refuses to build them.
        let mut buf = Vec::new();
        match &request {
            Request::GetHashes {
                start_height,
                stride,
                count,
            } => {
                buf.extend_from_slice(&start_height.to_le_bytes());
                buf.extend_from_slice(&stride.to_le_bytes());
                record::write_compact_size(&mut buf, *count).expect("write to Vec succeeds");
            }
            Request::GetBlockRange {
                final_hash,
                count,
                max_bytes,
            } => {
                buf.extend_from_slice(&final_hash.0);
                record::write_compact_size(&mut buf, *count).expect("write to Vec succeeds");
                record::write_compact_size(&mut buf, *max_bytes).expect("write to Vec succeeds");
            }
            Request::GetTreeRoots {
                start_height,
                final_hash,
                count,
            } => {
                buf.extend_from_slice(&start_height.to_le_bytes());
                buf.extend_from_slice(&final_hash.0);
                record::write_compact_size(&mut buf, *count).expect("write to Vec succeeds");
            }
            Request::GetObject {
                hash,
                offset,
                length,
            } => {
                buf.extend_from_slice(&hash.0);
                record::write_compact_size(&mut buf, *offset).expect("write to Vec succeeds");
                record::write_compact_size(&mut buf, *length).expect("write to Vec succeeds");
            }
            other => unreachable!("only sync requests are tested here, got: {other:?}"),
        }

        let mut reader = buf.as_slice();
        let result = Request::read(request.stream_type(), &mut reader).await;
        assert_protocol_error(&result, "reading {request:?} got:");
    }

    // A request truncated mid-element is a connection error of type
    // `PROTOCOL_ERROR`.
    let mut reader = &500_000u32.to_le_bytes()[..2];
    let result = Request::read(StreamType::GetHashes, &mut reader).await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn headers_response_round_trip_and_contiguity() {
    let _init_guard = zebra_test::init();

    let block_1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector parses")
        .into();
    let block_2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector parses")
        .into();

    let headers = HeadersResponse(vec![
        block::CountedHeader {
            header: block_1.header.clone(),
        },
        block::CountedHeader {
            header: block_2.header.clone(),
        },
    ]);

    let mut buf = Vec::new();
    headers.encode(&mut buf).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = HeadersResponse::read(&mut reader)
        .await
        .expect("headers parse");
    assert_eq!(read.0.len(), 2);
    assert_eq!(read.0[0].header, block_1.header);
    assert_eq!(read.0[1].header, block_2.header);

    let hashes = read
        .check_contiguous()
        .expect("blocks 1 and 2 are contiguous");
    assert_eq!(hashes, vec![block_1.hash(), block_2.hash()]);

    // Reversed headers are not contiguous.
    let reversed = HeadersResponse(vec![
        block::CountedHeader {
            header: block_2.header.clone(),
        },
        block::CountedHeader {
            header: block_1.header.clone(),
        },
    ]);
    let result = reversed.check_contiguous();
    assert!(
        matches!(result, Err(WireError::Misbehavior { .. })),
        "got: {result:?}",
    );
}

#[tokio::test]
async fn sync_response_round_trips() {
    use zebra_chain::block::{SyncHashEntry, TreeRootsEntry};

    use super::super::response::{HashesResponse, TreeRootsResponse};

    let _init_guard = zebra_test::init();

    let hashes = HashesResponse(vec![
        SyncHashEntry {
            hash: block::Hash([0x11; 32]),
            span_size: 1,
            span_txs: 1,
            span_notes: 0,
        },
        SyncHashEntry {
            hash: block::Hash([0x22; 32]),
            span_size: 400,
            span_txs: 12_345,
            span_notes: 0xFFFF_FFFF,
        },
    ]);
    let mut buf = Vec::new();
    hashes.encode(&mut buf).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let read = HashesResponse::read(&mut reader)
        .await
        .expect("entries parse");
    assert_eq!(read.0, hashes.0);
    assert!(reader.is_empty());

    // An empty response is valid: a peer without matching heights.
    let mut buf = Vec::new();
    HashesResponse(Vec::new())
        .encode(&mut buf)
        .expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let read = HashesResponse::read(&mut reader)
        .await
        .expect("empty response parses");
    assert!(read.0.is_empty());

    let roots = TreeRootsResponse(vec![
        TreeRootsEntry {
            sapling_root: [0x0A; 32],
            orchard_root: [0; 32],
            ironwood_root: [0; 32],
            sapling_txs: 3,
            orchard_txs: 0,
            ironwood_txs: 0,
            auth_data_root: [0xFF; 32],
        },
        TreeRootsEntry {
            sapling_root: [0x0B; 32],
            orchard_root: [0x0C; 32],
            ironwood_root: [0x0D; 32],
            sapling_txs: 1,
            orchard_txs: 2,
            ironwood_txs: 3,
            auth_data_root: [0xEE; 32],
        },
    ]);
    let mut buf = Vec::new();
    roots.encode(&mut buf).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let read = TreeRootsResponse::read(&mut reader)
        .await
        .expect("entries parse");
    assert_eq!(read.0, roots.0);
    assert!(reader.is_empty());

    // Counts over the request limits are rejected while reading.
    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, MAX_GET_HASHES_COUNT + 1).expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let result = HashesResponse::read(&mut reader).await;
    assert_protocol_error(&result, "the encoding");

    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, MAX_GET_TREE_ROOTS_COUNT + 1)
        .expect("write to Vec succeeds");
    let mut reader = buf.as_slice();
    let result = TreeRootsResponse::read(&mut reader).await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn block_response_entries_round_trip() {
    let _init_guard = zebra_test::init();

    let block = test_block();

    let mut buf = Vec::new();
    encode_result_entry(&mut buf, Some(block.as_ref())).expect("write to Vec succeeds");
    encode_result_entry::<Block, _>(&mut buf, None).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read_full = read_result_entry::<Block, _>(&mut reader, "get-blocks")
        .await
        .expect("entry parses");
    assert_eq!(read_full.as_deref(), Some(block.as_ref()));
    let read_missing = read_result_entry::<Block, _>(&mut reader, "get-blocks")
        .await
        .expect("entry parses");
    assert!(read_missing.is_none());
    assert!(reader.is_empty());

    // The compact block result byte of earlier draft revisions was removed:
    // compact blocks occur only as announcements, so `0x01` is unrecognized.
    let mut reader = &[0x01][..];
    let result = read_result_entry::<Block, _>(&mut reader, "get-blocks").await;
    assert_protocol_error(&result, "the encoding");

    // An unrecognized result byte is rejected.
    let mut reader = &[0x03][..];
    let result = read_result_entry::<Block, _>(&mut reader, "get-blocks").await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn tx_response_entries_round_trip() {
    let _init_guard = zebra_test::init();

    let block = test_block();
    let tx = block.transactions[1].clone();

    let mut buf = Vec::new();
    encode_result_entry(&mut buf, Some(tx.as_ref())).expect("write to Vec succeeds");
    encode_result_entry::<Transaction, _>(&mut buf, None).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read_found = read_result_entry::<Transaction, _>(&mut reader, "get-tx")
        .await
        .expect("entry parses");
    assert_eq!(read_found.as_deref(), Some(tx.as_ref()));
    let read_missing = read_result_entry::<Transaction, _>(&mut reader, "get-tx")
        .await
        .expect("entry parses");
    assert!(read_missing.is_none());
    assert!(reader.is_empty());

    // A compact block result byte is not valid in a get-tx response.
    let mut reader = &[0x01][..];
    let result = read_result_entry::<Transaction, _>(&mut reader, "get-tx").await;
    assert_protocol_error(&result, "the encoding");
}

#[tokio::test]
async fn addr_response_round_trip() {
    let _init_guard = zebra_test::init();

    let addrs = vec![
        MetaAddr::new_gossiped_meta_addr(
            "192.0.2.1:8233".parse().expect("valid address"),
            PeerServices::NODE_NETWORK,
            DateTime32::from(1_700_000_000),
        ),
        MetaAddr::new_gossiped_meta_addr(
            "[2001:db8::1]:8233".parse().expect("valid address"),
            PeerServices::NODE_NETWORK,
            DateTime32::from(1_700_000_100),
        ),
    ];

    let response = AddrResponse(addrs.clone());

    let mut buf = Vec::new();
    response.encode(&mut buf).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = AddrResponse::read(&mut reader)
        .await
        .expect("addresses parse");
    assert_eq!(read.0.len(), addrs.len());
    for (read, sent) in read.0.iter().zip(&addrs) {
        assert_eq!(read.addr(), sent.addr());
    }
    assert!(reader.is_empty());
}

#[tokio::test]
async fn mempool_response_round_trip() {
    let _init_guard = zebra_test::init();

    let refs: Vec<TransactionReference> = test_txrefs()
        .into_iter()
        .filter(|txref| !txref.is_short_id())
        .collect();

    let response = MempoolResponse(refs.clone());

    let mut buf = Vec::new();
    response.encode(&mut buf).expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let read = MempoolResponse::read(&mut reader)
        .await
        .expect("references parse");
    assert_eq!(read.0, refs);

    // A SHORTID reference in a get-mempool response is rejected.
    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, 1).expect("write to Vec succeeds");
    TransactionReference::ShortId {
        block_hash: block::Hash([0; 32]),
        short_id: ShortTxId([0; 6]),
    }
    .encode(&mut buf)
    .expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let result = MempoolResponse::read(&mut reader).await;
    assert_protocol_error(&result, "the encoding");

    // A reference count over the response limit is a FLOOD connection
    // error, before any references are read.
    let mut buf = Vec::new();
    record::write_compact_size(&mut buf, MAX_MEMPOOL_RESPONSE_REFS as u64 + 1)
        .expect("write to Vec succeeds");

    let mut reader = buf.as_slice();
    let result = MempoolResponse::read(&mut reader).await;
    assert_flood_error(&result, "the encoding");
}
