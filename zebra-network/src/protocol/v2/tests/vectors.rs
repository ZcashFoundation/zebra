//! Fixed values pinned to the version 2 protocol draft.
//!
//! The draft publishes no test vectors, so the tables here are transcribed by
//! hand from the draft's tables at zcash/zips commit `65ec4f743`, and the
//! byte layouts are derived field by field from its "Records", "Init Record",
//! "Transaction References", "Request Stream Types" and "Announcement Stream
//! Types" sections. A failure means either the draft moved or this
//! implementation did; either way, the change must be deliberate.

use std::{collections::HashSet, future::Future, net::SocketAddr, time::Duration};

use bytes::BytesMut;
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio_util::codec::Decoder;

use zebra_chain::{
    block::{self, Block},
    parameters::{testnet, Network},
    serialization::{CompactSize64, DateTime32, SerializationError, ZcashSerialize},
    transaction::{self, AuthDigest, UnminedTxId, WtxId},
};

use super::test_block;
use crate::{
    meta_addr::MetaAddr,
    protocol::{
        external::{
            types::{Nonce, PeerServices, Version},
            MAX_USER_AGENT_LENGTH,
        },
        v2::{
            announcement::{
                decode_addr_record, decode_block_announcement, decode_tx_record, encode_tx_record,
            },
            constants::{
                alpn_protocol_id, MAX_ADDRS_PER_RESPONSE, MAX_BLOCKS_PER_REQUEST,
                MAX_HEADERS_PER_RESPONSE, MAX_RECORD_PAYLOAD_LEN, MAX_TXS_PER_REQUEST,
                MIN_BIDIRECTIONAL_STREAMS, MIN_IDLE_TIMEOUT, MIN_UNIDIRECTIONAL_STREAMS,
            },
            handshake::{decode_record, InitRecord},
            record::RecordCodec,
            request::{max_body_len, GetBlocks, GetHeaders, GetTx, Request},
            response::{
                decode_addr_response, encode_addr_response, read_headers, read_objects,
                write_headers_response, ObjectResult,
            },
            txref::{ShortTxId, TxRef},
            types::{ErrorCode, StreamType},
            wire::{Violation, WireError},
        },
    },
    PeerSocketAddr,
};

/// The "Stream Types" table: code, name, and type. The draft assigns
/// identifiers only to the request stream types; the handshake and
/// announcement rows are named in prose, so those four names are Zebra's
/// (they are live metrics labels).
const STREAM_TYPE_TABLE: [(u8, &str, StreamType); 13] = [
    (0x00, "handshake", StreamType::Handshake),
    (0x01, "get-headers", StreamType::GetHeaders),
    (0x02, "get-blocks", StreamType::GetBlocks),
    (0x03, "get-tx", StreamType::GetTx),
    (0x04, "get-addr", StreamType::GetAddr),
    (0x05, "get-mempool", StreamType::GetMempool),
    (0x06, "get-hashes", StreamType::GetHashes),
    (0x07, "get-block-range", StreamType::GetBlockRange),
    (0x08, "get-tree-roots", StreamType::GetTreeRoots),
    (0x09, "get-object", StreamType::GetObject),
    (0x10, "block-announcements", StreamType::BlockAnnouncements),
    (
        0x11,
        "transaction-announcements",
        StreamType::TransactionAnnouncements,
    ),
    (
        0x12,
        "address-announcements",
        StreamType::AddressAnnouncements,
    ),
];

/// The "Application Error Codes" table: code, constant name.
const ERROR_CODE_TABLE: [(u64, &str, ErrorCode); 10] = [
    (0x00, "NO_ERROR", ErrorCode::NoError),
    (0x01, "PROTOCOL_ERROR", ErrorCode::ProtocolError),
    (
        0x02,
        "UNSUPPORTED_STREAM_TYPE",
        ErrorCode::UnsupportedStreamType,
    ),
    (0x03, "OBSOLETE", ErrorCode::Obsolete),
    (0x04, "SELF_CONNECTION", ErrorCode::SelfConnection),
    (0x05, "FLOOD", ErrorCode::Flood),
    (0x06, "MISBEHAVIOR", ErrorCode::Misbehavior),
    (0x07, "CANCELLED", ErrorCode::Cancelled),
    (0x08, "REFUSED", ErrorCode::Refused),
    (0x09, "INTERNAL_ERROR", ErrorCode::InternalError),
];

/// A well-formed `addrv2` record for a TORV2 address, which Zebra does not
/// support: time, services, network ID, the 10-byte address, and port.
const TORV2_RECORD: [u8; 19] = [
    0x00, 0x00, 0x00, 0x60, 0x01, 0x03, 10, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB,
    0xAB, 0x00, 0x00,
];

#[test]
fn stream_types_match_the_draft_table() {
    for (code, name, stream_type) in STREAM_TYPE_TABLE {
        assert_eq!(stream_type.code(), code, "{stream_type}");
        assert_eq!(
            StreamType::from_code(code),
            Some(stream_type),
            "{stream_type}"
        );
        assert_eq!(stream_type.name(), name);
        assert_eq!(stream_type.to_string(), name);
        assert_eq!(
            stream_type.is_request(),
            (0x01..=0x0F).contains(&code),
            "{stream_type}"
        );
        assert_eq!(
            stream_type.is_announcement(),
            (0x10..=0x1F).contains(&code),
            "{stream_type}"
        );
    }

    let table: Vec<StreamType> = STREAM_TYPE_TABLE.iter().map(|row| row.2).collect();
    assert_eq!(
        table,
        StreamType::ALL,
        "ALL must list the table in code order"
    );

    let known: HashSet<u8> = STREAM_TYPE_TABLE.iter().map(|row| row.0).collect();
    assert_eq!(
        known.len(),
        STREAM_TYPE_TABLE.len(),
        "codes must be distinct"
    );
    let names: HashSet<&str> = STREAM_TYPE_TABLE.iter().map(|row| row.1).collect();
    assert_eq!(
        names.len(),
        STREAM_TYPE_TABLE.len(),
        "names must be distinct"
    );

    for code in u8::MIN..=u8::MAX {
        match StreamType::from_code(code) {
            Some(stream_type) => {
                assert!(known.contains(&code));
                assert_eq!(stream_type.code(), code, "round trip");
            }
            None => assert!(
                !known.contains(&code),
                "{code:#04x} is known but not decoded"
            ),
        }
    }
}

