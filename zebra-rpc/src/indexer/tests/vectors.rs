//! Fixed test vectors for indexer RPCs

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{
    sync::{broadcast, Semaphore},
    task::JoinHandle,
};
use tower::BoxError;
use zebra_chain::{
    block::{Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::ZcashDeserializeInto,
    transaction::{self, UnminedTxId},
};
use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
use zebra_state::{
    BlockReadError, HashOrHeight, NonFinalizedBlocksListener, ReadRequest, ReadResponse,
};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{
    self, indexer_client::IndexerClient, methods::block_range_stream, BlockAndHeight,
    BlockRangeRequest, BlockRequest, Empty, NonFinalizedStateChangeRequest,
};

#[tokio::test]
async fn rpc_server_spawn() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (
        _server_task,
        client,
        mock_read_service,
        mock_chain_tip_sender,
        mempool_transaction_sender,
    ) = start_server_and_get_client().await?;

    test_chain_tip_change(client.clone(), mock_chain_tip_sender).await?;
    test_mempool_change(client.clone(), mempool_transaction_sender).await?;
    test_get_block(client.clone(), mock_read_service.clone()).await?;
    test_get_block_range(client.clone(), mock_read_service.clone()).await?;
    test_non_finalized_state_change(client.clone(), mock_read_service).await?;

    Ok(())
}

/// Tests that `NonFinalizedStateChange` streams every buffered block when the listener channel is
/// already full as the subscription starts.
///
/// A full listener buffer is ordinary backpressure: the state side fills it while sending the
/// initial dump of the non-finalized state, faster than the consumer can serialize blocks onto the
/// wire. Treating it as a dead consumer drops the subscription on connect, and the client then
/// re-subscribes into the same condition.
async fn test_non_finalized_state_change(
    client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<()> {
    let blocks: Vec<Arc<Block>> = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_3_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
    ]
    .into_iter()
    .map(|bytes| bytes.zcash_deserialize_into())
    .collect::<std::result::Result<_, _>>()?;

    // Fill the listener channel to capacity, like the state side does when the non-finalized state
    // has more blocks than the buffer has slots.
    let (sender, receiver) = tokio::sync::mpsc::channel(blocks.len());
    for block in &blocks {
        sender
            .try_send((block.hash(), block.clone()))
            .expect("channel has one slot per block");
    }
    assert_eq!(
        receiver.capacity(),
        0,
        "the listener buffer should be full before the subscription starts"
    );

    let mut request_client = client.clone();
    let request_task = tokio::spawn(async move {
        request_client
            .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
                chain_tip_hashes: Vec::new(),
            }))
            .await
    });

    mock_read_service
        .expect_request(ReadRequest::NonFinalizedBlocksListener {
            known_chain_tips: HashSet::new(),
        })
        .await
        .respond(ReadResponse::NonFinalizedBlocksListener(
            NonFinalizedBlocksListener(Arc::new(receiver)),
        ));

    let mut response = request_task
        .await?
        .expect("non_finalized_state_change should succeed")
        .into_inner();

    for block in &blocks {
        let message = tokio::time::timeout(Duration::from_secs(3), response.next())
            .await
            .expect("should receive a non-finalized block before timeout")
            .expect("response stream should not end while blocks are buffered")
            .expect("non-finalized state change response should not be an error message");

        let (_decoded_block, decoded_hash) = message.decode().expect("response should decode");
        assert_eq!(decoded_hash, block.hash());
    }

    // Keep the state side of the channel open until every block has been received, so the stream
    // can only end early because the subscription was dropped.
    drop(sender);

    Ok(())
}

