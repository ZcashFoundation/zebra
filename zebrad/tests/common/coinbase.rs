//! Tests for checking that Zebra produces valid coinbase transactions.

use std::sync::Arc;

use color_eyre::eyre::{self, Context};
use futures::future::try_join_all;
use strum::IntoEnumIterator;

use zebra_chain::{
    parameters::{testnet::ConfiguredActivationHeights, Network},
    primitives::byte_array::increment_big_endian,
};
use zebra_consensus::difficulty_is_valid;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{config::mining::MinerAddressType, server::OPENED_RPC_ENDPOINT_MSG};
use zebra_test::args;
use zebrad::components::With;

use super::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

/// Tests that Zebra can mine blocks with valid coinbase transactions on Regtest.
pub(crate) async fn regtest_coinbase() -> eyre::Result<()> {
    async fn regtest_coinbase(addr_type: MinerAddressType) -> eyre::Result<()> {
        let _init_guard = zebra_test::init();

        let net = Network::new_regtest(
            ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );

        let mut config = os_assigned_rpc_port_config(false, &net)?.with(addr_type);
        config.mempool.debug_enable_at_height = Some(0);

        let mut zebrad = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;

        tokio::time::sleep(LAUNCH_DELAY).await;

        let client = RpcRequestClient::new(read_listen_addr_from_logs(
            &mut zebrad,
            OPENED_RPC_ENDPOINT_MSG,
        )?);

        for _ in 0..2 {
            let (mut block, height) = client.new_block_from_gbt().await?;

            // If the network requires PoW, find a valid nonce.
            if !net.disable_pow() {
                let header = Arc::make_mut(&mut block.header);

                loop {
                    let hash = header.hash();

                    if difficulty_is_valid(header, &net, &height, &hash).is_ok() {
                        break;
                    }

                    increment_big_endian(header.nonce.as_mut());
                }
            }

            client.submit_block(block).await?;
        }

        zebrad.kill(false)?;

        zebrad
            .wait_with_output()?
            .assert_failure()?
            .assert_was_killed()
            .wrap_err("possible port conflict with another zebrad instance")
    }

    try_join_all(MinerAddressType::iter().map(regtest_coinbase))
        .await
        .map(|_| ())
}
