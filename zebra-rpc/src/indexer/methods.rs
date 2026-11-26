//! Implements `Indexer` methods on the `IndexerRPC` type

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::Stream;
use tokio::sync::{broadcast, mpsc, oneshot, OwnedSemaphorePermit};
use tonic::{Response, Status};
use tower::util::ServiceExt;

use tracing::Span;
use zebra_chain::{
    block, chain_tip::ChainTip, serialization::BytesInDisplayOrder, transaction::UnminedTxId,
};
use zebra_node_services::mempool::MempoolChangeKind;
use zebra_state::{
    ReadRequest, ReadResponse, ReadState, MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
};

use super::{
    indexer_server::Indexer, server::IndexerRPC, BlockAndHash, BlockHashAndHeight, BlockRequest,
    Empty, MempoolChangeMessage, NonFinalizedStateChangeRequest,
};

/// The maximum number of messages that can be queued to be streamed to a client.
const RESPONSE_BUFFER_SIZE: usize = 64;

/// How long to wait for a backpressured send before treating the consumer as hung and dropping
/// the subscription.
///
/// All three indexer streams apply backpressure so a slow consumer doesn't miss notifications,
/// but without a bound a consumer whose connection is half-open (dead TCP not yet detected) would
/// block the listener task indefinitely.
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

impl<ReadStateService, Tip> IndexerRPC<ReadStateService, Tip>
where
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    /// Acquires a server-wide permit for an active streaming subscription.
    fn acquire_subscription_permit(&self) -> Result<Arc<OwnedSemaphorePermit>, Status> {
        self.subscription_permits
            .clone()
            .try_acquire_owned()
            .map(Arc::new)
            .map_err(|_| {
                Status::resource_exhausted("too many active indexer streaming subscriptions")
            })
    }
}

