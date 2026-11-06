//! Tests for the `get-hashes` request and response encodings (draft,
//! "`get-hashes`").

use proptest::{collection::vec, prelude::*};

use zebra_chain::block::{self, SyncHashEntry};

use crate::protocol::v2::{
    constants::{GET_HASHES_REQUEST_LEN, MAX_GET_HASHES_COUNT},
    request::{max_body_len, GetHashes, Request},
    response::hashes::{decode_hashes_response, encode_hashes_response, MAX_HASHES_RESPONSE_LEN},
    types::{ErrorCode, StreamType},
    wire::{Violation, WireError},
};

/// A distinct entry for each `seed`.
fn entry(seed: u8) -> SyncHashEntry {
    SyncHashEntry {
        hash: block::Hash([seed; 32]),
        size: 1_000 + u64::from(seed),
        txs: u64::from(seed) + 1,
        txouts: 2 * u64::from(seed),
        notes: 0xFFFF_FFFF,
    }
}

fn request(start_height: u32, count: u32) -> GetHashes {
    GetHashes {
        start_height: block::Height(start_height),
        count,
    }
}

#[test]
fn request_encoding_vector() {
    let _init_guard = zebra_test::init();

    let request = request(0x0102_0304, 50_000);
    let body = Request::Hashes(request)
        .encode()
        .expect("the request is within the bounds");

    // `start_height u32 LE ‖ count u32 LE`.
    assert_eq!(
        body.as_ref(),
        [0x04, 0x03, 0x02, 0x01, 0x50, 0xC3, 0x00, 0x00]
    );
    assert_eq!(body.len(), GET_HASHES_REQUEST_LEN);
    assert_eq!(
        Request::Hashes(request).stream_type(),
        StreamType::GetHashes
    );
    assert_eq!(
        Request::decode(StreamType::GetHashes, &body).expect("valid"),
        Request::Hashes(request)
    );
}

#[test]
fn request_bounds() {
    let _init_guard = zebra_test::init();

    // The largest count, and the greatest height exactly at `u32::MAX`, are valid, as is an
    // empty request at any height.
    for request in [
        request(0, MAX_GET_HASHES_COUNT),
        request(u32::MAX - MAX_GET_HASHES_COUNT + 1, MAX_GET_HASHES_COUNT),
        request(u32::MAX, 1),
        request(u32::MAX, 0),
        request(0, 0),
    ] {
        request.check().expect("within the bounds");
        let body = Request::Hashes(request)
            .encode()
            .expect("within the bounds");
        assert_eq!(
            Request::decode(StreamType::GetHashes, &body).expect("valid"),
            Request::Hashes(request),
        );
    }

    // Over the count bound: not sent, and a connection error when received.
    let too_many = request(0, MAX_GET_HASHES_COUNT + 1);
    assert_err!(
        too_many.check(),
        WireError::LocalCountTooLarge { count, .. } if count == MAX_GET_HASHES_COUNT as usize + 1
    );
    assert_err!(
        Request::Hashes(too_many).encode(),
        WireError::LocalCountTooLarge { .. }
    );
    assert_err!(
        Request::decode(StreamType::GetHashes, &raw_request(too_many)),
        WireError::CountTooLarge { count, .. } if count == u64::from(MAX_GET_HASHES_COUNT) + 1,
        Violation::Close(ErrorCode::ProtocolError)
    );

    // The greatest requested height above `u32::MAX`.
    for overflowing in [request(u32::MAX, 2), request(u32::MAX - 9, 11)] {
        assert_err!(overflowing.check(), WireError::LocalUnencodable { .. });
        assert_err!(
            Request::decode(StreamType::GetHashes, &raw_request(overflowing)),
            WireError::CountTooLarge { max, .. } if max == (u32::MAX - overflowing.start_height.0) as usize + 1,
            Violation::Close(ErrorCode::ProtocolError)
        );
    }
}

