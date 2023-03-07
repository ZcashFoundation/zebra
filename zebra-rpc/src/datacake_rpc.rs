//! Datacake RPC Server
// TODO: use hyper directly?

use std::{io, net::SocketAddr};

use bytecheck::CheckBytes;
use datacake_rpc::{ErrorCode, Handler, RpcService, Server, ServiceRegistry, Status};
use rkyv::{Archive, Deserialize, Serialize};
use tower::Service;
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::mempool;

use zebra_chain::{
    block::{self, Height},
    parameters::ConsensusBranchId,
};

use crate::methods::{
    AddressBalance, AddressStrings, ConsensusBranchIdHex, GetBlockChainInfo, GetInfo,
    NetworkUpgradeInfo, Rpc, RpcImpl, TipConsensusBranch,
};

#[cfg(test)]
mod tests;

/// An RPC method request
#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub enum Request {
    /// Returns software information from the RPC server, as a [`GetInfo`].
    ///
    /// See [`Rpc::get_info`] for more information.
    GetInfo,

    /// Returns blockchain state information, as a [`GetBlockChainInfo`].
    ///
    /// See [`Rpc::get_blockchain_info`] for more information.
    GetBlockChainInfo,

    /// Returns the total balance of a provided `addresses` in an [`AddressBalance`] instance.
    ///
    /// See [`Rpc::get_address_balance`] for more information.
    AddressBalance(AddressStrings),
}

/// An IndexMap entry, see 'upgrades' field [`GetBlockChainInfo`]
#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub struct NetworkUpgradeInfoEntry(pub ConsensusBranchId, pub NetworkUpgradeInfo);

/// An RPC method request
#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub enum Response {
    /// Software information from the RPC server, as a [`GetInfo`].
    ///
    /// See [`Rpc::get_info`] for more information.
    GetInfo(GetInfo),

    /// Blockchain state information, as a [`GetBlockChainInfo`].
    ///
    /// See [`Rpc::get_blockchain_info`] for more information.
    GetBlockChainInfo {
        /// Current network name as defined in BIP70 (main, test, regtest)
        chain: String,

        /// The current number of blocks processed in the server, numeric
        blocks: Height,

        /// The hash of the currently best block, in big-endian order, hex-encoded
        best_block_hash: block::Hash,

        /// If syncing, the estimated height of the chain, else the current best height, numeric.
        ///
        /// In Zebra, this is always the height estimate, so it might be a little inaccurate.
        estimated_height: Height,

        /// Status of network upgrades
        upgrades: Vec<NetworkUpgradeInfoEntry>,

        /// Branch IDs of the current consensus rules
        consensus_chain_tip: ConsensusBranchId,

        /// Branch IDs of the upcoming consensus rules
        consensus_next_block: ConsensusBranchId,
    },

    /// Total balance of a provided `addresses` in an [`AddressBalance`] instance.
    ///
    /// See [`Rpc::get_address_balance`] for more information.
    AddressBalance(AddressBalance),
}

impl From<GetInfo> for Response {
    fn from(get_info: GetInfo) -> Self {
        Self::GetInfo(get_info)
    }
}

impl From<GetBlockChainInfo> for Response {
    fn from(
        GetBlockChainInfo {
            chain,
            blocks,
            best_block_hash,
            estimated_height,
            upgrades,
            consensus:
                TipConsensusBranch {
                    chain_tip: ConsensusBranchIdHex(consensus_chain_tip),
                    next_block: ConsensusBranchIdHex(consensus_next_block),
                },
        }: GetBlockChainInfo,
    ) -> Self {
        let upgrades = upgrades
            .into_iter()
            .map(|(ConsensusBranchIdHex(k), v)| NetworkUpgradeInfoEntry(k, v))
            .collect();

        Self::GetBlockChainInfo {
            chain,
            blocks,
            best_block_hash,
            estimated_height,
            upgrades,
            consensus_chain_tip,
            consensus_next_block,
        }
    }
}

impl From<AddressBalance> for Response {
    fn from(address_balance: AddressBalance) -> Self {
        Self::AddressBalance(address_balance)
    }
}

struct ErrorResponse(Status);

impl From<jsonrpc_core::Error> for ErrorResponse {
    fn from(
        jsonrpc_core::Error {
            code,
            message,
            data: _,
        }: jsonrpc_core::Error,
    ) -> Self {
        let code = match code {
            jsonrpc_core::ErrorCode::ParseError
            | jsonrpc_core::ErrorCode::InvalidRequest
            | jsonrpc_core::ErrorCode::MethodNotFound
            | jsonrpc_core::ErrorCode::InvalidParams => ErrorCode::InvalidPayload,
            jsonrpc_core::ErrorCode::InternalError | jsonrpc_core::ErrorCode::ServerError(_) => {
                ErrorCode::InternalError
            }
        };

        Self(Status { code, message })
    }
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
        registry.add_handler::<Request>();
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<Request> for RpcImpl<Mempool, State, Tip>
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
    type Reply = Response;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<Request>,
    ) -> Result<Self::Reply, Status> {
        match request.to_owned().unwrap() {
            Request::GetInfo => self.get_info().map(Response::GetInfo),
            Request::GetBlockChainInfo => self.get_blockchain_info().map(Into::into),
            Request::AddressBalance(address_strings) => self
                .get_address_balance(address_strings)
                .await
                .map(Response::AddressBalance),
        }
        .map_err(make_status_error)
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
