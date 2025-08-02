//! Test all gRPC calls a wallet connected to a lightwalletd instance backed by
//! zebra can do.
//!
//! This test requires a cached chain state that is partially synchronized,
//! i.e., it should be a few blocks below the network chain tip height. It also
//! requires a lightwalletd data dir in sync with the cached chain state.
//!
//! Current coverage of all available rpc methods according to
//! `CompactTxStreamer`:
//!
//! - `GetLatestBlock`: Covered.
//! - `GetBlock`: Covered.
//! - `GetBlockRange`: Covered.
//!
//! - `GetTransaction`: Covered.
//! - `SendTransaction`: Covered by the send_transaction_test.
//!
//! - `GetTaddressTxids`: Covered.
//! - `GetTaddressBalance`: Covered.
//! - `GetTaddressBalanceStream`: Covered.
//!
//! - `GetMempoolTx`: Covered by the send_transaction_test,
//!   currently disabled by `lightwalletd`.
//! - `GetMempoolStream`: Covered by the send_transaction_test,
//!   currently disabled by `lightwalletd`.
//!
//! - `GetTreeState`: Covered.
//!
//! - `GetAddressUtxos`: Covered.
//! - `GetAddressUtxosStream`: Covered.
//!
//! - `GetLightdInfo`: Covered.
//!
//! - `Ping`: Not covered and it will never be. `Ping` is only used for testing purposes.

use color_eyre::eyre::Result;
use hex_literal::hex;

use zebra_chain::{
    block::{Block, Height},
    parameters::{
        Network,
        NetworkUpgrade::{Nu5, Sapling},
    },
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::funding_stream_address;
use zebra_state::state_database_format_version_in_code;

use crate::common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    launch::spawn_zebrad_for_rpc,
    lightwalletd::{
        can_spawn_lightwalletd_for_rpc, spawn_lightwalletd_for_rpc,
        sync::wait_for_zebrad_and_lightwalletd_sync,
        wallet_grpc::{
            connect_to_lightwalletd, Address, AddressList, BlockId, BlockRange, ChainSpec, Empty,
            GetAddressUtxosArg, GetSubtreeRootsArg, ShieldedProtocol,
            TransparentAddressBlockFilter, TxFilter,
        },
    },
    test_type::TestType::UpdateCachedState,
};

