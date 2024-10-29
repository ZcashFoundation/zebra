//! Implements `Indexer` methods on the `IndexerRPC` type

use std::pin::Pin;

use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};
use tower::BoxError;

use zebra_chain::chain_tip::ChainTip;

use super::{indexer_server::Indexer, server::IndexerRPC, Empty};

/// The maximum number of messages that can be queued to be streamed to a client
const RESPONSE_BUFFER_SIZE: usize = 10_000;

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
    type ChainTipChangeStream = Pin<Box<dyn Stream<Item = Result<Empty, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);
        let mut chain_tip_change = self.chain_tip_change.clone();

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed
            while let Ok(()) = chain_tip_change.best_tip_changed().await {
                let tx = response_sender.clone();
                tokio::spawn(async move { tx.send(Ok(Empty {})).await });
            }

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "chain_tip_change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }
}
