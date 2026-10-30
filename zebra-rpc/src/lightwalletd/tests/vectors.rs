//! Fixed test vectors for the lightwalletd gRPC server.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::{buffer::Buffer, BoxError};

use zebra_chain::{
    block::{Block, Height},
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network::Mainnet,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::mempool::{self, MempoolChange, MempoolTxSubscriber};
use zebra_state::{BlockReadError, MinedTx, ReadRequest, ReadResponse, TransactionLocation};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::{
    lightwalletd::{
        self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, CompactTx,
        Empty, TransparentAddressBlockFilter, TxFilter,
    },
    methods::RpcImpl,
};

/// The mocked read state service the server is built on.
type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

/// The mocked mempool service the server is built on.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

/// Compact block lookups distinguish pruning from unknown blocks and other read failures.
#[tokio::test]
async fn compact_block_distinguishes_pruned_history() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (_server_task, client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let height = Height(42);

    for nullifiers_only in [false, true] {
        for (response, expected_code) in [
            (Ok(ReadResponse::Block(None)), tonic::Code::NotFound),
            (
                Err(BoxError::from(BlockReadError::BlockBodyUnavailable {
                    height,
                })),
                tonic::Code::FailedPrecondition,
            ),
            (
                Err(BoxError::from(BlockReadError::CorruptBlock { height })),
                tonic::Code::Internal,
            ),
            (
                Err(BoxError::from(
                    "block not available at height 42 (pruned data)",
                )),
                tonic::Code::Internal,
            ),
        ] {
            let mut client = client.clone();
            let request_task = tokio::spawn(async move {
                let request = tonic::Request::new(BlockId {
                    height: height.0.into(),
                    hash: Vec::new(),
                });
                if nullifiers_only {
                    client.get_block_nullifiers(request).await
                } else {
                    client.get_block(request).await
                }
            });
            read_state
                .expect_request(ReadRequest::Block(height.into()))
                .await
                .respond(response);

            let status = tokio::time::timeout(Duration::from_secs(3), request_task)
                .await??
                .expect_err("an unavailable block must not be served");
            assert_eq!(status.code(), expected_code);
            if expected_code == tonic::Code::FailedPrecondition {
                assert!(status.message().contains("42"));
            }
        }
    }

    Ok(())
}

/// Raw transaction lookups preserve unknown and operational errors.
#[tokio::test]
async fn get_transaction_distinguishes_pruned_history() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (_server_task, client, mut read_state, mut mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let height = Height(42);
    let hash = zebra_chain::transaction::Hash([7; 32]);

    for (response, expected_code) in [
        (
            Ok(ReadResponse::AnyChainTransaction(None)),
            tonic::Code::NotFound,
        ),
        (
            Err(BoxError::from(BlockReadError::BlockBodyUnavailable {
                height,
            })),
            tonic::Code::FailedPrecondition,
        ),
        (
            Err(BoxError::from(BlockReadError::CorruptBlock { height })),
            tonic::Code::Internal,
        ),
        (
            Err(BoxError::from(
                "transaction not available at height 42 (pruned data)",
            )),
            tonic::Code::Internal,
        ),
    ] {
        let mut client = client.clone();
        let request_task = tokio::spawn(async move {
            client
                .get_transaction(tonic::Request::new(TxFilter {
                    hash: hash.0.to_vec(),
                    ..Default::default()
                }))
                .await
        });
        read_state
            .expect_request(ReadRequest::AnyChainTransaction(hash))
            .await
            .respond(response);
        if expected_code == tonic::Code::NotFound {
            mempool
                .expect_request(mempool::Request::TransactionsByMinedId(
                    [hash].into_iter().collect(),
                ))
                .await
                .respond(mempool::Response::Transactions(Vec::new()));
        }

        let status = tokio::time::timeout(Duration::from_secs(3), request_task)
            .await??
            .expect_err("an unavailable transaction must not be served");
        assert_eq!(status.code(), expected_code);
        if expected_code == tonic::Code::FailedPrecondition {
            assert!(status.message().contains("42"));
        }
    }

    Ok(())
}

/// Both treestate endpoints distinguish unknown blocks from failed header reads.
#[tokio::test]
async fn treestate_preserves_header_read_errors() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (_server_task, client, mut read_state, _mempool, chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let height = Height(42);
    let hash = zebra_chain::block::Hash([7; 32]);
    chain_tip_sender.send_best_tip_height(height);
    chain_tip_sender.send_best_tip_hash(hash);

    for latest in [false, true] {
        for (response, code, message) in [
            (
                Ok(ReadResponse::BlockHeader(None)),
                tonic::Code::NotFound,
                "the requested block is not in the main chain".to_owned(),
            ),
            (
                Err(BoxError::from("database read failed")),
                tonic::Code::Internal,
                "database read failed".to_owned(),
            ),
            (
                Err(BoxError::from(BlockReadError::CorruptBlock { height })),
                tonic::Code::Internal,
                BlockReadError::CorruptBlock { height }.to_string(),
            ),
        ] {
            let mut client = client.clone();
            let request_task = tokio::spawn(async move {
                if latest {
                    client
                        .get_latest_tree_state(tonic::Request::new(Empty {}))
                        .await
                } else {
                    client
                        .get_tree_state(tonic::Request::new(BlockId {
                            height: height.0.into(),
                            hash: Vec::new(),
                        }))
                        .await
                }
            });
            let query = if latest { hash.into() } else { height.into() };
            read_state
                .expect_request(ReadRequest::BlockHeader(query))
                .await
                .respond(response);
            let status = tokio::time::timeout(Duration::from_secs(3), request_task)
                .await??
                .expect_err("unknown or unreadable headers cannot produce a treestate");
            assert_eq!(status.code(), code);
            assert_eq!(status.message(), message);
        }
    }

    Ok(())
}

/// A descending block stream delivers its retained block before the terminal pruning error.
#[tokio::test]
async fn block_range_ends_with_pruned_error_after_success() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (_server_task, client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;

    for nullifiers_only in [false, true] {
        let mut client = client.clone();
        let request_task = tokio::spawn(async move {
            let request = tonic::Request::new(BlockRange {
                start: Some(BlockId {
                    height: 3,
                    hash: Vec::new(),
                }),
                end: Some(BlockId {
                    height: 1,
                    hash: Vec::new(),
                }),
            });
            if nullifiers_only {
                client.get_block_range_nullifiers(request).await
            } else {
                client.get_block_range(request).await
            }
        });
        read_state
            .expect_request(ReadRequest::Block(Height(3).into()))
            .await
            .respond(ReadResponse::Block(Some(block.clone())));
        if !nullifiers_only {
            for _ in 0..2 {
                let handler = read_state
                    .expect_request_that(|request| {
                        matches!(
                            request,
                            ReadRequest::SaplingTree(_) | ReadRequest::OrchardTree(_)
                        )
                    })
                    .await;
                let response = match handler.request() {
                    ReadRequest::SaplingTree(_) => {
                        ReadResponse::SaplingTree(Some(Arc::new(Default::default())))
                    }
                    _ => ReadResponse::OrchardTree(Some(Arc::new(Default::default()))),
                };
                handler.respond(response);
            }
        }

        let mut stream = tokio::time::timeout(Duration::from_secs(3), request_task)
            .await???
            .into_inner();
        let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("the retained block must arrive")?;
        assert_eq!(first.height, 3);

        read_state
            .expect_request(ReadRequest::Block(Height(2).into()))
            .await
            .respond_error(BlockReadError::BlockBodyUnavailable { height: Height(2) }.into());
        let status = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("pruning must not become successful EOF")
            .expect_err("the pruned block must fail");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(status.message().contains("height 2"));
        assert!(tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .is_none());
    }

    Ok(())
}

/// Address transaction streams report a pruning race after sending an earlier transaction.
#[tokio::test]
async fn address_transactions_end_with_pruned_error_after_success() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (_server_task, client, mut read_state, _mempool, chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let first: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let second: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let first_tx = first.transactions[0].clone();
    let second_txid = second.transactions[0].hash();
    chain_tip_sender.send_best_tip_height(Height(2));
    chain_tip_sender.send_best_tip_hash(second.hash());

    for legacy in [false, true] {
        let mut client = client.clone();
        let address = first_tx.outputs()[1].address(&Mainnet).unwrap();
        let request_task = tokio::spawn(async move {
            let request = tonic::Request::new(TransparentAddressBlockFilter {
                address: address.to_string(),
                range: Some(BlockRange {
                    start: Some(BlockId {
                        height: 1,
                        hash: Vec::new(),
                    }),
                    end: Some(BlockId {
                        height: 2,
                        hash: Vec::new(),
                    }),
                }),
            });
            if legacy {
                client.get_taddress_txids(request).await
            } else {
                client.get_taddress_transactions(request).await
            }
        });
        read_state
            .expect_request_that(|request| {
                matches!(request, ReadRequest::TransactionIdsByAddresses { .. })
            })
            .await
            .respond(ReadResponse::AddressesTransactionIds(
                [
                    (
                        TransactionLocation::from_usize(Height(1), 0),
                        first_tx.hash(),
                    ),
                    (TransactionLocation::from_usize(Height(2), 0), second_txid),
                ]
                .into_iter()
                .collect(),
            ));
        read_state
            .expect_request(ReadRequest::Transaction(first_tx.hash()))
            .await
            .respond(ReadResponse::Transaction(Some(MinedTx::new(
                first_tx.clone(),
                Height(1),
                2,
                first.header.time,
                second.hash(),
            ))));

        let mut stream = tokio::time::timeout(Duration::from_secs(3), request_task)
            .await???
            .into_inner();
        let first_response = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("the first transaction must arrive")?;
        assert_eq!(first_response.data, first_tx.zcash_serialize_to_vec()?);
        assert_eq!(first_response.height, 1);

        read_state
            .expect_request(ReadRequest::Transaction(second_txid))
            .await
            .respond_error(BlockReadError::BlockBodyUnavailable { height: Height(2) }.into());
        let status = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("pruning must not become successful EOF")
            .expect_err("the pruned transaction must fail");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(status.message().contains("height 2"));
        assert!(tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .is_none());
    }

    Ok(())
}

/// A missing note commitment tree must not be served as an empty tree.
///
/// The block read can succeed while a tree read fails, for example when a reorg
/// lands between them. Reporting a tree size of zero would tell a light client
/// that no notes exist as of that block, corrupting every note position it
/// derives from it, so the request must fail instead.
#[tokio::test]
async fn get_block_errors_when_a_commitment_tree_is_missing() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let request_task = tokio::spawn(async move {
        client
            .get_block(tonic::Request::new(BlockId {
                height: height.0.into(),
                hash: Vec::new(),
            }))
            .await
    });

    read_state
        .expect_request_that(|request| matches!(request, ReadRequest::Block(_)))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // The trees are read concurrently, so answer whichever arrives first. Both are
    // missing here. This block is pre-NU6.3, so no Ironwood read is issued.
    for _ in 0..2 {
        let handler = read_state
            .expect_request_that(|request| {
                matches!(
                    request,
                    ReadRequest::SaplingTree(_) | ReadRequest::OrchardTree(_)
                )
            })
            .await;

        let response = match handler.request() {
            ReadRequest::SaplingTree(_) => ReadResponse::SaplingTree(None),
            _ => ReadResponse::OrchardTree(None),
        };

        handler.respond(response);
    }

    let status = request_task
        .await
        .expect("request task should not panic")
        .expect_err("a missing commitment tree must not be served as a zero tree size");

    assert_ne!(status.code(), tonic::Code::Ok);

    Ok(())
}