#[test]
fn request_framing() {
    let _init_guard = zebra_test::init();

    let body = raw_request(request(7, 3));
    for len in 0..GET_HASHES_REQUEST_LEN {
        assert_err!(
            Request::decode(StreamType::GetHashes, &body[..len]),
            WireError::Truncated { .. },
            Violation::Close(ErrorCode::ProtocolError)
        );
    }

    let mut trailing = body.clone();
    trailing.push(0);
    assert_err!(
        Request::decode(StreamType::GetHashes, &trailing),
        WireError::TrailingBytes { count: 1, .. },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // Decodable, but not served: the stream's owner has no body bound for it, and refuses it.
    assert_eq!(max_body_len(StreamType::GetHashes), None);
}

#[test]
fn response_encoding_vector() {
    let _init_guard = zebra_test::init();

    let entries = [
        SyncHashEntry {
            hash: block::Hash([0x11; 32]),
            size: 1,
            txs: 1,
            txouts: 0,
            notes: 0,
        },
        SyncHashEntry {
            hash: block::Hash([0x22; 32]),
            size: 400,
            txs: 0xFD,
            txouts: 0x1_0000,
            notes: 0xFFFF_FFFF,
        },
    ];

    let mut expected = vec![0x02];
    expected.extend([0x11; 32]);
    expected.extend([0x01, 0x01, 0x00, 0x00]);
    expected.extend([0x22; 32]);
    expected.extend([0xFD, 0x90, 0x01]);
    expected.extend([0xFD, 0xFD, 0x00]);
    expected.extend([0xFE, 0x00, 0x00, 0x01, 0x00]);
    expected.extend([0xFE, 0xFF, 0xFF, 0xFF, 0xFF]);

    let body = encode_hashes_response(&entries).expect("within the bound");
    assert_eq!(body.as_ref(), expected.as_slice());
    assert_eq!(
        decode_hashes_response(&body, &request(10, 2)).expect("valid"),
        entries
    );

    // An empty response is valid, even for an empty request.
    let empty = encode_hashes_response(&[]).expect("within the bound");
    assert_eq!(empty.as_ref(), [0x00]);
    assert_eq!(
        decode_hashes_response(&empty, &request(10, 0)).expect("valid"),
        vec![]
    );
}

#[test]
fn response_count_is_at_most_the_request_count() {
    let _init_guard = zebra_test::init();

    let entries: Vec<_> = (0..3).map(entry).collect();
    let body = encode_hashes_response(&entries).expect("within the bound");

    // A prefix of the request is valid: entry `k` is height `start_height + k`.
    for count in [3, 4, MAX_GET_HASHES_COUNT] {
        assert_eq!(
            decode_hashes_response(&body, &request(100, count)).expect("a prefix"),
            entries
        );
    }

    // More entries than requested close the connection.
    for count in [0, 2] {
        assert_err!(
            decode_hashes_response(&body, &request(100, count)),
            WireError::CountTooLarge { count: 3, max, .. } if max == count as usize,
            Violation::Close(ErrorCode::ProtocolError)
        );
    }

    // The bound is checked from the count, before any entry: a huge claim with no entries.
    let mut huge = vec![0xFE];
    huge.extend(u32::MAX.to_le_bytes());
    assert_err!(
        decode_hashes_response(&huge, &request(0, MAX_GET_HASHES_COUNT)),
        WireError::CountTooLarge { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // This node never sends more than the maximum.
    let too_many = vec![entry(0); MAX_GET_HASHES_COUNT as usize + 1];
    assert_err!(
        encode_hashes_response(&too_many),
        WireError::LocalCountTooLarge { .. }
    );

    let most = vec![entry(0); MAX_GET_HASHES_COUNT as usize];
    assert!(encode_hashes_response(&most).expect("at the bound").len() <= MAX_HASHES_RESPONSE_LEN);
}

#[test]
fn response_framing() {
    let _init_guard = zebra_test::init();

    let entries: Vec<_> = (0..2).map(entry).collect();
    let body = encode_hashes_response(&entries).expect("within the bound");
    let request = request(0, 2);

    for len in 0..body.len() {
        assert_err!(
            decode_hashes_response(&body[..len], &request),
            WireError::Malformed { .. },
            Violation::Close(ErrorCode::ProtocolError)
        );
    }

    let mut trailing = body.to_vec();
    trailing.push(0);
    assert_err!(
        decode_hashes_response(&trailing, &request),
        WireError::TrailingBytes { count: 1, .. },
        Violation::Close(ErrorCode::ProtocolError)
    );
}

#[test]
fn response_rejects_non_canonical_compact_sizes() {
    let _init_guard = zebra_test::init();

    // 0xFC in three bytes: a value that fits in one.
    let non_canonical = [0xFD, 0xFC, 0x00];

    // In the count.
    assert_err!(
        decode_hashes_response(&non_canonical, &request(0, MAX_GET_HASHES_COUNT)),
        WireError::Malformed { .. },
        Violation::Close(ErrorCode::ProtocolError)
    );

    // In each of an entry's counts.
    for field in 0..4 {
        let mut body = vec![0x01];
        body.extend([0x33; 32]);
        for other in 0..4 {
            if other == field {
                body.extend(non_canonical);
            } else {
                body.push(0x01);
            }
        }
        assert_err!(
            decode_hashes_response(&body, &request(0, 1)),
            WireError::Malformed { .. },
            Violation::Close(ErrorCode::ProtocolError)
        );
    }
}

/// The request body as it appears on the wire, bypassing the sender's checks.
fn raw_request(request: GetHashes) -> Vec<u8> {
    let mut body = request.start_height.0.to_le_bytes().to_vec();
    body.extend(request.count.to_le_bytes());
    body
}

proptest! {
    #[test]
    fn request_roundtrip(
        // Half the cases near `u32::MAX`, where the greatest height can overflow.
        start_height in prop_oneof![any::<u32>(), (u32::MAX - 2 * MAX_GET_HASHES_COUNT)..=u32::MAX],
        count in 0..=MAX_GET_HASHES_COUNT,
    ) {
        let _init_guard = zebra_test::init();

        let request = request(start_height, count);
        let raw = raw_request(request);
        let fits = u64::from(start_height) + u64::from(count) <= u64::from(u32::MAX) + 1;

        prop_assert_eq!(request.check().is_ok(), fits);
        match Request::decode(StreamType::GetHashes, &raw) {
            Ok(decoded) => {
                prop_assert!(fits);
                prop_assert_eq!(decoded, Request::Hashes(request));
                let encoded = Request::Hashes(request).encode()?;
                prop_assert_eq!(encoded.as_ref(), raw.as_slice());
            }
            Err(error) => {
                prop_assert!(!fits);
                prop_assert!(matches!(error, WireError::CountTooLarge { .. }), "{:?}", error);
            }
        }
    }

    #[test]
    fn response_roundtrip(
        entries in vec(any::<SyncHashEntry>(), 0..32),
        extra in 0u32..4,
    ) {
        let _init_guard = zebra_test::init();

        // `entries.len()` is below 32, so it fits in a `u32`.
        let len = entries.len() as u32;
        let body = encode_hashes_response(&entries)?;

        // Any request count at least the response's accepts it.
        prop_assert_eq!(&decode_hashes_response(&body, &request(0, len + extra))?, &entries);

        // And any smaller one rejects it.
        if let Some(count) = len.checked_sub(1 + extra) {
            prop_assert!(decode_hashes_response(&body, &request(0, count)).is_err());
        }
    }
}
