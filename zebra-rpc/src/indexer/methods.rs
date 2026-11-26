//! Implements `Indexer` methods on the `IndexerRPC` type

use std::{
    collections::HashSet,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{future, stream, Stream, StreamExt};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};
use tower::util::ServiceExt;

use tracing::{Instrument, Span};
use zebra_chain::{block, chain_tip::ChainTip, serialization::BytesInDisplayOrder};
use zebra_node_services::mempool::MempoolChangeKind;
use zebra_state::{
    BlockReadError, ReadRequest, ReadResponse, ReadState,
    MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
};

use super::{
    indexer_server::Indexer, server::IndexerRPC, BlockAndHash, BlockAndHeight, BlockHashAndHeight,
    BlockRangeRequest, BlockRequest, Empty, MempoolChangeMessage, NonFinalizedStateChangeRequest,
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

/// The number of blocks a `get_block_range` stream queues for its consumer, and reads from the
/// state at a time. A stream holds at most twice this many, plus the one block the transport is
/// sending: 17 blocks, about 34 MB at the maximum block size.
const BLOCK_RANGE_BUFFER_SIZE: usize = 8;

/// [`BLOCK_RANGE_BUFFER_SIZE`] as a state request count.
//
// The cast is safe because `BLOCK_RANGE_BUFFER_SIZE` is a small constant that fits in a `u32`.
const BLOCK_RANGE_CHUNK_SIZE: u32 = BLOCK_RANGE_BUFFER_SIZE as u32;

/// The number of `get_block_range` streams the server serves at once, across all connections.
///
/// Together with [`BLOCK_RANGE_BUFFER_SIZE`], this bounds the memory held by open streams at about
/// 544 MB at the maximum block size. A request beyond the limit is refused with
/// `RESOURCE_EXHAUSTED`.
pub(super) const MAX_BLOCK_RANGE_STREAMS: usize = 16;

/// The stream type returned by `get_block_range`.
type BlockRangeStream = Pin<Box<dyn Stream<Item = Result<BlockAndHeight, Status>> + Send>>;

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
    type GetBlockRangeStream = BlockRangeStream;

    async fn chain_tip_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        let span = Span::current();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);
        let mut chain_tip_change = self.chain_tip_change.clone();

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed
            while let Ok(()) = chain_tip_change.best_tip_changed().await {
                let Some((tip_height, tip_hash)) = chain_tip_change.best_tip_height_and_hash()
                else {
                    continue;
                };

                let send = response_sender.send(Ok(BlockHashAndHeight::new(tip_hash, tip_height)));
                match tokio::time::timeout(SEND_TIMEOUT, send).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping chain_tip_change task");
                        });
                        return;
                    }
                    Err(_) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                "slow consumer, dropping chain_tip_change stream after \
                                 send timed out"
                            );
                        });
                        return;
                    }
                }
            }

            span.in_scope(|| {
                tracing::warn!("chain_tip_change channel has closed");
            });

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "chain_tip_change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn non_finalized_state_change(
        &self,
        request: tonic::Request<NonFinalizedStateChangeRequest>,
    ) -> Result<Response<Self::NonFinalizedStateChangeStream>, Status> {
        let span = Span::current();
        let read_state = self.read_state.clone();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        // The caller may provide the hashes of the chain tips it already has so the server only
        // streams blocks after those tips. Malformed hashes (wrong length) are rejected up front.
        let known_chain_tips = decode_known_chain_tips(request.into_inner().chain_tip_hashes)?;

        tokio::spawn(async move {
            let mut non_finalized_state_change = match read_state
                .oneshot(ReadRequest::NonFinalizedBlocksListener { known_chain_tips })
                .await
            {
                Ok(ReadResponse::NonFinalizedBlocksListener(listener)) => listener.unwrap(),
                Ok(_) => unreachable!("unexpected response type from ReadStateService"),
                Err(error) => {
                    span.in_scope(|| {
                        tracing::error!(
                            ?error,
                            "failed to subscribe to non-finalized state changes"
                        );
                    });

                    let _ = response_sender
                        .send(Err(Status::unavailable(
                            "failed to subscribe to non-finalized state changes",
                        )))
                        .await;
                    return;
                }
            };

            loop {
                let Some((hash, block)) = non_finalized_state_change.recv().await else {
                    break;
                };

                let send = response_sender.send(Ok(BlockAndHash::new(hash, block)));
                match tokio::time::timeout(SEND_TIMEOUT, send).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        span.in_scope(|| {
                            tracing::info!(
                                "client disconnected, dropping non_finalized_state_change task"
                            );
                        });
                        return;
                    }
                    Err(_) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                "slow consumer, dropping non_finalized_state_change stream after \
                                 send timed out"
                            );
                        });
                        return;
                    }
                }
            }

            span.in_scope(|| {
                tracing::warn!("non-finalized state change channel has closed");
            });

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "non-finalized state change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn mempool_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::MempoolChangeStream>, Status> {
        let span = Span::current();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);
        let mut mempool_change = self.mempool_change.subscribe();

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed
            while let Ok(change) = mempool_change.recv().await {
                for tx_id in change.tx_ids() {
                    span.in_scope(|| {
                        tracing::debug!("mempool change: {:?}", change);
                    });

                    let msg = Ok(MempoolChangeMessage {
                        change_type: match change.kind() {
                            MempoolChangeKind::Added => 0,
                            MempoolChangeKind::Invalidated => 1,
                            MempoolChangeKind::Mined => 2,
                        },
                        tx_hash: tx_id.mined_id().bytes_in_display_order().to_vec(),
                        auth_digest: tx_id
                            .auth_digest()
                            .map(|d| d.bytes_in_display_order().to_vec())
                            .unwrap_or_default(),
                    });

                    let send = response_sender.send(msg);
                    match tokio::time::timeout(SEND_TIMEOUT, send).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            span.in_scope(|| {
                                tracing::info!("client disconnected, dropping mempool_change task");
                            });
                            return;
                        }
                        Err(_) => {
                            span.in_scope(|| {
                                tracing::warn!(
                                    "slow consumer, dropping mempool_change stream after \
                                     send timed out"
                                );
                            });
                            return;
                        }
                    }
                }
            }

            span.in_scope(|| {
                tracing::warn!("mempool_change channel has closed");
            });

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "mempool_change channel has closed",
                )))
                .await;
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

    async fn get_block_range(
        &self,
        request: tonic::Request<BlockRangeRequest>,
    ) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        let BlockRangeRequest {
            start_height,
            end_height,
        } = request.into_inner();

        // Invalid ranges are rejected up front, without touching the state.
        let start = block::Height::try_from(start_height).map_err(|_| {
            Status::invalid_argument(format!("start height out of range: {start_height}"))
        })?;
        let end = block::Height::try_from(end_height).map_err(|_| {
            Status::invalid_argument(format!("end height out of range: {end_height}"))
        })?;
        if end < start {
            return Err(Status::invalid_argument(format!(
                "end height {end_height} must not be below start height {start_height}"
            )));
        }

        block_range_stream(
            self.read_state.clone(),
            &self.block_range_permits,
            start,
            end,
            SEND_TIMEOUT,
        )
        .map(Response::new)
    }
}

