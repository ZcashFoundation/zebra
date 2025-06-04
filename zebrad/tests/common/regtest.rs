//! Test submitblock RPC method on Regtest.
//!
//! This test will get block templates via the `getblocktemplate` RPC method and submit them as new blocks
//! via the `submitblock` RPC method on Regtest.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Context, Result};
use tower::BoxError;
use tracing::*;

use zebra_chain::{
    block::{Block, Height},
    parameters::{testnet::REGTEST_NU5_ACTIVATION_HEIGHT, Network, NetworkUpgrade},
    primitives::byte_array::increment_big_endian,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::types::{HexData, SubmitBlockResponse, TemplateResponse, TimeSource},
    proposal_block_from_template,
    server::{self, OPENED_RPC_ENDPOINT_MSG},
};
use zebra_test::args;

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::ZebradTestDirExt,
};

/// Number of blocks that should be submitted before the test is considered successful.
const NUM_BLOCKS_TO_SUBMIT: usize = 200;

pub(crate) async fn submit_blocks_test() -> Result<()> {
    let _init_guard = zebra_test::init();
    info!("starting regtest submit_blocks test");

    let network = Network::new_regtest(Default::default());
    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    let rpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("attempting to submit blocks");
    submit_blocks(network, rpc_address).await?;

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}

/// Get block templates and submit blocks
async fn submit_blocks(network: Network, rpc_address: SocketAddr) -> Result<()> {
    let client = RpcRequestClient::new(rpc_address);

    for _ in 1..=NUM_BLOCKS_TO_SUBMIT {
        let (mut block, height) = client
            .block_from_template(Height(REGTEST_NU5_ACTIVATION_HEIGHT))
            .await?;

        while !network.disable_pow()
            && zebra_consensus::difficulty_is_valid(&block.header, &network, &height, &block.hash())
                .is_err()
        {
            increment_big_endian(Arc::make_mut(&mut block.header).nonce.as_mut());
        }

        if height.0 % 40 == 0 {
            info!(?block, ?height, "submitting block");
        }

        client.submit_block(block).await?;
    }

    Ok(())
}

pub trait MiningRpcMethods {
    async fn block_from_template(&self, nu5_activation_height: Height) -> Result<(Block, Height)>;
    async fn submit_block(&self, block: Block) -> Result<()>;
    async fn submit_block_from_template(&self) -> Result<(Block, Height)>;
    async fn get_block(&self, height: i32) -> Result<Option<Arc<Block>>, BoxError>;
}

impl MiningRpcMethods for RpcRequestClient {
    async fn block_from_template(&self, nu5_activation_height: Height) -> Result<(Block, Height)> {
        let block_template: TemplateResponse = self
            .json_result_from_call("getblocktemplate", "[]".to_string())
            .await
            .expect("response should be success output with a serialized `GetBlockTemplate`");

        let height = Height(block_template.height());

        let network_upgrade = if height < nu5_activation_height {
            NetworkUpgrade::Canopy
        } else {
            NetworkUpgrade::Nu5
        };

        Ok((
            proposal_block_from_template(&block_template, TimeSource::default(), network_upgrade)?,
            height,
        ))
    }

    async fn submit_block(&self, block: Block) -> Result<()> {
        let block_data = hex::encode(block.zcash_serialize_to_vec()?);

        let submit_block_response: SubmitBlockResponse = self
            .json_result_from_call("submitblock", format!(r#"["{block_data}"]"#))
            .await
            .map_err(|err| eyre!(err))?;

        match submit_block_response {
            SubmitBlockResponse::Accepted => Ok(()),
            SubmitBlockResponse::ErrorResponse(err) => {
                Err(eyre!("block submission failed: {err:?}"))
            }
        }
    }

    async fn submit_block_from_template(&self) -> Result<(Block, Height)> {
        let (block, height) = self
            .block_from_template(Height(REGTEST_NU5_ACTIVATION_HEIGHT))
            .await?;

        self.submit_block(block.clone()).await?;

        Ok((block, height))
    }

    async fn get_block(&self, height: i32) -> Result<Option<Arc<Block>>, BoxError> {
        match self
            .json_result_from_call("getblock", format!(r#"["{}", 0]"#, height))
            .await
        {
            Ok(HexData(raw_block)) => {
                let block = raw_block.zcash_deserialize_into::<Block>()?;
                Ok(Some(Arc::new(block)))
            }
            Err(err)
                if err
                    .downcast_ref::<jsonrpsee_types::ErrorObject>()
                    .is_some_and(|err| {
                        let error: i32 = server::error::LegacyCode::InvalidParameter.into();
                        err.code() == error
                    }) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}
