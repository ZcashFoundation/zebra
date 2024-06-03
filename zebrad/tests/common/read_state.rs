use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Context, Result};
use tower::ServiceExt;
use tracing::*;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::methods::GetBlockHash;
use zebra_state::{self as zs, init_read_only};
use zebra_test::args;
use zs::{NonFinalizedState, SemanticallyVerifiedBlock};

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
    config.state.ephemeral = false;
    config.mempool.debug_enable_at_height = Some(0);
    let rpc_address = config.rpc.listen_addr.unwrap();

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(45)).await;

    // TODO: Define expected forks to test that this code handles chain reorgs correctly.
    info!("attempting to add blocks to the best chain");
    tokio::spawn(submit_blocks(
        rpc_address,
        NUM_BLOCKS_TO_SUBMIT,
        Some(Duration::from_millis(500)),
    ));

    let (mut non_finalized_state, non_finalized_state_sender, read_state) =
        init_read_only(config.state, &network);
    let finalized_state = read_state.db().clone();

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

        // Wait until the best block hash in Zebra is different from the tip hash in this read state
        let best_block_hash = loop {
            let GetBlockHash(best_block_hash) = rpc_client
                .json_result_from_call("getbestblockhash", "[]")
                .await?;

            let zs::ReadResponse::Tip(tip_block) = read_state
                .clone()
                .oneshot(zs::ReadRequest::Tip)
                .await
                .map_err(|err| eyre!(err))?
            else {
                unreachable!("wrong response to ReadRequest::Tip");
            };

            if Some(best_block_hash) != tip_block.map(|(_, hash)| hash) {
                break best_block_hash;
            } else {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };

        loop {
            let raw_block: serde_json::Value = serde_json::from_str(
                &rpc_client
                    .text_from_call("getblock", format!(r#"["{}", 0]"#, next_block_height.0))
                    .await?,
            )?;

            let Some(block_data) = raw_block["result"].as_str().map(hex::decode) else {
                // If there is no next block despite the best block hash changing, the chain hasn't grown, and so there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block past the finalized tip.
                let finalized_state = finalized_state.clone();
                let Some((finalized_tip_height, finalized_tip_hash)) =
                    tokio::task::spawn_blocking(move || finalized_state.tip()).await?
                else {
                    continue;
                };

                next_block_height = finalized_tip_height.next()?;
                expected_prev_block_hash = finalized_tip_hash;
                non_finalized_state = NonFinalizedState::new(&network);
                non_finalized_state_sender.send(non_finalized_state.clone())?;

                continue;
            };

            let block: Block = block_data?.zcash_deserialize_into()?;
            let block: SemanticallyVerifiedBlock = Arc::new(block).into();
            let parent_hash = block.block.header.previous_block_hash;

            if parent_hash != expected_prev_block_hash {
                // If the next block's previous block hash doesn't match the expected hash, there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block
                // past the finalized tip.
                let finalized_state = finalized_state.clone();
                let Some((finalized_tip_height, finalized_tip_hash)) =
                    tokio::task::spawn_blocking(move || finalized_state.tip()).await?
                else {
                    continue;
                };

                next_block_height = finalized_tip_height.next()?;
                expected_prev_block_hash = finalized_tip_hash;
                non_finalized_state = NonFinalizedState::new(&network);
                non_finalized_state_sender.send(non_finalized_state.clone())?;

                continue;
            } else {
                let block_hash = block.hash;
                let block_height = block.height;

                let finalized_state = finalized_state.clone();
                let finalized_tip_hash =
                    tokio::task::spawn_blocking(move || finalized_state.finalized_tip_hash())
                        .await?;

                let commit_result = if finalized_tip_hash == parent_hash {
                    non_finalized_state.commit_new_chain(block, read_state.db())
                } else {
                    non_finalized_state.commit_block(block, read_state.db())
                };

                if commit_result.is_ok() {
                    non_finalized_state_sender.send(non_finalized_state.clone())?;
                    expected_prev_block_hash = block_hash;
                    next_block_height = block_height.next()?;

                    // If the block hash matches the output from the `getbestblockhash` RPC method, we can wait until
                    // the best block hash changes to get the next block.
                    if block_hash == best_block_hash {
                        break;
                    }
                }
            }
        }
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}
