use super::*;

pub(super) fn frame(command: &[u8; 12], body: &[u8]) -> BytesMut {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&Network::Mainnet.magic().0);
    bytes.extend_from_slice(command);
    bytes
        .write_u32::<LittleEndian>(u32::try_from(body.len()).unwrap())
        .unwrap();
    bytes.extend_from_slice(&sha256d::Checksum::from(body).0);
    bytes.extend_from_slice(body);
    BytesMut::from(bytes.as_slice())
}

fn negotiate_compact(codec: &mut Codec) {
    codec.reconfigure_early_block_relay(true);
    let mut bytes = BytesMut::new();
    let mode = Message::SendCompact {
        high_bandwidth: true,
        version: 1,
    };
    codec.encode(mode.clone(), &mut bytes).unwrap();
    assert_eq!(codec.decode(&mut bytes).unwrap(), Some(mode));
}

fn track_compact_recovery(
    codec: &mut Codec,
    hash: block::Hash,
) -> tokio::sync::watch::Sender<Option<(block::Hash, tokio::time::Instant)>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let (sender, receiver) = tokio::sync::watch::channel(Some((hash, deadline)));
    codec.reconfigure_compact_recovery(receiver);
    sender
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_messages_round_trip() {
    let _guard = zebra_test::init();
    let block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    let compact = Arc::new(CompactBlock::from_block(&block, 23).unwrap());
    let messages = [
        Message::SendCompact {
            high_bandwidth: true,
            version: 1,
        },
        Message::SendCompact {
            high_bandwidth: false,
            version: u64::MAX,
        },
        Message::SendCompact {
            high_bandwidth: true,
            version: 1,
        },
        Message::CompactBlock(compact),
        Message::GetBlockTransactions(BlockTransactionsRequest {
            block_hash: block.hash(),
            indexes: vec![0, 1, 255],
        }),
        Message::BlockTransactions(BlockTransactions {
            block_hash: block.hash(),
            transactions: block.transactions.clone(),
        }),
    ];
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();
    negotiate_compact(&mut codec);
    let _recovery = track_compact_recovery(&mut codec, block.hash());
    for message in messages {
        let mut bytes = BytesMut::new();
        codec.encode(message.clone(), &mut bytes).unwrap();
        assert_eq!(
            &bytes[4..4 + message.command().len()],
            message.command().as_bytes()
        );
        assert_eq!(codec.decode(&mut bytes).unwrap(), Some(message));
        assert!(bytes.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_codec_rejects_malformed_messages() {
    let _guard = zebra_test::init();
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();
    negotiate_compact(&mut codec);
    let _recovery = track_compact_recovery(&mut codec, block::Hash([0; 32]));
    let block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    codec
        .encode(
            Message::CompactBlock(Arc::new(CompactBlock::from_block(&block, 23).unwrap())),
            &mut BytesMut::new(),
        )
        .unwrap();
    let mut mode = vec![2];
    mode.extend_from_slice(&1u64.to_le_bytes());
    assert!(codec.decode(&mut frame(b"sendcmpct\0\0\0", &mode)).is_err());
    assert!(codec.decode(&mut frame(b"sendcmpct\0\0\0", &[1])).is_err());
    let mut indexes = vec![0; 32];
    indexes.extend_from_slice(&[1, 0xfd, 0, 0]);
    assert!(codec
        .decode(&mut frame(b"getblocktxn\0", &indexes))
        .is_err());
    let mut count = vec![0; 32];
    count.extend_from_slice(&[0xfe, 255, 255, 255, 255]);
    assert!(codec
        .decode(&mut frame(b"blocktxn\0\0\0\0", &count))
        .is_err());
    assert!(codec.decode(&mut frame(b"cmpctblock\0\0", &[])).is_err());
    assert!(codec
        .encode(
            Message::GetBlockTransactions(BlockTransactionsRequest {
                block_hash: block::Hash([0; 32]),
                indexes: vec![2, 1],
            }),
            &mut BytesMut::new()
        )
        .is_err());
}

#[test]
fn compact_codec_enforces_message_size_limit() {
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();
    let mut bytes = frame(b"cmpctblock\0\0", &[]);
    let oversized = u32::try_from(MAX_PROTOCOL_MESSAGE_LEN + 1)
        .unwrap()
        .to_le_bytes();
    bytes[16..20].copy_from_slice(&oversized);
    assert!(codec.decode(&mut bytes).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_codec_negotiates_each_direction_independently() {
    let _guard = zebra_test::init();
    let block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    let compact = Message::CompactBlock(Arc::new(CompactBlock::from_block(&block, 23).unwrap()));
    for bilateral in [false, true] {
        for receive in [false, true] {
            for sent in [false, true] {
                let mut codec = Codec::builder()
                    .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
                    .finish();
                codec.reconfigure_early_block_relay(bilateral);
                codec
                    .encode(
                        Message::SendCompact {
                            high_bandwidth: receive,
                            version: 1,
                        },
                        &mut BytesMut::new(),
                    )
                    .unwrap();
                if sent {
                    codec.encode(compact.clone(), &mut BytesMut::new()).unwrap();
                }
                // The peer's receive preference cannot change our receive or serving permission.
                for (high_bandwidth, version) in [(false, 1u64), (true, 1), (true, 2)] {
                    let mut mode = vec![u8::from(high_bandwidth)];
                    mode.extend_from_slice(&version.to_le_bytes());
                    codec.decode(&mut frame(b"sendcmpct\0\0\0", &mode)).unwrap();
                    for (command, allowed) in [
                        (b"cmpctblock\0\0", bilateral && receive),
                        (b"blocktxn\0\0\0\0", false),
                        (b"getblocktxn\0", bilateral && sent),
                    ] {
                        let decoded = codec.decode(&mut frame(command, &[]));
                        if allowed {
                            assert!(decoded.is_err(), "negotiated bodies must be parsed");
                        } else {
                            assert_eq!(decoded.unwrap(), None, "unnegotiated bodies are ignored");
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_recovery_survives_receive_source_demotion() {
    let _guard = zebra_test::init();
    let block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();
    negotiate_compact(&mut codec);
    codec
        .encode(
            Message::GetBlockTransactions(BlockTransactionsRequest {
                block_hash: block.hash(),
                indexes: vec![0],
            }),
            &mut BytesMut::new(),
        )
        .unwrap();
    let recovery = track_compact_recovery(&mut codec, block.hash());
    codec
        .encode(
            Message::SendCompact {
                high_bandwidth: false,
                version: 1,
            },
            &mut BytesMut::new(),
        )
        .unwrap();
    assert_eq!(
        codec.decode(&mut frame(b"cmpctblock\0\0", &[])).unwrap(),
        None
    );
    let response = Message::BlockTransactions(BlockTransactions {
        block_hash: block.hash(),
        transactions: vec![block.transactions[0].clone()],
    });
    let mut bytes = BytesMut::new();
    codec.encode(response.clone(), &mut bytes).unwrap();
    assert_eq!(codec.decode(&mut bytes).unwrap(), Some(response));
    recovery.send_replace(None);
    assert_eq!(
        codec.decode(&mut frame(b"blocktxn\0\0\0\0", &[])).unwrap(),
        None
    );
}

#[tokio::test]
async fn compact_recovery_ignores_unrelated_expired_and_canceled_bodies_before_hashing() {
    let hash = block::Hash([1; 32]);
    for case in [
        "unrelated",
        "expired",
        "canceled",
        "disconnected",
        "disabled",
    ] {
        let mut codec = Codec::builder()
            .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
            .finish();
        negotiate_compact(&mut codec);
        let recovery = track_compact_recovery(&mut codec, hash);
        codec
            .encode(
                Message::SendCompact {
                    high_bandwidth: false,
                    version: 1,
                },
                &mut BytesMut::new(),
            )
            .unwrap();
        let mut body = hash.0.to_vec();
        match case {
            "unrelated" => body[0] ^= 1,
            "expired" => {
                recovery.send_replace(Some((hash, tokio::time::Instant::now())));
            }
            "canceled" => {
                recovery.send_replace(None);
            }
            "disconnected" => drop(recovery),
            "disabled" => codec.reconfigure_early_block_relay(false),
            _ => unreachable!(),
        }
        // Invalid checksums and malformed transaction counts must never reach either decoder stage.
        body.push(0xff);
        let mut bytes = frame(b"blocktxn\0\0\0\0", &body);
        bytes[20] ^= 1;
        assert_eq!(codec.decode(&mut bytes).unwrap(), None, "{case}");
        assert!(bytes.is_empty());
    }
}

/// A compact block message that the connection no longer negotiates, or a transaction recovery
/// response after its deadline, is ignored without holding up the message behind it in the same
/// read.
#[tokio::test]
async fn messages_after_unnegotiated_compact_messages_are_decoded() {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;
    use tokio_util::codec::FramedRead;

    let _guard = zebra_test::init();
    let ping = Message::Ping(Nonce(7));
    let mut ping_bytes = BytesMut::new();
    Codec::builder()
        .finish()
        .encode(ping.clone(), &mut ping_bytes)
        .unwrap();

    let hash = block::Hash([1; 32]);
    for case in ["withdrawn", "expired"] {
        let codec = || {
            let mut codec = Codec::builder()
                .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
                .finish();
            negotiate_compact(&mut codec);
            // This node stopped receiving compact announcements, and its recovery deadline passed.
            let recovery = track_compact_recovery(&mut codec, hash);
            recovery.send_replace(Some((hash, tokio::time::Instant::now())));
            codec
                .encode(
                    Message::SendCompact {
                        high_bandwidth: false,
                        version: 1,
                    },
                    &mut BytesMut::new(),
                )
                .unwrap();
            (codec, recovery)
        };
        let mut bytes = match case {
            "withdrawn" => frame(b"cmpctblock\0\0", &[0; 64]),
            "expired" => frame(b"blocktxn\0\0\0\0", &hash.0),
            _ => unreachable!(),
        };
        bytes.extend_from_slice(&ping_bytes);

        // One call decodes the message behind the ignored one.
        let (mut decoder, _recovery) = codec();
        let mut buffered = bytes.clone();
        assert_eq!(
            decoder.decode(&mut buffered).unwrap(),
            Some(ping.clone()),
            "{case}"
        );
        assert!(buffered.is_empty(), "{case}");

        // A reader delivers the message without waiting for more bytes from the peer.
        let (decoder, _recovery) = codec();
        let (mut writer, reader) = tokio::io::duplex(bytes.len());
        writer.write_all(&bytes).await.unwrap();
        let mut messages = FramedRead::new(reader, decoder);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), messages.next())
            .await
            .expect("the message is decoded from the bytes already received")
            .expect("the connection is open")
            .unwrap();
        assert_eq!(message, ping, "{case}");
        drop(writer);
    }
}