/// A `nullifiers_only` response must not read the note commitment trees.
///
/// lightwalletd's `pruneCompactBlockToNullifiers` zeroes the tree sizes, so reading
/// them costs two state reads per block on the bulk-scan path for a value the client
/// discards, and lets a reorg between the reads abort the whole stream.
#[tokio::test]
async fn get_block_nullifiers_does_not_read_the_commitment_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let request_task = tokio::spawn(async move {
        client
            .get_block_nullifiers(tonic::Request::new(BlockId {
                height: height.0.into(),
                hash: Vec::new(),
            }))
            .await
    });

    read_state
        .expect_request_that(|request| matches!(request, ReadRequest::Block(_)))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // Only the block was answered: if the trees were still read, this would hang.
    let compact_block = request_task
        .await
        .expect("request task should not panic")
        .expect("the block should be served without reading the trees")
        .into_inner();

    let chain_metadata = compact_block
        .chain_metadata
        .expect("compact blocks always carry chain metadata");

    assert_eq!(chain_metadata.sapling_commitment_tree_size, 0);
    assert_eq!(chain_metadata.orchard_commitment_tree_size, 0);
    assert_eq!(chain_metadata.ironwood_commitment_tree_size, 0);

    Ok(())
}

/// Before NU6.3 there is no Ironwood tree, so the Ironwood read is skipped entirely.
///
/// The Sapling and Orchard trees are missing-is-an-error, because a zero size would
/// corrupt the note positions a client derives. Ironwood is different only before its
/// activation height: zero is then the true size, so a pre-NU6.3 block must be served
/// with a zero Ironwood size without issuing the third state read at all.
#[tokio::test]
async fn get_block_serves_pre_nu6_3_blocks_without_an_ironwood_tree() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let request_task = tokio::spawn(async move {
        client
            .get_block(tonic::Request::new(BlockId {
                height: height.0.into(),
                hash: Vec::new(),
            }))
            .await
    });

    read_state
        .expect_request_that(|request| matches!(request, ReadRequest::Block(_)))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // Sapling and Orchard have trees at this height; Ironwood does not exist yet, so
    // only two reads arrive. If an Ironwood read were still issued, this would hang.
    for _ in 0..2 {
        let handler = read_state
            .expect_request_that(|request| {
                matches!(
                    request,
                    ReadRequest::SaplingTree(_) | ReadRequest::OrchardTree(_)
                )
            })
            .await;

        let response = match handler.request() {
            ReadRequest::SaplingTree(_) => {
                ReadResponse::SaplingTree(Some(Arc::new(Default::default())))
            }
            _ => ReadResponse::OrchardTree(Some(Arc::new(Default::default()))),
        };

        handler.respond(response);
    }

    let compact_block = request_task
        .await
        .expect("request task should not panic")
        .expect("a pre-NU6.3 block must be served without an Ironwood tree")
        .into_inner();

    let chain_metadata = compact_block
        .chain_metadata
        .expect("compact blocks always carry chain metadata");

    assert_eq!(chain_metadata.ironwood_commitment_tree_size, 0);

    Ok(())
}

