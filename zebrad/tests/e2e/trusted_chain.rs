use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Error, Result};
use tokio::time::timeout;
use tower::ServiceExt;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, ChainHistoryBlockTxAuthCommitmentHash},
    chain_tip::ChainTip,
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    primitives::byte_array::increment_big_endian,
    serialization::BytesInDisplayOrder,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{methods::GetBlockHashResponse, server::OPENED_RPC_ENDPOINT_MSG};
use zebra_state::{ReadResponse, Response};
use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{
    config::{
        os_assigned_rpc_port_config, random_known_rpc_port_config, read_listen_addr_from_logs,
        testdir,
    },
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

const TIP_CHANGE_TIMEOUT: Duration = Duration::from_secs(1);

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn trusted_chain_sync_handles_forks_correctly() -> Result<()> {
    let _init_guard = zebra_test::init();

    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );
    let mut config = os_assigned_rpc_port_config(false, &net)?;

    config.state.ephemeral = false;
    config.rpc.indexer_listen_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));

    let test_dir = testdir()?.with_config(&mut config)?;
    let mut child = test_dir.spawn_child(args!["start"])?;

    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let indexer_listen_addr = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("starting read state with syncer");
    // Spawn a read state with the RPC syncer to check that it has the same best chain as Zebra
    let (read_state, _latest_chain_tip, mut chain_tip_change, _sync_task) =
        zebra_rpc::sync::init_read_state_with_syncer(
            config.state,
            &config.network.network,
            indexer_listen_addr,
        )
        .await?
        .map_err(|err| eyre!(err))?;

    tracing::info!("waiting for first chain tip change");

    // Wait for Zebrad to start up
    let tip_action = timeout(LAUNCH_DELAY, chain_tip_change.wait_for_tip_change()).await??;
    assert!(
        tip_action.is_reset(),
        "first tip action should be a reset for the genesis block"
    );

    tracing::info!("got genesis chain tip change, submitting more blocks ..");

    let rpc_client = RpcRequestClient::new(rpc_address);
    let mut blocks = Vec::new();
    for _ in 0..10 {
        let (block, height) = rpc_client.block_from_template(&net).await?;

        rpc_client.submit_block(block.clone()).await?;

        blocks.push(block);
        let tip_action =
            timeout(TIP_CHANGE_TIMEOUT, chain_tip_change.wait_for_tip_change()).await??;

        assert_eq!(
            tip_action.best_tip_height(),
            height,
            "tip action height should match block submission"
        );
    }

    tracing::info!("checking that read state has the new non-finalized best chain blocks");
    for expected_block in blocks.clone() {
        let height = expected_block.coinbase_height().unwrap();
        let zebra_block = rpc_client
            .get_block(height.0 as i32)
            .await
            .map_err(|err| eyre!(err))?
            .expect("Zebra test child should have the expected block");

        assert_eq!(
            zebra_block,
            Arc::new(expected_block),
            "Zebra should have the same block"
        );

        let ReadResponse::Block(read_state_block) = read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(height.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("unexpected read response to a block request")
        };

        assert_eq!(
            zebra_block,
            read_state_block.expect("read state should have the block"),
            "read state should have the same block"
        );
    }

    tracing::info!("getting next block template");
    let (block_11, _) = rpc_client.block_from_template(&net).await?;
    blocks.push(block_11);
    let next_blocks: Vec<_> = blocks.split_off(5);

    tracing::info!("creating populated state");
    let genesis_block = regtest_genesis_block();
    let (state2, read_state2, latest_chain_tip2, _chain_tip_change2) =
        zebra_state::populated_state(
            std::iter::once(genesis_block).chain(blocks.iter().cloned().map(Arc::new)),
            &net,
        )
        .await;

    tracing::info!("attempting to trigger a best chain change");
    for mut block in next_blocks {
        let ReadResponse::ChainInfo(chain_info) = read_state2
            .clone()
            .oneshot(zebra_state::ReadRequest::ChainInfo)
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("wrong response variant");
        };

        let height = block.coinbase_height().unwrap();
        let auth_root = block.auth_data_root();
        let hist_root = chain_info.chain_history_root.unwrap_or_default();
        let header = Arc::make_mut(&mut block.header);

        header.commitment_bytes = match NetworkUpgrade::current(&net, height) {
            NetworkUpgrade::Canopy => hist_root.bytes_in_serialized_order(),
            NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu6_2
            | NetworkUpgrade::Nu7 => {
                ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&hist_root, &auth_root)
                    .bytes_in_serialized_order()
            }
            #[cfg(zcash_unstable = "zfuture")]
            NetworkUpgrade::ZFuture => {
                ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&hist_root, &auth_root)
                    .bytes_in_serialized_order()
            }
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom
            | NetworkUpgrade::Heartwood => Err(eyre!(
                "Zebra does not support generating pre-Canopy block templates"
            ))?,
        }
        .into();

        increment_big_endian(header.nonce.as_mut());

        header.previous_block_hash = chain_info.tip_hash;

        let Response::Committed(block_hash) = state2
            .clone()
            .oneshot(zebra_state::Request::CommitSemanticallyVerifiedBlock(
                Arc::new(block.clone()).into(),
            ))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("wrong response variant");
        };

        assert!(
            chain_tip_change.last_tip_change().is_none(),
            "there should be no tip change until the last block is submitted"
        );

        rpc_client.submit_block(block.clone()).await?;
        blocks.push(block);
        let best_block_hash: GetBlockHashResponse = rpc_client
            .json_result_from_call("getbestblockhash", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        if block_hash == best_block_hash.hash() {
            break;
        }
    }

    tracing::info!("newly submitted blocks are in the best chain, checking for reset");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let tip_action = timeout(LAUNCH_DELAY, chain_tip_change.wait_for_tip_change()).await??;
    let (expected_height, expected_hash) = latest_chain_tip2
        .best_tip_height_and_hash()
        .expect("should have a chain tip");
    assert!(tip_action.is_reset(), "tip action should be reset");
    assert_eq!(
        tip_action.best_tip_hash_and_height(),
        (expected_hash, expected_height),
        "tip action hashes and heights should match"
    );

    tracing::info!("checking that read state has the new non-finalized best chain blocks");
    for expected_block in blocks {
        let height = expected_block.coinbase_height().unwrap();
        let zebra_block = rpc_client
            .get_block(height.0 as i32)
            .await
            .map_err(|err| eyre!(err))?
            .expect("Zebra test child should have the expected block");

        assert_eq!(
            zebra_block,
            Arc::new(expected_block),
            "Zebra should have the same block"
        );

        let ReadResponse::Block(read_state_block) = read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(height.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("unexpected read response to a block request")
        };

        assert_eq!(
            zebra_block,
            read_state_block.expect("read state should have the block"),
            "read state should have the same block"
        );
    }

    tracing::info!("restarting Zebra on Mainnet");

    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    let mut config = random_known_rpc_port_config(false, &Network::Mainnet)?;
    config.state.ephemeral = false;
    config.rpc.indexer_listen_addr = Some(std::net::SocketAddr::from((
        [127, 0, 0, 1],
        random_known_port(),
    )));
    let indexer_listen_addr = config.rpc.indexer_listen_addr.unwrap();
    let test_dir = testdir()?.with_config(&mut config)?;

    let _child = test_dir.spawn_child(args!["start"])?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("starting read state with syncer");
    // Spawn a read state with the RPC syncer to check that it has the same best chain as Zebra
    let (_read_state, _latest_chain_tip, mut chain_tip_change, _sync_task) =
        zebra_rpc::sync::init_read_state_with_syncer(
            config.state,
            &config.network.network,
            indexer_listen_addr,
        )
        .await?
        .map_err(|err| eyre!(err))?;

    tracing::info!("waiting for finalized chain tip changes");

    timeout(
        Duration::from_secs(200),
        tokio::spawn(async move {
            for _ in 0..2 {
                chain_tip_change
                    .wait_for_tip_change()
                    .await
                    .map_err(|err| eyre!(err))?;
            }

            Ok::<(), Error>(())
        }),
    )
    .await???;

    Ok(())
}
