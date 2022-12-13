//! Fixed test vectors for codec.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use chrono::DateTime;
use futures::prelude::*;
use lazy_static::lazy_static;

use super::*;

lazy_static! {
    static ref VERSION_TEST_VECTOR: Message = {
        let services = PeerServices::NODE_NETWORK;
        let timestamp = Utc
            .timestamp_opt(1_568_000_000, 0)
            .single()
            .expect("in-range number of seconds and valid nanosecond");

        VersionMessage {
            version: crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            services,
            timestamp,
            address_recv: AddrInVersion::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
                services,
            ),
            address_from: AddrInVersion::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
                services,
            ),
            nonce: Nonce(0x9082_4908_8927_9238),
            user_agent: "Zebra".to_owned(),
            start_height: block::Height(540_000),
            relay: true,
        }
        .into()
    };
}

/// Check that the version test vector serializes and deserializes correctly
#[test]
fn version_message_round_trip() {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = &*VERSION_TEST_VECTOR;

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }
        bytes
    });

    let v_parsed = rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next()
            .await
            .expect("a next message should be available")
            .expect("that message should deserialize")
    });

    assert_eq!(*v, v_parsed);
}

/// Check that version deserialization rejects out-of-range timestamps with
/// an error.
#[test]
fn version_timestamp_out_of_range() {
    let v_err = deserialize_version_with_time(i64::MAX);
    assert!(
        matches!(v_err, Err(Error::Parse(_))),
        "expected error with version timestamp: {}",
        i64::MAX
    );

    let v_err = deserialize_version_with_time(i64::MIN);
    assert!(
        matches!(v_err, Err(Error::Parse(_))),
        "expected error with version timestamp: {}",
        i64::MIN
    );

    deserialize_version_with_time(1620777600).expect("recent time is valid");
    deserialize_version_with_time(0).expect("zero time is valid");
    deserialize_version_with_time(DateTime::<Utc>::MIN_UTC.timestamp()).expect("min time is valid");
    deserialize_version_with_time(DateTime::<Utc>::MAX_UTC.timestamp()).expect("max time is valid");
}

/// Deserialize a `Version` message containing `time`, and return the result.
fn deserialize_version_with_time(time: i64) -> Result<Message, Error> {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = &*VERSION_TEST_VECTOR;

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }

        let old_bytes = bytes.clone();

        // tweak the version bytes so the timestamp is set to `time`
        // Version serialization is specified at:
        // https://developer.bitcoin.org/reference/p2p_networking.html#version
        bytes[36..44].copy_from_slice(&time.to_le_bytes());

        // Checksum is specified at:
        // https://developer.bitcoin.org/reference/p2p_networking.html#message-headers
        let checksum = sha256d::Checksum::from(&bytes[HEADER_LEN..]);
        bytes[20..24].copy_from_slice(&checksum.0);

        debug!(?time,
               old_len = ?old_bytes.len(), new_len = ?bytes.len(),
               old_bytes = ?&old_bytes[36..44], new_bytes = ?&bytes[36..44]);

        bytes
    });

    rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next().await.expect("a next message should be available")
    })
}

#[test]
fn filterload_message_round_trip() {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = Message::FilterLoad {
        filter: Filter(vec![0; 35999]),
        hash_functions_count: 0,
        tweak: Tweak(0),
        flags: 0,
    };

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }
        bytes
    });

    let v_parsed = rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next()
            .await
            .expect("a next message should be available")
            .expect("that message should deserialize")
    });

    assert_eq!(v, v_parsed);
}

#[test]
fn reject_message_no_extra_data_round_trip() {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = Message::Reject {
        message: "experimental".to_string(),
        ccode: RejectReason::Malformed,
        reason: "message could not be decoded".to_string(),
        data: None,
    };

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }
        bytes
    });

    let v_parsed = rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next()
            .await
            .expect("a next message should be available")
            .expect("that message should deserialize")
    });

    assert_eq!(v, v_parsed);
}

#[test]
fn reject_message_extra_data_round_trip() {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = Message::Reject {
        message: "block".to_string(),
        ccode: RejectReason::Invalid,
        reason: "invalid block difficulty".to_string(),
        data: Some([0xff; 32]),
    };

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }
        bytes
    });

    let v_parsed = rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next()
            .await
            .expect("a next message should be available")
            .expect("that message should deserialize")
    });

    assert_eq!(v, v_parsed);
}