/// Ironwood actions must be served in their own field, not merged into `actions`.
///
/// Ironwood reuses the Orchard action encoding, so the two are structurally identical
/// and easy to conflate. They commit to separate note commitment trees, so a client
/// that received them in one list would derive Ironwood note positions from the Orchard
/// tree.
#[test]
fn compact_tx_keeps_ironwood_actions_separate_from_orchard() {
    use zebra_chain::{
        parameters::NetworkUpgrade,
        transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction},
    };

    let _init_guard = zebra_test::init();

    let orchard = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        orchard::ValuePool::Orchard,
        1,
        1,
    )
    .expect("the Orchard pool is defined at NU6.3");
    let ironwood = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        orchard::ValuePool::Ironwood,
        2,
        2,
    )
    .expect("the Ironwood pool is defined at NU6.3");

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, Some(orchard), Some(ironwood));
    let compact_tx = CompactTx::from_transaction(0, tx.hash(), &tx, false);

    assert_eq!(
        compact_tx.actions.len(),
        1,
        "the Orchard bundle's actions belong in `actions`",
    );
    assert_eq!(
        compact_tx.ironwood_actions.len(),
        2,
        "the Ironwood bundle's actions belong in `ironwood_actions`",
    );

    // A nullifiers-only tx carries nullifiers for both pools, and nothing else.
    let nullifiers_only = CompactTx::from_transaction(0, tx.hash(), &tx, true);

    assert_eq!(nullifiers_only.ironwood_actions.len(), 2);
    assert!(nullifiers_only
        .ironwood_actions
        .iter()
        .all(|action| !action.nullifier.is_empty() && action.cmx.is_empty()));
}

