//! Randomised property tests for the version 2 protocol.

use bytes::{Bytes, BytesMut};
use proptest::{collection::vec, prelude::*};
use tokio_util::codec::Decoder;

use std::sync::Arc;

use zebra_chain::{
    block::{self, Block},
    transaction::{self, AuthDigest, WtxId},
};

use super::{decode_in_chunks, test_block};
use crate::protocol::{
    external::{
        types::{Nonce, PeerServices, Version},
        MAX_USER_AGENT_LENGTH,
    },
    v2::{
        handshake::{decode_record, InitRecord},
        record::RecordCodec,
        request::{max_body_len, GetBlocks, GetHeaders, GetTx, Request},
        response::{read_headers, read_objects, write_headers_response, ObjectResult},
        txref::{ShortTxId, TxRef},
        types::ErrorCode,
        wire::{Violation, WireError},
    },
};

/// The largest payload the property tests generate.
///
/// Well under the 2 MiB limit; the limit itself is pinned in `vectors.rs`.
const MAX_TEST_PAYLOAD_LEN: usize = 512;

/// The most records a generated stream holds.
const MAX_TEST_RECORDS: usize = 16;

/// Encodes `payloads` back to back into one buffer.
fn encode_all(payloads: &[Vec<u8>]) -> BytesMut {
    let mut wire = BytesMut::new();
    for payload in payloads {
        wire.extend_from_slice(
            &RecordCodec::encode_record(payload).expect("generated payloads are within the limit"),
        );
    }
    wire
}

/// Decodes every complete record currently in `src`.
fn decode_available(src: &mut BytesMut) -> Vec<Bytes> {
    let mut records = Vec::new();
    while let Some(record) = RecordCodec
        .decode(src)
        .expect("a well-formed stream never errors")
    {
        records.push(record);
    }
    records
}

/// A strategy for `init` records with any field values the format allows,
/// including unknown service bits and user agents up to the byte limit.
fn init_record_strategy() -> impl Strategy<Value = InitRecord> {
    (
        any::<u32>(),
        any::<u64>(),
        any::<u64>(),
        // Multi-byte characters, so byte length and character count differ.
        proptest::string::string_regex(&format!("[a-zA-Z0-9/:. é€]{{0,{MAX_USER_AGENT_LENGTH}}}"))
            .expect("valid regex")
            .prop_filter("user agent within the byte limit", |user_agent| {
                user_agent.len() <= MAX_USER_AGENT_LENGTH
            }),
        any::<u32>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(version, services, nonce, user_agent, start_height, relay, announce, full_ids)| {
                InitRecord {
                    version: Version(version),
                    services: PeerServices::from_bits_retain(services),
                    nonce: Nonce(nonce),
                    user_agent,
                    start_height: block::Height(start_height),
                    relay,
                    announce,
                    full_ids,
                }
            },
        )
}

/// A strategy for any transaction reference.
fn txref_strategy() -> impl Strategy<Value = TxRef> {
    prop_oneof![
        any::<[u8; 32]>().prop_map(|id| TxRef::Txid(transaction::Hash(id))),
        (any::<[u8; 32]>(), any::<[u8; 32]>()).prop_map(|(id, auth_digest)| {
            TxRef::Wtxid(WtxId {
                id: transaction::Hash(id),
                auth_digest: AuthDigest(auth_digest),
            })
        }),
        (any::<[u8; 32]>(), any::<[u8; 6]>()).prop_map(|(block, short_id)| TxRef::ShortId {
            block: block::Hash(block),
            short_id: ShortTxId(short_id),
        }),
    ]
}

/// A strategy for block hashes.
fn block_hash_strategy() -> impl Strategy<Value = block::Hash> {
    any::<[u8; 32]>().prop_map(block::Hash)
}

