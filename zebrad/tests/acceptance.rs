//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! ## Note on port conflict
//!
//! If the test child has a cache or port conflict with another test, or a
//! running zebrad or zcashd, then it will panic. But the acceptance tests
//! expect it to run until it is killed.
//!
//! If these conflicts cause test failures:
//!   - run the tests in an isolated environment,
//!   - run zebrad on a custom cache path and port,
//!   - run zcashd on a custom port.
//!
//! ## Failures due to Configured Network Interfaces or Network Connectivity
//!
//! If your test environment does not have any IPv6 interfaces configured, skip IPv6 tests
//! by setting the `SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, IPv4 localhost is not on `127.0.0.1`,
//! or you have poor network connectivity,
//! skip all the network tests by setting the `SKIP_NETWORK_TESTS` environmental variable.
//!
//! ## Large/full sync tests
//!
//! This file has sync tests that are marked as ignored because they take too much time to run.
//! Some of them require environment variables or directories to be present:
//!
//! - `SYNC_FULL_MAINNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//!   will allow this test to run or give up. Value for the Mainnet full sync tests.
//! - `SYNC_FULL_TESTNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//!   will allow this test to run or give up. Value for the Testnet full sync tests.
//! - A zebrad state cache directory is required for some tests, either at the default state cache
//!   directory path, or at the path defined in the `ZEBRA_STATE__CACHE_DIR` env variable.
//!   For some sync tests, this directory needs to be created in the file system
//!   with write permissions.
//!
//! Here are some examples on how to run each of the tests using nextest profiles:
//!
//! ```console
//! $ cargo nextest run --profile sync-large-checkpoints-empty
//!
//! $ ZEBRA_STATE__CACHE_DIR="/zebrad-cache" cargo nextest run --profile sync-full-mainnet
//!
//! $ ZEBRA_STATE__CACHE_DIR="/zebrad-cache" cargo nextest run --profile sync-full-testnet
//! ```
//!
//! For tests that require a cache directory, you may need to create it first:
//! ```console
//! $ export ZEBRA_STATE__CACHE_DIR="/zebrad-cache"
//! $ sudo mkdir -p "$ZEBRA_STATE__CACHE_DIR"
//! $ sudo chmod 777 "$ZEBRA_STATE__CACHE_DIR"
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Lightwalletd tests
//!
//! The lightwalletd software is an interface service that uses zebrad or zcashd RPC methods to serve wallets or other applications with blockchain content in an efficient manner.
//!
//! Zebra's lightwalletd tests are executed using nextest profiles.
//! Some tests require environment variables to be set:
//!
//! - `TEST_LIGHTWALLETD`: Must be set to run any of the lightwalletd tests.
//! - `ZEBRA_STATE__CACHE_DIR`: The path to a Zebra cached state directory.
//! - `LWD_CACHE_DIR`: The path to a lightwalletd database.
//!
//! Here are some examples of running each test:
//!
//! ```console
//! # Run the lightwalletd integration test
//! $ TEST_LIGHTWALLETD=1 cargo nextest run --profile lwd-integration
//!
//! # Run the lightwalletd update sync test
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-sync-update
//!
//! # Run the lightwalletd full sync test
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" cargo nextest run --profile lwd-sync-full
//!
//! # Run the lightwalletd gRPC wallet test (requires --features lightwalletd-grpc-tests)
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-grpc-wallet --features lightwalletd-grpc-tests
//!
//! # Run the lightwalletd send transaction test (requires --features lightwalletd-grpc-tests)
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-rpc-send-tx --features lightwalletd-grpc-tests
//! ```
//!
//! ## Getblocktemplate tests
//!
//! Example of how to run the rpc_get_block_template test:
//!
//! ```console
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile rpc-get-block-template
//! ```
//!
//! Example of how to run the rpc_submit_block test:
//!
//! ```console
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile rpc-submit-block
//! ```
//!
//! Example of how to run the has_spending_transaction_ids test (requires indexer feature):
//!
//! ```console
//! RUST_LOG=info ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile indexer-has-spending-transaction-ids --features "indexer"
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Checkpoint Generation Tests
//!
//! Generate checkpoints on mainnet and testnet using a cached state:
//! ```console
//! # Generate checkpoints for mainnet:
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile generate-checkpoints-mainnet
//!
//! # Generate checkpoints for testnet:
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile generate-checkpoints-testnet
//! ```
//!
//! You can also use the entrypoint script directly:
//! ```console
//! FEATURES=zebra-checkpoints ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state docker/entrypoint.sh
//! ```
//!
//! ## Disk Space for Testing
//!
//! The full sync and lightwalletd tests with cached state expect a temporary directory with
//! at least 300 GB of disk space (2 copies of the full chain). To use another disk for the
//! temporary test files:
//!
//! ```sh
//! export TMPDIR=/path/to/disk/directory
//! ```