/// Tests that the `GetBlock` method returns the requested block and rejects an empty request.
async fn test_get_block(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<()> {
    // A request whose bytes are neither a 32-byte hash nor a 4-byte height is rejected without
    // touching the state.
    let status = client
        .get_block(tonic::Request::new(BlockRequest {
            hash_or_height: Vec::new(),
        }))
        .await
        .expect_err("a block request without a valid hash or height should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // A height above the maximum valid block height is rejected without touching the state.
    let status = client
        .get_block(tonic::Request::new(BlockRequest {
            hash_or_height: u32::MAX.to_be_bytes().to_vec(),
        }))
        .await
        .expect_err("an out-of-range block height should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // A block requested by height is returned along with its hash.
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let expected_hash = block.hash();
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let mut request_client = client.clone();
    let request_task = tokio::spawn(async move {
        request_client
            .get_block(tonic::Request::new(BlockRequest {
                hash_or_height: height.0.to_be_bytes().to_vec(),
            }))
            .await
    });

    mock_read_service
        .expect_request(ReadRequest::Block(HashOrHeight::Height(height)))
        .await
        .respond(ReadResponse::Block(Some(block.clone())));

    let response = request_task
        .await?
        .expect("get_block should succeed")
        .into_inner();
    let (decoded_block, decoded_hash) = response.decode().expect("response should decode");
    assert_eq!(decoded_hash, expected_hash);
    assert_eq!(decoded_block.hash(), expected_hash);

    Ok(())
}

/// Tests how a `GetBlockRange` stream ends: after the whole range, at the finalized tip, at a
/// missing block body, or on a state error. Invalid ranges are rejected without reading the state.
async fn test_get_block_range(
    client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<()> {
    for (start_height, end_height) in [(2, 1), (u32::MAX, u32::MAX), (0, u32::MAX)] {
        let status = client
            .clone()
            .get_block_range(tonic::Request::new(BlockRangeRequest {
                start_height,
                end_height,
            }))
            .await
            .expect_err("an invalid block range should be rejected");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    let blocks: Vec<(Height, Vec<u8>)> = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
    ]
    .into_iter()
    .map(|bytes| {
        let block: Block = bytes.zcash_deserialize_into()?;
        let height = block.coinbase_height().expect("test block has a height");
        Ok((height, bytes.to_vec()))
    })
    .collect::<Result<_>>()?;
    let raw_blocks =
        |range: std::ops::Range<usize>| ReadResponse::RawBlocks(blocks[range].to_vec());
    // Placeholder blocks for ranges longer than the test vectors: the server doesn't parse them.
    let placeholder_blocks = |heights: std::ops::RangeInclusive<u32>| {
        ReadResponse::RawBlocks(
            heights
                .map(|height| (Height(height), height.to_le_bytes().to_vec()))
                .collect(),
        )
    };
    let read = |start, count| ReadRequest::RawBlocksByHeightRange {
        start: Height(start),
        count,
    };

    // Each case is the requested range, the state's answer to each read, and the final status.
    type Reads = Vec<(ReadRequest, std::result::Result<ReadResponse, BoxError>)>;
    let cases: Vec<(u32, u32, Reads, Option<tonic::Code>)> = vec![
        // The whole range: ends `OK` after the last requested block, without another read.
        (1, 2, vec![(read(1, 2), Ok(raw_blocks(0..2)))], None),
        // The finalized tip is below the end: ends `OK` once a read is empty.
        (
            1,
            5,
            vec![
                (read(1, 5), Ok(raw_blocks(0..2))),
                (read(3, 3), Ok(raw_blocks(0..0))),
            ],
            None,
        ),
        // More than one chunk: each read starts after the last block of the one before, and the
        // stream ends `OK` after the last requested block.
        (
            1,
            17,
            vec![
                (read(1, 8), Ok(placeholder_blocks(1..=8))),
                (read(9, 8), Ok(placeholder_blocks(9..=16))),
                (read(17, 1), Ok(placeholder_blocks(17..=17))),
            ],
            None,
        ),
        // A missing body at height 3: ends `FAILED_PRECONDITION` after the blocks before it.
        (
            1,
            5,
            vec![
                (read(1, 5), Ok(raw_blocks(0..2))),
                (
                    read(3, 3),
                    Err(BlockReadError::BlockBodyUnavailable { height: Height(3) }.into()),
                ),
            ],
            Some(tonic::Code::FailedPrecondition),
        ),
        // Corrupt block metadata is a state error, not a missing body: ends `UNAVAILABLE`.
        (
            1,
            2,
            vec![(
                read(1, 2),
                Err(BlockReadError::CorruptBlock { height: Height(1) }.into()),
            )],
            Some(tonic::Code::Unavailable),
        ),
        // A state read error: ends `UNAVAILABLE`.
        (
            1,
            2,
            vec![(read(1, 2), Err("state read failed".into()))],
            Some(tonic::Code::Unavailable),
        ),
    ];

    for (start_height, end_height, reads, expected_status) in cases {
        let mut request_client = client.clone();
        let request_task = tokio::spawn(async move {
            request_client
                .get_block_range(tonic::Request::new(BlockRangeRequest {
                    start_height,
                    end_height,
                }))
                .await
        });

        let expected_blocks: Vec<_> = reads
            .iter()
            .filter_map(|(_, response)| match response {
                Ok(ReadResponse::RawBlocks(blocks)) => Some(blocks),
                _ => None,
            })
            .flatten()
            .map(|(height, data)| (height.0, data.clone()))
            .collect();
        for (request, response) in reads {
            mock_read_service
                .expect_request(request)
                .await
                .respond(response);
        }

        let stream = request_task
            .await?
            .expect("a valid block range should be accepted")
            .into_inner();
        let (received, status) = read_block_range(stream).await;

        assert_eq!(received, expected_blocks);
        assert_eq!(status.as_ref().map(tonic::Status::code), expected_status);
        assert!(
            status
                .as_ref()
                .is_none_or(|status| status.code() != tonic::Code::FailedPrecondition
                    || status.message().contains("height 3")),
            "FAILED_PRECONDITION should name the missing height: {status:?}"
        );
    }

    // A read the cases don't expect fails the next case's `expect_request`, or this check.
    mock_read_service.expect_no_requests().await;

    Ok(())
}

/// Reads a `GetBlockRange` stream to its end, returning its blocks and its final error status.
async fn read_block_range(
    mut stream: tonic::Streaming<BlockAndHeight>,
) -> (Vec<(u32, Vec<u8>)>, Option<tonic::Status>) {
    let mut received = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("the stream should make progress before timeout");
        match message {
            Some(Ok(block)) => received.push((block.height, block.data)),
            Some(Err(status)) => return (received, Some(status)),
            None => return (received, None),
        }
    }
}

/// Tests that a `GetBlockRange` stream whose consumer stops reading ends with `DEADLINE_EXCEEDED`
/// after the blocks already queued, even though the block channel is full, and that the stream
/// keeps its permit while those blocks are queued, after its producer task has ended.
#[tokio::test]
async fn get_block_range_slow_consumer_ends_with_deadline_exceeded() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError> =
        MockService::build().for_unit_tests();
    let send_timeout = Duration::from_millis(100);
    let permits = Arc::new(Semaphore::new(1));

    let mut stream = block_range_stream(
        mock_read_service.clone(),
        &permits,
        Height(0),
        Height(1000),
        send_timeout,
    )?;

    // The first chunk fills the block channel, so the first send of the second chunk times out.
    for start in [0, 8] {
        mock_read_service
            .expect_request(ReadRequest::RawBlocksByHeightRange {
                start: Height(start),
                count: 8,
            })
            .await
            .respond(ReadResponse::RawBlocks(
                (start..start + 8)
                    .map(|height| (Height(height), Vec::new()))
                    .collect(),
            ));
    }
    tokio::time::sleep(send_timeout * 10).await;
    mock_read_service.expect_no_requests().await;

    // The producer has timed out and ended, but its eight blocks are still queued.
    assert_eq!(permits.available_permits(), 0);

    for expected_height in 0..8 {
        let block = stream
            .next()
            .await
            .expect("queued blocks are delivered")
            .expect("queued blocks are not errors");
        assert_eq!(block.height, expected_height);
    }

    let status = stream
        .next()
        .await
        .expect("the stream ends with a status")
        .expect_err("the final status is an error");
    assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
    assert!(stream.next().await.is_none());

    assert_eq!(permits.available_permits(), 0);
    drop(stream);
    assert_eq!(permits.available_permits(), 1);

    Ok(())
}

/// Tests that a `GetBlockRange` stream whose client has gone away reads nothing from the state.
#[tokio::test]
async fn get_block_range_stops_reading_after_the_client_disconnects() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError> =
        MockService::build().for_unit_tests();

    drop(block_range_stream(
        mock_read_service.clone(),
        &Arc::new(Semaphore::new(1)),
        Height(0),
        Height(1000),
        Duration::from_secs(60),
    )?);
    mock_read_service.expect_no_requests().await;

    Ok(())
}

/// Tests that `GetBlockRange` refuses a stream with `RESOURCE_EXHAUSTED` while every permit is held,
/// that a short stream keeps its permit after its producer task has queued every block and ended,
/// and that the permit is free again once the stream is dropped.
#[tokio::test]
async fn get_block_range_refuses_streams_beyond_the_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError> =
        MockService::build().for_unit_tests();
    let permits = Arc::new(Semaphore::new(1));
    let read_service = mock_read_service.clone();
    let open_stream = || {
        block_range_stream(
            read_service.clone(),
            &permits,
            Height(0),
            Height(0),
            Duration::from_secs(60),
        )
    };

    let mut first = open_stream()?;

    // Refused while the first stream is reading from the state.
    let read = mock_read_service
        .expect_request(ReadRequest::RawBlocksByHeightRange {
            start: Height(0),
            count: 1,
        })
        .await;
    let Err(status) = open_stream() else {
        panic!("a stream beyond the limit should be refused");
    };
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);

    read.respond(ReadResponse::RawBlocks(vec![(Height(0), Vec::new())]));

    // The whole range fits in the queue, so the producer ends without waiting for the client.
    // The unread block still holds the permit.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(permits.available_permits(), 0);
    assert_eq!(
        open_stream().err().map(|status| status.code()),
        Some(tonic::Code::ResourceExhausted),
        "a stream with unread blocks should still count against the limit"
    );

    assert_eq!(first.next().await.expect("one block")?.height, 0);
    assert!(first.next().await.is_none());
    assert_eq!(permits.available_permits(), 0);

    // Dropping the drained stream frees its permit, once the producer task has also ended.
    drop(first);
    tokio::time::timeout(Duration::from_secs(3), async {
        while permits.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    drop(open_stream()?);

    Ok(())
}

async fn test_chain_tip_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mock_chain_tip_sender: MockChainTipSender,
) -> Result<()> {
    let request = tonic::Request::new(Empty {});
    let mut response = client.chain_tip_change(request).await?.into_inner();
    mock_chain_tip_sender.send_best_tip_height(Height::MIN);
    mock_chain_tip_sender.send_best_tip_hash(zebra_chain::block::Hash([0; 32]));

    tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive chain tip change notification before timeout")
        .expect("response stream should not be empty")
        .expect("chain tip change response should not be an error message");

    Ok(())
}