/// A transaction whose only shielded data is an Ironwood bundle belongs in a compact block.
#[test]
fn ironwood_only_transactions_have_compact_data() {
    use zebra_chain::{
        parameters::NetworkUpgrade,
        transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction},
    };

    let _init_guard = zebra_test::init();

    let ironwood = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        orchard::ValuePool::Ironwood,
        1,
        1,
    )
    .expect("the Ironwood pool is defined at NU6.3");

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));

    assert!(
        lightwalletd::has_compact_data(&tx),
        "an Ironwood-only transaction must not be filtered out of a compact block",
    );
    assert!(
        lightwalletd::has_nullifiers(&tx),
        "an Ironwood-only transaction must not be filtered out of a nullifiers-only block",
    );
}

/// A raw transaction larger than a block must be rejected before it is hex-encoded.
///
/// Hex-encoding doubles a client-controlled buffer, and a transaction that big can
/// never be mined.
#[tokio::test]
async fn send_transaction_rejects_oversized_raw_transactions() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, _read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let oversized = vec![0; zebra_chain::block::MAX_BLOCK_BYTES as usize + 1];

    let status = client
        .send_transaction(tonic::Request::new(lightwalletd::RawTransaction {
            data: oversized,
            height: 0,
        }))
        .await
        .expect_err("an oversized transaction must be rejected");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// A lagged mempool change subscription must not silently drop transactions.