#![allow(clippy::unwrap_in_result)]
#![allow(unused_imports)]

mod common;

use std::{
    cmp::Ordering,
    collections::HashSet,
    env, fs, panic,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::{
    eyre::{eyre, WrapErr},
    Help,
};
use futures::{stream::FuturesUnordered, StreamExt};
use semver::Version;
use serde_json::Value;
use tower::ServiceExt;

use zcash_keys::address::Address;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network::{self, *},
        NetworkUpgrade,
    },
    serialization::BytesInDisplayOrder,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{
        BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateRequestMode,
        GetBlockTemplateResponse, SubmitBlockErrorResponse, SubmitBlockResponse,
    },
    fetch_state_tip_and_local_time, generate_coinbase_and_roots,
    methods::{RpcImpl, RpcServer},
    proposal_block_from_template,
    server::OPENED_RPC_ENDPOINT_MSG,
    SubmitBlockChannel,
};
use zebra_state::{constants::LOCK_FILE_ERROR, state_database_format_version_in_code};
use zebra_test::{
    args,
    command::{to_regex::CollectRegexSet, ContextFrom},
    net::random_known_port,
    prelude::*,
};

#[cfg(not(target_os = "windows"))]
use zebra_network::constants::PORT_IN_USE_ERROR;

use common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    check::{is_zebrad_version, EphemeralCheck, EphemeralConfig},
    config::{
        config_file_full_path, configs_dir, default_test_config, external_address_test_config,
        os_assigned_rpc_port_config, persistent_test_config, random_known_rpc_port_config,
        read_listen_addr_from_logs, testdir,
    },
    launch::{
        spawn_zebrad_for_rpc, spawn_zebrad_without_rpc, ZebradTestDirExt, BETWEEN_NODES_DELAY,
        EXTENDED_LAUNCH_DELAY, LAUNCH_DELAY,
    },
    lightwalletd::{can_spawn_lightwalletd_for_rpc, spawn_lightwalletd_for_rpc},
    sync::{
        create_cached_database_height, sync_until, MempoolBehavior, LARGE_CHECKPOINT_TEST_HEIGHT,
        LARGE_CHECKPOINT_TIMEOUT, MEDIUM_CHECKPOINT_TEST_HEIGHT, STOP_AT_HEIGHT_REGEX,
        STOP_ON_LOAD_TIMEOUT, SYNC_FINISHED_REGEX, TINY_CHECKPOINT_TEST_HEIGHT,
        TINY_CHECKPOINT_TIMEOUT,
    },
    test_type::TestType::{self, *},
};

use crate::common::regtest::MiningRpcMethods;

