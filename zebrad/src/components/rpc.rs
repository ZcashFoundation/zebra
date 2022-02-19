//! An RPC endpoint.

use jsonrpc_core;

use jsonrpc_http_server::ServerBuilder;

use zebra_rpc::rpc::{Rpc, RpcImpl};

pub mod config;
pub use config::Config;

/// Zebra RPC Server
pub struct RpcServer {}

impl RpcServer {
    /// Start a new RPC server endpoint
    pub async fn new(config: Config) -> Self {
        if config.listen {
            info!("Trying to open RPC endpoint at {}...", config.listen_addr);

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io =
                jsonrpc_core::IoHandler::with_compatibility(jsonrpc_core::Compatibility::Both);
            io.extend_with(RpcImpl.to_delegate());

            let server = ServerBuilder::new(io)
                .threads(1)
                .start_http(&config.listen_addr)
                .expect("Unable to start RPC server");

            info!("Opened RPC endpoint at {}", server.address());

            server.wait();
        }
        RpcServer {}
    }
}
