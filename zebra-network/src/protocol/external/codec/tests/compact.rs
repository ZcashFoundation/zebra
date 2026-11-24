use super::*;

fn frame(command: &[u8; 12], body: &[u8]) -> BytesMut {
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
    count: usize,
) -> tokio::sync::watch::Sender<Option<ExpectedBlockTransactions>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let (sender, receiver) = tokio::sync::watch::channel(Some(ExpectedBlockTransactions {
        block_hash: hash,
        count,
        deadline,
    }));
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
    let _recovery = track_compact_recovery(&mut codec, block.hash(), block.transactions.len());
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
    let _recovery = track_compact_recovery(&mut codec, block::Hash([0; 32]), 1);
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
    // A response with the expected count and a missing transaction is decoded, and rejected.
    let mut missing = vec![0; 32];
    missing.push(1);
    assert!(codec
        .decode(&mut frame(b"blocktxn\0\0\0\0", &missing))
        .is_err());
    // A count that is larger than any message is ignored, like any other unexpected count.
    let mut count = vec![0; 32];
    count.extend_from_slice(&[0xfe, 255, 255, 255, 255]);
    assert_eq!(
        codec
            .decode(&mut frame(b"blocktxn\0\0\0\0", &count))
            .unwrap(),
        None
    );
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
    let recovery = track_compact_recovery(&mut codec, block.hash(), 1);
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
    for case in ["unrelated", "expired", "canceled", "disconnected"] {
        let mut codec = Codec::builder()
            .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
            .finish();
        negotiate_compact(&mut codec);
        let recovery = track_compact_recovery(&mut codec, hash, 0);
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
                recovery.send_replace(Some(ExpectedBlockTransactions {
                    block_hash: hash,
                    count: 0,
                    deadline: tokio::time::Instant::now(),
                }));
            }
            "canceled" => {
                recovery.send_replace(None);
            }
            "disconnected" => drop(recovery),
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

/// Without compact relay, compact messages are unknown commands: a wrong checksum is an error,
/// as it is for any other message, and a message with the right checksum is ignored.
#[test]
fn compact_messages_are_unknown_commands_without_compact_relay() {
    let _guard = zebra_test::init();

    for command in [
        b"sendcmpct\0\0\0",
        b"cmpctblock\0\0",
        b"getblocktxn\0",
        b"blocktxn\0\0\0\0",
    ] {
        let mut codec = Codec::builder().finish();

        let mut bytes = frame(command, &[1; 64]);
        assert_eq!(codec.decode(&mut bytes).unwrap(), None);
        assert!(bytes.is_empty());

        // The checksum is the last four bytes of the 24-byte header.
        let mut bytes = frame(command, &[1; 64]);
        bytes[20] ^= 1;
        assert!(codec.decode(&mut bytes).is_err());
    }
}

/// A `blocktxn` response with another number of transactions than the request asked for is
/// ignored without decoding its transactions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_recovery_ignores_responses_with_another_transaction_count() {
    let _guard = zebra_test::init();
    let block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    let mut codec = Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish();
    negotiate_compact(&mut codec);
    let _recovery = track_compact_recovery(&mut codec, block.hash(), 1);

    for transactions in [Vec::new(), block.transactions[..2].to_vec()] {
        let mut bytes = BytesMut::new();
        Codec::builder()
            .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
            .finish()
            .encode(
                Message::BlockTransactions(BlockTransactions {
                    block_hash: block.hash(),
                    transactions,
                }),
                &mut bytes,
            )
            .unwrap();
        assert_eq!(codec.decode(&mut bytes).unwrap(), None);
        assert!(bytes.is_empty());
    }

    let response = Message::BlockTransactions(BlockTransactions {
        block_hash: block.hash(),
        transactions: block.transactions[..1].to_vec(),
    });
    let mut bytes = BytesMut::new();
    codec.encode(response.clone(), &mut bytes).unwrap();
    assert_eq!(codec.decode(&mut bytes).unwrap(), Some(response));
}