/// === Unit ===
///
/// CLI Tests:
/// - cli.rs:
///     - generate_no_args
///     - generate_args
///     - help_no_args
///     - help_args
///     - start_no_args
///     - start_args
///     - version_no_args
///     - version_args
///
/// Config Tests:
/// - config.rs:
///     - config_tests
///         - valid_generated_config
///         - invalid_generated_config
///         - last_config_is_stored
///         - stored_configs_work
///         - app_no_args
///     - stored_configs_parsed_correctly
/// Ephemeral tests:
/// - ephemeral.rs:
///     - ephemeral_existing_directory
///     - ephemeral_missing_directory
///     - misconfigured_ephemeral_existing_directory
///     - misconfigured_ephemeral_missing_directory
///     - ephemeral (helper function)
///     - persistent_mode
///
/// === Integration ===
/// Conflict tests:
/// - conflict.rs:
///     - zebra_zcash_listener_conflict
///     - zebra_metrics_conflict
///     - zebra_tracing_conflict
///     - zebra_rpc_conflict
///     - zebra_state_conflict
///     - check_config_conflict (helper)
///     - delete_old_databases
///     - end_of_support_is_checked_at_start
///     - db_init_outside_future_executor
///     - non_blocking_logger
/// Endpoint tests:
/// - endpoint.rs:
///     - metrics_endpoint
///     - tracing_endpoint
///     - rpc_endpoint_single_thread
///     - rpc_endpoint_parallel_threads
///     - rpc_endpoint (helper)
///     - rpc_endpoint_client_content_type
/// Synchronization tests:
/// - sync.rs:
///     - sync_one_checkpoint_mainnet
///     - sync_one_checkpoint_testnet (disabled)
///     - restart_stop_at_height
///     - restart_stop_at_height_for_network (helper)
///     - activate_mempool_mainnet
///     - sync_large_checkpoints_empty (ignored by default)
///     - sync_large_checkpoints_mempool_mainnet (ignored by default)
///
/// === Stateful ===
/// Checkpoint sync tests:
/// - stateful.rs:
///     - sync_to_mandatory_checkpoint_mainnet
///     - sync_to_mandatory_checkpoint_testnet
///     - sync_to_mandatory_checkpoint_for_network (helper)
///     - create_cached_database (helper)
///     - sync_past_mandatory_checkpoint_mainnet
///     - sync_past_mandatory_checkpoint_testnet
///     - sync_past_mandatory_checkpoint (helper)
/// Lightwallet tests:
/// - lightwallet.rs:
///     - lwd_integration
///     - sync_update_mainnet
///     - lwd_rpc_test
///     - lwd_sync_update
///     - lwd_rpc_send_tx
///     - lwd_grpc_wallet
///     - lightwalletd_test_suite
///     - lwd_integration_test (helper)
/// Mining-related RPC tests:
/// - mining_rpcs.rs:
///     - get_peer_info
///     - rpc_get_block_template
///     - rpc_submit_block
///     - fully_synced_rpc_z_getsubtreesbyindex_snapshot_test
///     - has_spending_transaction_ids
/// Tests for database state format versions:
/// - state_format.rs:
///     - new_state_format
///     - update_state_format
///     - downgrade_state_format
///     - state_format_test
///
/// === E2E ===
/// Full-Sync Tests:
///     - full_sync.rs
///     - sync_full_mainnet
///     - sync_full_testnet
///     - full_sync_test (helper function)
///     - generate_checkpoints_mainnet
///     - generate_checkpoints_testnet
/// Lightwalletd RPC Tests:
/// - wallet_integration.rs
///     - lwd_sync_full
///     - lwd_integration_test (helper function)
///
/// Checks that the Regtest genesis block can be validated.
#[tokio::test]
async fn validate_regtest_genesis_block() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Default::default());
    let state = zebra_state::init_test(&network).await;
    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(zebra_consensus::Config::default(), &network, state)
        .await;

    let genesis_hash = block_verifier_router
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    assert_eq!(
        genesis_hash,
        network.genesis_hash(),
        "validated block hash should match network genesis hash"
    )
}

/// Test that Version messages are sent with the external address when configured to do so.
#[test]
fn external_address() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut external_address_test_config(&Mainnet)?)?;
    let mut child = testdir.spawn_child(args!["start"])?;

    // Give enough time to start connecting to some peers.
    std::thread::sleep(Duration::from_secs(10));

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Zebra started
    output.stdout_line_contains("Starting zebrad")?;

    // Make sure we are using external address for Version messages.
    output.stdout_line_contains("using external address for Version messages")?;

    // Make sure the command was killed.
    output.assert_was_killed()?;

    Ok(())
}