#[test]
fn error_codes_match_the_draft_table() {
    for (code, name, error_code) in ERROR_CODE_TABLE {
        assert_eq!(error_code.code(), code, "{name}");
        assert_eq!(error_code.name(), name);
        assert_eq!(error_code.to_string(), name);
    }

    let codes: HashSet<u64> = ERROR_CODE_TABLE.iter().map(|row| row.0).collect();
    assert_eq!(
        codes.len(),
        ERROR_CODE_TABLE.len(),
        "codes must be distinct"
    );

    // Every variant has a row: a new variant without one fails to compile here.
    let variants: HashSet<u8> = ERROR_CODE_TABLE
        .iter()
        .map(|row| match row.2 {
            ErrorCode::NoError => 0,
            ErrorCode::ProtocolError => 1,
            ErrorCode::UnsupportedStreamType => 2,
            ErrorCode::Obsolete => 3,
            ErrorCode::SelfConnection => 4,
            ErrorCode::Flood => 5,
            ErrorCode::Misbehavior => 6,
            ErrorCode::Cancelled => 7,
            ErrorCode::Refused => 8,
            ErrorCode::InternalError => 9,
        })
        .collect();
    assert_eq!(variants.len(), 10, "every variant has one row");
}

/// The "Network Identification" table, and the gap for custom testnets.
#[test]
fn alpn_identifiers_match_the_draft_table() {
    assert_eq!(
        alpn_protocol_id(&Network::Mainnet),
        Some(&b"zcash/main"[..])
    );
    assert_eq!(
        alpn_protocol_id(&Network::new_default_testnet()),
        Some(&b"zcash/test"[..])
    );
    assert_eq!(
        alpn_protocol_id(&Network::new_regtest(Default::default())),
        Some(&b"zcash/regtest"[..])
    );

    let custom = testnet::Parameters::build()
        .with_network_name("ConfiguredTestnet")
        .expect("valid custom testnet name")
        .to_network()
        .expect("valid custom testnet parameters");
    assert!(!custom.is_default_testnet());
    assert!(!custom.is_regtest());
    assert_eq!(alpn_protocol_id(&custom), None);
}

#[test]
fn record_and_stream_limits_match_the_draft() {
    assert_eq!(MAX_RECORD_PAYLOAD_LEN, 2_097_152, "2 MiB");
    assert_eq!(MAX_USER_AGENT_LENGTH, 256);
    assert_eq!(MIN_BIDIRECTIONAL_STREAMS, 32);
    assert_eq!(MIN_UNIDIRECTIONAL_STREAMS, 8);
    assert_eq!(MIN_IDLE_TIMEOUT.as_secs(), 120);
}

#[test]
fn record_encode_pins_prefix_layout() {
    let record = RecordCodec::encode_record(b"").expect("empty payload is within the limit");
    assert_eq!(record.as_ref(), [0, 0, 0, 0]);

    let record =
        RecordCodec::encode_record([0xAA, 0xBB, 0xCC]).expect("three bytes is within the limit");
    assert_eq!(record.as_ref(), [3, 0, 0, 0, 0xAA, 0xBB, 0xCC]);

    // A length that needs more than one of the 4 prefix bytes pins the byte order.
    let payload = vec![0x5A; 0x0102];
    let record = RecordCodec::encode_record(&payload).expect("258 bytes is within the limit");
    assert_eq!(&record[..4], [0x02, 0x01, 0, 0]);
    assert_eq!(&record[4..], payload.as_slice());

    assert_eq!(
        RecordCodec
            .decode(&mut BytesMut::from(&[3u8, 0, 0, 0, 0xAA, 0xBB, 0xCC][..]))
            .expect("well-formed")
            .expect("complete")
            .as_ref(),
        [0xAA, 0xBB, 0xCC]
    );
}

/// A prefix above the limit is a `FLOOD` error as soon as the prefix arrives,
/// without reserving space for the claimed payload; a stream that ends
/// mid-record is a `PROTOCOL_ERROR`; a payload above the limit is a local
/// error; and I/O failures have no violation of their own.
#[test]
fn record_payload_limit_is_enforced_both_ways() {
    // `MAX_RECORD_PAYLOAD_LEN` is 2 MiB, pinned by `record_and_stream_limits_match_the_draft`,
    // so it fits in a `u32`.
    let limit = MAX_RECORD_PAYLOAD_LEN as u32;

    for length in [limit + 1, u32::MAX] {
        let mut src = BytesMut::from(&length.to_le_bytes()[..]);
        let capacity_before = src.capacity();

        assert_err!(
            RecordCodec.decode(&mut src),
            WireError::Flood { field: "record length", count, max }
                if count == u64::from(length) && max == MAX_RECORD_PAYLOAD_LEN,
            Violation::Close(ErrorCode::Flood)
        );
        assert_eq!(
            src.capacity(),
            capacity_before,
            "decoder must not reserve space from an untrusted length prefix",
        );
    }

    // Exactly the limit is accepted, and the decoder only reserves on arrival.
    let mut src = BytesMut::from(&limit.to_le_bytes()[..]);
    assert!(RecordCodec
        .decode(&mut src)
        .expect("a prefix at the limit is allowed")
        .is_none());
    assert!(src.capacity() < MAX_RECORD_PAYLOAD_LEN);

    src.resize(4 + MAX_RECORD_PAYLOAD_LEN, 0);
    let record = RecordCodec
        .decode(&mut src)
        .expect("a record at the limit is well-formed")
        .expect("the whole payload is in the buffer");
    assert_eq!(record.len(), MAX_RECORD_PAYLOAD_LEN);

    // A flood is still a flood at EOF.
    assert!(matches!(
        RecordCodec.decode_eof(&mut BytesMut::from(&u32::MAX.to_le_bytes()[..])),
        Err(WireError::Flood { .. })
    ));

    // A stream that ends mid-record is a protocol error; an empty or exactly consumed one is not.
    assert_err!(
        RecordCodec.decode_eof(&mut BytesMut::from(&[3u8, 0][..])),
        WireError::Truncated { what: "record" },
        Violation::Close(ErrorCode::ProtocolError)
    );
    assert!(RecordCodec
        .decode_eof(&mut BytesMut::new())
        .expect("an empty finished stream is fine")
        .is_none());
    let mut src = BytesMut::from(&[1u8, 0, 0, 0, 0x11][..]);
    let last = RecordCodec
        .decode_eof(&mut src)
        .expect("a finished stream ending on a record boundary is fine")
        .expect("the final record is yielded");
    assert_eq!(last.as_ref(), [0x11]);
    assert!(RecordCodec
        .decode_eof(&mut src)
        .expect("an exactly consumed stream is fine")
        .is_none());

    // The encoder refuses a payload above the limit: a local bug, reported as INTERNAL_ERROR.
    let payload = vec![0u8; MAX_RECORD_PAYLOAD_LEN + 1];
    assert_err!(
        RecordCodec::encode_record(&payload),
        WireError::LocalCountTooLarge { count, max, .. }
            if count == MAX_RECORD_PAYLOAD_LEN + 1 && max == MAX_RECORD_PAYLOAD_LEN,
        Violation::Close(ErrorCode::InternalError)
    );
    assert_eq!(
        RecordCodec::encode_record(&payload[..MAX_RECORD_PAYLOAD_LEN])
            .expect("a payload at the limit is accepted")
            .len(),
        4 + MAX_RECORD_PAYLOAD_LEN
    );

    // I/O failures carry no violation: the stream's owner decides what a reset or a failed read
    // means.
    assert!(WireError::from(std::io::Error::other("x"))
        .violation()
        .is_none());

    // Close codes: a violation's own code; `PROTOCOL_ERROR` for a misbehaviour on a stream that is
    // closing anyway; and `NO_ERROR` for a transport failure, which is not the peer's fault.
    assert_eq!(
        WireError::from(std::io::Error::other("x")).close_code(),
        ErrorCode::NoError
    );
    assert_eq!(
        WireError::TooManyResults {
            field: "count",
            count: 0,
            max: 0
        }
        .close_code(),
        ErrorCode::ProtocolError
    );
    assert_eq!(
        WireError::Flood {
            field: "record length",
            count: 0,
            max: 0
        }
        .close_code(),
        ErrorCode::Flood
    );
}

