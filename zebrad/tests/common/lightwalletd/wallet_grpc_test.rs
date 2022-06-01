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
//! - `SendTransaction`: Not covered and it will never be, it has its own test.
//!
//! - `GetTaddressTxids`: Covered.
//! - `GetTaddressBalance`: Covered.
//! - `GetTaddressBalanceStream`: Covered.
//!
//! - `GetMempoolTx`: Not covered.
//! - `GetMempoolStream`: Not covered.
//!
//! - `GetTreeState`: Covered.
//!
//! - `GetAddressUtxos`: Covered.
//! - `GetAddressUtxosStream`: Covered.
//!
//! - `GetLightdInfo`: Covered.
//! - `Ping`: Not covered and it will never be. `Ping` is only used for testing
//! purposes.

use color_eyre::eyre::Result;

use zebra_chain::{
    block::Block,
    parameters::Network,
    parameters::NetworkUpgrade::{self, Canopy},
    serialization::ZcashDeserializeInto,
};

use zebra_network::constants::USER_AGENT;

use crate::common::{
    launch::spawn_zebrad_for_rpc_without_initial_peers,
    lightwalletd::{
        wallet_grpc::{
            connect_to_lightwalletd, spawn_lightwalletd_with_rpc_server, Address, AddressList,
            BlockId, BlockRange, ChainSpec, Empty, GetAddressUtxosArg,
            TransparentAddressBlockFilter, TxFilter,
        },
        zebra_skip_lightwalletd_tests,
        LightwalletdTestType::UpdateCachedState,
    },
};

/// The test entry point.
pub async fn run() -> Result<()> {
    zebra_test::init();

    // Skip the test unless the user specifically asked for it
    if zebra_skip_lightwalletd_tests() {
        return Ok(());
    }

    // We want a zebra state dir and a lightwalletd data dir in place,
    // so `UpdateCachedState` can be used as our test type
    let test_type = UpdateCachedState;

    // Require to have a `ZEBRA_CACHED_STATE_DIR` in place
    let zebrad_state_path = test_type.zebrad_state_path("wallet_grpc_test".to_string());
    if zebrad_state_path.is_none() {
        return Ok(());
    }

    // Require to have a `LIGHTWALLETD_DATA_DIR` in place
    let lightwalletd_state_path = test_type.lightwalletd_state_path("wallet_grpc_test".to_string());
    if lightwalletd_state_path.is_none() {
        return Ok(());
    }

    // This test is only for the mainnet
    let network = Network::Mainnet;

    tracing::info!(
        ?network,
        ?test_type,
        ?zebrad_state_path,
        ?lightwalletd_state_path,
        "running gRPC query tests using lightwalletd & zebrad, \
         launching disconnected zebrad...",
    );

    // Launch zebra using a predefined zebrad state path
    let (_zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc_without_initial_peers(network, zebrad_state_path.unwrap(), test_type)?;

    tracing::info!(
        ?zebra_rpc_address,
        "launching lightwalletd connected to zebrad...",
    );

    // Launch lightwalletd
    let (_lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_with_rpc_server(
        zebra_rpc_address,
        lightwalletd_state_path,
        test_type,
        false,
    )?;

    // Give lightwalletd a few seconds to open its grpc port before connecting to it
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

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

    // As we are using a pretty much synchronized blockchain, we can assume the tip is above the Canopy network upgrade
    assert!(block_tip.height > Canopy.activation_height(network).unwrap().0 as u64);

    // `lightwalletd` only supports post-Sapling blocks, so we begin at the
    // Sapling activation height.
    let sapling_activation_height = NetworkUpgrade::Sapling
        .activation_height(network)
        .unwrap()
        .0 as u64;

    // Call `GetBlock` with block 1 height
    let block_one = rpc_client
        .get_block(BlockId {
            height: sapling_activation_height,
            hash: vec![],
        })
        .await?
        .into_inner();

    // Make sure we got block 1 back
    assert_eq!(block_one.height, sapling_activation_height);

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

    // Get the first transction of the first block in the mainnet
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

    // Call `GetTaddressBalance` with the ZF funding stream address
    let balance = rpc_client
        .get_taddress_balance(AddressList {
            addresses: vec!["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1".to_string()],
        })
        .await?
        .into_inner();

    // With ZFND or Major Grants funding stream address, the balance will always be greater than zero,
    // because new coins are created in each block
    assert!(balance.value_zat > 0);

    // Call `GetTaddressBalanceStream` with the ZF funding stream address as a stream argument
    let zf_stream_address = Address {
        address: "t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1".to_string(),
    };

    let balance_zf = rpc_client
        .get_taddress_balance_stream(tokio_stream::iter(vec![zf_stream_address.clone()]))
        .await?
        .into_inner();

    // With ZFND funding stream address, the balance will always be greater than zero,
    // because new coins are created in each block
    assert!(balance_zf.value_zat > 0);

    // Call `GetTaddressBalanceStream` with the MG funding stream address as a stream argument
    let mg_stream_address = Address {
        address: "t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym".to_string(),
    };

    let balance_mg = rpc_client
        .get_taddress_balance_stream(tokio_stream::iter(vec![mg_stream_address.clone()]))
        .await?
        .into_inner();

    // With Major Grants funding stream address, the balance will always be greater than zero,
    // because new coins are created in each block
    assert!(balance_mg.value_zat > 0);

    // Call `GetTaddressBalanceStream` with both, the ZFND and the MG funding stream addresses as a stream argument
    let balance_both = rpc_client
        .get_taddress_balance_stream(tokio_stream::iter(vec![
            zf_stream_address,
            mg_stream_address,
        ]))
        .await?
        .into_inner();

    // The result is the sum of the values in both addresses
    assert_eq!(
        balance_both.value_zat,
        balance_zf.value_zat + balance_mg.value_zat
    );

    // TODO: Create call and checks for `GetMempoolTx` and `GetMempoolTxStream`?

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
        treestate.tree,
        *zebra_test::vectors::SAPLING_TREESTATE_MAINNET_419201_STRING
    );

    // Call `GetAddressUtxos` with the ZF funding stream address that will always have utxos
    let utxos = rpc_client
        .get_address_utxos(GetAddressUtxosArg {
            addresses: vec!["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1".to_string()],
            start_height: 1,
            max_entries: 1,
        })
        .await?
        .into_inner();

    // As we requested one entry we should get a response of length 1
    assert_eq!(utxos.address_utxos.len(), 1);

    // Call `GetAddressUtxosStream` with the ZF funding stream address that will always have utxos
    let mut utxos_zf = rpc_client
        .get_address_utxos_stream(GetAddressUtxosArg {
            addresses: vec!["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1".to_string()],
            start_height: 1,
            max_entries: 2,
        })
        .await?
        .into_inner();

    let mut counter = 0;
    while let Some(_utxos) = utxos_zf.message().await? {
        counter += 1;
    }
    // As we are in a "in sync" chain we know there are more than 2 utxos for this address
    // but we will receive the max of 2 from the stream response because we used a limit of 2 `max_entries`.
    assert_eq!(2, counter);

    // Call `GetLightdInfo`
    let lightd_info = rpc_client.get_lightd_info(Empty {}).await?.into_inner();

    // Make sure the subversion field is zebra the user agent
    assert_eq!(lightd_info.zcashd_subversion, USER_AGENT);

    Ok(())
}
