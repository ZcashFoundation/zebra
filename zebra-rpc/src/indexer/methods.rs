//! Implements `Indexer` methods on the `IndexerRPC` type

use std::pin::Pin;

use futures::Stream;
use tonic::{Response, Status};
use tower::BoxError;

use super::{indexer_server::Indexer, server::IndexerRPC, Empty};

#[tonic::async_trait]
impl<ReadStateService> Indexer for IndexerRPC<ReadStateService>
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
{
    type ChainTipChangeStream = Pin<Box<dyn Stream<Item = Result<Empty, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        todo!()
    }
}
