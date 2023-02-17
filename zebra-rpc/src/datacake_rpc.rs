//! Datacake RPC Server

use std::{io, net::SocketAddr};

use bytecheck::CheckBytes;
use datacake_rpc::{Handler, Request, RpcService, Server, ServiceRegistry, Status};
use rkyv::{Archive, Deserialize, Serialize};
use tower::Service;
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::mempool;

use crate::methods::RpcImpl;

#[cfg(test)]
mod tests;

/// An RPC method request
#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive(compare(PartialEq))]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub enum Method {
    /// Returns software information from the RPC server, as a [`GetInfo`].
    ///
    /// See [`RpcImpl`] for more information.
    GetInfo,

    /// Returns blockchain state information, as a [`GetBlockChainInfo`].
    ///
    /// See [`RpcImpl`] for more information.
    GetBlockChainInfo,
}

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
        registry.add_handler::<Method>();
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<Method> for RpcImpl<Mempool, State, Tip>
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
    type Reply = String;

    async fn on_message(&self, request: Request<Method>) -> Result<Self::Reply, Status> {
        match request.to_owned().unwrap() {
            Method::GetInfo => Ok("getinfo".to_string()),
            Method::GetBlockChainInfo => Ok("getblockchaininfo".to_string()),
        }
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