#[test]
fn filterload_message_too_large_round_trip() {
    let (rt, _init_guard) = zebra_test::init_async();

    let v = Message::FilterLoad {
        filter: Filter(vec![0; 40000]),
        hash_functions_count: 0,
        tweak: Tweak(0),
        flags: 0,
    };

    use tokio_util::codec::{FramedRead, FramedWrite};
    let v_bytes = rt.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
            fw.send(v.clone())
                .await
                .expect("message should be serialized");
        }
        bytes
    });

    rt.block_on(async {
        let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
        fr.next()
            .await
            .expect("a next message should be available")
            .expect_err("that message should not deserialize")
    });
}

#[test]
fn max_msg_size_round_trip() {
    use zebra_chain::serialization::ZcashDeserializeInto;

    //let (rt, _init_guard) = zebra_test::init_async();
    let _init_guard = zebra_test::init();

    // make tests with a Tx message
    let tx: Transaction = zebra_test::vectors::DUMMY_TX1
        .zcash_deserialize_into()
        .unwrap();
    let msg = Message::Tx(tx.into());

    use tokio_util::codec::{FramedRead, FramedWrite};

    // i know the above msg has a body of 85 bytes
    let size = 85;

    // reducing the max size to body size - 1
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(
                &mut bytes,
                Codec::builder().with_max_body_len(size - 1).finish(),
            );
            fw.send(msg.clone())
                .await
                .expect_err("message should not encode as it is bigger than the max allowed value");
        }
    });

    // send again with the msg body size as max size
    let msg_bytes = zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let mut bytes = Vec::new();
        {
            let mut fw = FramedWrite::new(
                &mut bytes,
                Codec::builder().with_max_body_len(size).finish(),
            );
            fw.send(msg.clone())
                .await
                .expect("message should encode with the msg body size as max allowed value");
        }
        bytes
    });

    // receive with a reduced max size
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let mut fr = FramedRead::new(
            Cursor::new(&msg_bytes),
            Codec::builder().with_max_body_len(size - 1).finish(),
        );
        fr.next()
            .await
            .expect("a next message should be available")
            .expect_err("message should not decode as it is bigger than the max allowed value")
    });

    // receive again with the tx size as max size
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let mut fr = FramedRead::new(
            Cursor::new(&msg_bytes),
            Codec::builder().with_max_body_len(size).finish(),
        );
        fr.next()
            .await
            .expect("a next message should be available")
            .expect("message should decode with the msg body size as max allowed value")
    });
}

/// Check that the version test vector deserializes correctly without the relay byte
#[test]
fn version_message_omitted_relay() {
    let _init_guard = zebra_test::init();

    let version = match VERSION_TEST_VECTOR.clone() {
        Message::Version(mut version) => {
            version.relay = false;
            version.into()
        }
        _ => unreachable!("const is the Message::Version variant"),
    };

    let codec = Codec::builder().finish();
    let mut bytes = BytesMut::new();

    codec
        .write_body(&version, &mut (&mut bytes).writer())
        .expect("encoding should succeed");
    bytes.truncate(bytes.len() - 1);

    let relay = match codec.read_version(Cursor::new(&bytes)) {
        Ok(Message::Version(VersionMessage { relay, .. })) => relay,
        err => panic!("bytes should successfully decode to version message, got: {err:?}"),
    };

    assert!(relay, "relay should be true when omitted from message");
}

/// Check that the version test vector deserializes `relay` correctly with the relay byte
#[test]
fn version_message_with_relay() {
    let _init_guard = zebra_test::init();
    let codec = Codec::builder().finish();
    let mut bytes = BytesMut::new();

    codec
        .write_body(&VERSION_TEST_VECTOR, &mut (&mut bytes).writer())
        .expect("encoding should succeed");

    let relay = match codec.read_version(Cursor::new(&bytes)) {
        Ok(Message::Version(VersionMessage { relay, .. })) => relay,
        err => panic!("bytes should successfully decode to version message, got: {err:?}"),
    };

    assert!(relay, "relay should be true");

    bytes.clear();

    let version = match VERSION_TEST_VECTOR.clone() {
        Message::Version(mut version) => {
            version.relay = false;
            version.into()
        }
        _ => unreachable!("const is the Message::Version variant"),
    };

    codec
        .write_body(&version, &mut (&mut bytes).writer())
        .expect("encoding should succeed");

    let relay = match codec.read_version(Cursor::new(&bytes)) {
        Ok(Message::Version(VersionMessage { relay, .. })) => relay,
        err => panic!("bytes should successfully decode to version message, got: {err:?}"),
    };

    assert!(!relay, "relay should be false");
}
