//! Tests for [`TrustedChainSync`].

use std::{
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{Stream, StreamExt};
use tonic::{transport::server::TcpIncoming, Request, Response, Status};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
};
use zebra_state::{CheckpointVerifiedBlock, Config, FinalizedState, NonFinalizedState};

use crate::indexer::{
    indexer_server::{Indexer, IndexerServer},
    BlockAndHash, BlockHashAndHeight, BlockRequest, Empty, MempoolChangeMessage,
    NonFinalizedStateChangeRequest,
};

use super::TrustedChainSync;

/// How many times `get_block` for the gap block fails before it starts succeeding.
const GET_BLOCK_FAILURES: usize = 2;

/// A fully-custom mock implementation of the indexer gRPC server.
///
/// Unlike the production server it does not wrap a [`ReadStateService`]; instead each method is
/// driven directly to reproduce the "follower's finalized state is behind the primary" scenario:
/// the non-finalized stream yields a single block (the streamed block) whose parent (the gap block)
/// is not yet in the follower's finalized state, and `get_block` for that parent fails
/// [`GET_BLOCK_FAILURES`] times before succeeding, forcing the syncer to retry the streamed block
/// in place.
struct MockIndexer {
    /// Incremented at the start of every `non_finalized_state_change` call.
    subscribe_count: Arc<AtomicUsize>,
    /// Incremented on every `get_block` call for the gap block's height.
    get_block_attempts: Arc<AtomicUsize>,
    /// The block streamed on the non-finalized subscription (the gap block's child).
    streamed_block: Arc<Block>,
    /// The gap block fetched via `get_block` (the streamed block's parent).
    gap_block: Arc<Block>,
    /// The height of the gap block, the only height `get_block` is expected to be called with.
    gap_height: u32,
    /// How many times `get_block` for the gap block fails before succeeding.
    get_block_failures: usize,
}

#[tonic::async_trait]
impl Indexer for MockIndexer {
    type ChainTipChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockHashAndHeight, Status>> + Send>>;
    type NonFinalizedStateChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockAndHash, Status>> + Send>>;
    type MempoolChangeStream =
        Pin<Box<dyn Stream<Item = Result<MempoolChangeMessage, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        // The syncer's finalized-tip task subscribes here; never yield, so it just parks.
        Ok(Response::new(Box::pin(futures::stream::pending())))
    }

    async fn non_finalized_state_change(
        &self,
        _request: Request<NonFinalizedStateChangeRequest>,
    ) -> Result<Response<Self::NonFinalizedStateChangeStream>, Status> {
        self.subscribe_count.fetch_add(1, Ordering::SeqCst);

        // Yield exactly one block (the streamed block) then stay pending forever, so the syncer
        // parks after committing it rather than re-subscribing.
        let item = Ok(BlockAndHash::new(
            self.streamed_block.hash(),
            self.streamed_block.clone(),
        ));
        let stream = futures::stream::once(async move { item }).chain(futures::stream::pending());

        Ok(Response::new(Box::pin(stream)))
    }

    async fn mempool_change(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<Self::MempoolChangeStream>, Status> {
        Ok(Response::new(Box::pin(futures::stream::pending())))
    }

    async fn get_block(
        &self,
        request: Request<BlockRequest>,
    ) -> Result<Response<BlockAndHash>, Status> {
        // Decode the request the same way the production server does: a 4-byte big-endian height or
        // a 32-byte hash.
        let hash_or_height = request.into_inner().hash_or_height;
        let height = match hash_or_height.len() {
            4 => u32::from_be_bytes(
                hash_or_height
                    .try_into()
                    .expect("a 4-byte vec always converts to a [u8; 4]"),
            ),
            _ => return Err(Status::not_found("unexpected get_block")),
        };

        if height == self.gap_height {
            let n = self.get_block_attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.get_block_failures {
                Err(Status::unavailable("gap not ready"))
            } else {
                Ok(Response::new(BlockAndHash::new(
                    self.gap_block.hash(),
                    self.gap_block.clone(),
                )))
            }
        } else {
            Err(Status::not_found("unexpected get_block"))
        }
    }
}

/// Binds a mock indexer gRPC server on an ephemeral port and spawns it, returning its address.
async fn spawn_mock_indexer(mock: MockIndexer) -> SocketAddr {
    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral port should succeed");
    let local_addr = tcp_listener
        .local_addr()
        .expect("a bound listener has a local address");

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(IndexerServer::new(mock))
            .serve_with_incoming(TcpIncoming::from(tcp_listener))
            .await
            .expect("mock indexer server should run");
    });

    // Give the listener a moment to start accepting before the syncer connects.
    tokio::time::sleep(Duration::from_millis(200)).await;

    local_addr
}

/// Reads a continuous mainnet block from the test vectors by height.
fn mainnet_block(height: u32) -> Block {
    zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .get(&height)
        .unwrap_or_else(|| panic!("test vectors should contain mainnet block {height}"))
        .zcash_deserialize_into()
        .unwrap_or_else(|_| panic!("mainnet block {height} should deserialize"))
}

