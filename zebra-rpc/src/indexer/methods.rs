//! Implements `Indexer` methods on the `IndexerRPC` type

use std::{collections::HashSet, pin::Pin, time::Duration};

use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};
use tower::util::ServiceExt;

use tracing::Span;
use zebra_chain::{block, chain_tip::ChainTip, serialization::BytesInDisplayOrder};
use zebra_node_services::mempool::MempoolChangeKind;
use zebra_state::{ReadRequest, ReadResponse, ReadState, MAX_NON_FINALIZED_CHAIN_FORKS};

use super::{
    block_request, indexer_server::Indexer, server::IndexerRPC, BlockAndHash, BlockHashAndHeight,
    BlockRequest, Empty, MempoolChangeMessage, NonFinalizedStateChangeRequest,
};

/// The maximum number of messages that can be queued to be streamed to a client.
const RESPONSE_BUFFER_SIZE: usize = 64;

/// How long to wait for a backpressured send to the non-finalized stream before treating the
/// consumer as hung and dropping the subscription.
///
/// The non-finalized stream applies backpressure (rather than dropping blocks) so a slow consumer
/// doesn't miss blocks, but without a bound a consumer whose connection is half-open (dead TCP not
/// yet detected) would block the listener task indefinitely.
const NON_FINALIZED_SEND_TIMEOUT: Duration = Duration::from_secs(60);

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

                match response_sender.try_send(Ok(BlockHashAndHeight::new(tip_hash, tip_height))) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping chain_tip_change task");
                        });
                        return;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        span.in_scope(|| {
                            tracing::warn!("slow consumer, dropping chain_tip_change stream");
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

            // Notify the client of new blocks until the channel is closed.
            //
            // Unlike the other streams, this uses `send().await` to apply backpressure to the
            // non-finalized state listener rather than dropping blocks for a slow consumer. A send
            // error means the client disconnected; a send that doesn't complete within
            // `NON_FINALIZED_SEND_TIMEOUT` means the consumer is hung. In both cases the task ends
            // rather than blocking forever.
            while let Some((hash, block)) = non_finalized_state_change.recv().await {
                let send = response_sender.send(Ok(BlockAndHash::new(hash, block)));
                match tokio::time::timeout(NON_FINALIZED_SEND_TIMEOUT, send).await {
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

                    match response_sender.try_send(msg) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            span.in_scope(|| {
                                tracing::info!("client disconnected, dropping mempool_change task");
                            });
                            return;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            span.in_scope(|| {
                                tracing::warn!("slow consumer, dropping mempool_change stream");
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
        let hash_or_height = match request.into_inner().hash_or_height {
            Some(block_request::HashOrHeight::Hash(hash)) => {
                let bytes: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
                    Status::invalid_argument(format!(
                        "invalid block hash length: expected 32 bytes, got {}",
                        hash.len()
                    ))
                })?;
                zebra_state::HashOrHeight::Hash(block::Hash::from_bytes_in_display_order(&bytes))
            }
            Some(block_request::HashOrHeight::Height(height)) => {
                zebra_state::HashOrHeight::Height(block::Height(height))
            }
            None => {
                return Err(Status::invalid_argument(
                    "block request must specify a hash or height",
                ));
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

/// Decodes the chain tip hashes from a [`NonFinalizedStateChangeRequest`] into a set of
/// [`block::Hash`]es.
///
/// Each hash is expected to be 32 bytes in display order, matching the encoding used when the
/// server streams [`BlockAndHash`] messages back to the caller.
///
/// # Errors
///
/// Returns an [`invalid_argument`](Status::invalid_argument) status if there are more hashes than
/// the non-finalized state can hold chains ([`MAX_NON_FINALIZED_CHAIN_FORKS`]), or if any hash is
/// not exactly 32 bytes long.
fn decode_known_chain_tips(chain_tip_hashes: Vec<Vec<u8>>) -> Result<HashSet<block::Hash>, Status> {
    // The non-finalized state holds at most `MAX_NON_FINALIZED_CHAIN_FORKS` chains, so a caller can
    // never legitimately have more chain tips than that. Bound the untrusted input up front rather
    // than allocating a set sized by the request.
    if chain_tip_hashes.len() > MAX_NON_FINALIZED_CHAIN_FORKS {
        return Err(Status::invalid_argument(format!(
            "too many chain tip hashes: got {}, expected at most {MAX_NON_FINALIZED_CHAIN_FORKS}",
            chain_tip_hashes.len(),
        )));
    }

    chain_tip_hashes
        .into_iter()
        .map(|hash| {
            let bytes: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
                Status::invalid_argument(format!(
                    "invalid chain tip hash length: expected 32 bytes, got {}",
                    hash.len()
                ))
            })?;
            Ok(block::Hash::from_bytes_in_display_order(&bytes))
        })
        .collect()
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
    fn decode_known_chain_tips_rejects_too_many() {
        let encoded = (0..=MAX_NON_FINALIZED_CHAIN_FORKS as u8)
            .map(|b| hash(b).bytes_in_display_order().to_vec())
            .collect();

        let status = decode_known_chain_tips(encoded)
            .expect_err("more than MAX_NON_FINALIZED_CHAIN_FORKS hashes should be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
    }
}