/// Test successful `getblocktemplate` and `submitblock` RPC calls on Regtest on Canopy.
///
/// See [`common::regtest::submit_blocks`] for more information.
// TODO: Test this with an NU5 activation height too once config can be serialized.
#[tokio::test]
async fn regtest_block_templates_are_valid_block_submissions() -> Result<()> {
    common::regtest::submit_blocks_test().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_chain_sync_handles_forks_correctly() -> Result<()> {
    use std::sync::Arc;

    use eyre::Error;
    use tokio::time::timeout;
    use zebra_chain::{chain_tip::ChainTip, primitives::byte_array::increment_big_endian};
    use zebra_rpc::methods::GetBlockHashResponse;
    use zebra_state::{ReadResponse, Response};

    use common::regtest::MiningRpcMethods;

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
        let tip_action = timeout(
            Duration::from_secs(1),
            chain_tip_change.wait_for_tip_change(),
        )
        .await??;

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
            | NetworkUpgrade::Nu7 => {
                ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&hist_root, &auth_root)
                    .bytes_in_serialized_order()
            }
            _ => Err(eyre!(
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
    let tip_action = timeout(
        Duration::from_secs(1),
        chain_tip_change.wait_for_tip_change(),
    )
    .await??;
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

/// Test successful block template submission as a block proposal or submission on a custom Testnet.
///
/// This test can be run locally with:
/// `cargo test --package zebrad --test acceptance -- nu6_funding_streams_and_coinbase_balance --exact --show-output`
#[tokio::test(flavor = "multi_thread")]
async fn nu6_funding_streams_and_coinbase_balance() -> Result<()> {
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        parameters::{
            subsidy::{FundingStreamReceiver, FUNDING_STREAM_MG_ADDRESSES_TESTNET},
            testnet::{
                self, ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
                ConfiguredFundingStreams,
            },
        },
        serialization::ZcashSerialize,
        work::difficulty::U256,
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::client::HexData;
    use zebra_test::mock_service::MockService;
    let _init_guard = zebra_test::init();

    tracing::info!("running nu6_funding_streams_and_coinbase_balance test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .with_checkpoints(false)
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(1),
            ..Default::default()
        });

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network();

    tracing::info!("built configured Testnet, starting state service and block verifier");

    let default_test_config = default_test_config(&network)?;
    let mining_config = default_test_config.mining;
    let miner_address = Address::try_from_zcash_address(
        &network,
        mining_config
            .miner_address
            .clone()
            .expect("mining address should be configured"),
    )
    .expect("configured mining address should be valid");

    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(&network).await;

    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(
        zebra_consensus::Config::default(),
        &network,
        state.clone(),
    )
    .await;

    tracing::info!("started state service and block verifier, committing Regtest genesis block");

    let genesis_hash = block_verifier_router
        .clone()
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    let mut mempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let submitblock_channel = SubmitBlockChannel::new();

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        mining_config,
        false,
        "0.0.1",
        "Zebra tests",
        mempool.clone(),
        state.clone(),
        read_state.clone(),
        block_verifier_router,
        mock_sync_status,
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        Some(submitblock_channel.sender()),
    );

    let make_mock_mempool_request_handler = || async move {
        mempool
            .expect_request(mempool::Request::FullTransactions)
            .await
            .respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                // tip hash needs to match chain info for long poll requests
                last_seen_tip_hash: genesis_hash,
            });
    };

    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;
    let hex_proposal_block = HexData(proposal_block.zcash_serialize_to_vec()?);

    // Check that the block template is a valid block proposal
    let GetBlockTemplateResponse::ProposalMode(block_proposal_result) = rpc
        .get_block_template(Some(GetBlockTemplateParameters::new(
            GetBlockTemplateRequestMode::Proposal,
            Some(hex_proposal_block),
            Default::default(),
            Default::default(),
            Default::default(),
        )))
        .await?
    else {
        panic!(
            "this getblocktemplate call should return the `ProposalMode` variant of the response"
        )
    };

    assert!(
        block_proposal_result.is_valid(),
        "block proposal should succeed"
    );

    // Submit the same block
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    // Check that the submitblock channel received the submitted block
    let mut submit_block_receiver = submitblock_channel.receiver();
    let submit_block_channel_data = submit_block_receiver.recv().await.expect("channel is open");
    assert_eq!(
        submit_block_channel_data,
        (
            proposal_block.hash(),
            proposal_block.coinbase_height().unwrap()
        ),
        "submitblock channel should receive the submitted block"
    );

    // Use an invalid coinbase transaction (with an output value greater than the `block_subsidy + miner_fees - expected_lockbox_funding_stream`)

    let make_configured_recipients_with_lockbox_numerator = |numerator| {
        Some(vec![
            ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Deferred,
                numerator,
                addresses: None,
            },
            ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::MajorGrants,
                numerator: 8,
                addresses: Some(
                    FUNDING_STREAM_MG_ADDRESSES_TESTNET
                        .map(ToString::to_string)
                        .to_vec(),
                ),
            },
        ])
    };

    // Gets the next block template
    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let valid_original_block_template = block_template.clone();

    let zebra_state::GetBlockTemplateChainInfo {
        chain_history_root, ..
    } = fetch_state_tip_and_local_time(read_state.clone()).await?;

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: make_configured_recipients_with_lockbox_numerator(0),
        }])
        .to_network();

    let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
        &network,
        Height(block_template.height()),
        &miner_address,
        &[],
        chain_history_root,
        vec![],
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        None,
    )
    .expect("coinbase transaction should be valid under the given parameters");

    let block_template = BlockTemplateResponse::new(
        block_template.capabilities().clone(),
        block_template.version(),
        block_template.previous_block_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots,
        block_template.transactions().clone(),
        coinbase_txn,
        block_template.long_poll_id(),
        block_template.target(),
        block_template.min_time(),
        block_template.mutable().clone(),
        block_template.nonce_range().clone(),
        block_template.sigop_limit(),
        block_template.size_limit(),
        block_template.cur_time(),
        block_template.bits(),
        block_template.height(),
        block_template.max_time(),
        block_template.submit_old(),
    );

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;

    // Submit the invalid block with an excessive coinbase output value
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    tracing::info!(?submit_block_response, "submitted invalid block");

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::ErrorResponse(SubmitBlockErrorResponse::Rejected),
        "invalid block with excessive coinbase output value should be rejected"
    );

    // Use an invalid coinbase transaction (with an output value less than the `block_subsidy + miner_fees - expected_lockbox_funding_stream`)
    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: make_configured_recipients_with_lockbox_numerator(20),
        }])
        .to_network();

    let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
        &network,
        Height(block_template.height()),
        &miner_address,
        &[],
        chain_history_root,
        vec![],
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        None,
    )
    .expect("coinbase transaction should be valid under the given parameters");

    let block_template = BlockTemplateResponse::new(
        block_template.capabilities().clone(),
        block_template.version(),
        block_template.previous_block_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots,
        block_template.transactions().clone(),
        coinbase_txn,
        block_template.long_poll_id(),
        block_template.target(),
        block_template.min_time(),
        block_template.mutable().clone(),
        block_template.nonce_range().clone(),
        block_template.sigop_limit(),
        block_template.size_limit(),
        block_template.cur_time(),
        block_template.bits(),
        block_template.height(),
        block_template.max_time(),
        block_template.submit_old(),
    );

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;

    // Submit the invalid block with an excessive coinbase input value
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    tracing::info!(?submit_block_response, "submitted invalid block");

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::ErrorResponse(SubmitBlockErrorResponse::Rejected),
        "invalid block with insufficient coinbase output value should be rejected"
    );

    // Check that the original block template can be submitted successfully
    let proposal_block =
        proposal_block_from_template(&valid_original_block_template, None, &network)?;

    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    Ok(())
}