/// Streams the finalized blocks from `start` to `end` inclusive, ending the stream with the status
/// [`serve_block_range`] returns, if any.
///
/// The final status has its own channel, chained after the block channel, so it is delivered after
/// every queued block even when a slow consumer has filled the block channel.
///
/// Each stream holds one of `permits` until both its producer task has ended and the returned stream
/// has been dropped, so the permit also covers blocks still queued for a client that has stopped
/// reading. Returns `RESOURCE_EXHAUSTED` without starting a stream if none is free.
pub(super) fn block_range_stream<ReadStateService: ReadState>(
    read_state: ReadStateService,
    permits: &Arc<Semaphore>,
    start: block::Height,
    end: block::Height,
    send_timeout: Duration,
) -> Result<BlockRangeStream, Status> {
    let permit = Arc::new(permits.clone().try_acquire_owned().map_err(|_| {
        Status::resource_exhausted(format!(
            "the server is already serving {MAX_BLOCK_RANGE_STREAMS} GetBlockRange streams"
        ))
    })?);
    let stream_permit = permit.clone();

    let (block_sender, block_receiver) = mpsc::channel(BLOCK_RANGE_BUFFER_SIZE);
    let (status_sender, status_receiver) = oneshot::channel();

    tokio::spawn(
        async move {
            let _permit = permit;
            let status =
                serve_block_range(read_state, start, end, send_timeout, block_sender).await;
            if let Some(status) = status {
                let _ = status_sender.send(status);
            }
        }
        .instrument(Span::current()),
    );

    let status =
        stream::once(status_receiver).filter_map(|status| future::ready(status.ok().map(Err)));

    Ok(Box::pin(PermitStream {
        inner: Box::pin(ReceiverStream::new(block_receiver).chain(status)),
        _permit: stream_permit,
    }))
}

