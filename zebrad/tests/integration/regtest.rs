use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Result};
use tower::ServiceExt;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    serialization::BytesInDisplayOrder,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{
    config::{
        default_test_config, os_assigned_rpc_port_config, random_known_rpc_port_config,
        read_listen_addr_from_logs, testdir,
    },
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

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

/// Test successful `getblocktemplate` and `submitblock` RPC calls on Regtest on Canopy.
///
/// See [`crate::common::regtest::submit_blocks`] for more information.
// TODO: Test this with an NU5 activation height too once config can be serialized.
#[tokio::test]
async fn regtest_block_templates_are_valid_block_submissions() -> Result<()> {
    crate::common::regtest::submit_blocks_test().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_chain_sync_handles_forks_correctly() -> Result<()> {
    use eyre::Error;
    use tokio::time::timeout;
    use zebra_chain::{chain_tip::ChainTip, primitives::byte_array::increment_big_endian};
    use zebra_rpc::methods::GetBlockHashResponse;
    use zebra_state::{ReadResponse, Response};

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
            subsidy::FundingStreamReceiver,
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

    use zcash_keys::address::Address;

    use zebra_rpc::{
        client::{
            BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateRequestMode,
            GetBlockTemplateResponse, SubmitBlockErrorResponse, SubmitBlockResponse,
        },
        fetch_state_tip_and_local_time, generate_coinbase_and_roots,
        methods::{RpcImpl, RpcServer},
        proposal_block_from_template, SubmitBlockChannel,
    };

    let _init_guard = zebra_test::init();

    tracing::info!("running nu6_funding_streams_and_coinbase_balance test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .expect("failed to set genesis hash")
        .with_checkpoints(false)
        .expect("failed to verify checkpoints")
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .expect("failed to set target difficulty limit")
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(1),
            ..Default::default()
        })
        .expect("failed to set activation heights");

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network()
        .expect("failed to build configured network");

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
            ConfiguredFundingStreamRecipient::new_for(FundingStreamReceiver::MajorGrants),
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
        .to_network()
        .expect("failed to build configured network");

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
        .to_network()
        .expect("failed to build configured network");

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

    use zebra_rpc::{
        client::{
            BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateRequestMode,
            GetBlockTemplateResponse, SubmitBlockResponse,
        },
        methods::{RpcImpl, RpcServer},
        proposal_block_from_template, SubmitBlockChannel,
    };

    let _init_guard = zebra_test::init();

    tracing::info!("running nu7_nsm_transactions test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .unwrap()
        .with_checkpoints(false)
        .unwrap()
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .unwrap()
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_lockbox_disbursements(vec![])
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        });

    let network = base_network_params
        .clone()
        .unwrap()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network()
        .unwrap();

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
    use zebra_chain::block;

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
