//! Implements `Indexer` methods on the `IndexerRPC` type

use std::pin::Pin;

use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};
use tower::{util::ServiceExt, BoxError};

use tracing::Span;
use zebra_chain::{chain_tip::ChainTip, serialization::BytesInDisplayOrder};
use zebra_node_services::mempool::MempoolChangeKind;
use zebra_state::{ReadRequest, ReadResponse};

use super::{
    indexer_server::Indexer, server::IndexerRPC, BlockAndHash, BlockHashAndHeight, Empty,
    MempoolChangeMessage,
};

/// The maximum number of messages that can be queued to be streamed to a client
const RESPONSE_BUFFER_SIZE: usize = 4_000;

#[tonic::async_trait]
impl<ReadStateService, Tip> Indexer for IndexerRPC<ReadStateService, Tip>
where
    ReadStateService: tower::Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ReadStateService as tower::Service<zebra_state::ReadRequest>>::Future: Send,
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

                if let Err(error) = response_sender
                    .send(Ok(BlockHashAndHeight::new(tip_hash, tip_height)))
                    .await
                {
                    span.in_scope(|| {
                        tracing::info!(?error, "failed to send chain tip change, dropping task");
                    });
                    return;
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
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::NonFinalizedStateChangeStream>, Status> {
        let span = Span::current();
        let read_state = self.read_state.clone();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            let mut non_finalized_state_change = match read_state
                .oneshot(ReadRequest::NonFinalizedBlocksListener)
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

            // Notify the client of chain tip changes until the channel is closed
            while let Some((hash, block)) = non_finalized_state_change.recv().await {
                if let Err(error) = response_sender
                    .send(Ok(BlockAndHash::new(hash, block)))
                    .await
                {
                    span.in_scope(|| {
                        tracing::info!(
                            ?error,
                            "failed to send non-finalized state change, dropping task"
                        );
                    });
                    return;
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

                    if let Err(error) = response_sender
                        .send(Ok(MempoolChangeMessage {
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
                        }))
                        .await
                    {
                        span.in_scope(|| {
                            tracing::info!(?error, "failed to send mempool change, dropping task");
                        });
                        return;
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
}