/// The test entry point.
pub async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir and a lightwalletd data dir in place,
    // so `UpdateCachedState` can be used as our test type
    let test_type = UpdateCachedState;

    // This test is only for the mainnet
    let network = Network::Mainnet;
    let test_name = "wallet_grpc_test";

    // We run these gRPC tests with a network connection, for better test coverage.
    let use_internet_connection = true;

    if test_type.launches_lightwalletd() && !can_spawn_lightwalletd_for_rpc(test_name, test_type) {
        tracing::info!("skipping test due to missing lightwalletd network or cached state");
        return Ok(());
    }

    // Launch zebra with peers and using a predefined zebrad state path.
    // As this tests are just queries we can have a live chain where blocks are coming.
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network.clone(),
        test_name,
        test_type,
        use_internet_connection,
    )? {
        tracing::info!(
            ?network,
            ?test_type,
            "running gRPC query tests using lightwalletd & zebrad...",
        );

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("lightwalletd test must have RPC port");

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    tracing::info!(
        ?test_type,
        ?zebra_rpc_address,
        "launched zebrad, waiting for zebrad to open its RPC port..."
    );

    // Wait for the state to upgrade, if the upgrade is short.
    //
    // If incompletely upgraded states get written to the CI cache,
    // change DATABASE_FORMAT_UPGRADE_IS_LONG to true.
    //
    // If this line hangs, move it before the RPC port check.
    // (The RPC port is usually much faster than even a quick state upgrade.)
    if !DATABASE_FORMAT_UPGRADE_IS_LONG {
        wait_for_state_version_upgrade(
            &mut zebrad,
            &state_version_message,
            state_database_format_version_in_code(),
            [format!("Opened RPC endpoint at {zebra_rpc_address}")],
        )?;
    }

    tracing::info!(
        ?zebra_rpc_address,
        "zebrad opened its RPC port, spawning lightwalletd...",
    );

    // Launch lightwalletd
    let (lightwalletd, lightwalletd_rpc_port) =
        spawn_lightwalletd_for_rpc(network.clone(), test_name, test_type, zebra_rpc_address)?
            .expect("already checked cached state and network requirements");

    tracing::info!(
        ?lightwalletd_rpc_port,
        "spawned lightwalletd connected to zebrad, waiting for them both to sync...",
    );

    let (_lightwalletd, mut zebrad) = wait_for_zebrad_and_lightwalletd_sync(
        lightwalletd,
        lightwalletd_rpc_port,
        zebrad,
        zebra_rpc_address,
        test_type,
        // We want our queries to include the mempool and network for better coverage
        true,
        use_internet_connection,
    )?;

    // Wait for the state to upgrade, if the upgrade is long.
    // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false.
    if DATABASE_FORMAT_UPGRADE_IS_LONG {
        wait_for_state_version_upgrade(
            &mut zebrad,
            &state_version_message,
            state_database_format_version_in_code(),
            None,
        )?;
    }

    tracing::info!(
        ?lightwalletd_rpc_port,
        "connecting gRPC client to lightwalletd...",
    );

    // Connect to the lightwalletd instance
    let mut rpc_client = connect_to_lightwalletd(lightwalletd_rpc_port).await?;

    // End of the setup and start the tests
    tracing::info!(?lightwalletd_rpc_port, "sending gRPC queries...");

    // Call `GetLatestBlock`
    let block_tip = rpc_client
        .get_latest_block(ChainSpec {})
        .await?
        .into_inner();

    // Get `Sapling` activation height.
    let sapling_activation_height = Sapling.activation_height(&network).unwrap().0 as u64;

    // As we are using a pretty much synchronized blockchain, we can assume the tip is above the Nu5 network upgrade
    assert!(block_tip.height > Nu5.activation_height(&network).unwrap().0 as u64);

    // The first block in the mainnet that has sapling and orchard information.
    let block_with_trees = 1687107;

    // Call `GetBlock` with `block_with_trees`.
    let get_block_response = rpc_client
        .get_block(BlockId {
            height: block_with_trees,
            hash: vec![],
        })
        .await?
        .into_inner();

    // Make sure we got block `block_with_trees` back
    assert_eq!(get_block_response.height, block_with_trees);

    // Testing the `trees` field of `GetBlock`.
    assert_eq!(
        get_block_response
            .chain_metadata
            .unwrap()
            .sapling_commitment_tree_size,
        1170439
    );
    assert_eq!(
        get_block_response
            .chain_metadata
            .unwrap()
            .orchard_commitment_tree_size,
        2
    );

    // Call `GetBlockRange` with the range starting at block 1 up to block 10
    let mut block_range = rpc_client
        .get_block_range(BlockRange {
            start: Some(BlockId {
                height: sapling_activation_height,
                hash: vec![],
            }),
            end: Some(BlockId {
                height: sapling_activation_height + 10,
                hash: vec![],
            }),
        })
        .await?
        .into_inner();

    // Make sure the returned Stream of blocks is what we expect
    let mut counter = sapling_activation_height;
    while let Some(block) = block_range.message().await? {
        assert_eq!(block.height, counter);
        counter += 1;
    }

    // Get the first transaction of the first block in the mainnet
    let hash = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize")
        .transactions[0]
        .hash()
        .0
        .to_vec();

    // Call `GetTransaction` with the transaction hash
    let transaction = rpc_client
        .get_transaction(TxFilter {
            block: None,
            index: 0,
            hash,
        })
        .await?
        .into_inner();

    // Check the height of transactions is 1 as expected
    assert_eq!(transaction.height, 1);

    // Call `GetTaddressTxids` with a founders reward address that we know exists and have transactions in the first
    // few blocks of the mainnet
    let mut transactions = rpc_client
        .get_taddress_txids(TransparentAddressBlockFilter {
            address: "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string(),
            range: Some(BlockRange {
                start: Some(BlockId {
                    height: 1,
                    hash: vec![],
                }),
                end: Some(BlockId {
                    height: 10,
                    hash: vec![],
                }),
            }),
        })
        .await?
        .into_inner();

    let mut counter = 0;
    while let Some(_transaction) = transactions.message().await? {
        counter += 1;
    }

    // For the provided address in the first 10 blocks there are 10 transactions in the mainnet
    assert_eq!(10, counter);

    let lwd_tip_height: Height = u32::try_from(block_tip.height)
        .expect("should be below max block height")
        .try_into()
        .expect("should be below max block height");

    let mut all_stream_addresses = Vec::new();
    let mut all_balance_streams = Vec::new();
    for &fs_receiver in network
        .funding_streams(lwd_tip_height)
        .unwrap_or(network.all_funding_streams().last().unwrap().clone())
        .recipients()
        .keys()
    {
        let Some(fs_address) = funding_stream_address(lwd_tip_height, &network, fs_receiver) else {
            // Skip if the lightwalletd tip height is above the funding stream end height.
            continue;
        };

        tracing::info!(?fs_address, "getting balance for active fs address");

        // Call `GetTaddressBalance` with the active funding stream address.
        let balance = rpc_client
            .get_taddress_balance(AddressList {
                addresses: vec![fs_address.to_string()],
            })
            .await?
            .into_inner();

        // Call `GetTaddressBalanceStream` with the active funding stream address as a stream argument.
        let stream_address = Address {
            address: fs_address.to_string(),
        };

        let balance_stream = rpc_client
            .get_taddress_balance_stream(tokio_stream::iter(vec![stream_address.clone()]))
            .await?
            .into_inner();

        // With any active funding stream address, the balance will always be greater than zero for blocks
        // below the funding stream end height because new coins are created in each block.
        assert!(balance.value_zat > 0);
        assert!(balance_stream.value_zat > 0);

        all_stream_addresses.push(stream_address);
        all_balance_streams.push(balance_stream.value_zat);

        // Call `GetAddressUtxos` with the active funding stream address that will always have utxos
        let utxos = rpc_client
            .get_address_utxos(GetAddressUtxosArg {
                addresses: vec![fs_address.to_string()],
                start_height: 1,
                max_entries: 1,
            })
            .await?
            .into_inner();

        // As we requested one entry we should get a response of length 1
        assert_eq!(utxos.address_utxos.len(), 1);

        // Call `GetAddressUtxosStream` with the active funding stream address that will always have utxos
        let mut utxos_zf = rpc_client
            .get_address_utxos_stream(GetAddressUtxosArg {
                addresses: vec![fs_address.to_string()],
                start_height: 1,
                max_entries: 2,
            })
            .await?
            .into_inner();

        let mut counter = 0;
        while let Some(_utxos) = utxos_zf.message().await? {
            counter += 1;
        }
        // As we are in a "in sync" chain we know there are more than 2 utxos for this address (coinbase maturity rule)
        // but we will receive the max of 2 from the stream response because we used a limit of 2 `max_entries`.
        assert_eq!(2, counter);
    }

    if let Some(expected_total_balance) = all_balance_streams.into_iter().reduce(|a, b| a + b) {
        // Call `GetTaddressBalanceStream` for all active funding stream addresses as a stream argument.
        let total_balance = rpc_client
            .get_taddress_balance_stream(tokio_stream::iter(all_stream_addresses))
            .await?
            .into_inner();

        // The result should be the sum of the values in all active funding stream addresses.
        assert_eq!(total_balance.value_zat, expected_total_balance);
    }

    let sapling_treestate_init_height = sapling_activation_height + 1;

    // Call `GetTreeState`.
    let treestate = rpc_client
        .get_tree_state(BlockId {
            height: sapling_treestate_init_height,
            hash: vec![],
        })
        .await?
        .into_inner();

    // Check that the network is correct.
    assert_eq!(treestate.network, "main");
    // Check that the height is correct.
    assert_eq!(treestate.height, sapling_treestate_init_height);
    // Check that the hash is correct.
    assert_eq!(
        treestate.hash,
        "00000000014d117faa2ea701b24261d364a6c6a62e5bc4bc27335eb9b3c1e2a8"
    );
    // Check that the time is correct.
    assert_eq!(treestate.time, 1540779438);
    // Check that the note commitment tree is correct.
    assert_eq!(
        treestate.sapling_tree,
        *zebra_test::vectors::SAPLING_TREESTATE_MAINNET_419201_STRING
    );

    // Call `GetLightdInfo`
    let lightd_info = rpc_client.get_lightd_info(Empty {}).await?.into_inner();

    // Make sure the subversion field is zebra the user agent
    assert_eq!(
        lightd_info.zcashd_subversion,
        zebrad::application::user_agent()
    );

    // Call `z_getsubtreesbyindex` separately for...

    // ... Sapling.
    let mut subtrees = rpc_client
        .get_subtree_roots(GetSubtreeRootsArg {
            start_index: 0u32,
            shielded_protocol: ShieldedProtocol::Sapling.into(),
            max_entries: 2u32,
        })
        .await?
        .into_inner();

    let mut counter = 0;
    while let Some(subtree) = subtrees.message().await? {
        match counter {
            0 => {
                assert_eq!(
                    subtree.root_hash,
                    hex!("754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13")
                );
                assert_eq!(subtree.completing_block_height, 558822u64);
            }
            1 => {
                assert_eq!(
                    subtree.root_hash,
                    hex!("03654c3eacbb9b93e122cf6d77b606eae29610f4f38a477985368197fd68e02d")
                );
                assert_eq!(subtree.completing_block_height, 670209u64);
            }
            _ => {
                panic!("The response from the `z_getsubtreesbyindex` RPC contains a wrong number of Sapling subtrees.")
            }
        }
        counter += 1;
    }
    assert_eq!(counter, 2);

    // ... Orchard.
    let mut subtrees = rpc_client
        .get_subtree_roots(GetSubtreeRootsArg {
            start_index: 0u32,
            shielded_protocol: ShieldedProtocol::Orchard.into(),
            max_entries: 2u32,
        })
        .await?
        .into_inner();

    let mut counter = 0;
    while let Some(subtree) = subtrees.message().await? {
        match counter {
            0 => {
                assert_eq!(
                    subtree.root_hash,
                    hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
                );
                assert_eq!(subtree.completing_block_height, 1707429u64);
            }
            1 => {
                assert_eq!(
                    subtree.root_hash,
                    hex!("8c47d0ca43f323ac573ee57c90af4ced484682827248ca5f3eead95eb6415a14")
                );
                assert_eq!(subtree.completing_block_height, 1708132u64);
            }
            _ => {
                panic!("The response from the `z_getsubtreesbyindex` RPC contains a wrong number of Orchard subtrees.")
            }
        }
        counter += 1;
    }
    assert_eq!(counter, 2);

    Ok(())
}
