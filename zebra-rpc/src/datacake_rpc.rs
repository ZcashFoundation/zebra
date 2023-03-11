//! Datacake RPC Server
//!
//! Note that this is currently an unstable feature.

use std::{io, net::SocketAddr};

use datacake_rpc::{ErrorCode, Handler, RpcService, Server, ServiceRegistry, Status};
use tower::Service;
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::mempool;

use crate::methods::{Rpc, RpcImpl};

mod methods;
pub use methods::{request, response};

#[cfg(test)]
mod tests;

/// Handles rkyv-serialized RPC requests
impl<Mempool, State, Tip> RpcService for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
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
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    fn service_name() -> &'static str {
        "zebra_datacake_rpc_service"
    }

    fn register_handlers(registry: &mut ServiceRegistry<Self>) {
        registry.add_handler::<request::Info>();
        registry.add_handler::<request::BlockChainInfo>();
        registry.add_handler::<request::AddressBalance>();
        registry.add_handler::<request::SendRawTransaction>();
        registry.add_handler::<request::Block>();
        registry.add_handler::<request::BestTipBlockHash>();
        registry.add_handler::<request::RawMempool>();
        registry.add_handler::<request::RawTransaction>();
        registry.add_handler::<request::ZTreestate>();
        registry.add_handler::<request::GetAddressTxIdsRequest>();
        registry.add_handler::<request::AddressUtxos>();
    }
}

#[cfg(feature = "getblocktemplate-rpcs")]
/// Handles rkyv-serialized getblocktemplate RPC requests
impl<Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook> RpcService
    for crate::methods::GetBlockTemplateRpcImpl<
        Mempool,
        State,
        Tip,
        ChainVerifier,
        SyncStatus,
        AddressBook,
    >
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
    fn service_name() -> &'static str {
        "zebra_datacake_getblocktemplate_rpc_service"
    }

    fn register_handlers(registry: &mut ServiceRegistry<Self>) {
        use methods::get_block_template_rpcs::request;

        registry.add_handler::<request::BlockCount>();
        registry.add_handler::<request::BlockHash>();
        registry.add_handler::<request::BlockTemplate>();
    }
}

fn make_status_error(
    jsonrpc_core::Error {
        code: error_code,
        message,
        data: _,
    }: jsonrpc_core::Error,
) -> Status {
    Status {
        code: match error_code {
            jsonrpc_core::ErrorCode::ParseError
            | jsonrpc_core::ErrorCode::InvalidRequest
            | jsonrpc_core::ErrorCode::InvalidParams
            | jsonrpc_core::ErrorCode::MethodNotFound => datacake_rpc::ErrorCode::InvalidPayload,
            jsonrpc_core::ErrorCode::InternalError | jsonrpc_core::ErrorCode::ServerError(_) => {
                datacake_rpc::ErrorCode::InternalError
            }
        },
        message,
    }
}

/// Accepts a [`SocketAddr`] and runs rkyv RPC server
///
/// Returns the [`Server`] with the JoinHandle if successful or an `io::Error`
pub async fn spawn_server(address: SocketAddr) -> io::Result<Server> {
    let server = Server::listen(address).await?;

    tracing::info!("Listening to address {}!", address);

    Ok(server)
}