/// Returns a `Transaction::V4` carrying the coinbase data from `coinbase`.
///
/// The non-finalized state rejects the V1–V3 transactions in the early mainnet blocks (they only
/// ever exist in finalized blocks). Re-emitting the coinbase as V4 makes the block committable to
/// the non-finalized state without changing its (empty) spend/anchor footprint.
fn coinbase_as_v4(coinbase: &Transaction) -> Transaction {
    assert!(
        !coinbase.has_sapling_shielded_data(),
        "early-block coinbase transactions have no sapling shielded data"
    );

    Transaction::V4 {
        inputs: coinbase.inputs().to_vec(),
        outputs: coinbase.outputs().to_vec(),
        lock_time: coinbase.lock_time().unwrap_or_else(LockTime::unlocked),
        expiry_height: coinbase.expiry_height().unwrap_or(Height(0)),
        joinsplit_data: None,
        sapling_shielded_data: None,
    }
}

/// Rewrites `block` to hold only its coinbase transaction (re-emitted as V4) and to build on
/// `parent_hash`.
///
/// This yields a coinbase-only block that the non-finalized state accepts: there are no transparent
/// spends or shielded anchors to validate, and the explicit re-linking keeps the chain contiguous
/// even though re-encoding the coinbase changes each block's hash.
fn v4_coinbase_block(mut block: Block, parent_hash: Option<block::Hash>) -> Arc<Block> {
    let coinbase = coinbase_as_v4(&block.transactions[0]);
    block.transactions = vec![Arc::new(coinbase)];
    if let Some(parent_hash) = parent_hash {
        Arc::make_mut(&mut block.header).previous_block_hash = parent_hash;
    }

    Arc::new(block)
}

/// While a streamed block repeatedly fails to commit because the follower's finalized state is
/// behind and the gap block must be fetched via `get_block`, the syncer retries the same block in
/// place (keeping the subscription open) rather than re-subscribing, and commits it once the gap
/// becomes fillable.
#[tokio::test(flavor = "multi_thread")]
async fn syncer_retries_in_place_without_resubscribing() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // A short, contiguous chain of coinbase-only blocks (genesis, gap, streamed), each re-emitted
    // with a V4 coinbase so the gap and streamed blocks are committable to the non-finalized state,
    // and re-linked so the chain stays contiguous after the re-encoding changes their hashes.
    let genesis_block = v4_coinbase_block(mainnet_block(0), None);
    let gap_block = v4_coinbase_block(mainnet_block(1), Some(genesis_block.hash()));
    let streamed_block = v4_coinbase_block(mainnet_block(2), Some(gap_block.hash()));

    // Build a writable, genesis-only finalized state and take its db. The gap block (height 1) and
    // streamed block (height 2) sit above this finalized tip.
    let mut finalized = FinalizedState::new(&Config::ephemeral(), &network)
        .expect("creating an ephemeral finalized state should succeed");

    finalized
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis_block.clone()).into(),
            None,
            "test",
        )
        .expect("committing genesis to the finalized state should succeed");

    let db = finalized.db.clone();

    // Set up the mock indexer's shared counters.
    let subscribe_count = Arc::new(AtomicUsize::new(0));
    let get_block_attempts = Arc::new(AtomicUsize::new(0));

    let mock = MockIndexer {
        subscribe_count: subscribe_count.clone(),
        get_block_attempts: get_block_attempts.clone(),
        streamed_block: streamed_block.clone(),
        gap_block: gap_block.clone(),
        gap_height: 1,
        get_block_failures: GET_BLOCK_FAILURES,
    };

    let mock_addr = spawn_mock_indexer(mock).await;

    // Spawn the syncer against the mock indexer and the follower db.
    let (sender, _receiver) = tokio::sync::watch::channel(NonFinalizedState::new(&network));
    let (latest_chain_tip, _chain_tip_change, _sync_task) =
        TrustedChainSync::spawn(mock_addr, db, sender)
            .await
            .expect("spawning the syncer should succeed");

    let streamed_height = Height(2);

    // Poll until the syncer commits the streamed block (or time out).
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if latest_chain_tip.best_tip_height() == Some(streamed_height) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("syncer should commit the streamed block within the timeout");

    // The block was committed: in-place retry recovered once the gap became fillable.
    assert_eq!(
        latest_chain_tip.best_tip_height(),
        Some(streamed_height),
        "the streamed block should have been committed"
    );

    // THE key assertion: the syncer retried in place and never re-subscribed.
    assert_eq!(
        subscribe_count.load(Ordering::SeqCst),
        1,
        "the syncer should have subscribed exactly once, retrying in place instead of re-subscribing"
    );

    // It actively retried `get_block` while the gap was unfillable: more than the failing
    // attempts, i.e. it kept trying until the gap became fillable and the fetch finally succeeded.
    assert!(
        get_block_attempts.load(Ordering::SeqCst) > GET_BLOCK_FAILURES,
        "get_block should have been attempted more than {} times, got {}",
        GET_BLOCK_FAILURES,
        get_block_attempts.load(Ordering::SeqCst),
    );
}