/// An `init` record exercising every field, and its wire bytes.
fn init_vector() -> (InitRecord, Vec<u8>) {
    let init = InitRecord {
        version: Version(170_180),
        services: PeerServices::NODE_NETWORK,
        nonce: Nonce(0x0102_0304_0506_0708),
        user_agent: "/Zebra:test/".to_string(),
        start_height: block::Height(1_000),
        relay: true,
        announce: false,
        full_ids: true,
    };

    let mut wire = vec![0x00]; // kind: init
    wire.extend_from_slice(&[0xC4, 0x98, 0x02, 0x00]); // version 170180
    wire.push(0x01); // services: CompactSize(1)
    wire.extend_from_slice(&[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]); // nonce
    wire.push(12); // user_agent length
    wire.extend_from_slice(b"/Zebra:test/");
    wire.extend_from_slice(&[0xE8, 0x03, 0x00, 0x00]); // start_height 1000
    wire.extend_from_slice(&[1, 0, 1]); // relay, announce, full_ids

    (init, wire)
}

#[test]
fn init_record_golden_vector_round_trips() {
    let (init, wire) = init_vector();

    assert_eq!(init.encode().as_ref(), wire.as_slice());
    assert_eq!(decode_record(&wire).expect("valid"), Some(init.clone()));

    // Bit 10 (`NODE_NETWORK_LIMITED` in the draft) and bit 40, neither known to Zebra: a 9-byte
    // CompactSize after the kind and version bytes, kept rather than truncated.
    let services = PeerServices::from_bits_retain((1 << 10) | (1 << 40));
    let with_unknown_bits = InitRecord {
        services,
        ..init.clone()
    };
    let encoded = with_unknown_bits.encode();
    assert_eq!(
        &encoded[5..14],
        [0xFF, 0x00, 0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
    );
    assert_eq!(encoded.len(), wire.len() + 8);
    let decoded = decode_record(&encoded)
        .expect("valid")
        .expect("an init record");
    assert_eq!(decoded.services.bits(), (1 << 10) | (1 << 40));
    assert_eq!(decoded, with_unknown_bits);

    let at_limit = InitRecord {
        user_agent: "x".repeat(MAX_USER_AGENT_LENGTH),
        ..init
    };
    assert_eq!(
        decode_record(&at_limit.encode()).expect("valid"),
        Some(at_limit)
    );
}

/// Malformed payloads built by mutating the golden bytes are rejected, and
/// unknown record kinds are ignored. Every decode error is a `PROTOCOL_ERROR`
/// by construction, pinned end to end by `malformed_init_is_a_protocol_error`.
#[test]
fn init_record_rejects_malformed_payloads() {
    let (_, wire) = init_vector();

    // A non-canonical CompactSize for `services` is malformed, as in the legacy protocol.
    let mut non_canonical = wire.clone();
    non_canonical.splice(5..6, [0xFD, 0x01, 0x00]);
    assert!(decode_record(&non_canonical).is_err());

    // The flag fields are the last three bytes and must be 0 or 1; the error names the field.
    let flags_start = wire.len() - 3;
    for (offset, field) in ["relay", "announce", "full_ids"].into_iter().enumerate() {
        for value in 2..=u8::MAX {
            let mut bad = wire.clone();
            bad[flags_start + offset] = value;
            assert_err!(
                decode_record(&bad),
                SerializationError::Parse(msg) if msg.starts_with(field)
            );
        }
    }

    // A 257-byte user agent claim with no bytes behind it is rejected from the prefix alone.
    let mut too_long = wire[..14].to_vec();
    too_long.extend_from_slice(&[0xFD, 0x01, 0x01]);
    assert_err!(
        decode_record(&too_long),
        SerializationError::Parse(msg) if msg.starts_with("user agent too long")
    );

    // A claim above the legacy 2 MiB message bound fails inside the CompactSize itself.
    let mut huge = wire[..14].to_vec();
    huge.extend_from_slice(&[0xFF, 0, 0, 0, 0, 0, 0, 0, 0x80]);
    assert!(decode_record(&huge).is_err());

    // Invalid UTF-8 in the first user agent byte.
    let mut bad_utf8 = wire.clone();
    bad_utf8[15] = 0xFF;
    assert!(decode_record(&bad_utf8).is_err());

    let mut long = wire.clone();
    long.push(0);
    assert_err!(
        decode_record(&long),
        SerializationError::Parse(msg) if msg.starts_with("trailing bytes")
    );

    // An empty payload has no kind byte.
    assert!(decode_record(&[]).is_err());

    // Every other kind is ignored without reading its payload, whatever it holds.
    for kind in 1..=u8::MAX {
        assert!(matches!(decode_record(&[kind]), Ok(None)), "kind {kind}");
        assert!(
            matches!(decode_record(&[kind, 0xDE, 0xAD]), Ok(None)),
            "kind {kind}"
        );
    }
}

#[test]
fn txref_encoding_pins_types_and_sizes() {
    let txid = transaction::Hash([0x11; 32]);
    let wtxid = WtxId {
        id: transaction::Hash([0x22; 32]),
        auth_digest: AuthDigest([0x33; 32]),
    };
    let block_hash = block::Hash([0x44; 32]);
    let short_id = ShortTxId([0x55, 0x56, 0x57, 0x58, 0x59, 0x5A]);

    // Reference, type byte, encoded size, identifier bytes.
    let cases: [(TxRef, u8, usize, Vec<u8>); 3] = [
        (TxRef::Txid(txid), 1, 33, vec![0x11; 32]),
        (
            TxRef::Wtxid(wtxid),
            2,
            65,
            [[0x22; 32], [0x33; 32]].concat(),
        ),
        (
            TxRef::ShortId {
                block: block_hash,
                short_id,
            },
            3,
            39,
            [&[0x44; 32][..], &short_id.0].concat(),
        ),
    ];

    for (txref, kind, size, identifier) in cases {
        let mut wire = Vec::new();
        txref.write(&mut wire).expect("writing to a Vec succeeds");

        assert_eq!(wire.len(), size);
        assert_eq!(wire[0], kind);
        assert_eq!(&wire[1..], identifier.as_slice());

        let mut cursor = &wire[..];
        let decoded = TxRef::read(&mut cursor).expect("valid reference");
        assert_eq!(decoded, txref);
        assert!(cursor.is_empty());

        // Every strict prefix is an error.
        for cut in 0..wire.len() {
            assert!(
                TxRef::read(&mut &wire[..cut]).is_err(),
                "cut at {cut} of {size}",
            );
        }
    }

    // An unmined ID is referenced by its own kind; only a short ID names no unmined ID.
    assert_eq!(
        TxRef::from(UnminedTxId::Legacy(txid)).unmined_id(),
        Some(UnminedTxId::Legacy(txid))
    );
    assert_eq!(
        TxRef::from(UnminedTxId::Witnessed(wtxid)).unmined_id(),
        Some(UnminedTxId::Witnessed(wtxid))
    );
    assert_eq!(
        TxRef::ShortId {
            block: block_hash,
            short_id,
        }
        .unmined_id(),
        None
    );
}

#[test]
fn txref_unknown_type_is_protocol_error() {
    for value in [0x00, 0x04, 0x7F, 0xFF] {
        assert_err!(
            TxRef::read(&mut &[value][..]),
            WireError::UnknownByte { what: "transaction reference type", value: got }
                if got == value,
            Violation::Close(ErrorCode::ProtocolError)
        );
    }
}

#[test]
fn get_headers_request_pins_layout_and_limits() {
    let request = GetHeaders {
        locator_hashes: vec![block::Hash([0xA1; 32]), block::Hash([0xA2; 32])],
        hash_stop: block::Hash([0; 32]),
        tx_ids: true,
    };
    let wire = Request::Headers(request.clone())
        .encode()
        .expect("valid request");

    let mut expected = vec![2];
    expected.extend_from_slice(&[0xA1; 32]);
    expected.extend_from_slice(&[0xA2; 32]);
    expected.extend_from_slice(&[0; 32]);
    expected.push(1);
    assert_eq!(wire.as_ref(), expected.as_slice());

    assert_eq!(
        Request::decode(StreamType::GetHeaders, &wire).expect("valid"),
        Request::Headers(request)
    );

    // 101 locators, the limit, round-trips.
    let at_limit = Request::Headers(GetHeaders {
        locator_hashes: vec![block::Hash([0xA3; 32]); 101],
        hash_stop: block::Hash([0; 32]),
        tx_ids: false,
    });
    let wire = at_limit.encode().expect("valid request");
    assert!(wire.len() <= max_body_len(StreamType::GetHeaders).expect("served"));
    assert_eq!(
        Request::decode(StreamType::GetHeaders, &wire).expect("valid"),
        at_limit
    );

    // 102 locators is rejected from the count alone.
    assert_err!(
        Request::decode(StreamType::GetHeaders, &[102]),
        WireError::CountTooLarge {
            field: "locator_count",
            count: 102,
            max: 101
        },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // The flag must be 0 or 1.
    let mut bad = expected.clone();
    *bad.last_mut().unwrap() = 2;
    assert_err!(
        Request::decode(StreamType::GetHeaders, &bad),
        WireError::InvalidFlag {
            field: "tx_ids",
            value: 2
        },
        Violation::Close(ErrorCode::ProtocolError)
    );

    let mut long = expected.clone();
    long.push(0);
    assert_err!(
        Request::decode(StreamType::GetHeaders, &long),
        WireError::TrailingBytes {
            what: "request",
            count: 1
        }
    );
}

#[test]
fn get_blocks_request_pins_layout_and_limit() {
    let request = GetBlocks {
        hashes: vec![block::Hash([0xB1; 32])],
    };
    let wire = Request::Blocks(request.clone()).encode().expect("valid");
    assert_eq!(wire[0], 1);
    assert_eq!(&wire[1..], &[0xB1; 32]);
    assert_eq!(
        Request::decode(StreamType::GetBlocks, &wire).expect("valid"),
        Request::Blocks(request)
    );

    let at_limit = GetBlocks {
        hashes: vec![block::Hash([0; 32]); MAX_BLOCKS_PER_REQUEST],
    };
    let wire = Request::Blocks(at_limit.clone())
        .encode()
        .expect("at the limit is valid");
    assert_eq!(max_body_len(StreamType::GetBlocks), Some(3 + 128 * 32));
    assert!(wire.len() <= max_body_len(StreamType::GetBlocks).expect("served"));
    assert_eq!(
        Request::decode(StreamType::GetBlocks, &wire).expect("valid"),
        Request::Blocks(at_limit)
    );

    let over = GetBlocks {
        hashes: vec![block::Hash([0; 32]); MAX_BLOCKS_PER_REQUEST + 1],
    };
    assert_err!(
        Request::Blocks(over).encode(),
        WireError::LocalCountTooLarge { count: 129, .. },
        Violation::Close(ErrorCode::InternalError)
    );
    assert_err!(
        Request::decode(StreamType::GetBlocks, &[129]),
        WireError::CountTooLarge {
            count: 129,
            max: 128,
            ..
        },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

/// `get-tx` encodes as a counted reference list, bounded at 50,000, and the
/// bound is applied before any reference is read.
#[test]
fn get_tx_request_pins_layout_and_limit() {
    let txid = transaction::Hash([0xC1; 32]);
    let request = GetTx {
        txrefs: vec![
            TxRef::Txid(txid),
            TxRef::ShortId {
                block: block::Hash([0xC2; 32]),
                short_id: ShortTxId([1, 2, 3, 4, 5, 6]),
            },
        ],
    };
    let wire = Request::Tx(request.clone()).encode().expect("valid");
    assert_eq!(wire.len(), 1 + 33 + 39);
    assert_eq!(wire[0], 2);
    assert_eq!(wire[1], 1);
    assert_eq!(wire[34], 3);
    assert_eq!(
        Request::decode(StreamType::GetTx, &wire).expect("valid"),
        Request::Tx(request)
    );

    assert_eq!(max_body_len(StreamType::GetTx), Some(3 + 50_000 * 65));
    assert_eq!(
        max_body_len(StreamType::GetHeaders),
        Some(3 + 101 * 32 + 32 + 1)
    );

    // 50,000 references, the limit, round-trip; with the largest reference type the body is
    // exactly the bound it is read with.
    let at_limit = Request::Tx(GetTx {
        txrefs: vec![TxRef::Txid(txid); MAX_TXS_PER_REQUEST],
    });
    let wire = at_limit.encode().expect("at the limit is valid");
    assert!(wire.len() <= max_body_len(StreamType::GetTx).expect("served"));
    assert_eq!(
        Request::decode(StreamType::GetTx, &wire).expect("valid"),
        at_limit
    );

    let largest = Request::Tx(GetTx {
        txrefs: vec![
            TxRef::Wtxid(WtxId {
                id: txid,
                auth_digest: AuthDigest([0xC3; 32]),
            });
            MAX_TXS_PER_REQUEST
        ],
    });
    let wire = largest.encode().expect("at the limit is valid");
    assert_eq!(
        Some(wire.len()),
        max_body_len(StreamType::GetTx),
        "the largest conforming request fills the bound exactly",
    );
    assert_eq!(
        Request::decode(StreamType::GetTx, &wire).expect("valid"),
        largest
    );

    assert_err!(
        Request::Tx(GetTx {
            txrefs: vec![TxRef::Txid(txid); MAX_TXS_PER_REQUEST + 1],
        })
        .encode(),
        WireError::LocalCountTooLarge {
            count: 50_001,
            max: 50_000,
            ..
        },
        Violation::Close(ErrorCode::InternalError)
    );

    // 50,001 as a CompactSize, and nothing else: rejected without waiting for references.
    assert_err!(
        Request::decode(StreamType::GetTx, &[0xFD, 0x51, 0xC3]),
        WireError::CountTooLarge {
            count: 50_001,
            max: 50_000,
            ..
        },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // A non-canonical count is malformed.
    assert_err!(
        Request::decode(StreamType::GetTx, &[0xFD, 0x02, 0x00]),
        WireError::Malformed { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

#[test]
fn empty_requests_and_stream_type_handling() {
    // The two body-less requests accept nothing after the stream type.
    for stream_type in [StreamType::GetAddr, StreamType::GetMempool] {
        assert_eq!(max_body_len(stream_type), Some(0));
        Request::decode(stream_type, &[]).expect("an empty body is the whole request");
        assert_err!(
            Request::decode(stream_type, &[0]),
            WireError::TrailingBytes { count: 1, .. }
        );
    }

    for stream_type in StreamType::ALL {
        let served = matches!(
            stream_type,
            StreamType::GetHeaders
                | StreamType::GetBlocks
                | StreamType::GetTx
                | StreamType::GetAddr
                | StreamType::GetMempool
        );
        if served {
            assert!(max_body_len(stream_type).is_some());
            continue;
        }
        assert!(
            max_body_len(stream_type).is_none(),
            "{stream_type} is not served"
        );

        // The handshake and announcement types never carry a request, and a request type this
        // node does not serve is refused by the stream's owner before anything is decoded.
        assert_err!(
            Request::decode(stream_type, &[]),
            WireError::NotARequest { stream_type: got } if got == stream_type,
            Violation::Close(ErrorCode::ProtocolError)
        );
    }
}

#[tokio::test]
async fn blocks_response_pins_entries_and_early_finish() {
    let genesis = test_block(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES);
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);

    let entries = vec![
        ObjectResult::Found(genesis.clone()),
        ObjectResult::NotFound,
        ObjectResult::Found(block_1.clone()),
    ];
    let mut wire = Vec::new();
    for entry in &entries {
        entry.write(&mut wire).expect("valid");
    }

    // Layout: result byte, then (for found) a CompactSize length and the block bytes.
    let genesis_len = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.len();
    let [genesis_len_lo, genesis_len_hi] = u16::try_from(genesis_len)
        .expect("the genesis block is under 64 KiB")
        .to_le_bytes();
    assert_eq!(wire[0], 0x00);
    assert_eq!(&wire[1..3], &[0xFD, genesis_len_lo]);
    assert_eq!(wire[3], genesis_len_hi);
    assert_eq!(
        &wire[4..4 + genesis_len],
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.as_slice()
    );
    assert_eq!(wire[4 + genesis_len], 0x02);
    assert_eq!(wire[5 + genesis_len], 0x00);

    let decoded = read_objects::<_, Block>(&mut &wire[..], 3)
        .await
        .expect("valid");
    assert_eq!(decoded, entries);

    // The responder may finish after any complete entry, including before the first.
    let first_two = 5 + genesis_len;
    let decoded = read_objects::<_, Block>(&mut &wire[..first_two], 3)
        .await
        .expect("valid");
    assert_eq!(decoded, entries[..2]);
    assert!(read_objects::<_, Block>(&mut &[][..], 3)
        .await
        .expect("an empty response is valid")
        .is_empty());

    // But it must not answer more entries than were requested.
    assert_err!(
        read_objects::<_, Block>(&mut &wire[..], 2).await,
        WireError::TrailingBytes {
            what: "response",
            ..
        },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // Finishing inside an entry is truncation, whether inside the length prefix or the block.
    for cut in [1, 4 + genesis_len / 2] {
        assert_err!(
            read_objects::<_, Block>(&mut &wire[..cut], 3).await,
            WireError::Truncated {
                what: "response entry"
            },
            Violation::Close(ErrorCode::ProtocolError)
        );
    }

    // An unknown result byte, an oversized length, and a block that does not fill its length.
    assert_err!(
        read_objects::<_, Block>(&mut &[0x01][..], 1).await,
        WireError::UnknownByte {
            what: "result code",
            value: 0x01
        }
    );

    let mut oversized = vec![0x00];
    CompactSize64::from(MAX_RECORD_PAYLOAD_LEN as u64 + 1)
        .zcash_serialize(&mut oversized)
        .expect("Vec write");
    assert_err!(
        read_objects::<_, Block>(&mut &oversized[..], 1).await,
        WireError::Flood { field: "object", count, max }
            if count == MAX_RECORD_PAYLOAD_LEN as u64 + 1 && max == MAX_RECORD_PAYLOAD_LEN,
        Violation::Close(ErrorCode::Flood)
    );
    // A 9-byte length prefix is read whole, and its flood is detected without any object bytes.
    assert_err!(
        read_objects::<_, Block>(&mut &[0x00, 0xFF, 0, 0, 0, 0, 1, 0, 0, 0][..], 1).await,
        WireError::Flood {
            field: "object",
            count: 0x1_0000_0000,
            ..
        },
        Violation::Close(ErrorCode::Flood)
    );
    assert_err!(
        read_objects::<_, Block>(&mut &[0x00, 0x03, 1, 2, 3][..], 1).await,
        WireError::Malformed { what: "object", .. },
        Violation::Close(ErrorCode::ProtocolError)
    );

    let mut padded = Vec::new();
    ObjectResult::Found(genesis.clone())
        .write(&mut padded)
        .expect("valid");
    // Bump the length by one and append a byte the block does not use.
    padded[2] = padded[2].wrapping_add(1);
    if padded[2] == 0 {
        padded[3] += 1;
    }
    padded.push(0);
    assert_err!(
        read_objects::<_, Block>(&mut &padded[..], 1).await,
        WireError::TrailingBytes {
            what: "object",
            count: 1
        }
    );
}

/// Feeds `wire` to `read` one byte at a time, ending with the FIN, and
/// returns what it read.
async fn read_byte_by_byte<T, F>(wire: Vec<u8>, read: impl FnOnce(DuplexStream) -> F) -> T
where
    F: Future<Output = Result<T, WireError>>,
{
    let (mut writer, reader) = tokio::io::duplex(64);
    let feed = tokio::spawn(async move {
        for byte in wire {
            writer
                .write_all(&[byte])
                .await
                .expect("the reader is alive");
        }
        // Dropping the writer is the FIN.
    });

    // The timeout covers the feeder too: a reader that stops early leaves it blocked on the
    // duplex buffer, which must fail the test rather than hang it.
    let (decoded, fed) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(read(reader), feed)
    })
    .await
    .expect("the response is read and fed before the timeout");
    fed.expect("the feeding task finishes");
    decoded.expect("valid")
}

/// A response reads the same when its bytes arrive one at a time as when
/// they arrive whole, in both the entry-per-object and the counted layout.
#[tokio::test]
async fn responses_read_across_chunk_boundaries() {
    let genesis = test_block(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES);
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);

    let entries = vec![
        ObjectResult::Found(genesis.clone()),
        ObjectResult::NotFound,
        ObjectResult::Found(block_1.clone()),
    ];
    let mut wire = Vec::new();
    for entry in &entries {
        entry.write(&mut wire).expect("valid");
    }
    let decoded = read_byte_by_byte(wire, |mut reader| async move {
        read_objects::<_, Block>(&mut reader, 3).await
    })
    .await;
    assert_eq!(decoded, entries);

    let headers = vec![genesis.header.clone(), block_1.header.clone()];
    let mut wire = Vec::new();
    write_headers_response(&mut wire, &headers, false).expect("valid");
    let decoded = read_byte_by_byte(
        wire,
        |mut reader| async move { read_headers(&mut reader).await },
    )
    .await;
    assert_eq!(decoded, headers);
}

#[tokio::test]
async fn txs_response_pins_entries() {
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);
    let coinbase = block_1.transactions[0].clone();

    let entries = vec![
        ObjectResult::NotFound,
        ObjectResult::Found(coinbase.clone()),
    ];
    let mut wire = Vec::new();
    for entry in &entries {
        entry.write(&mut wire).expect("valid");
    }

    let tx_bytes = coinbase.zcash_serialize_to_vec().expect("Vec write");
    assert_eq!(wire[0], 0x02);
    assert_eq!(wire[1], 0x00);
    assert!(tx_bytes.len() < 0xFD, "test assumes a one-byte length");
    assert_eq!(usize::from(wire[2]), tx_bytes.len());
    assert_eq!(&wire[3..], tx_bytes.as_slice());

    let decoded = read_objects::<_, transaction::Transaction>(&mut &wire[..], 2)
        .await
        .expect("valid");
    assert_eq!(decoded, entries);

    assert_err!(
        read_objects::<_, transaction::Transaction>(&mut &wire[..wire.len() - 1], 2).await,
        WireError::Truncated {
            what: "response entry"
        }
    );
}

#[tokio::test]
async fn headers_response_pins_layout() {
    let genesis = test_block(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES);
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);
    let headers = vec![
        genesis.header.clone(),
        block_1.header.clone(),
        test_block(&zebra_test::vectors::BLOCK_MAINNET_2_BYTES)
            .header
            .clone(),
    ];

    // Without `tx_ids`, entries are bare prefixed headers.
    let mut bare = Vec::new();
    write_headers_response(&mut bare, &headers, false).expect("valid");
    let header_bytes = genesis.header.zcash_serialize_to_vec().expect("Vec write");
    let [header_len_lo, header_len_hi] = u16::try_from(header_bytes.len())
        .expect("a block header is under 64 KiB")
        .to_le_bytes();
    assert_eq!(bare[0], 3);
    assert_eq!(&bare[1..3], &[0xFD, header_len_lo]);
    assert_eq!(bare[3], header_len_hi);
    assert_eq!(&bare[4..4 + header_bytes.len()], header_bytes.as_slice());
    assert_eq!(bare.len(), 1 + 3 * (3 + header_bytes.len()));

    let decoded = read_headers(&mut &bare[..]).await.expect("valid");
    assert_eq!(decoded, headers);

    // With `tx_ids`, a `has_txs = 0` byte follows each header: transaction IDs are never
    // supplied. The reader is for `tx_ids = 0` responses, so this layout is only pinned.
    let mut full = Vec::new();
    write_headers_response(&mut full, &headers, true).expect("valid");
    assert_eq!(full.len(), bare.len() + headers.len());
    assert_eq!(
        full[1 + 3 + header_bytes.len()],
        0,
        "genesis entry has no txs"
    );

    // A zero count is a complete, empty response.
    assert!(read_headers(&mut &[0][..]).await.expect("valid").is_empty());
}

/// Header response violations: too many entries is misbehavior, not a
/// connection error; truncation or trailing data is `PROTOCOL_ERROR`; and a
/// local response above the limit is never written.
#[tokio::test]
async fn headers_response_pins_violations() {
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);
    let entry = block_1.header.clone();

    // 161 as a CompactSize, nothing else.
    assert_err!(
        read_headers(&mut &[161][..]).await,
        WireError::TooManyResults {
            field: "count",
            count: 161,
            max: 160
        },
        Violation::Misbehavior
    );
    // 160 entries, the limit, round-trip.
    let at_limit = vec![entry.clone(); MAX_HEADERS_PER_RESPONSE];
    let mut wire = Vec::new();
    write_headers_response(&mut wire, &at_limit, false).expect("at the limit is valid");
    assert_eq!(read_headers(&mut &wire[..]).await.expect("valid"), at_limit);

    // A canonical 9-byte count is read whole and is still only misbehavior.
    assert_err!(
        read_headers(&mut &[0xFF, 0, 0, 0, 0, 1, 0, 0, 0][..]).await,
        WireError::TooManyResults {
            field: "count",
            count: 0x1_0000_0000,
            max: 160
        },
        Violation::Misbehavior
    );

    let too_many = vec![entry.clone(); MAX_HEADERS_PER_RESPONSE + 1];
    assert_err!(
        write_headers_response(&mut Vec::new(), &too_many, false),
        WireError::LocalCountTooLarge {
            count: 161,
            max: 160,
            ..
        },
        Violation::Close(ErrorCode::InternalError)
    );

    // Fewer entries than the count is truncation; an empty stream has no count.
    let mut wire = Vec::new();
    write_headers_response(&mut wire, &[entry.clone(), entry.clone()], false).expect("valid");
    let one_entry_len = {
        let mut one = Vec::new();
        write_headers_response(&mut one, std::slice::from_ref(&entry), false).expect("valid");
        one.len()
    };
    assert_err!(
        read_headers(&mut &wire[..one_entry_len]).await,
        WireError::Truncated {
            what: "header entry"
        }
    );
    assert_err!(
        read_headers(&mut &[][..]).await,
        WireError::Truncated {
            what: "headers response count"
        }
    );

    // Bytes after the last entry are trailing data.
    wire.push(0);
    assert_err!(
        read_headers(&mut &wire[..]).await,
        WireError::TrailingBytes {
            what: "headers response",
            ..
        }
    );

    // Non-canonical entry length prefixes, in each of the 3-, 5- and 9-byte forms.
    for prefix in [
        &[0xFD, 0x05, 0x00][..],
        &[0xFE, 0xFF, 0xFF, 0x00, 0x00][..],
        &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00][..],
    ] {
        let mut wire = vec![1];
        wire.extend_from_slice(prefix);
        assert_err!(
            read_headers(&mut &wire[..]).await,
            WireError::Malformed {
                what: "header entry",
                ..
            },
            Violation::Close(ErrorCode::ProtocolError)
        );
    }

    // A canonical 9-byte entry length above the limit is a flood, from the prefix alone.
    assert_err!(
        read_headers(&mut &[1, 0xFF, 0, 0, 0, 0, 1, 0, 0, 0][..]).await,
        WireError::Flood {
            field: "header",
            count: 0x1_0000_0000,
            ..
        },
        Violation::Close(ErrorCode::Flood)
    );

    // An entry that is not a header, and a header that does not fill its entry.
    assert_err!(
        read_headers(&mut &[1, 3, 0xAA, 0xBB, 0xCC][..]).await,
        WireError::Malformed { what: "header", .. },
        Violation::Close(ErrorCode::ProtocolError)
    );
    let mut padded = Vec::new();
    write_headers_response(&mut padded, std::slice::from_ref(&entry), false).expect("valid");
    assert_eq!(
        padded[1], 0xFD,
        "a header length takes a 3-byte CompactSize"
    );
    let length = u16::from_le_bytes([padded[2], padded[3]]) + 1;
    padded[2..4].copy_from_slice(&length.to_le_bytes());
    padded.push(0);
    assert_err!(
        read_headers(&mut &padded[..]).await,
        WireError::TrailingBytes {
            what: "header",
            count: 1
        },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

/// A `get-addr` response is a counted list of `addrv2` records, bounded at
/// 1000 as misbehavior; unsupported network IDs are skipped.
#[test]
fn addr_response_pins_layout_and_limit() {
    let services = PeerServices::NODE_NETWORK;
    let last_seen = DateTime32::from(0x6000_0000);
    let v4: PeerSocketAddr = "203.0.113.7:8233".parse::<SocketAddr>().unwrap().into();
    let v6: PeerSocketAddr = "[2001:db8::1]:8233".parse::<SocketAddr>().unwrap().into();
    let addrs = vec![
        MetaAddr::new_gossiped_meta_addr(v4, services, last_seen),
        MetaAddr::new_gossiped_meta_addr(v6, services, last_seen),
    ];

    let wire = encode_addr_response(&addrs).expect("sanitized addresses encode");

    // Count, then an IPv4 record: time LE, services CompactSize, network ID, length, address, port BE.
    let mut expected = vec![2];
    expected.extend_from_slice(&[0x00, 0x00, 0x00, 0x60]);
    expected.push(0x01);
    expected.extend_from_slice(&[0x01, 0x04, 203, 0, 113, 7]);
    expected.extend_from_slice(&8233u16.to_be_bytes());
    assert_eq!(&wire[..expected.len()], expected.as_slice());
    assert_eq!(wire.len(), expected.len() + (4 + 1 + 1 + 1 + 16 + 2));

    assert_eq!(decode_addr_response(&wire).expect("valid"), addrs);

    // 1000 records, the limit, round-trip; 1001 are never written.
    let at_limit = vec![addrs[0].clone(); MAX_ADDRS_PER_RESPONSE];
    let at_limit_wire = encode_addr_response(&at_limit).expect("at the limit is valid");
    assert_eq!(
        decode_addr_response(&at_limit_wire).expect("valid"),
        at_limit
    );
    assert_err!(
        encode_addr_response(&vec![addrs[0].clone(); MAX_ADDRS_PER_RESPONSE + 1]),
        WireError::LocalCountTooLarge {
            count: 1001,
            max: 1000,
            ..
        },
        Violation::Close(ErrorCode::InternalError)
    );

    // Unknown service bits are discarded when a record is read, as the shared legacy `addrv2`
    // reader does, so they are not relayed (the draft's "Service Flags" says they SHOULD be).
    let mut unknown_bits = vec![1];
    unknown_bits.extend_from_slice(&[0x00, 0x00, 0x00, 0x60, 0xFD, 0x01, 0x04]);
    unknown_bits.extend_from_slice(&[0x01, 0x04, 203, 0, 113, 7]);
    unknown_bits.extend_from_slice(&8233u16.to_be_bytes());
    let decoded = decode_addr_response(&unknown_bits).expect("valid");
    assert_eq!(decoded, addrs[..1]);
    assert_eq!(decoded[0].services(), Some(PeerServices::NODE_NETWORK));

    // An unsupported network ID is consumed and skipped.
    let mut with_unsupported = vec![3];
    with_unsupported.extend_from_slice(&wire[1..]);
    with_unsupported.extend_from_slice(&TORV2_RECORD);
    assert_eq!(
        decode_addr_response(&with_unsupported).expect("valid"),
        addrs
    );

    assert_err!(
        decode_addr_response(&[0xFD, 0xE9, 0x03]),
        WireError::TooManyResults {
            field: "count",
            count: 1001,
            max: 1000
        },
        Violation::Misbehavior
    );

    assert_err!(
        decode_addr_response(&wire[..wire.len() - 1]),
        WireError::Malformed { .. }
    );
    let mut long = wire.to_vec();
    long.push(0);
    assert_err!(
        decode_addr_response(&long),
        WireError::TrailingBytes {
            what: "addr response",
            count: 1
        }
    );

    // An unsanitized address (no services) is a local error.
    let unsanitized = MetaAddr::new_initial_peer(v4)
        .into_new_meta_addr(std::time::Instant::now(), DateTime32::now());
    assert!(unsanitized.services().is_none());
    assert_err!(
        encode_addr_response(&[unsanitized]),
        WireError::LocalUnencodable { .. },
        Violation::Close(ErrorCode::InternalError)
    );
}

#[test]
fn block_announcement_header_pins_layout() {
    let block_1 = test_block(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES);
    let header_bytes = block_1.header.zcash_serialize_to_vec().expect("Vec write");

    let mut payload = vec![0x00];
    payload.extend_from_slice(&header_bytes);
    assert_eq!(
        decode_block_announcement(&payload).expect("valid"),
        block_1.header
    );

    // Truncated, trailing, unknown kind, and empty payloads are nonconforming records.
    assert_err!(
        decode_block_announcement(&payload[..payload.len() - 1]),
        WireError::Malformed { .. }
    );
    let mut long = payload.clone();
    long.push(0);
    assert_err!(
        decode_block_announcement(&long),
        WireError::TrailingBytes { .. }
    );
    assert_err!(
        decode_block_announcement(&[0x02]),
        WireError::UnknownByte {
            what: "block announcement kind",
            value: 0x02
        },
        Violation::Close(ErrorCode::ProtocolError)
    );
    assert_err!(decode_block_announcement(&[]), WireError::Truncated { .. });

    // This node sets `announce = 0`, so a compact block is rejected from its kind byte, without
    // parsing the body (the three trailing bytes are never looked at).
    assert_err!(
        decode_block_announcement(&[0x01, 0, 0, 0]),
        WireError::Nonconforming { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

/// Transaction announcement and `get-mempool` records are a single `TXID`
/// or `WTXID` reference; `SHORTID` is a nonconforming record.
#[test]
fn tx_record_pins_layout_and_rejects_short_ids() {
    let hash = transaction::Hash([0xD1; 32]);
    let payload = encode_tx_record(UnminedTxId::Legacy(hash));
    assert_eq!(payload.len(), 33);
    assert_eq!(payload[0], 1);
    assert_eq!(
        decode_tx_record(&payload).expect("valid"),
        TxRef::Txid(hash)
    );

    let wtxid = WtxId {
        id: transaction::Hash([0xD2; 32]),
        auth_digest: AuthDigest([0xD3; 32]),
    };
    let payload = encode_tx_record(UnminedTxId::Witnessed(wtxid));
    assert_eq!(payload.len(), 65);
    assert_eq!(
        decode_tx_record(&payload).expect("valid"),
        TxRef::Wtxid(wtxid)
    );

    let short = TxRef::ShortId {
        block: block::Hash([0xD4; 32]),
        short_id: ShortTxId([1, 2, 3, 4, 5, 6]),
    };
    let mut wire = Vec::new();
    short.write(&mut wire).expect("Vec write");
    assert_err!(
        decode_tx_record(&wire),
        WireError::Nonconforming { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );

    let mut long = payload.to_vec();
    long.push(0);
    assert_err!(decode_tx_record(&long), WireError::TrailingBytes { .. });
    assert_err!(
        decode_tx_record(&payload[..10]),
        WireError::Truncated { .. } | WireError::Malformed { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

#[test]
fn addr_record_pins_layout_and_skips_unsupported() {
    let addr: PeerSocketAddr = "203.0.113.9:8233".parse::<SocketAddr>().unwrap().into();
    let meta = MetaAddr::new_gossiped_meta_addr(
        addr,
        PeerServices::NODE_NETWORK,
        DateTime32::from(0x6000_0000),
    );

    let mut expected = vec![0x00, 0x00, 0x00, 0x60, 0x01, 0x01, 0x04, 203, 0, 113, 9];
    expected.extend_from_slice(&8233u16.to_be_bytes());
    assert_eq!(
        decode_addr_record(&expected).expect("valid"),
        Some(meta.clone())
    );

    // Unknown service bits are discarded, as in `get-addr` responses.
    let mut unknown_bits = expected.clone();
    unknown_bits.splice(4..5, [0xFD, 0x01, 0x04]);
    assert_eq!(
        decode_addr_record(&unknown_bits).expect("valid"),
        Some(meta)
    );

    // A TORV2 record is well-formed but unsupported.
    assert_eq!(decode_addr_record(&TORV2_RECORD).expect("valid"), None);

    assert_err!(
        decode_addr_record(&expected[..5]),
        WireError::Malformed { .. }
    );
    let mut long = expected.clone();
    long.push(0);
    assert_err!(decode_addr_record(&long), WireError::TrailingBytes { .. });
}
