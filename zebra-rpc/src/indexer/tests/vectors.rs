//! Fixed test vectors for indexer RPCs

use zebra_test::{
    mock_service::MockService,
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer;

#[tokio::test]
pub async fn test_server_spawn() -> Result<()> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let mock_read_service = MockService::build().for_unit_tests();

    let (server_task, listen_addr) = indexer::server::init(listen_addr, mock_read_service)
        .await
        .map_err(|err| eyre!(err))?;

    assert!(!server_task.is_finished(), "server task should be running");

    Ok(())
}
