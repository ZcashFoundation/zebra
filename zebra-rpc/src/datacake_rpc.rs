//! Datacake RPC Server

use std::{io, net::SocketAddr};

use datacake_rpc::{ErrorCode, Handler, RpcService, Server, ServiceRegistry, Status};
use tower::Service;
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::mempool;

use crate::methods::{Rpc, RpcImpl};

mod methods;
pub mod request;
pub mod response;

#[cfg(test)]
mod tests;

/// Handles rkyv-serialized RPC requests
impl<Mempool, State, Tip> RpcService for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
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
        "zebra_datacake_rpc_server"
    }

    fn register_handlers(registry: &mut ServiceRegistry<Self>) {
        registry.add_handler::<request::Info>();
        registry.add_handler::<request::BlockChainInfo>();
        registry.add_handler::<request::AddressBalance>();
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
pub async fn spawn_server<Mempool, State, Tip>(
    address: SocketAddr,
    rpc_impl: RpcImpl<Mempool, State, Tip>,
) -> io::Result<Server>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
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
    let server = Server::listen(address).await?;
    server.add_service(rpc_impl);

    tracing::info!("Listening to address {}!", address);

    Ok(server)
}