async fn test_mempool_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mempool_transaction_sender: tokio::sync::broadcast::Sender<MempoolChange>,
) -> Result<()> {
    let request = tonic::Request::new(Empty {});
    let mut response = client.mempool_change(request).await?.into_inner();

    let change_tx_ids = [UnminedTxId::Legacy(transaction::Hash::from([0; 32]))]
        .into_iter()
        .collect();

    mempool_transaction_sender
        .send(MempoolChange::added(change_tx_ids))
        .expect("rpc server should have a receiver");

    tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive chain tip change notification before timeout")
        .expect("response stream should not be empty")
        .expect("chain tip change response should not be an error message");

    Ok(())
}

async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
    MockChainTipSender,
    broadcast::Sender<MempoolChange>,
)> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();

    let (mock_chain_tip_change, mock_chain_tip_change_sender) = MockChainTip::new();
    let (mempool_transaction_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_tx_subscriber = MempoolTxSubscriber::new(mempool_transaction_sender.clone());
    let (server_task, listen_addr) = indexer::server::init(
        listen_addr,
        mock_read_service.clone(),
        mock_chain_tip_change,
        mempool_tx_subscriber.clone(),
    )
    .await
    .map_err(|err| eyre!(err))?;

    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{listen_addr}"))
        .unwrap()
        .timeout(Duration::from_secs(2));

    // connect to the gRPC server
    let client = IndexerClient::connect(endpoint)
        .await
        .expect("server should receive connection");

    Ok((
        server_task,
        client,
        mock_read_service,
        mock_chain_tip_change_sender,
        mempool_transaction_sender,
    ))
}
