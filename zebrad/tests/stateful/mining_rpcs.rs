//! Mining RPC tests

use std::env;

use color_eyre::eyre::WrapErr;
use serde_json::Value;

use zebra_chain::parameters::Network;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::state_database_format_version_in_code;
use zebra_test::prelude::*;

use crate::common::{
    cached_state::{wait_for_state_version_message, wait_for_state_version_upgrade},
    get_block_template_rpcs,
    launch::spawn_zebrad_for_rpc,
    sync::SYNC_FINISHED_REGEX,
    test_type::TestType,
};

/// Test successful getpeerinfo rpc call
///
/// See [`common::get_block_template_rpcs::get_peer_info`] for more information.
#[tokio::test]
async fn get_peer_info() -> Result<()> {
    get_block_template_rpcs::get_peer_info::run().await
}

/// Test successful getblocktemplate rpc call
///
/// See [`common::get_block_template_rpcs::get_block_template`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_get_block_template() -> Result<()> {
    get_block_template_rpcs::get_block_template::run().await
}

/// Test successful submitblock rpc call
///
/// See [`common::get_block_template_rpcs::submit_block`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_submit_block() -> Result<()> {
    get_block_template_rpcs::submit_block::run().await
}

/// Snapshot the `z_getsubtreesbyindex` method in a synchronized chain.
///
/// This test name must have the same prefix as the `lwd_rpc_test`, so they can be run in the same test job.
#[tokio::test]
#[ignore]
async fn fully_synced_rpc_z_getsubtreesbyindex_snapshot_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network,
        "rpc_z_getsubtreesbyindex_sync_snapshots",
        test_type,
        true,
    )? {
        tracing::info!("running fully synced zebrad z_getsubtreesbyindex RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    // It doesn't matter how long the state version upgrade takes,
    // because the sync finished regex is repeated every minute.
    wait_for_state_version_upgrade(
        &mut zebrad,
        &state_version_message,
        state_database_format_version_in_code(),
        None,
    )?;

    // Wait for zebrad to load the full cached blockchain.
    zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

    // Create an http client
    let client =
        RpcRequestClient::new(zebra_rpc_address.expect("already checked that address is valid"));

    // Create test vector matrix
    let zcashd_test_vectors = vec![
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_1".to_string(),
            r#"["sapling", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_11".to_string(),
            r#"["sapling", 0, 11]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_17_1".to_string(),
            r#"["sapling", 17, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_1090_6".to_string(),
            r#"["sapling", 1090, 6]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_0_1".to_string(),
            r#"["orchard", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_338_1".to_string(),
            r#"["orchard", 338, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_585_1".to_string(),
            r#"["orchard", 585, 1]"#.to_string(),
        ),
    ];

    for i in zcashd_test_vectors {
        let res = client.call("z_getsubtreesbyindex", i.1).await?;
        let body = res.bytes().await;
        let parsed: Value = serde_json::from_slice(&body.expect("Response is valid json"))?;
        insta::assert_json_snapshot!(i.0, parsed);
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Checks that the cached finalized state has the spending transaction ids for every
/// spent outpoint and revealed nullifier in the last 100 blocks of a cached state.
//
// Note: This test is meant to be run locally with a prepared finalized state that
//       has spending transaction ids. This can be done by starting Zebra with the
//       `indexer` feature and waiting until the db format upgrade is complete. It
//       can be undone (removing the indexes) by starting Zebra without the feature
//       and waiting until the db format downgrade is complete.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
#[cfg(feature = "indexer")]
async fn has_spending_transaction_ids() -> Result<()> {
    use std::sync::Arc;
    use tower::Service;
    use zebra_chain::{chain_tip::ChainTip, transparent::Input};
    use zebra_state::{ReadRequest, ReadResponse, SemanticallyVerifiedBlock, Spend};

    use common::cached_state::future_blocks;

    let _init_guard = zebra_test::init();
    let test_type = UpdateZebraCachedStateWithRpc;
    let test_name = "has_spending_transaction_ids_test";
    let network = Mainnet;

    let Some(zebrad_state_path) = test_type.zebrad_state_path(test_name) else {
        // Skip test if there's no cached state.
        return Ok(());
    };

    tracing::info!("loading blocks for non-finalized state");

    let non_finalized_blocks = future_blocks(&network, test_type, test_name, 100).await?;

    let (mut state, mut read_state, latest_chain_tip, _chain_tip_change) =
        common::cached_state::start_state_service_with_cache_dir(&Mainnet, zebrad_state_path)
            .await?;

    tracing::info!("committing blocks to non-finalized state");

    for block in non_finalized_blocks {
        use zebra_state::{CommitSemanticallyVerifiedBlockRequest, MappedRequest};

        let expected_hash = block.hash();
        let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), expected_hash);
        let block_hash = CommitSemanticallyVerifiedBlockRequest(block)
            .mapped_oneshot(&mut state)
            .await
            .map_err(|err| eyre!(err))?;

        assert_eq!(
            expected_hash, block_hash,
            "state should respond with expected block hash"
        );
    }

    let mut tip_hash = latest_chain_tip
        .best_tip_hash()
        .expect("cached state must not be empty");

    tracing::info!("checking indexes of spending transaction ids");

    // Read the last 500 blocks - should be greater than the MAX_BLOCK_REORG_HEIGHT so that
    // both the finalized and non-finalized state are checked.
    let num_blocks_to_check = 500;
    let mut is_failure = false;
    for i in 0..num_blocks_to_check {
        let ReadResponse::Block(block) = read_state
            .ready()
            .await
            .map_err(|err| eyre!(err))?
            .call(ReadRequest::Block(tip_hash.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            panic!("unexpected response to Block request");
        };

        let block = block.expect("should have block with latest_chain_tip hash");

        let spends_with_spending_tx_hashes = block.transactions.iter().cloned().flat_map(|tx| {
            let tx_hash = tx.hash();
            tx.inputs()
                .iter()
                .filter_map(Input::outpoint)
                .map(Spend::from)
                .chain(tx.sprout_nullifiers().cloned().map(Spend::from))
                .chain(tx.sapling_nullifiers().cloned().map(Spend::from))
                .chain(tx.orchard_nullifiers().cloned().map(Spend::from))
                .map(|spend| (spend, tx_hash))
                .collect::<Vec<_>>()
        });

        for (spend, expected_transaction_hash) in spends_with_spending_tx_hashes {
            let ReadResponse::TransactionId(transaction_hash) = read_state
                .ready()
                .await
                .map_err(|err| eyre!(err))?
                .call(ReadRequest::SpendingTransactionId(spend))
                .await
                .map_err(|err| eyre!(err))?
            else {
                panic!("unexpected response to Block request");
            };

            let Some(transaction_hash) = transaction_hash else {
                tracing::warn!(
                    ?spend,
                    depth = i,
                    height = ?block.coinbase_height(),
                    "querying spending tx id for spend failed"
                );
                is_failure = true;
                continue;
            };

            assert_eq!(
                transaction_hash, expected_transaction_hash,
                "spending transaction hash should match expected transaction hash"
            );
        }

        if i % 25 == 0 {
            tracing::info!(
                height = ?block.coinbase_height(),
                "has all spending tx ids at and above block"
            );
        }

        tip_hash = block.header.previous_block_hash;
    }

    assert!(
        !is_failure,
        "at least one spend was missing a spending transaction id"
    );

    Ok(())
}