/// Test successful block template submission as a block proposal.
///
/// This test can be run locally with:
/// `RUSTFLAGS='--cfg zcash_unstable="nu7"' cargo test --package zebrad --test acceptance --features tx_v6 -- nu7_nsm_transactions --exact --show-output`
#[tokio::test(flavor = "multi_thread")]
#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
async fn nu7_nsm_transactions() -> Result<()> {
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        parameters::testnet::{self, ConfiguredActivationHeights, ConfiguredFundingStreams},
        serialization::ZcashSerialize,
        work::difficulty::U256,
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::client::HexData;
    use zebra_test::mock_service::MockService;
    let _init_guard = zebra_test::init();

    tracing::info!("running nu7_nsm_transactions test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .with_checkpoints(false)
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_lockbox_disbursements(vec![])
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        });

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network();

    tracing::info!("built configured Testnet, starting state service and block verifier");

    let default_test_config = default_test_config(&network)?;
    let mining_config = default_test_config.mining;

    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(&network).await;

    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(
        zebra_consensus::Config::default(),
        &network,
        state.clone(),
    )
    .await;

    tracing::info!("started state service and block verifier, committing Regtest genesis block");

    let genesis_hash = block_verifier_router
        .clone()
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    let mut mempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let submitblock_channel = SubmitBlockChannel::new();

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        mining_config,
        false,
        "0.0.1",
        "Zebra tests",
        mempool.clone(),
        state.clone(),
        read_state.clone(),
        block_verifier_router,
        mock_sync_status,
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        Some(submitblock_channel.sender()),
    );

    let make_mock_mempool_request_handler = || async move {
        mempool
            .expect_request(mempool::Request::FullTransactions)
            .await
            .respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                // tip hash needs to match chain info for long poll requests
                last_seen_tip_hash: genesis_hash,
            });
    };

    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!("this getblocktemplate call without parameters should return the `TemplateMode` variant of the response")
    };

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;
    let hex_proposal_block = HexData(proposal_block.zcash_serialize_to_vec()?);

    // Check that the block template is a valid block proposal
    let GetBlockTemplateResponse::ProposalMode(block_proposal_result) = rpc
        .get_block_template(Some(GetBlockTemplateParameters::new(
            GetBlockTemplateRequestMode::Proposal,
            Some(hex_proposal_block),
            Default::default(),
            Default::default(),
            Default::default(),
        )))
        .await?
    else {
        panic!(
            "this getblocktemplate call should return the `ProposalMode` variant of the response"
        )
    };

    assert!(
        block_proposal_result.is_valid(),
        "block proposal should succeed"
    );

    // Submit the same block
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_and_reconsider_block() -> Result<()> {
    use std::sync::Arc;

    use common::regtest::MiningRpcMethods;

    let _init_guard = zebra_test::init();
    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu7: Some(100),
            ..Default::default()
        }
        .into(),
    );
    let mut config = os_assigned_rpc_port_config(false, &net)?;
    config.state.ephemeral = false;

    let test_dir = testdir()?.with_config(&mut config)?;

    let mut child = test_dir.spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    let rpc_client = RpcRequestClient::new(rpc_address);
    let mut blocks = Vec::new();
    for _ in 0..50 {
        let (block, _) = rpc_client.block_from_template(&net).await?;

        rpc_client.submit_block(block.clone()).await?;
        blocks.push(block);
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
    }

    tracing::info!("invalidating blocks");

    // Note: This is the block at height 7, it's the 6th generated block.
    let block_6_hash = blocks
        .get(5)
        .expect("should have 50 blocks")
        .hash()
        .to_string();
    let params = serde_json::to_string(&vec![block_6_hash]).expect("should serialize successfully");

    let _: () = rpc_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|err| eyre!(err))?;

    let expected_reconsidered_hashes = blocks
        .iter()
        .skip(5)
        .map(|block| block.hash())
        .collect::<Vec<_>>();

    tracing::info!("reconsidering blocks");

    let reconsidered_hashes: Vec<block::Hash> = rpc_client
        .json_result_from_call("reconsiderblock", &params)
        .await
        .map_err(|err| eyre!(err))?;

    assert_eq!(
        reconsidered_hashes, expected_reconsidered_hashes,
        "reconsidered hashes should match expected hashes"
    );

    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    Ok(())
}

