//! Datacake RPC server tests

use datacake_rpc::{Channel, RpcClient};
use tower::{buffer::Buffer, BoxError};
use zebra_chain::{
    block, chain_sync_status::MockSyncStatus, chain_tip::mock::MockChainTip, parameters::Network,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{datacake_rpc::*, methods::get_block_template_rpcs::GetBlockTemplateRpcImpl};

type GetBlockTemplateRpcService = GetBlockTemplateRpcImpl<
    MockService<mempool::Request, mempool::Response, PanicAssertion>,
    MockService<zebra_state::ReadRequest, zebra_state::ReadResponse, PanicAssertion>,
    MockChainTip,
    MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
    MockSyncStatus,
    MockAddressBookPeers,
>;

#[tokio::test]
async fn datacake_get_block_template() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_known_port();
    let address = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port).into();

    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();

    // Init RPC
    let get_block_template_rpc_impl: GetBlockTemplateRpcService = GetBlockTemplateRpcImpl::new(
        Network::Mainnet,
        Default::default(),
        Buffer::new(MockService::build().for_unit_tests(), 1),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
    );

    tracing::info!("spawning datacake RPC server...");

    let server = spawn_server(address).await?;
    tracing::info!("Listening to address {}!", address);

    server.add_service(get_block_template_rpc_impl);

    let client = Channel::connect(address);
    tracing::info!("Connected to address {}!", address);

    let rpc_client: RpcClient<GetBlockTemplateRpcService> = RpcClient::new(client);

    let response::get_block_template::Response::TemplateMode(_template) = rpc_client
        .send(&request::BlockTemplate(None))
        .await?
        .to_owned()? else {
            panic!("getblocktemplate method call without params should return template.");
        };

    Ok(())
}
