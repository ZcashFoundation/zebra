use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Context, Result};
use tower::ServiceExt;
use tracing::*;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{self as zs, init_read_only};
use zebra_test::args;
use zs::SemanticallyVerifiedBlock;

use crate::common::{
    config::{random_known_rpc_port_config, testdir},
    launch::ZebradTestDirExt,
    regtest::submit_blocks,
};

/// Number of blocks to submit during the test.
const NUM_BLOCKS_TO_SUBMIT: usize = 500;

pub(crate) async fn has_non_finalized_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();
    info!("starting read_state::has_non_finalized_best_chain test");

    let network = Network::new_regtest(None);
    let mut config = random_known_rpc_port_config(false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);
    let rpc_address = config.rpc.listen_addr.unwrap();

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("attempting to add blocks to the best chain");
    tokio::spawn(submit_blocks(
        rpc_address,
        NUM_BLOCKS_TO_SUBMIT,
        Some(Duration::from_millis(500)),
    ));

    let (mut non_finalized_state, non_finalized_state_sender, read_state) =
        init_read_only(config.state, &network);
    let finalized_state = read_state.db();

    let zs::ReadResponse::Tip(tip_block) = read_state
        .clone()
        .oneshot(zs::ReadRequest::Tip)
        .await
        .map_err(|err| eyre!(err))?
    else {
        unreachable!("wrong response to ReadRequest::Tip");
    };

    let (tip_height, tip_hash) = tip_block.expect("should have genesis block");

    let rpc_client = RpcRequestClient::new(rpc_address);

    let mut next_block_height = tip_height.next()?;
    let mut expected_prev_block_hash = tip_hash;

    loop {
        if next_block_height.0 > NUM_BLOCKS_TO_SUBMIT.try_into().expect("should fit in u32") {
            break;
        }

        let raw_block: serde_json::Value = serde_json::from_str(
            &rpc_client
                .text_from_call("getblock", format!(r#"["{}", 0]"#, next_block_height.0))
                .await?,
        )?;

        if let Some(block_data) = raw_block["result"].as_str().map(hex::decode) {
            let block: Block = block_data?.zcash_deserialize_into()?;
            let block: SemanticallyVerifiedBlock = Arc::new(block).into();
            let parent_hash = block.block.header.previous_block_hash;
            // If the best chain has changed and is longer than the previous best chain, it should return a block
            // at the expected block height an unexpected previous block hash
            if parent_hash != expected_prev_block_hash {
                let zs::ReadResponse::Tip(tip_block) = read_state
                    .clone()
                    .oneshot(zs::ReadRequest::Tip)
                    .await
                    .map_err(|err| eyre!(err))?
                else {
                    unreachable!("wrong response to ReadRequest::Tip");
                };

                let (tip_height, tip_hash) = tip_block.expect("should have genesis block");
                next_block_height = tip_height.next()?;
                expected_prev_block_hash = tip_hash;
            } else {
                let block_hash = block.hash;
                let block_height = block.height;

                let commit_result = if finalized_state.finalized_tip_hash() == parent_hash {
                    non_finalized_state.commit_new_chain(block, read_state.db())
                } else {
                    non_finalized_state.commit_block(block, read_state.db())
                };

                if commit_result.is_ok() {
                    non_finalized_state_sender.send(non_finalized_state.clone())?;
                    expected_prev_block_hash = block_hash;
                    next_block_height = block_height.next()?;
                }
            }
        } else {
            // If the best chain has changed and is shorter than or the same length as the previous best chain, it should
            // return a MISSING_BLOCK_ERROR_CODE, then we can check the best block hash to see if we have still have the best chain tip
            // or if the best chain has changed.
            //
            // TODO: Add a call to `getbestblockhash` here, if it's different from `expected_prev_block_hash`,
            //       try getblock once more on the next block height, if the next block height is still unavailable, then
            //       consider that a chain tip reset and start over from the finalized tip.
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}
