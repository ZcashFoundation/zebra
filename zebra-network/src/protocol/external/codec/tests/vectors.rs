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

/// Check that the codec enforces size limits on `user_agent` field of version messages.
#[test]
fn version_user_agent_size_limits() {
    let _init_guard = zebra_test::init();
    let codec = Codec::builder().finish();
    let mut bytes = BytesMut::new();
    let [valid_version_message, invalid_version_message]: [Message; 2] = {
        let services = PeerServices::NODE_NETWORK;
        let timestamp = Utc
            .timestamp_opt(1_568_000_000, 0)
            .single()
            .expect("in-range number of seconds and valid nanosecond");

        [
            "X".repeat(MAX_USER_AGENT_LENGTH),
            "X".repeat(MAX_USER_AGENT_LENGTH + 1),
        ]
        .map(|user_agent| {
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
                user_agent,
                start_height: block::Height(540_000),
                relay: true,
            }
            .into()
        })
    };

    // Check that encoding and decoding will succeed when the user_agent is not longer than MAX_USER_AGENT_LENGTH
    codec
        .write_body(&valid_version_message, &mut (&mut bytes).writer())
        .expect("encoding valid version msg should succeed");
    codec
        .read_version(Cursor::new(&bytes))
        .expect("decoding valid version msg should succeed");

    bytes.clear();

    let mut writer = (&mut bytes).writer();

    // Check that encoding will return an error when the user_agent is longer than MAX_USER_AGENT_LENGTH
    match codec.write_body(&invalid_version_message, &mut writer) {
        Err(Error::Parse(error_msg)) if error_msg.contains("user agent too long") => {}
        result => panic!("expected write error: user agent too long, got: {result:?}"),
    };

    // Encode the rest of the message onto `bytes` (relay should be optional)
    {
        let Message::Version(VersionMessage {
            user_agent,
            start_height,
            ..
        }) = invalid_version_message
        else {
            unreachable!("version_message is a version");
        };

        user_agent
            .zcash_serialize(&mut writer)
            .expect("writing user_agent should succeed");
        writer
            .write_u32::<LittleEndian>(start_height.0)
            .expect("writing start_height should succeed");
    }

    // Check that decoding will return an error when the user_agent is longer than MAX_USER_AGENT_LENGTH
    match codec.read_version(Cursor::new(&bytes)) {
        Err(Error::Parse(error_msg)) if error_msg.contains("user agent too long") => {}
        result => panic!("expected read error: user agent too long, got: {result:?}"),
    };
}

/// Check that the codec enforces size limits on `message` and `reason` fields of reject messages.
#[test]
fn reject_command_and_reason_size_limits() {
    let _init_guard = zebra_test::init();
    let codec = Codec::builder().finish();
    let mut bytes = BytesMut::new();

    let valid_message = "X".repeat(MAX_REJECT_MESSAGE_LENGTH);
    let invalid_message = "X".repeat(MAX_REJECT_MESSAGE_LENGTH + 1);
    let valid_reason = "X".repeat(MAX_REJECT_REASON_LENGTH);
    let invalid_reason = "X".repeat(MAX_REJECT_REASON_LENGTH + 1);

    let valid_reject_message = Message::Reject {
        message: valid_message.clone(),
        ccode: RejectReason::Invalid,
        reason: valid_reason.clone(),
        data: None,
    };

    // Check that encoding and decoding will succeed when `message` and `reason` fields are within size limits.
    codec
        .write_body(&valid_reject_message, &mut (&mut bytes).writer())
        .expect("encoding valid reject msg should succeed");
    codec
        .read_reject(Cursor::new(&bytes))
        .expect("decoding valid reject msg should succeed");

    let invalid_reject_messages = [
        (
            "reject message too long",
            Message::Reject {
                message: invalid_message,
                ccode: RejectReason::Invalid,
                reason: valid_reason,
                data: None,
            },
        ),
        (
            "reject reason too long",
            Message::Reject {
                message: valid_message,
                ccode: RejectReason::Invalid,
                reason: invalid_reason,
                data: None,
            },
        ),
    ];

    for (expected_err_msg, invalid_reject_message) in invalid_reject_messages {
        // Check that encoding will return an error when the reason or message are too long.
        match codec.write_body(&invalid_reject_message, &mut (&mut bytes).writer()) {
            Err(Error::Parse(error_msg)) if error_msg.contains(expected_err_msg) => {}
            result => panic!("expected write error: {expected_err_msg}, got: {result:?}"),
        };

        bytes.clear();

        // Encode the message onto `bytes` without size checks
        {
            let Message::Reject {
                message,
                ccode,
                reason,
                data,
            } = invalid_reject_message
            else {
                unreachable!("invalid_reject_message is a reject");
            };

            let mut writer = (&mut bytes).writer();

            message
                .zcash_serialize(&mut writer)
                .expect("writing message should succeed");

            writer
                .write_u8(ccode as u8)
                .expect("writing ccode should succeed");

            reason
                .zcash_serialize(&mut writer)
                .expect("writing reason should succeed");

            if let Some(data) = data {
                writer
                    .write_all(&data)
                    .expect("writing data should succeed");
            }
        }

        // Check that decoding will return an error when the reason or message are too long.
        match codec.read_reject(Cursor::new(&bytes)) {
            Err(Error::Parse(error_msg)) if error_msg.contains(expected_err_msg) => {}
            result => panic!("expected read error: {expected_err_msg}, got: {result:?}"),
        };
    }
}

/// Check that the version test vector deserialization fails when there's a network magic mismatch.
#[test]
fn message_with_wrong_network_magic_returns_error() {
    let _init_guard = zebra_test::init();
    let mut codec = Codec::builder().finish();
    let mut bytes = BytesMut::new();

    codec
        .encode(VERSION_TEST_VECTOR.clone(), &mut bytes)
        .expect("encoding should succeed");

    let mut codec = Codec::builder()
        .for_network(&Network::new_default_testnet())
        .finish();

    codec
        .decode(&mut bytes)
        .expect_err("decoding message with mismatching network magic should return an error");
}
