//! Automated tests for indexer gRPC latency and active stream metrics.

use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::sync::{broadcast, mpsc};
use tower::BoxError;
use zebra_chain::{
    block::{self, Block},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
use zebra_state::{HashOrHeight, NonFinalizedBlocksListener, ReadRequest, ReadResponse};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::{
    indexer::{
        self, indexer_client::IndexerClient, BlockAndHash, BlockRequest, Empty,
        NonFinalizedStateChangeRequest,
    },
    metrics::tests::TestMetricsRecorder,
};

const STREAM_METHODS: [&str; 3] = ["ChainTipChange", "NonFinalizedStateChange", "MempoolChange"];

async fn start_indexer_server() -> Result<(
    tokio::task::JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
    MockChainTipSender,
    broadcast::Sender<MempoolChange>,
)> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse");
    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();
    let (mempool_tx_sender, _) = broadcast::channel(1);
    let mempool_tx_subscriber = MempoolTxSubscriber::new(mempool_tx_sender.clone());

    let (server_task, addr) = indexer::server::init(
        listen_addr,
        mock_read_service.clone(),
        mock_chain_tip,
        mempool_tx_subscriber,
    )
    .await
    .map_err(|err| eyre!(err))?;

    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{addr}"))
        .unwrap()
        .timeout(Duration::from_secs(2));
    let client = IndexerClient::connect(endpoint)
        .await
        .expect("client should connect to indexer server");

    Ok((
        server_task,
        client,
        mock_read_service,
        mock_chain_tip_sender,
        mempool_tx_sender,
    ))
}

async fn wait_for_condition(description: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

async fn open_non_finalized_stream(
    client: &IndexerClient<tonic::transport::Channel>,
    mock_read_service: &mut MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<(
    tonic::Streaming<BlockAndHash>,
    mpsc::Sender<(block::Hash, Arc<Block>)>,
)> {
    let (nf_tx, nf_rx) = mpsc::channel(1);
    let mut nf_client = client.clone();
    let nf_task = tokio::spawn(async move {
        nf_client
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
            NonFinalizedBlocksListener(Arc::new(nf_rx)),
        ));
    let stream = nf_task
        .await?
        .expect("valid non_finalized_state_change stream opens")
        .into_inner();
    Ok((stream, nf_tx))
}

#[tokio::test]
async fn indexer_grpc_initializes_active_stream_gauges_at_zero() -> Result<()> {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (server_task, _client, _read_state, _tip_sender, _mempool_tx) =
        start_indexer_server().await?;

    for method in STREAM_METHODS {
        assert_eq!(
            recorder.gauge("indexer.grpc.streams.active", &[("method", method)]),
            Some(0.0)
        );
    }

    server_task.abort();
    Ok(())
}

#[tokio::test]
async fn indexer_grpc_get_block_records_duration_for_valid_and_invalid_requests() -> Result<()> {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (server_task, mut client, mut mock_read_service, _tip_sender, _mempool_tx) =
        start_indexer_server().await?;

    let err = client
        .get_block(tonic::Request::new(BlockRequest {
            hash_or_height: Vec::new(),
        }))
        .await
        .expect_err("empty hash_or_height should fail");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block.coinbase_height().expect("test block has height");
    let mut client_clone = client.clone();
    let req_task = tokio::spawn(async move {
        client_clone
            .get_block(tonic::Request::new(BlockRequest {
                hash_or_height: height.0.to_be_bytes().to_vec(),
            }))
            .await
    });
    mock_read_service
        .expect_request(ReadRequest::Block(HashOrHeight::Height(height)))
        .await
        .respond(ReadResponse::Block(Some(block)));
    req_task.await?.expect("valid get_block succeeds");

    assert_eq!(
        recorder
            .histogram(
                "indexer.grpc.request.duration_seconds",
                &[("method", "GetBlock")],
            )
            .len(),
        2
    );

    server_task.abort();
    Ok(())
}

#[tokio::test]
async fn indexer_grpc_rejected_non_finalized_stream_does_not_touch_active_gauge() -> Result<()> {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (server_task, mut client, _read_state, _tip_sender, _mempool_tx) =
        start_indexer_server().await?;

    let err = client
        .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
            chain_tip_hashes: vec![vec![0; 31]],
        }))
        .await
        .expect_err("31-byte hash should be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        recorder
            .histogram(
                "indexer.grpc.request.duration_seconds",
                &[("method", "NonFinalizedStateChange")],
            )
            .len(),
        1
    );
    assert_eq!(
        recorder.gauge_updates(
            "indexer.grpc.streams.active",
            &[("method", "NonFinalizedStateChange")],
        ),
        1,
        "only the startup zero-init should have updated the gauge",
    );

    server_task.abort();
    Ok(())
}

#[tokio::test]
async fn indexer_grpc_active_stream_gauges_return_to_zero_and_stop_producers_on_drop() -> Result<()>
{
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (server_task, mut client, mut mock_read_service, _tip_sender, mempool_tx_sender) =
        start_indexer_server().await?;

    let chain_stream = client
        .chain_tip_change(tonic::Request::new(Empty {}))
        .await?
        .into_inner();
    let mempool_stream = client
        .mempool_change(tonic::Request::new(Empty {}))
        .await?
        .into_inner();
    let (nf_stream, nf_tx) = open_non_finalized_stream(&client, &mut mock_read_service).await?;

    for method in STREAM_METHODS {
        assert_eq!(
            recorder.gauge("indexer.grpc.streams.active", &[("method", method)]),
            Some(1.0)
        );
        assert_eq!(
            recorder
                .histogram(
                    "indexer.grpc.request.duration_seconds",
                    &[("method", method)],
                )
                .len(),
            1
        );
    }

    drop(chain_stream);
    drop(nf_stream);
    drop(mempool_stream);

    for method in STREAM_METHODS {
        wait_for_condition(&format!("{method} active gauge == 0.0"), || {
            recorder.gauge("indexer.grpc.streams.active", &[("method", method)]) == Some(0.0)
        })
        .await;
    }

    // Dropping the `TrackedStream` must also drop its inner `ReceiverStream` and stop producers
    // promptly without waiting for an upstream event.
    tokio::time::timeout(Duration::from_secs(3), nf_tx.closed())
        .await
        .expect("non_finalized_state_change producer should exit promptly on client drop");
    wait_for_condition("mempool_change producer to unsubscribe", || {
        mempool_tx_sender.receiver_count() == 0
    })
    .await;

    server_task.abort();
    Ok(())
}
