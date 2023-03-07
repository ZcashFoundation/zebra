//! Datacake RPC server tests

use datacake_rpc::{Channel, RpcClient};
use tower::{buffer::Buffer, BoxError};
use zebra_chain::{chain_tip::NoChainTip, parameters::Network};
use zebra_network::constants::USER_AGENT;
use zebra_test::mock_service::MockService;

use crate::datacake_rpc::*;

#[tokio::test]
async fn datacake_rpc_server_spawn() -> Result<(), BoxError> {
    let port = zebra_test::net::random_known_port();
    let address = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port).into();

    let _init_guard = zebra_test::init();

    let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let app_version = "v0.0.1-datacake-RPC-server-test";

    let (rpc_impl, _rpc_tx_queue_task_handle) = RpcImpl::new(
        app_version,
        Network::Mainnet,
        false,
        false,
        Buffer::new(mempool, 1),
        state,
        NoChainTip,
    );

    tracing::info!("spawning datacake RPC server...");

    let _server = spawn_server(address, rpc_impl).await?;
    tracing::info!("Listening to address {}!", address);

    let client = Channel::connect(address);
    tracing::info!("Connected to address {}!", address);

    let rpc_client: RpcClient<
        RpcImpl<
            MockService<
                mempool::Request,
                mempool::Response,
                zebra_node_services::BoxError,
                BoxError,
            >,
            MockService<
                zebra_state::ReadRequest,
                zebra_state::ReadResponse,
                zebra_state::BoxError,
                BoxError,
            >,
            NoChainTip,
        >,
    > = RpcClient::new(client);

    let resp = rpc_client.send(&Request::GetInfo).await?.to_owned()?;

    assert_eq!(
        resp,
        Response::GetInfo(GetInfo {
            build: app_version.into(),
            subversion: USER_AGENT.into(),
        })
    );

    Ok(())
}