#[tonic::async_trait]
impl<ReadStateService, Tip> Indexer for IndexerRPC<ReadStateService, Tip>
where
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type ChainTipChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockHashAndHeight, Status>> + Send>>;
    type NonFinalizedStateChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockAndHash, Status>> + Send>>;
    type MempoolChangeStream =
        Pin<Box<dyn Stream<Item = Result<MempoolChangeMessage, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        let subscription_permit = self.acquire_subscription_permit()?;
        let span = Span::current();
        let (mut response_sender, response_stream) = indexer_stream_channel(subscription_permit);
        let mut chain_tip_change = self.chain_tip_change.clone();

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed or the client
            // disconnects.
            loop {
                let changed = tokio::select! {
                    biased;
                    () = response_sender.closed() => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping chain_tip_change task");
                        });
                        return;
                    }
                    result = chain_tip_change.best_tip_changed() => result,
                };

                if changed.is_err() {
                    break;
                }

                let Some((tip_height, tip_hash)) = chain_tip_change.best_tip_height_and_hash()
                else {
                    continue;
                };

                if !response_sender
                    .send_item(
                        BlockHashAndHeight::new(tip_hash, tip_height),
                        "chain_tip_change",
                        &span,
                    )
                    .await
                {
                    return;
                }
            }

            span.in_scope(|| {
                tracing::warn!("chain_tip_change channel has closed");
            });

            response_sender
                .finish_with_status(Status::unavailable("chain_tip_change channel has closed"));
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn non_finalized_state_change(
        &self,
        request: tonic::Request<NonFinalizedStateChangeRequest>,
    ) -> Result<Response<Self::NonFinalizedStateChangeStream>, Status> {
        // The caller may provide the hashes of the chain tips it already has so the server only
        // streams blocks after those tips. Malformed hashes (wrong length) are rejected up front.
        let known_chain_tips = decode_known_chain_tips(request.into_inner().chain_tip_hashes)?;
        let subscription_permit = self.acquire_subscription_permit()?;

        let span = Span::current();
        let read_state = self.read_state.clone();
        let (mut response_sender, response_stream) = indexer_stream_channel(subscription_permit);

        tokio::spawn(async move {
            let subscribe_result = tokio::select! {
                biased;
                () = response_sender.closed() => {
                    span.in_scope(|| {
                        tracing::info!(
                            "client disconnected, dropping non_finalized_state_change task"
                        );
                    });
                    return;
                }
                result = read_state.oneshot(ReadRequest::NonFinalizedBlocksListener {
                    known_chain_tips,
                }) => result,
            };

            let mut non_finalized_state_change = match subscribe_result {
                Ok(ReadResponse::NonFinalizedBlocksListener(listener)) => listener.unwrap(),
                Ok(_) => unreachable!("unexpected response type from ReadStateService"),
                Err(error) => {
                    span.in_scope(|| {
                        tracing::error!(
                            ?error,
                            "failed to subscribe to non-finalized state changes"
                        );
                    });

                    response_sender.finish_with_status(Status::unavailable(
                        "failed to subscribe to non-finalized state changes",
                    ));
                    return;
                }
            };

            loop {
                let next_block = tokio::select! {
                    biased;
                    () = response_sender.closed() => {
                        span.in_scope(|| {
                            tracing::info!(
                                "client disconnected, dropping non_finalized_state_change task"
                            );
                        });
                        return;
                    }
                    block = non_finalized_state_change.recv() => block,
                };

                let Some((hash, block)) = next_block else {
                    break;
                };

                if !response_sender
                    .send_item(
                        BlockAndHash::new(hash, block),
                        "non_finalized_state_change",
                        &span,
                    )
                    .await
                {
                    return;
                }
            }

            span.in_scope(|| {
                tracing::warn!("non-finalized state change channel has closed");
            });

            response_sender.finish_with_status(Status::unavailable(
                "non-finalized state change channel has closed",
            ));
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn mempool_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::MempoolChangeStream>, Status> {
        let subscription_permit = self.acquire_subscription_permit()?;
        let span = Span::current();
        let (mut response_sender, response_stream) = indexer_stream_channel(subscription_permit);
        let mut mempool_change = self.mempool_change.subscribe();

        tokio::spawn(async move {
            // Notify the client of mempool changes until the channel closes, lags, or the client
            // disconnects.
            loop {
                let change = tokio::select! {
                    biased;
                    () = response_sender.closed() => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping mempool_change task");
                        });
                        return;
                    }
                    result = mempool_change.recv() => result,
                };

                match change {
                    Ok(change) => {
                        for tx_id in change.tx_ids() {
                            span.in_scope(|| {
                                tracing::debug!("mempool change: {:?}", change);
                            });

                            let msg = mempool_change_message(change.kind(), tx_id);
                            if !response_sender
                                .send_item(msg, "mempool_change", &span)
                                .await
                            {
                                return;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                skipped,
                                "mempool_change stream lagged, dropping subscription"
                            );
                        });

                        response_sender.finish_with_status(Status::aborted(format!(
                            "mempool_change stream lagged by {skipped} updates; \
                             resubscribe and resync"
                        )));
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        span.in_scope(|| {
                            tracing::warn!("mempool_change channel has closed");
                        });

                        response_sender.finish_with_status(Status::unavailable(
                            "mempool_change channel has closed",
                        ));
                        return;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn get_block(
        &self,
        request: tonic::Request<BlockRequest>,
    ) -> Result<Response<BlockAndHash>, Status> {
        // The request carries a single `hash_or_height` byte string: a 32-byte block hash in
        // display order, or a 4-byte big-endian block height. The length tells the two apart.
        let hash_or_height = request.into_inner().hash_or_height;
        let hash_or_height = match hash_or_height.len() {
            32 => zebra_state::HashOrHeight::Hash(hash_from_display_bytes(hash_or_height)?),
            4 => {
                let height = u32::from_be_bytes(
                    hash_or_height
                        .try_into()
                        .expect("a 4-byte vec always converts to a [u8; 4]"),
                );
                let height = block::Height::try_from(height).map_err(|_| {
                    Status::invalid_argument(format!("block height out of range: {height}"))
                })?;
                zebra_state::HashOrHeight::Height(height)
            }
            len => {
                return Err(Status::invalid_argument(format!(
                    "block request must be a 32-byte hash or a 4-byte height, got {len} bytes"
                )));
            }
        };

        match self
            .read_state
            .clone()
            .oneshot(ReadRequest::Block(hash_or_height))
            .await
        {
            Ok(ReadResponse::Block(Some(block))) => {
                Ok(Response::new(BlockAndHash::new(block.hash(), block)))
            }
            Ok(ReadResponse::Block(None)) => Err(Status::not_found("block not found")),
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => Err(Status::unavailable(format!(
                "failed to read block: {error}"
            ))),
        }
    }
}

/// Producer half of an indexer server stream, pairing a bounded item queue with a side channel
/// for its terminal gRPC status so status delivery never blocks on or loses to a full item queue.
struct StreamSender<T> {
    items: mpsc::Sender<T>,
    terminal_status: Option<oneshot::Sender<Status>>,
    _subscription_permit: Arc<OwnedSemaphorePermit>,
}

/// Consumer half of an indexer server stream that drains buffered items before yielding any
/// terminal error status.
struct IndexerStream<T> {
    items: mpsc::Receiver<T>,
    terminal_status: Option<oneshot::Receiver<Status>>,
    _subscription_permit: Arc<OwnedSemaphorePermit>,
}

/// Creates a paired [`StreamSender`] and [`IndexerStream`] with a bounded item buffer, retaining
/// `subscription_permit` until both halves release their resources.
fn indexer_stream_channel<T>(
    subscription_permit: Arc<OwnedSemaphorePermit>,
) -> (StreamSender<T>, IndexerStream<T>) {
    let (items_tx, items_rx) = mpsc::channel(RESPONSE_BUFFER_SIZE);
    let (status_tx, status_rx) = oneshot::channel();
    (
        StreamSender {
            items: items_tx,
            terminal_status: Some(status_tx),
            _subscription_permit: subscription_permit.clone(),
        },
        IndexerStream {
            items: items_rx,
            terminal_status: Some(status_rx),
            _subscription_permit: subscription_permit,
        },
    )
}

impl<T> StreamSender<T> {
    async fn closed(&self) {
        self.items.closed().await;
    }

    /// Sends a stream item to the client with [`SEND_TIMEOUT`] backpressure, returning `false` if
    /// the client disconnected or timed out.
    async fn send_item(&mut self, item: T, stream_name: &'static str, span: &Span) -> bool {
        match tokio::time::timeout(SEND_TIMEOUT, self.items.send(item)).await {
            Ok(Ok(())) => true,
            Ok(Err(_)) => {
                span.in_scope(|| {
                    tracing::info!("client disconnected, dropping {stream_name} task");
                });
                false
            }
            Err(_) => {
                span.in_scope(|| {
                    tracing::warn!(
                        "slow consumer, dropping {stream_name} stream after send timed out"
                    );
                });
                if let Some(status_tx) = self.terminal_status.take() {
                    let _ = status_tx.send(Status::deadline_exceeded(format!(
                        "{stream_name} stream timed out sending to slow consumer"
                    )));
                }
                false
            }
        }
    }

    /// Records a terminal gRPC error status independently of the item queue and closes the stream.
    fn finish_with_status(mut self, status: Status) {
        if let Some(status_tx) = self.terminal_status.take() {
            let _ = status_tx.send(status);
        }
    }
}

impl<T> Stream for IndexerStream<T> {
    type Item = Result<T, Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.items.poll_recv(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item))),
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                let Some(status_rx) = this.terminal_status.as_mut() else {
                    return Poll::Ready(None);
                };
                match Pin::new(status_rx).poll(cx) {
                    Poll::Ready(result) => {
                        this.terminal_status = None;
                        match result {
                            Ok(status) => Poll::Ready(Some(Err(status))),
                            Err(_) => Poll::Ready(None),
                        }
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

/// Converts a mempool change kind and transaction ID into a [`MempoolChangeMessage`].
fn mempool_change_message(kind: MempoolChangeKind, tx_id: &UnminedTxId) -> MempoolChangeMessage {
    MempoolChangeMessage {
        change_type: match kind {
            MempoolChangeKind::Added => 0,
            MempoolChangeKind::Invalidated => 1,
            MempoolChangeKind::Mined => 2,
        },
        tx_hash: tx_id.mined_id().bytes_in_display_order().to_vec(),
        auth_digest: tx_id
            .auth_digest()
            .map(|d| d.bytes_in_display_order().to_vec())
            .unwrap_or_default(),
    }
}

/// Decodes the chain tip hashes from a [`NonFinalizedStateChangeRequest`] into a set of
/// [`block::Hash`]es.
///
/// Each hash is expected to be 32 bytes in display order, matching the encoding used when the
/// server streams [`BlockAndHash`] messages back to the caller.
///
/// # Errors
///
/// Returns an [`invalid_argument`](Status::invalid_argument) status if there are more hashes than
/// the non-finalized state can hold chains ([`MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS`]), or
/// if any hash is not exactly 32 bytes long.
fn decode_known_chain_tips(chain_tip_hashes: Vec<Vec<u8>>) -> Result<HashSet<block::Hash>, Status> {
    // Test networks keep the most chains, so a caller can't legitimately send more tips than that.
    // Bound the untrusted input before allocating.
    if chain_tip_hashes.len() > MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS {
        return Err(Status::invalid_argument(format!(
            "too many chain tip hashes: got {}, expected at most {}",
            chain_tip_hashes.len(),
            MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
        )));
    }

    chain_tip_hashes
        .into_iter()
        .map(hash_from_display_bytes)
        .collect()
}

/// Decodes a 32-byte block hash in display order, rejecting wrong-length input.
fn hash_from_display_bytes(hash: Vec<u8>) -> Result<block::Hash, Status> {
    let bytes: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
        Status::invalid_argument(format!(
            "invalid block hash length: expected 32 bytes, got {}",
            hash.len()
        ))
    })?;

    Ok(block::Hash::from_bytes_in_display_order(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    fn hash(byte: u8) -> block::Hash {
        block::Hash::from_bytes_in_display_order(&[byte; 32])
    }

    #[test]
    fn decode_known_chain_tips_round_trips_display_order() {
        let hashes = [hash(1), hash(2), hash(3)];
        let encoded = hashes
            .iter()
            .map(|h| h.bytes_in_display_order().to_vec())
            .collect();

        let decoded = decode_known_chain_tips(encoded).expect("valid hashes should decode");

        assert_eq!(decoded, hashes.into_iter().collect());
    }

    #[test]
    fn decode_known_chain_tips_accepts_empty() {
        assert!(decode_known_chain_tips(Vec::new())
            .expect("empty input should decode")
            .is_empty());
    }

    #[test]
    fn decode_known_chain_tips_dedups() {
        let encoded = vec![
            hash(7).bytes_in_display_order().to_vec(),
            hash(7).bytes_in_display_order().to_vec(),
        ];

        let decoded = decode_known_chain_tips(encoded).expect("duplicate hashes should decode");

        assert_eq!(decoded, std::iter::once(hash(7)).collect());
    }

    #[test]
    fn decode_known_chain_tips_rejects_wrong_length() {
        let status = decode_known_chain_tips(vec![vec![0; 31]])
            .expect_err("a 31-byte hash should be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn decode_known_chain_tips_accepts_test_network_forks() {
        let encoded = (0..MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS as u8)
            .map(|b| hash(b).bytes_in_display_order().to_vec())
            .collect();

        let tips = decode_known_chain_tips(encoded)
            .expect("as many hashes as test networks keep chains should be accepted");

        assert!(tips.len() > zebra_state::MAX_NON_FINALIZED_CHAIN_FORKS);
    }

    #[test]
    fn decode_known_chain_tips_rejects_too_many() {
        let encoded = (0..=MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS as u8)
            .map(|b| hash(b).bytes_in_display_order().to_vec())
            .collect();

        let status = decode_known_chain_tips(encoded).expect_err(
            "more than MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS hashes should be rejected",
        );

        assert_eq!(status.code(), Code::InvalidArgument);
    }

    mod stream_lifecycle {
        use std::{
            future::Future,
            pin::Pin,
            sync::{Arc, Weak},
            task::{Context, Poll},
        };

        use chrono::{DateTime, Utc};
        use futures::StreamExt;
        use tokio::sync::{broadcast, mpsc, oneshot};
        use tower::{BoxError, Service};
        use zebra_chain::{
            chain_tip::mock::MockChainTip,
            parameters::Network,
            transaction::{self, UnminedTxId},
        };
        use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
        use zebra_state::NonFinalizedBlocksListener;
        use zebra_test::mock_service::MockService;

        use super::*;

        /// Wrapper around [`MockChainTip`] that tracks when all clones held by spawned tasks are
        /// dropped.
        #[derive(Clone)]
        struct TrackedChainTip {
            inner: MockChainTip,
            _alive: Arc<()>,
        }

        impl TrackedChainTip {
            fn new() -> (
                Self,
                zebra_chain::chain_tip::mock::MockChainTipSender,
                Weak<()>,
            ) {
                let (inner, sender) = MockChainTip::new();
                let alive = Arc::new(());
                let weak = Arc::downgrade(&alive);
                (
                    Self {
                        inner,
                        _alive: alive,
                    },
                    sender,
                    weak,
                )
            }
        }

        impl ChainTip for TrackedChainTip {
            fn best_tip_height(&self) -> Option<block::Height> {
                self.inner.best_tip_height()
            }

            fn best_tip_hash(&self) -> Option<block::Hash> {
                self.inner.best_tip_hash()
            }

            fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
                self.inner.best_tip_height_and_hash()
            }

            fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
                self.inner.best_tip_block_time()
            }

            fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
                self.inner.best_tip_height_and_block_time()
            }

            fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
                self.inner.best_tip_mined_transaction_ids()
            }

            fn estimate_distance_to_network_chain_tip(
                &self,
                network: &Network,
            ) -> Option<(block::HeightDiff, block::Height)> {
                self.inner.estimate_distance_to_network_chain_tip(network)
            }

            async fn best_tip_changed(&mut self) -> Result<(), BoxError> {
                self.inner.best_tip_changed().await
            }

            fn mark_best_tip_seen(&mut self) {
                self.inner.mark_best_tip_seen();
            }
        }

        /// A `ReadState` service whose `call()` future stays pending forever and signals when
        /// dropped.
        #[derive(Clone)]
        struct PendingReadState {
            dropped: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
        }

        impl Service<ReadRequest> for PendingReadState {
            type Response = ReadResponse;
            type Error = BoxError;
            type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _req: ReadRequest) -> Self::Future {
                let tx = self.dropped.lock().expect("test mutex not poisoned").take();
                Box::pin(async move {
                    let _guard = tx;
                    std::future::pending().await
                })
            }
        }

        async fn wait_until(predicate: impl Fn() -> bool) -> bool {
            tokio::time::timeout(Duration::from_secs(1), async {
                while !predicate() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .is_ok()
        }

        #[tokio::test]
        async fn chain_tip_change_stops_promptly_on_disconnect_without_events() {
            let (tip, _tip_sender, weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(4);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(read_state, tip, MempoolTxSubscriber::new(mempool_tx));

            let stream = rpc
                .chain_tip_change(tonic::Request::new(Empty {}))
                .await
                .expect("chain_tip_change should return a stream")
                .into_inner();
            drop(rpc);

            assert!(
                weak.upgrade().is_some(),
                "spawned chain_tip_change task should still hold its ChainTip clone"
            );

            drop(stream);

            assert!(
                wait_until(|| weak.upgrade().is_none()).await,
                "chain_tip_change task should exit promptly after client disconnects"
            );
        }

        #[tokio::test]
        async fn non_finalized_state_change_stops_promptly_on_disconnect_without_events() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(4);
            let mut read_state = MockService::build()
                .with_max_request_delay(Duration::from_secs(2))
                .for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state.clone(),
                tip,
                MempoolTxSubscriber::new(mempool_tx),
            );

            let (blocks_tx, blocks_rx) = mpsc::channel(4);
            let stream = rpc
                .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
                    chain_tip_hashes: Vec::new(),
                }))
                .await
                .expect("non_finalized_state_change should return a stream")
                .into_inner();

            read_state
                .expect_request(ReadRequest::NonFinalizedBlocksListener {
                    known_chain_tips: HashSet::new(),
                })
                .await
                .respond(ReadResponse::NonFinalizedBlocksListener(
                    NonFinalizedBlocksListener(Arc::new(blocks_rx)),
                ));

            // Yield so the spawned task moves into waiting on `non_finalized_state_change.recv()`.
            tokio::task::yield_now().await;
            assert!(
                !blocks_tx.is_closed(),
                "listener receiver should still be alive"
            );

            drop(stream);

            tokio::time::timeout(Duration::from_secs(1), blocks_tx.closed())
                .await
                .expect("non_finalized_state_change task should exit promptly after disconnect");
        }

        #[tokio::test]
        async fn non_finalized_state_change_stops_promptly_if_disconnected_during_subscribe() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(4);
            let (dropped_tx, dropped_rx) = oneshot::channel();
            let read_state = PendingReadState {
                dropped: Arc::new(std::sync::Mutex::new(Some(dropped_tx))),
            };

            let rpc = IndexerRPC::new(read_state, tip, MempoolTxSubscriber::new(mempool_tx));

            let stream = rpc
                .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
                    chain_tip_hashes: Vec::new(),
                }))
                .await
                .expect("non_finalized_state_change should return a stream")
                .into_inner();

            // Let the spawned task start awaiting `read_state.oneshot(...)`.
            tokio::task::yield_now().await;

            drop(stream);

            tokio::time::timeout(Duration::from_secs(1), dropped_rx)
                .await
                .expect("pending read_state subscription should be dropped on disconnect")
                .expect_err("oneshot sender should be dropped, not sent to");
        }

        #[tokio::test]
        async fn mempool_change_stops_promptly_on_disconnect_without_events() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(4);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("mempool_change should return a stream")
                .into_inner();

            assert_eq!(mempool_tx.receiver_count(), 1);

            drop(stream);

            assert!(
                wait_until(|| mempool_tx.receiver_count() == 0).await,
                "mempool_change task should exit promptly after client disconnects"
            );
        }

        #[tokio::test]
        async fn mempool_change_returns_aborted_when_lagged() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(1);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let mut stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("mempool_change should return a stream")
                .into_inner();

            // Send 3 updates into a capacity-1 broadcast channel before the task polls, forcing
            // `RecvError::Lagged(2)`.
            for byte in 1..=3u8 {
                let tx_ids = [UnminedTxId::Legacy(transaction::Hash::from([byte; 32]))]
                    .into_iter()
                    .collect();
                mempool_tx
                    .send(MempoolChange::added(tx_ids))
                    .expect("mempool_change task should be subscribed");
            }

            let status = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("stream should yield lag error promptly")
                .expect("stream should yield a terminal status")
                .expect_err("lagged stream should return an error status");

            assert_eq!(status.code(), Code::Aborted);
            assert!(
                status.message().contains("lagged by 2"),
                "expected lag count in status message, got: {}",
                status.message()
            );
        }

        #[tokio::test]
        async fn mempool_change_returns_unavailable_when_closed() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(1);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let mut stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("mempool_change should return a stream")
                .into_inner();

            // Drop both `rpc` (which holds a `MempoolTxSubscriber` clone of the sender) and
            // `mempool_tx` so the broadcast channel closes.
            drop(rpc);
            drop(mempool_tx);

            let status = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("stream should yield closed error promptly")
                .expect("stream should yield a terminal status")
                .expect_err("closed stream should return an error status");

            assert_eq!(status.code(), Code::Unavailable);
            assert!(
                status.message().contains("closed"),
                "expected closed message, got: {}",
                status.message()
            );
        }

        #[tokio::test(start_paused = true)]
        async fn mempool_change_preserves_aborted_status_when_response_queue_is_full() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(1);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let mut stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("mempool_change should return a stream")
                .into_inner();

            // Fill every slot of the bounded response queue via a single broadcast message.
            let full_batch: HashSet<UnminedTxId> = (0..RESPONSE_BUFFER_SIZE)
                .map(|idx| {
                    let mut bytes = [0u8; 32];
                    bytes[..8].copy_from_slice(
                        &u64::try_from(idx)
                            .expect("buffer index fits in u64")
                            .to_le_bytes(),
                    );
                    UnminedTxId::Legacy(transaction::Hash::from(bytes))
                })
                .collect();
            mempool_tx
                .send(MempoolChange::added(full_batch))
                .expect("mempool_change task should be subscribed");
            tokio::task::yield_now().await;

            // Overflow the capacity-1 broadcast channel to force `RecvError::Lagged(1)` while the
            // response queue is completely full.
            for byte in [0xfeu8, 0xff] {
                let tx_ids = [UnminedTxId::Legacy(transaction::Hash::from([byte; 32]))]
                    .into_iter()
                    .collect();
                mempool_tx
                    .send(MempoolChange::added(tx_ids))
                    .expect("mempool_change task should still be subscribed");
            }

            // Advance past `SEND_TIMEOUT` so task exit cannot depend on waiting longer.
            tokio::time::advance(SEND_TIMEOUT + Duration::from_secs(1)).await;
            assert!(
                wait_until(|| mempool_tx.receiver_count() == 0).await,
                "lagged mempool_change task should exit promptly even with a full response queue"
            );

            for idx in 0..RESPONSE_BUFFER_SIZE {
                let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
                    .await
                    .expect("buffered item should be readable")
                    .unwrap_or_else(|| panic!("missing buffered item {idx}"));
                assert!(item.is_ok(), "buffered item {idx} should succeed");
            }

            let status = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("terminal lag status should follow buffered items")
                .expect("stream must yield terminal ABORTED status instead of EOF")
                .expect_err("terminal item must be an error status");
            assert_eq!(status.code(), Code::Aborted);
            assert!(
                stream.next().await.is_none(),
                "stream must end after status"
            );
        }

        #[tokio::test(start_paused = true)]
        async fn mempool_change_preserves_unavailable_status_when_response_queue_is_full() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(1);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let mut stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("mempool_change should return a stream")
                .into_inner();

            let full_batch: HashSet<UnminedTxId> = (0..RESPONSE_BUFFER_SIZE)
                .map(|idx| {
                    let mut bytes = [0u8; 32];
                    bytes[..8].copy_from_slice(
                        &u64::try_from(idx)
                            .expect("buffer index fits in u64")
                            .to_le_bytes(),
                    );
                    UnminedTxId::Legacy(transaction::Hash::from(bytes))
                })
                .collect();
            mempool_tx
                .send(MempoolChange::added(full_batch))
                .expect("mempool_change task should be subscribed");
            tokio::task::yield_now().await;

            // Drop upstream senders while the response queue is completely full.
            drop(rpc);
            drop(mempool_tx);

            tokio::time::advance(SEND_TIMEOUT + Duration::from_secs(1)).await;

            for idx in 0..RESPONSE_BUFFER_SIZE {
                let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
                    .await
                    .expect("buffered item should be readable")
                    .unwrap_or_else(|| panic!("missing buffered item {idx}"));
                assert!(item.is_ok(), "buffered item {idx} should succeed");
            }

            let status = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("terminal unavailable status should follow buffered items")
                .expect("stream must yield terminal UNAVAILABLE status instead of EOF")
                .expect_err("terminal item must be an error status");
            assert_eq!(status.code(), Code::Unavailable);
            assert!(
                stream.next().await.is_none(),
                "stream must end after status"
            );
        }

        #[tokio::test(start_paused = true)]
        async fn timed_out_buffered_stream_retains_subscription_permit_until_dropped() {
            let (tip, _tip_sender, _weak) = TrackedChainTip::new();
            let (mempool_tx, _) = broadcast::channel(1);
            let read_state = MockService::build().for_unit_tests();

            let rpc = IndexerRPC::new(
                read_state,
                tip,
                MempoolTxSubscriber::new(mempool_tx.clone()),
            );

            let mut other_streams =
                Vec::with_capacity(super::super::server::MAX_ACTIVE_SUBSCRIPTIONS - 1);
            for _ in 0..super::super::server::MAX_ACTIVE_SUBSCRIPTIONS - 1 {
                other_streams.push(
                    rpc.chain_tip_change(tonic::Request::new(Empty {}))
                        .await
                        .expect("stream within subscription cap should succeed")
                        .into_inner(),
                );
            }

            let mut slow_stream = rpc
                .mempool_change(tonic::Request::new(Empty {}))
                .await
                .expect("final slot mempool_change should succeed")
                .into_inner();

            let overflow_batch: HashSet<UnminedTxId> = (0..=RESPONSE_BUFFER_SIZE)
                .map(|idx| {
                    let mut bytes = [0u8; 32];
                    bytes[..8].copy_from_slice(
                        &u64::try_from(idx)
                            .expect("buffer index fits in u64")
                            .to_le_bytes(),
                    );
                    UnminedTxId::Legacy(transaction::Hash::from(bytes))
                })
                .collect();
            mempool_tx
                .send(MempoolChange::added(overflow_batch))
                .expect("mempool_change task should be subscribed");

            // Advance past SEND_TIMEOUT so the producer times out and exits while `slow_stream`
            // still holds buffered items.
            tokio::time::advance(SEND_TIMEOUT + Duration::from_secs(1)).await;
            assert!(
                wait_until(|| mempool_tx.receiver_count() == 0).await,
                "timed-out mempool_change producer task should have exited"
            );

            let err = match rpc.chain_tip_change(tonic::Request::new(Empty {})).await {
                Ok(_) => {
                    panic!("timed-out buffered stream must still occupy its subscription permit")
                }
                Err(err) => err,
            };
            assert_eq!(err.code(), Code::ResourceExhausted);

            for _ in 0..RESPONSE_BUFFER_SIZE {
                assert!(slow_stream.next().await.expect("buffered item").is_ok());
            }
            let status = slow_stream
                .next()
                .await
                .expect("timed-out stream must yield terminal status")
                .expect_err("timed-out stream must return DEADLINE_EXCEEDED");
            assert_eq!(status.code(), Code::DeadlineExceeded);
            drop(slow_stream);

            let replacement = rpc
                .chain_tip_change(tonic::Request::new(Empty {}))
                .await
                .expect("dropping buffered stream must release its subscription permit")
                .into_inner();
            drop(replacement);
            drop(other_streams);
        }
    }
}
