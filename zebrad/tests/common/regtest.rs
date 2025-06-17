//! Test submitblock RPC method on Regtest.
//!
//! This test will get block templates via the `getblocktemplate` RPC method and submit them as new blocks
//! via the `submitblock` RPC method on Regtest.

use std::sync::Arc;

use color_eyre::eyre::{eyre, Context, Result};
use tower::BoxError;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    primitives::byte_array::increment_big_endian,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    methods::{
        hex_data::HexData,
        types::{
            get_block_template::{
                proposal::proposal_block_from_template, GetBlockTemplate, TimeSource,
            },
            submit_block,
        },
    },
    server::{self, OPENED_RPC_ENDPOINT_MSG},
};
use zebra_test::args;

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

/// Number of blocks that should be submitted before the test is considered successful.
const NUM_BLOCKS_TO_SUBMIT: usize = 200;

pub(crate) async fn submit_blocks_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Default::default());
    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    let rpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;

    tokio::time::sleep(LAUNCH_DELAY).await;

    let client = RpcRequestClient::new(rpc_address);

    for _ in 1..=NUM_BLOCKS_TO_SUBMIT {
        let (mut block, height) = client.block_from_template().await?;

        while !network.disable_pow()
            && zebra_consensus::difficulty_is_valid(&block.header, &network, &height, &block.hash())
                .is_err()
        {
            increment_big_endian(Arc::make_mut(&mut block.header).nonce.as_mut());
        }

        client.submit_block(block).await?;
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}

pub trait MiningRpcMethods {
    async fn block_from_template(&self) -> Result<(Block, Height)>;
    async fn submit_block(&self, block: Block) -> Result<()>;
    async fn get_block(&self, height: i32) -> Result<Option<Arc<Block>>, BoxError>;
}

impl MiningRpcMethods for RpcRequestClient {
    async fn block_from_template(&self) -> Result<(Block, Height)> {
        let block_template: GetBlockTemplate = self
            .json_result_from_call("getblocktemplate", "[]".to_string())
            .await
            .expect("response should be success output with a serialized `GetBlockTemplate`");

        Ok((
            proposal_block_from_template(&block_template, TimeSource::default())?,
            Height(block_template.height),
        ))
    }

    async fn submit_block(&self, block: Block) -> Result<()> {
        let block_data = hex::encode(block.zcash_serialize_to_vec()?);

        let submit_block_response: submit_block::Response = self
            .json_result_from_call("submitblock", format!(r#"["{block_data}"]"#))
            .await
            .map_err(|err| eyre!(err))?;

        match submit_block_response {
            submit_block::Response::Accepted => Ok(()),
            submit_block::Response::ErrorResponse(err) => {
                Err(eyre!("block submission failed: {err:?}"))
            }
        }
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
