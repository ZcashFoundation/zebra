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
