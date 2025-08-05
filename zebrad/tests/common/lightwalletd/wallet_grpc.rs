//! Lightwalletd gRPC interface and utility functions.

use std::env;

use zebra_test::prelude::*;

tonic::include_proto!("cash.z.wallet.sdk.rpc");

/// Type alias for the RPC client to communicate with a lightwalletd instance.
pub type LightwalletdRpcClient =
    compact_tx_streamer_client::CompactTxStreamerClient<tonic::transport::Channel>;

/// Connect to a lightwalletd RPC instance.
#[tracing::instrument]
pub async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}