/// A `get_block_range` stream that holds its share of the stream permit until it is dropped, so
/// the permit stays taken while blocks are queued for the client, even after the producer task
/// has ended.
struct PermitStream {
    inner: BlockRangeStream,
    _permit: Arc<OwnedSemaphorePermit>,
}

impl Stream for PermitStream {
    type Item = Result<BlockAndHeight, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// Sends the finalized blocks from `start` to `end` inclusive to `block_sender`, reading them from
/// the state in chunks of at most [`BLOCK_RANGE_CHUNK_SIZE`] blocks.
///
/// Returns `None` when the range or the finalized tip is reached, or when the consumer has
/// disconnected. Otherwise returns the status that ends the stream:
/// - `FAILED_PRECONDITION` for a block body at or below the finalized tip that is not stored,
/// - `DEADLINE_EXCEEDED` if the consumer doesn't read a block within `send_timeout`,
/// - `UNAVAILABLE` if the state can't be read.
async fn serve_block_range<ReadStateService: ReadState>(
    read_state: ReadStateService,
    start: block::Height,
    end: block::Height,
    send_timeout: Duration,
    block_sender: mpsc::Sender<Result<BlockAndHeight, Status>>,
) -> Option<Status> {
    let block::Height(end) = end;
    let mut next = start.0;
    while next <= end {
        // A client that has gone away costs no further state reads.
        if block_sender.is_closed() {
            return None;
        }

        // `next <= end`, so this can't underflow, and `end` is a valid height, far below
        // `u32::MAX`, so this can't overflow.
        let count = (end - next + 1).min(BLOCK_RANGE_CHUNK_SIZE);

        let blocks = match read_state
            .clone()
            .oneshot(ReadRequest::RawBlocksByHeightRange {
                start: block::Height(next),
                count,
            })
            .await
        {
            Ok(ReadResponse::RawBlocks(blocks)) => blocks,
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => {
                if let Some(BlockReadError::BlockBodyUnavailable { height }) =
                    error.downcast_ref::<BlockReadError>()
                {
                    return Some(Status::failed_precondition(format!(
                        "block body at height {} is not stored",
                        height.0
                    )));
                }

                tracing::error!(?error, "failed to read blocks for get_block_range");
                return Some(Status::unavailable(format!(
                    "failed to read blocks: {error}"
                )));
            }
        };

        // The state stops at the finalized tip, so an empty response means `next` is above it. A
        // short response ends before the tip or a missing body, which the next read tells apart.
        let Some(&(last_height, _)) = blocks.last() else {
            break;
        };

        for (height, data) in blocks {
            let send = block_sender.send(Ok(BlockAndHeight {
                height: height.0,
                data,
            }));
            match tokio::time::timeout(send_timeout, send).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    tracing::info!("client disconnected, dropping get_block_range task");
                    return None;
                }
                Err(_) => {
                    tracing::warn!(
                        "slow consumer, dropping get_block_range stream after send timed out"
                    );
                    return Some(Status::deadline_exceeded(format!(
                        "the client did not read a block for {send_timeout:?}, \
                         the stream ended before height {}",
                        height.0
                    )));
                }
            }
        }

        // The last height is at most `end`, far below `u32::MAX`, so this can't overflow.
        next = last_height.0 + 1;
    }

    None
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
}