/// A strategy for any of the five requests, with small lists.
///
/// The count limits are pinned in `vectors.rs`; these tests cover the
/// encoding, so they keep the lists short.
fn request_strategy() -> impl Strategy<Value = Request> {
    prop_oneof![
        (
            vec(block_hash_strategy(), 0..=8),
            block_hash_strategy(),
            any::<bool>(),
        )
            .prop_map(|(locator_hashes, hash_stop, tx_ids)| {
                Request::Headers(GetHeaders {
                    locator_hashes,
                    hash_stop,
                    tx_ids,
                })
            }),
        vec(block_hash_strategy(), 0..=8).prop_map(|hashes| Request::Blocks(GetBlocks { hashes })),
        vec(txref_strategy(), 0..=8).prop_map(|txrefs| Request::Tx(GetTx { txrefs })),
        Just(Request::Addr),
        Just(Request::Mempool),
    ]
}

/// The test vector blocks used to build responses.
fn vector_blocks() -> Vec<Arc<Block>> {
    [
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES[..],
    ]
    .into_iter()
    .map(test_block)
    .collect()
}

proptest! {
    #[test]
    fn txref_roundtrip(txref in txref_strategy(), trailing in vec(any::<u8>(), 0..=4)) {
        let mut wire = Vec::new();
        txref.write(&mut wire).expect("writing to a Vec succeeds");
        // `max_body_len(GetTx)` is sized from this bound.
        prop_assert!(wire.len() <= TxRef::MAX_ENCODED_LEN);

        wire.extend_from_slice(&trailing);
        let mut cursor = &wire[..];
        let decoded = TxRef::read(&mut cursor).expect("valid");
        prop_assert_eq!(decoded, txref);
        prop_assert_eq!(cursor, trailing.as_slice());
    }

    #[test]
    fn request_roundtrip(request in request_strategy()) {
        let stream_type = request.stream_type();
        let body = request.encode().expect("generated requests are within the limits");
        prop_assert!(
            body.len() <= max_body_len(stream_type).expect("generated requests are served types"),
            "a conforming request must fit in the bound it is read with",
        );
        let decoded = Request::decode(stream_type, &body).expect("encoded requests decode");
        prop_assert_eq!(decoded, request);

        // The body is complete and minimal: any strict prefix is truncated, a `PROTOCOL_ERROR`.
        // (A `get-addr` body is already empty, so it has no strict prefixes.)
        for cut in 0..body.len() {
            prop_assert_eq!(
                Request::decode(stream_type, &body[..cut])
                    .err()
                    .and_then(|error| error.violation()),
                Some(Violation::Close(ErrorCode::ProtocolError)),
                "request cut at {} of {}",
                cut,
                body.len(),
            );
        }
    }

    /// A `get-blocks` response reads back to the entries written, whether the
    /// responder answers every requested entry or finishes early after any
    /// complete entry; an entry past the requested count is trailing data. A
    /// `&[u8]` reader is always ready, so no runtime is needed.
    #[test]
    fn blocks_response_roundtrip(
        picks in vec(proptest::option::of(0..3usize), 0..=6),
        keep in 0..=6usize,
    ) {
        let blocks = vector_blocks();
        let entries: Vec<ObjectResult<Arc<Block>>> = picks
            .iter()
            .map(|pick| match pick {
                Some(index) => ObjectResult::Found(blocks[*index].clone()),
                None => ObjectResult::NotFound,
            })
            .collect();

        // The responder answers the first `written` of the requested entries.
        let written = keep.min(entries.len());
        let mut wire = Vec::new();
        for entry in &entries[..written] {
            entry.write(&mut wire).expect("valid");
        }

        let decoded = futures::executor::block_on(
            read_objects::<_, Block>(&mut &wire[..], entries.len()),
        )
        .expect("valid");
        prop_assert_eq!(&decoded[..], &entries[..written]);

        if written > 0 {
            let error = futures::executor::block_on(
                read_objects::<_, Block>(&mut &wire[..], written - 1),
            )
            .expect_err("an entry past the requested count is trailing data");
            prop_assert!(
                matches!(error, WireError::TrailingBytes { what: "response", .. }),
                "got {:?}",
                error,
            );
        }
    }

    #[test]
    fn headers_response_roundtrip(picks in vec(0..3usize, 0..=6)) {
        let blocks = vector_blocks();
        let headers: Vec<Arc<block::Header>> =
            picks.iter().map(|i| blocks[*i].header.clone()).collect();

        let mut wire = Vec::new();
        write_headers_response(&mut wire, &headers, false).expect("valid");

        let decoded = futures::executor::block_on(read_headers(&mut &wire[..])).expect("valid");
        prop_assert_eq!(decoded, headers);
    }

    /// An `init` record survives an encode and decode round trip, through
    /// the record codec as it would on the wire, and every strict prefix of
    /// its payload is malformed: no field is optional.
    #[test]
    fn init_record_roundtrip_and_rejects_truncation(init in init_record_strategy()) {
        let payload = init.encode();

        let mut wire = BytesMut::from(
            &RecordCodec::encode_record(&payload).expect("init records are far below the limit")[..],
        );
        let framed = RecordCodec
            .decode(&mut wire)
            .expect("well-formed")
            .expect("complete");
        prop_assert!(wire.is_empty());

        let decoded = decode_record(&framed).expect("encoded records decode");
        prop_assert_eq!(decoded, Some(init));

        for cut in 0..payload.len() {
            prop_assert!(
                decode_record(&payload[..cut]).is_err(),
                "record cut at {cut} of {} decoded",
                payload.len(),
            );
        }
    }

    #[test]
    fn record_decode_is_independent_of_chunking(
        payloads in vec(vec(any::<u8>(), 0..=MAX_TEST_PAYLOAD_LEN), 1..=MAX_TEST_RECORDS),
        chunk_lens in vec(1..=64usize, 1..=256),
    ) {
        let wire = encode_all(&payloads);

        let decoded = decode_in_chunks(&mut RecordCodec, &wire, &chunk_lens).expect("well-formed");

        prop_assert_eq!(decoded.len(), payloads.len());
        for (record, payload) in decoded.iter().zip(&payloads) {
            prop_assert_eq!(record.as_ref(), payload.as_slice());
        }
    }

    /// Cutting a well-formed stream anywhere inside a record makes
    /// `decode_eof` report truncation, after yielding every record before
    /// the cut; cutting on a record boundary does not.
    #[test]
    fn record_decode_eof_detects_any_cut(
        payloads in vec(vec(any::<u8>(), 0..=MAX_TEST_PAYLOAD_LEN), 1..=MAX_TEST_RECORDS),
        cut_fraction in 0.0..=1.0f64,
    ) {
        let wire = encode_all(&payloads);

        let boundaries: Vec<usize> = payloads
            .iter()
            .scan(0, |offset, payload| {
                *offset += 4 + payload.len();
                Some(*offset)
            })
            .collect();

        // `cut_fraction` is in `0.0..=1.0`, so the product is in `0..=wire.len()` and the
        // truncating cast is a valid index.
        let cut = ((wire.len() as f64) * cut_fraction) as usize;
        let mut src = BytesMut::from(&wire[..cut]);

        let before_cut = decode_available(&mut src);
        let eof = RecordCodec.decode_eof(&mut src);

        // Every record that ends at or before the cut is yielded, whatever follows.
        let complete = boundaries.iter().filter(|&&b| b <= cut).count();
        prop_assert_eq!(before_cut.len(), complete);
        for (record, payload) in before_cut.iter().zip(&payloads) {
            prop_assert_eq!(record.as_ref(), payload.as_slice());
        }

        if cut == 0 || boundaries.contains(&cut) {
            prop_assert!(eof.as_ref().is_ok_and(Option::is_none), "got {eof:?}");
        } else {
            prop_assert!(
                matches!(eof, Err(WireError::Truncated { what: "record" })),
                "cut at {cut}: expected Truncated, got {eof:?}",
            );
        }
    }
}