/// Check that Zebra does not depend on any crates from git sources.
#[test]
#[ignore]
fn check_no_git_dependencies() {
    let cargo_lock_contents =
        fs::read_to_string("../Cargo.lock").expect("should have Cargo.lock file in root dir");

    if cargo_lock_contents.contains(r#"source = "git+"#) {
        panic!("Cargo.lock includes git sources")
    }
}

#[tokio::test]
async fn restores_non_finalized_state_and_commits_new_blocks() -> Result<()> {
    let network = Network::new_regtest(Default::default());

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.state.ephemeral = false;
    let test_dir = testdir()?.with_config(&mut config)?;

    // Start Zebra and generate some blocks.

    tracing::info!("starting Zebra and generating some blocks");
    let mut child = test_dir.spawn_child(args!["start"])?;
    // Avoid dropping the test directory and cleanup of the state cache needed by the next zebrad instance.
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rpc_client = RpcRequestClient::new(rpc_address);
    let generated_block_hashes = rpc_client.generate(50).await?;
    // Wait for non-finalized backup task to make a second write to the backup cache
    tokio::time::sleep(Duration::from_secs(6)).await;

    child.kill(true)?;
    // Wait for Zebra to shut down.
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Prepare checkpoint heights/hashes
    let last_hash = *generated_block_hashes
        .last()
        .expect("should have at least one block hash");
    let configured_checkpoints = ConfiguredCheckpoints::HeightsAndHashes(vec![
        (Height(0), network.genesis_hash()),
        (Height(50), last_hash),
    ]);

    // Check that Zebra will restore its non-finalized state from backup when the finalized tip is past the
    // max checkpoint height and that it can still commit more blocks to its state.

    tracing::info!("restarting Zebra to check that non-finalized state is restored");
    let mut child = test_dir.spawn_child(args!["start"])?;
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let rpc_client = RpcRequestClient::new(rpc_address);

    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let blockchain_info = rpc_client.blockchain_info().await?;
    tracing::info!(
        ?blockchain_info,
        "got blockchain info after restarting Zebra"
    );

    assert_eq!(
        blockchain_info.best_block_hash(),
        last_hash,
        "tip block hash should match tip hash of previous zebrad instance"
    );

    tracing::info!("checking that Zebra can commit blocks after restoring non-finalized state");
    rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");

    tracing::info!("retrieving blocks to be used with configured checkpoints");
    let checkpointed_blocks = {
        let mut blocks = Vec::new();
        for height in 1..=50 {
            blocks.push(
                rpc_client
                    .get_block(height)
                    .await
                    .map_err(|err| eyre!(err))?
                    .expect("should have block at height"),
            )
        }
        blocks
    };

    tracing::info!(
        "restarting Zebra to check that non-finalized state is _not_ restored when \
         the finalized tip is below the max checkpoint height"
    );
    child.kill(true)?;
    // Wait for Zebra to shut down.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check that the non-finalized state is not restored from backup when the finalized tip height is below the
    // max checkpoint height and that it can still commit more blocks to its state

    tracing::info!("restarting Zebra with configured checkpoints to check that non-finalized state is not restored");
    let network = Network::new_regtest(RegtestParameters {
        checkpoints: Some(configured_checkpoints),
        ..Default::default()
    });
    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.state.ephemeral = false;
    let mut child = test_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rpc_client = RpcRequestClient::new(rpc_address);

    assert_eq!(
        rpc_client.blockchain_info().await?.best_block_hash(),
        network.genesis_hash(),
        "Zebra should not restore blocks from non-finalized backup if \
         its finalized tip is below the max checkpoint height"
    );

    let mut submit_block_futs: FuturesUnordered<_> = checkpointed_blocks
        .into_iter()
        .map(Arc::unwrap_or_clone)
        .map(|block| rpc_client.submit_block(block))
        .collect();

    while let Some(result) = submit_block_futs.next().await {
        result?
    }

    // Commit some blocks to check that Zebra's state will still commit blocks, and generate enough blocks
    // for Zebra's finalized tip to pass the max checkpoint height.

    rpc_client
        .generate(200)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)?;

    // Check that Zebra will can commit blocks to its state when its finalized tip is past the max checkpoint height
    // and the non-finalized backup cache is disabled or empty.

    tracing::info!(
        "restarting Zebra to check that blocks are committed when the non-finalized state \
         is initially empty and the finalized tip is past the max checkpoint height"
    );
    config.state.should_backup_non_finalized_state = false;
    let mut child = test_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let rpc_client = RpcRequestClient::new(rpc_address);

    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;

    tracing::info!("checking that Zebra commits blocks with empty non-finalized state");
    rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)
}

