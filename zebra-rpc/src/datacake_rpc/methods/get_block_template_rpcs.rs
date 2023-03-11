use super::*;

use crate::methods::{GetBlockTemplateRpc, GetBlockTemplateRpcImpl};

pub mod request;
pub mod response;

#[cfg(test)]
mod tests;

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook> Handler<request::BlockCount>
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    ChainVerifier: Service<
            zebra_consensus::Request,
            Response = zebra_chain::block::Hash,
            Error = zebra_consensus::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ChainVerifier as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: zebra_chain::chain_sync_status::ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: zebra_network::AddressBookPeers + Clone + Send + Sync + 'static,
{
    type Reply = u32;

    async fn on_message(
        &self,
        _request: datacake_rpc::Request<request::BlockCount>,
    ) -> Result<Self::Reply, Status> {
        self.get_block_count().map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook> Handler<request::BlockHash>
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    ChainVerifier: Service<
            zebra_consensus::Request,
            Response = zebra_chain::block::Hash,
            Error = zebra_consensus::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ChainVerifier as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: zebra_chain::chain_sync_status::ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: zebra_network::AddressBookPeers + Clone + Send + Sync + 'static,
{
    type Reply = super::response::GetBlockHash;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::BlockHash>,
    ) -> Result<Self::Reply, Status> {
        let request::BlockHash(index) = request.to_owned().map_err(|err| Status {
            code: ErrorCode::InvalidPayload,
            message: err.to_string(),
        })?;

        self.get_block_hash(index).await.map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook> Handler<request::BlockTemplate>
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    ChainVerifier: Service<
            zebra_consensus::Request,
            Response = zebra_chain::block::Hash,
            Error = zebra_consensus::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ChainVerifier as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: zebra_chain::chain_sync_status::ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: zebra_network::AddressBookPeers + Clone + Send + Sync + 'static,
{
    type Reply = response::get_block_template::Response;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::BlockTemplate>,
    ) -> Result<Self::Reply, Status> {
        let request::BlockTemplate(params) = request.to_owned().map_err(|err| Status {
            code: ErrorCode::InvalidPayload,
            message: err.to_string(),
        })?;

        self.get_block_template(params)
            .await
            .map_err(make_status_error)
    }
}