///
/// The change channel is bounded, so a stream that falls behind loses changes and the
/// transactions they carried. Skipping them would leave the client short with no way to
/// notice, so the mempool is re-read instead.
#[tokio::test]
async fn mempool_stream_re_reads_the_mempool_after_lagging() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, _read_state, mut mempool, _chain_tip_sender, mempool_change) =
        start_server_and_get_client().await?;

    // Hold the stream open for the whole test, so the server keeps serving it.
    let _stream_task = tokio::spawn(async move {
        let response = client
            .get_mempool_stream(tonic::Request::new(Empty {}))
            .await
            .expect("the mempool stream should open");

        let mut stream = response.into_inner();
        while stream.next().await.is_some() {}
    });

    // Hold the initial snapshot open, so the change subscription falls behind while the
    // server is busy with it.
    let initial_read = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .await;

    // The change channel holds one message, so this leaves the subscription lagging.
    for _ in 0..3 {
        let _ = mempool_change.send(MempoolChange::added(HashSet::new()));
    }

    initial_read.respond(empty_mempool_response());

    // Lagging must trigger a second read of the mempool rather than being skipped.
    mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .await
        .respond(empty_mempool_response());

    Ok(())
}

/// Returns a mempool response with no transactions in it.
fn empty_mempool_response() -> mempool::Response {
    mempool::Response::FullTransactions {
        transactions: Vec::new(),
        transaction_dependencies: Default::default(),
        last_seen_tip_hash: zebra_chain::block::Hash([0; 32]),
    }
}

/// Starts the lightwalletd gRPC server against mocked services, and connects a client to it.
async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    CompactTxStreamerClient<tonic::transport::Channel>,
    MockReadState,
    MockMempool,
    MockChainTipSender,
    broadcast::Sender<MempoolChange>,
)> {
    let listen_addr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and port should parse");

    let read_state: MockReadState = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let mempool: MockMempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    let (mempool_change_sender, _) = broadcast::channel(1);

    let (_misbehavior_tx, misbehavior_rx) = tokio::sync::watch::channel(None);
    let (rpc, _rpc_tx_queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "lightwalletd test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state, 1),
        Buffer::new(read_state.clone(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        chain_tip.clone(),
        MockAddressBookPeers::default(),
        misbehavior_rx,
        None,
    );

    let (server_task, listen_addr) = lightwalletd::server::init(
        listen_addr,
        rpc,
        read_state.clone(),
        Buffer::new(mempool.clone(), 1),
        chain_tip,
        MempoolTxSubscriber::new(mempool_change_sender.clone()),
        Mainnet,
    )
    .await
    .map_err(|error| eyre!(error))?;

    // Retry the connection instead of sleeping for a fixed time, so the test does
    // not depend on how long the server takes to start accepting.
    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{listen_addr}"))
        .expect("endpoint built from a bound address should parse")
        .timeout(Duration::from_secs(2));

    let mut client = None;
    for _ in 0..50 {
        if let Ok(connected) = CompactTxStreamerClient::connect(endpoint.clone()).await {
            client = Some(connected);
            break;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let client = client.ok_or_else(|| eyre!("could not connect to the lightwalletd server"))?;

    Ok((
        server_task,
        client,
        read_state,
        mempool,
        chain_tip_sender,
        mempool_change_sender,
    ))
}