/// Check that Zebra will disconnect from misbehaving peers.
///
/// In order to simulate a misbehaviour peer we start two zebrad instances:
/// - The first one is started with a custom Testnet where PoW is disabled.
/// - The second one is started with the default Testnet where PoW is enabled.
/// The second zebrad instance will connect to the first one, and when the first one mines
/// blocks with invalid PoW the second one should disconnect from it.
#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn disconnects_from_misbehaving_peers() -> Result<()> {
    use std::sync::{atomic::AtomicBool, Arc};

    use common::regtest::MiningRpcMethods;
    use zebra_chain::parameters::testnet::{self, ConfiguredActivationHeights};
    use zebra_rpc::client::PeerInfo;

    let _init_guard = zebra_test::init();
    let network1 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .with_slow_start_interval(Height::MIN)
        .with_disable_pow(true)
        .clear_checkpoints()
        .with_network_name("PoWDisabledTestnet")
        .to_network();

    let test_type = LaunchWithEmptyState {
        launches_lightwalletd: false,
    };
    let test_name = "disconnects_from_misbehaving_peers_test";

    if !common::launch::can_spawn_zebrad_for_test_type(test_name, test_type, false) {
        tracing::warn!("skipping disconnects_from_misbehaving_peers test");
        return Ok(());
    }

    // Get the zebrad config
    let mut config = test_type
        .zebrad_config(test_name, false, None, &network1)
        .expect("already checked config")?;

    config.network.cache_dir = false.into();
    config.network.listen_addr = format!("127.0.0.1:{}", random_known_port()).parse()?;
    config.state.ephemeral = true;
    config.network.initial_testnet_peers = [].into();
    config.network.crawl_new_peer_interval = Duration::from_secs(5);

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_1 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on incompatible custom Testnet"
    );

    let is_finished = Arc::new(AtomicBool::new(false));

    {
        let is_finished = Arc::clone(&is_finished);
        let config = config.clone();
        let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraA1".to_string()));
            }

            Ok(())
        });
    }

    let network2 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .with_slow_start_interval(Height::MIN)
        .clear_checkpoints()
        .with_network_name("PoWEnabledTestnet")
        .to_network();

    config.network.network = network2;
    config.network.initial_testnet_peers = [config.network.listen_addr.to_string()].into();
    config.network.listen_addr = "127.0.0.1:0".parse()?;
    config.rpc.listen_addr = Some(format!("127.0.0.1:{}", random_known_port()).parse()?);
    config.network.crawl_new_peer_interval = Duration::from_secs(5);
    config.network.cache_dir = false.into();
    config.state.ephemeral = true;

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_2 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on the default Testnet"
    );

    {
        let is_finished = Arc::clone(&is_finished);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let (zebrad_failure_messages, zebrad_ignore_messages) =
                test_type.zebrad_failure_messages();
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraB2".to_string()));
            }

            Ok(())
        });
    }

    tracing::info!("waiting for zebrad nodes to connect");

    // Wait a few seconds for Zebra to start up and make outbound peer connections
    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("checking for peers");

    // Call `getpeerinfo` to check that the zebrad instances have connected
    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    assert!(!peer_info.is_empty(), "should have outbound peer");

    tracing::info!(
        ?peer_info,
        "found peer connection, committing genesis block"
    );

    let genesis_block = network1.block_parsed_iter().next().unwrap();
    rpc_client_1.submit_block(genesis_block.clone()).await?;
    rpc_client_2.submit_block(genesis_block).await?;

    // Call the `generate` method to mine blocks in the zebrad instance where PoW is disabled
    tracing::info!("committed genesis block, mining blocks with invalid PoW");
    tokio::time::sleep(Duration::from_secs(2)).await;

    rpc_client_1.call("generate", "[500]").await?;

    tracing::info!("wait for misbehavior messages to flush into address updater channel");

    tokio::time::sleep(Duration::from_secs(30)).await;

    tracing::info!("calling getpeerinfo to confirm Zebra has dropped the peer connection");

    // Call `getpeerinfo` to check that the zebrad instances have disconnected
    for i in 0..600 {
        let peer_info: Vec<PeerInfo> = rpc_client_2
            .json_result_from_call("getpeerinfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        if peer_info.is_empty() {
            break;
        } else if i % 10 == 0 {
            tracing::info!(?peer_info, "has not yet disconnected from misbehaving peer");
        }

        rpc_client_1.call("generate", "[1]").await?;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    tracing::info!(?peer_info, "called getpeerinfo");

    assert!(peer_info.is_empty(), "should have no peers");

    is_finished.store(true, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}
