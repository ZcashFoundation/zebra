//! End-to-end tests for Zebra's built-in lightwalletd-compatible gRPC server on Regtest.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tonic::{Code, Status, Streaming};

use zebra_chain::{
    amount::Amount,
    block::{self, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkKind},
    serialization::ZcashSerialize as _,
    transaction::{self, Transaction, TransparentSigningKey},
    transparent,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{
        GetAddressBalanceResponse, GetAddressUtxosResponse, GetBlockHashResponse, GetInfoResponse,
        GetRawTransactionResponse, GetSubtreesByIndexResponse, GetTreestateResponse,
    },
    lightwalletd::{
        compact_tx_streamer_client::CompactTxStreamerClient, Address as LightwalletdAddress,
        AddressList, BlockId, BlockRange, ChainSpec, Duration as PingDuration, Empty, Exclude,
        GetAddressUtxosArg, GetSubtreeRootsArg, RawTransaction, ShieldedProtocol,
        TransparentAddressBlockFilter, TxFilter,
    },
    server::{error::LegacyCode, OPENED_LIGHTWALLETD_ENDPOINT_MSG, OPENED_RPC_ENDPOINT_MSG},
};
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

/// The number of blocks mined on top of genesis before the server is queried.
const NUM_BLOCKS: u32 = 10;

/// How long to wait for a single gRPC response, or for the next message of a stream.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Zebra's built-in `CompactTxStreamer` server must serve a chain it mined itself, and
/// every answer must agree with the JSON-RPC interface and the blocks that were mined.
///
/// A fresh Regtest chain is enough to prove this: the gRPC methods are thin views over
/// the state and the JSON-RPC methods, so the byte orders, status codes, and stream
/// edges they get wrong show up on a ten-block chain of transparent coinbase
/// transactions just as they would on Mainnet. This replaces the GCP `lwd-grpc-wallet`
/// job, which drove the Go `lightwalletd` binary against a cached Mainnet state.
///
/// The mempool methods are covered by [`lightwalletd_grpc_serves_the_mempool`].
#[tokio::test]
async fn lightwalletd_grpc_serves_a_regtest_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.rpc.lightwalletd_listen_addr = Some("127.0.0.1:0".parse()?);
    config.mempool.debug_enable_at_height = Some(0);
    let miner_address = config
        .mining
        .miner_address
        .as_ref()
        .expect("the default test config sets a transparent miner address")
        .to_string();

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    // The JSON-RPC server is started before the gRPC server, so the log lines come in this order.
    let rpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;
    let grpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_LIGHTWALLETD_ENDPOINT_MSG)?;

    tokio::time::sleep(LAUNCH_DELAY).await;

    let rpc = RpcRequestClient::new(rpc_address);

    // Mine the chain, then read the mined blocks back as the oracle for everything below.
    let generated_hashes = rpc.generate(NUM_BLOCKS).await?;
    assert_eq!(generated_hashes.len(), NUM_BLOCKS as usize);

    let mut blocks = Vec::new();
    for height in 1..=NUM_BLOCKS {
        let block = rpc
            .get_block(height as i32)
            .await
            .map_err(|err| eyre!(err))?
            .ok_or_else(|| eyre!("mined block {height} is missing from the state"))?;
        assert_eq!(block.hash(), generated_hashes[height as usize - 1]);
        blocks.push(block);
    }

    let endpoint = tonic::transport::Endpoint::new(format!("http://{grpc_address}"))?
        .timeout(RESPONSE_TIMEOUT);
    let mut grpc = CompactTxStreamerClient::connect(endpoint).await?;

    // GetLightdInfo: chain and node details must come from the node, not from the crate.
    let blockchain_info = rpc.blockchain_info().await?;
    let node_info: GetInfoResponse = rpc
        .json_result_from_call("getinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;
    let (tip_branch_id, _next_branch_id) = blockchain_info.consensus().into_parts();

    let lightd_info = grpc.get_lightd_info(Empty {}).await?.into_inner();
    assert_eq!(lightd_info.chain_name, *blockchain_info.chain());
    assert_eq!(lightd_info.block_height, u64::from(NUM_BLOCKS));
    assert_eq!(
        lightd_info.block_height,
        u64::from(blockchain_info.blocks().0)
    );
    assert_eq!(
        lightd_info.estimated_height,
        u64::from(blockchain_info.estimated_height().0)
    );
    assert_eq!(
        lightd_info.consensus_branch_id,
        format!("{tip_branch_id:08x}")
    );
    assert_eq!(
        lightd_info.sapling_activation_height,
        u64::from(network.sapling_activation_height().0)
    );
    assert_eq!(lightd_info.vendor, "ZcashFoundation/zebra");
    assert!(lightd_info.taddr_support);
    assert!(!lightd_info.version.is_empty());
    assert_eq!(lightd_info.version, *node_info.build());
    assert_eq!(lightd_info.zcashd_build, *node_info.build());
    assert_eq!(lightd_info.zcashd_subversion, *node_info.subversion());

    // GetLatestBlock: the tip, with the hash in internal byte order.
    let best_hash: GetBlockHashResponse = rpc
        .json_result_from_call("getbestblockhash", "[]")
        .await
        .map_err(|err| eyre!(err))?;
    let best_hash = best_hash.hash();
    assert_eq!(best_hash, blocks[NUM_BLOCKS as usize - 1].hash());

    let latest_block = grpc.get_latest_block(ChainSpec {}).await?.into_inner();
    assert_eq!(latest_block.height, u64::from(NUM_BLOCKS));
    assert_eq!(block_hash_from_lightwalletd(&latest_block.hash)?, best_hash);

    // GetBlock: by height and by hash, checked against the mined block and `getblock`.
    let mut compact_blocks = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let height = index as u64 + 1;
        let previous_hash = index.checked_sub(1).map_or_else(
            || network.genesis_hash(),
            |previous| blocks[previous].hash(),
        );

        let by_height = grpc
            .get_block(block_id_for_height(height))
            .await?
            .into_inner();
        let by_hash = grpc
            .get_block(block_id_for_hash(block.hash()))
            .await?
            .into_inner();
        assert_eq!(
            by_height, by_hash,
            "a block must not depend on how it is looked up"
        );

        assert_eq!(by_height.height, height);
        assert_eq!(block_hash_from_lightwalletd(&by_height.hash)?, block.hash());
        assert_eq!(
            block_hash_from_lightwalletd(&by_height.prev_hash)?,
            block.header.previous_block_hash
        );
        assert_eq!(block.header.previous_block_hash, previous_hash);
        assert_eq!(
            u64::from(by_height.time),
            u64::try_from(block.header.time.timestamp())?
        );
        // Like lightwalletd, transactions without shielded data are left out of compact blocks.
        assert_eq!(block.transactions.len(), 1);
        assert!(
            by_height.vtx.is_empty(),
            "a transparent coinbase transaction has no compact data"
        );

        // `getblock` omits a pool from `trees` when its tree is empty, like zcashd.
        let block_object: serde_json::Value = rpc
            .json_result_from_call("getblock", format!(r#"["{height}", 1]"#))
            .await
            .map_err(|err| eyre!(err))?;
        let trees = &block_object["trees"];
        assert!(trees.is_object(), "getblock must report the tree sizes");
        let tree_size = |pool: &str| trees[pool]["size"].as_u64().unwrap_or_default();
        let metadata = by_height
            .chain_metadata
            .as_ref()
            .expect("compact blocks carry the note commitment tree sizes");
        assert_eq!(
            u64::from(metadata.sapling_commitment_tree_size),
            tree_size("sapling")
        );
        assert_eq!(
            u64::from(metadata.orchard_commitment_tree_size),
            tree_size("orchard")
        );
        assert_eq!(
            u64::from(metadata.ironwood_commitment_tree_size),
            tree_size("ironwood")
        );

        compact_blocks.push(by_height);
    }

    // GetBlockRange: inclusive, in the direction of the range, and a range past the tip
    // yields the blocks that exist and then fails.
    let ascending = grpc
        .get_block_range(block_range(1, u64::from(NUM_BLOCKS)))
        .await?
        .into_inner();
    let (ascending, status) = drain_stream(ascending).await?;
    assert_eq!(ascending, compact_blocks);
    assert!(status.is_none(), "ascending range failed: {status:?}");

    let descending = grpc
        .get_block_range(block_range(u64::from(NUM_BLOCKS), 1))
        .await?
        .into_inner();
    let (descending, status) = drain_stream(descending).await?;
    let mut reversed_blocks = compact_blocks.clone();
    reversed_blocks.reverse();
    assert_eq!(descending, reversed_blocks);
    assert!(status.is_none(), "descending range failed: {status:?}");

    let past_tip_start = u64::from(NUM_BLOCKS) - 2;
    let past_tip = grpc
        .get_block_range(block_range(past_tip_start, u64::from(NUM_BLOCKS) + 5))
        .await?
        .into_inner();
    let (past_tip, status) = drain_stream(past_tip).await?;
    assert_eq!(past_tip, compact_blocks[past_tip_start as usize - 1..]);
    assert_eq!(
        status.map(|status| status.code()),
        Some(Code::NotFound),
        "a range past the tip must end with a not found status"
    );

    // GetBlockNullifiers and GetBlockRangeNullifiers: on a chain without shielded
    // transactions, the nullifier-only view is identical to the full compact blocks.
    let nullifiers = grpc
        .get_block_nullifiers(block_id_for_height(u64::from(NUM_BLOCKS)))
        .await?
        .into_inner();
    assert_eq!(nullifiers, compact_blocks[NUM_BLOCKS as usize - 1]);
    assert!(nullifiers.vtx.is_empty());

    let nullifier_range = grpc
        .get_block_range_nullifiers(block_range(1, u64::from(NUM_BLOCKS)))
        .await?
        .into_inner();
    let (nullifier_range, status) = drain_stream(nullifier_range).await?;
    assert_eq!(nullifier_range, compact_blocks);
    assert!(status.is_none(), "nullifier range failed: {status:?}");

    // GetTreeState and GetLatestTreeState: must match `z_gettreestate` for the tip.
    let treestate: GetTreestateResponse = rpc
        .json_result_from_call("z_gettreestate", format!(r#"["{NUM_BLOCKS}"]"#))
        .await
        .map_err(|err| eyre!(err))?;
    let final_state_hex = |treestate: &zebra_rpc::client::Treestate| {
        treestate
            .commitments()
            .final_state()
            .as_ref()
            .map(hex::encode)
            .unwrap_or_default()
    };

    let tree_state_by_height = grpc
        .get_tree_state(block_id_for_height(u64::from(NUM_BLOCKS)))
        .await?
        .into_inner();
    let tree_state_by_hash = grpc
        .get_tree_state(block_id_for_hash(best_hash))
        .await?
        .into_inner();
    let latest_tree_state = grpc.get_latest_tree_state(Empty {}).await?.into_inner();
    assert_eq!(tree_state_by_height, tree_state_by_hash);
    assert_eq!(tree_state_by_height, latest_tree_state);

    assert_eq!(tree_state_by_height.network, network.bip70_network_name());
    assert_eq!(tree_state_by_height.height, u64::from(treestate.height().0));
    assert_eq!(tree_state_by_height.height, u64::from(NUM_BLOCKS));
    // Unlike the binary hashes, the tree state hash is a hex string in display order.
    assert_eq!(tree_state_by_height.hash, treestate.hash().to_string());
    assert_eq!(tree_state_by_height.hash.parse::<block::Hash>()?, best_hash);
    assert_eq!(tree_state_by_height.time, treestate.time());
    assert_eq!(
        tree_state_by_height.sapling_tree,
        final_state_hex(treestate.sapling())
    );
    assert!(
        !tree_state_by_height.sapling_tree.is_empty(),
        "Sapling is active, so its tree state must be present"
    );
    assert_eq!(
        tree_state_by_height.orchard_tree,
        final_state_hex(treestate.orchard())
    );
    assert!(
        !tree_state_by_height.orchard_tree.is_empty(),
        "NU5 is active, so the Orchard tree state must be present"
    );

    // GetTransaction: the coinbase of block 1, byte for byte, with its mined height.
    let coinbase = &blocks[0].transactions[0];
    let GetRawTransactionResponse::Raw(raw_coinbase) = rpc
        .json_result_from_call(
            "getrawtransaction",
            format!(r#"["{}", 0]"#, coinbase.hash()),
        )
        .await
        .map_err(|err| eyre!(err))?
    else {
        panic!("getrawtransaction with verbosity 0 must return raw bytes");
    };

    let raw_coinbase: &[u8] = raw_coinbase.as_ref();

    let raw_transaction = grpc
        .get_transaction(tx_filter_for_hash(coinbase.hash()))
        .await?
        .into_inner();
    assert_eq!(raw_transaction.data, raw_coinbase);
    assert_eq!(raw_transaction.data, coinbase.zcash_serialize_to_vec()?);
    assert_eq!(raw_transaction.height, 1);

    let missing_txid = transaction::Hash([0xab; 32]);
    let status = grpc
        .get_transaction(tx_filter_for_hash(missing_txid))
        .await
        .expect_err("an unknown transaction must not be served");
    assert_eq!(status.code(), Code::NotFound);

    // GetTaddressBalance and GetTaddressBalanceStream: must match `getaddressbalance`.
    let address_balance: GetAddressBalanceResponse = rpc
        .json_result_from_call(
            "getaddressbalance",
            format!(r#"[{{"addresses": ["{miner_address}"]}}]"#),
        )
        .await
        .map_err(|err| eyre!(err))?;
    assert!(
        address_balance.balance() > 0,
        "the miner address must have been paid by the mined coinbase transactions"
    );

    let balance = grpc
        .get_taddress_balance(AddressList {
            addresses: vec![miner_address.clone()],
        })
        .await?
        .into_inner();
    assert_eq!(balance.value_zat, i64::try_from(address_balance.balance())?);

    let streamed_balance = grpc
        .get_taddress_balance_stream(tokio_stream::iter([LightwalletdAddress {
            address: miner_address.clone(),
        }]))
        .await?
        .into_inner();
    assert_eq!(streamed_balance, balance);

    // GetTaddressTxids and GetTaddressTransactions: every coinbase, in block order,
    // matching `getaddresstxids`.
    let address_txids: Vec<String> = rpc
        .json_result_from_call(
            "getaddresstxids",
            format!(r#"[{{"addresses": ["{miner_address}"], "start": 1, "end": {NUM_BLOCKS}}}]"#),
        )
        .await
        .map_err(|err| eyre!(err))?;
    assert_eq!(address_txids.len(), NUM_BLOCKS as usize);

    let address_filter = TransparentAddressBlockFilter {
        address: miner_address.clone(),
        range: Some(block_range(1, u64::from(NUM_BLOCKS))),
    };
    let (address_transactions, status) = drain_stream(
        grpc.get_taddress_txids(address_filter.clone())
            .await?
            .into_inner(),
    )
    .await?;
    assert!(status.is_none(), "taddress txids stream failed: {status:?}");
    assert_eq!(address_transactions.len(), NUM_BLOCKS as usize);
    for (index, raw_transaction) in address_transactions.iter().enumerate() {
        let coinbase = &blocks[index].transactions[0];
        assert_eq!(address_txids[index], coinbase.hash().to_string());
        assert_eq!(raw_transaction.height, index as u64 + 1);
        assert_eq!(raw_transaction.data, coinbase.zcash_serialize_to_vec()?);
    }

    let (address_transactions_alias, status) = drain_stream(
        grpc.get_taddress_transactions(address_filter)
            .await?
            .into_inner(),
    )
    .await?;
    assert!(
        status.is_none(),
        "taddress transactions stream failed: {status:?}"
    );
    assert_eq!(address_transactions_alias, address_transactions);

    // GetAddressUtxos and GetAddressUtxosStream: must match `getaddressutxos`, and the
    // start height and entry limit must be applied.
    let GetAddressUtxosResponse::Utxos(address_utxos) = rpc
        .json_result_from_call(
            "getaddressutxos",
            format!(r#"[{{"addresses": ["{miner_address}"]}}]"#),
        )
        .await
        .map_err(|err| eyre!(err))?
    else {
        panic!("getaddressutxos without chainInfo must return a plain list");
    };
    assert_eq!(address_utxos.len(), NUM_BLOCKS as usize);

    let utxos_arg = GetAddressUtxosArg {
        addresses: vec![miner_address.clone()],
        start_height: 0,
        max_entries: 0,
    };
    let utxos = grpc
        .get_address_utxos(utxos_arg.clone())
        .await?
        .into_inner()
        .address_utxos;
    assert_eq!(utxos.len(), address_utxos.len());
    for (utxo, expected) in utxos.iter().zip(&address_utxos) {
        assert_eq!(utxo.address, expected.address().to_string());
        assert_eq!(utxo.address, miner_address);
        assert_eq!(txid_from_lightwalletd(&utxo.txid)?, expected.txid());
        assert_eq!(u32::try_from(utxo.index)?, expected.output_index().index());
        assert_eq!(utxo.script, expected.script().as_raw_bytes());
        assert_eq!(utxo.value_zat, i64::try_from(expected.satoshis())?);
        assert_eq!(utxo.height, u64::from(expected.height().0));
    }
    let utxo_total: i64 = utxos.iter().map(|utxo| utxo.value_zat).sum();
    assert_eq!(utxo_total, balance.value_zat);

    let (streamed_utxos, status) = drain_stream(
        grpc.get_address_utxos_stream(utxos_arg.clone())
            .await?
            .into_inner(),
    )
    .await?;
    assert!(status.is_none(), "address utxos stream failed: {status:?}");
    assert_eq!(streamed_utxos, utxos);

    let limited_utxos = grpc
        .get_address_utxos(GetAddressUtxosArg {
            max_entries: 3,
            ..utxos_arg.clone()
        })
        .await?
        .into_inner()
        .address_utxos;
    assert_eq!(limited_utxos, utxos[..3]);

    let recent_utxos = grpc
        .get_address_utxos(GetAddressUtxosArg {
            start_height: u64::from(NUM_BLOCKS) - 1,
            ..utxos_arg
        })
        .await?
        .into_inner()
        .address_utxos;
    assert_eq!(recent_utxos, utxos[NUM_BLOCKS as usize - 2..]);

    // GetSubtreeRoots: a tiny chain has no complete subtrees, so the streams are empty
    // but not errors, matching `z_getsubtreesbyindex`.
    for (protocol, pool) in [
        (ShieldedProtocol::Sapling, "sapling"),
        (ShieldedProtocol::Orchard, "orchard"),
    ] {
        let subtrees: GetSubtreesByIndexResponse = rpc
            .json_result_from_call("z_getsubtreesbyindex", format!(r#"["{pool}", 0]"#))
            .await
            .map_err(|err| eyre!(err))?;
        assert!(subtrees.subtrees().is_empty());

        let (roots, status) = drain_stream(
            grpc.get_subtree_roots(GetSubtreeRootsArg {
                start_index: 0,
                shielded_protocol: protocol.into(),
                max_entries: 0,
            })
            .await?
            .into_inner(),
        )
        .await?;
        assert!(
            status.is_none(),
            "{pool} subtree roots stream failed: {status:?}"
        );
        assert!(roots.is_empty(), "{pool} must have no complete subtrees");
    }

    let status = grpc
        .get_subtree_roots(GetSubtreeRootsArg {
            start_index: 0,
            shielded_protocol: 99,
            max_entries: 0,
        })
        .await
        .expect_err("an unknown shielded protocol must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    // Ping is disabled, like on production lightwalletd instances.
    let status = grpc
        .ping(PingDuration { interval_us: 0 })
        .await
        .expect_err("ping must be disabled");
    assert_eq!(status.code(), Code::Unimplemented);

    // Error paths: an unset block id is a client error, and a missing block is not found.
    let status = grpc
        .get_block(BlockId {
            height: 0,
            hash: Vec::new(),
        })
        .await
        .expect_err("an unset block id must not be read as genesis");
    assert_eq!(status.code(), Code::InvalidArgument);

    let status = grpc
        .get_block(BlockId {
            height: 0,
            hash: vec![0; 31],
        })
        .await
        .expect_err("a hash of the wrong length must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    let missing_height = u64::from(NUM_BLOCKS) + 100;
    let status = grpc
        .get_block(block_id_for_height(missing_height))
        .await
        .expect_err("a block past the tip must not be served");
    assert_eq!(status.code(), Code::NotFound);

    let status = grpc
        .get_tree_state(block_id_for_height(missing_height))
        .await
        .expect_err("a tree state past the tip must not be served");
    assert_eq!(status.code(), Code::NotFound);

    let status = grpc
        .get_block_range(BlockRange {
            start: Some(block_id_for_hash(best_hash)),
            end: Some(block_id_for_height(u64::from(NUM_BLOCKS))),
        })
        .await
        .expect_err("block ranges must be given as heights");
    assert_eq!(status.code(), Code::InvalidArgument);

    zebrad.kill(false)?;
    let output = zebrad.wait_with_output()?;
    output.assert_failure()?.assert_was_killed()?;

    Ok(())
}

/// The mempool methods of Zebra's `CompactTxStreamer` server must accept a transaction,
/// stream it while it is unmined, and drop it once it is mined, agreeing with the JSON-RPC
/// mempool view at every step.
///
/// A signed transparent transaction spending a mature Regtest coinbase is enough to drive
/// `SendTransaction`, `GetMempoolStream`, and the mempool branch of `GetTransaction`
/// through their success and failure paths, and to prove that a `GetMempoolStream` opened
/// on an empty mempool delivers the transaction live and closes on the next block. This
/// replaces the GCP `lwd-rpc-send-tx` job, which drove the Go `lightwalletd` binary
/// against a cached Mainnet state.
///
/// `GetMempoolTx` only gets its limit and empty cases: it serves compact transactions,
/// and a transparent-only transaction has no compact data. Its positive case needs a
/// shielded Regtest transaction, which the signing helper does not build yet (see #9941).
#[tokio::test]
async fn lightwalletd_grpc_serves_the_mempool() -> Result<()> {
    /// Coinbase outputs can be spent once this many blocks are on top of them.
    const COINBASE_MATURITY: u32 = transparent::MIN_TRANSPARENT_COINBASE_MATURITY;

    /// The height of the first block mined after the transaction is sent.
    const MINED_HEIGHT: u32 = COINBASE_MATURITY + 2;

    /// The server's `GetMempoolTx` exclude list limit, matching lightwalletd.
    const MAX_EXCLUDE_ENTRIES: usize = 20_000;

    /// How long to wait for a sent transaction to show up in `getrawmempool`.
    const MEMPOOL_TIMEOUT: Duration = Duration::from_secs(10);

    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let key = TransparentSigningKey::random();
    let miner_address = key.address(NetworkKind::Regtest);

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.rpc.lightwalletd_listen_addr = Some("127.0.0.1:0".parse()?);
    config.mempool.debug_enable_at_height = Some(0);
    config.mining.miner_address = Some(miner_address.into());

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    // The JSON-RPC server is started before the gRPC server, so the log lines come in this order.
    let rpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;
    let grpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_LIGHTWALLETD_ENDPOINT_MSG)?;

    tokio::time::sleep(LAUNCH_DELAY).await;

    let rpc = RpcRequestClient::new(rpc_address);

    // Mine a coinbase to our key at height 1, then enough blocks for it to mature.
    rpc.generate(COINBASE_MATURITY + 1).await?;

    let block_1 = rpc
        .get_block(1)
        .await
        .map_err(|err| eyre!(err))?
        .ok_or_else(|| eyre!("block 1 must exist after generate"))?;
    let coinbase = &block_1.transactions[0];
    let (index, coinbase_output) = coinbase
        .outputs()
        .into_iter()
        .enumerate()
        .find(|(_, output)| output.lock_script == miner_address.script())
        .ok_or_else(|| eyre!("the coinbase must pay the configured miner address"))?;
    let coinbase_outpoint = transparent::OutPoint {
        hash: coinbase.hash(),
        index: index as u32,
    };

    // The transaction under test spends the mature coinbase. The double spend pays a
    // different amount from the same output, so it has a different ID and must be rejected.
    let tx = Transaction::signed_transparent(
        &network,
        Height(MINED_HEIGHT),
        &key,
        &[(coinbase_outpoint, coinbase_output.clone())],
        &[(miner_address, Amount::try_from(100_000_000)?)],
    )
    .map_err(|err| eyre!(err))?;
    let double_spend = Transaction::signed_transparent(
        &network,
        Height(MINED_HEIGHT),
        &key,
        &[(coinbase_outpoint, coinbase_output)],
        &[(miner_address, Amount::try_from(50_000_000)?)],
    )
    .map_err(|err| eyre!(err))?;
    assert_ne!(tx.hash(), double_spend.hash());

    let txid = tx.hash();
    let tx_bytes = tx.zcash_serialize_to_vec()?;

    let endpoint = tonic::transport::Endpoint::new(format!("http://{grpc_address}"))?
        .timeout(RESPONSE_TIMEOUT);
    let mut grpc = CompactTxStreamerClient::connect(endpoint).await?;

    // GetMempoolStream on an empty mempool: the stream stays open, so it is drained after
    // the next block below, where it must have delivered the sent transaction live.
    assert!(raw_mempool(&rpc).await?.is_empty());
    let live_stream = grpc.get_mempool_stream(Empty {}).await?.into_inner();

    // SendTransaction: a success carries the transaction ID as its message.
    let sent = grpc
        .send_transaction(RawTransaction {
            data: tx_bytes.clone(),
            height: 0,
        })
        .await?
        .into_inner();
    assert_eq!(sent.error_code, 0, "send failed: {}", sent.error_message);
    assert_eq!(sent.error_message.parse::<transaction::Hash>()?, txid);

    let deadline = std::time::Instant::now() + MEMPOOL_TIMEOUT;
    loop {
        let mempool = raw_mempool(&rpc).await?;
        if mempool == [txid.to_string()] {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sent transaction must reach the mempool within {MEMPOOL_TIMEOUT:?}, got {mempool:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let GetRawTransactionResponse::Raw(raw_mempool_tx) = rpc
        .json_result_from_call("getrawtransaction", format!(r#"["{txid}", 0]"#))
        .await
        .map_err(|err| eyre!(err))?
    else {
        panic!("getrawtransaction with verbosity 0 must return raw bytes");
    };
    let raw_mempool_tx: &[u8] = raw_mempool_tx.as_ref();
    assert_eq!(raw_mempool_tx, tx_bytes);

    // Mempool transactions are served with a height of 0, like lightwalletd.
    let expected_raw_tx = RawTransaction {
        data: tx_bytes.clone(),
        height: 0,
    };

    // GetTransaction: the mempool branch, before the transaction has a height.
    let unmined = grpc
        .get_transaction(tx_filter_for_hash(txid))
        .await?
        .into_inner();
    assert_eq!(unmined, expected_raw_tx);

    // GetMempoolStream after sending: the snapshot carries the transaction, and the
    // stream stays open until the next block, where it is drained below.
    let snapshot_stream = grpc.get_mempool_stream(Empty {}).await?.into_inner();

    // GetMempoolTx: empty and not an error, because a transparent-only transaction has
    // no compact data (see `has_compact_data`), then the exclude list limit.
    let (compact_txs, status) = drain_stream(
        grpc.get_mempool_tx(Exclude { txid: vec![] })
            .await?
            .into_inner(),
    )
    .await?;
    assert!(status.is_none(), "mempool tx stream failed: {status:?}");
    assert!(compact_txs.is_empty());

    let status = match grpc
        .get_mempool_tx(Exclude {
            txid: vec![vec![0]; MAX_EXCLUDE_ENTRIES + 1],
        })
        .await
    {
        Err(status) => status,
        Ok(response) => drain_stream(response.into_inner())
            .await?
            .1
            .expect("an oversized exclude list must be rejected"),
    };
    assert_eq!(status.code(), Code::InvalidArgument);

    // SendTransaction failure paths: rejections are reported inside the response, with
    // the JSON-RPC error code, and leave the mempool as it was.
    let rejected = grpc
        .send_transaction(RawTransaction {
            data: double_spend.zcash_serialize_to_vec()?,
            height: 0,
        })
        .await?
        .into_inner();
    assert_eq!(rejected.error_code, LegacyCode::Verify as i32);
    assert!(!rejected.error_message.is_empty());

    let undecodable = grpc
        .send_transaction(RawTransaction {
            data: vec![0; 10],
            height: 0,
        })
        .await?
        .into_inner();
    assert_eq!(undecodable.error_code, LegacyCode::Deserialization as i32);
    assert!(!undecodable.error_message.is_empty());

    // A transaction larger than a block is the one submission rejected before the RPC.
    let status = grpc
        .send_transaction(RawTransaction {
            data: vec![0; block::MAX_BLOCK_BYTES as usize + 1],
            height: 0,
        })
        .await
        .expect_err("a transaction larger than a block must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    assert_eq!(raw_mempool(&rpc).await?, [txid.to_string()]);

    // Mining a block takes the transaction out of the mempool and closes both streams,
    // each having delivered the transaction exactly once.
    let mined_hashes = rpc.generate(1).await?;
    assert_eq!(mined_hashes.len(), 1);

    let (live, status) = drain_stream(live_stream).await?;
    assert!(status.is_none(), "live mempool stream failed: {status:?}");
    assert_eq!(live, std::slice::from_ref(&expected_raw_tx));

    let (snapshot, status) = drain_stream(snapshot_stream).await?;
    assert!(
        status.is_none(),
        "mempool snapshot stream failed: {status:?}"
    );
    assert_eq!(snapshot, [expected_raw_tx]);

    assert!(raw_mempool(&rpc).await?.is_empty());

    let mined_block = rpc
        .get_block(MINED_HEIGHT as i32)
        .await
        .map_err(|err| eyre!(err))?
        .ok_or_else(|| eyre!("the mined block must be readable at its height"))?;
    assert_eq!(mined_block.hash(), mined_hashes[0]);
    let mined_txids: Vec<_> = mined_block
        .transactions
        .iter()
        .map(|tx| tx.hash())
        .collect();
    assert_eq!(mined_txids[1..], [txid]);

    // GetTransaction: the confirmed branch, now with the height it was mined at.
    let mined = grpc
        .get_transaction(tx_filter_for_hash(txid))
        .await?
        .into_inner();
    assert_eq!(mined.data, tx_bytes);
    assert_eq!(mined.height, u64::from(MINED_HEIGHT));

    // GetMempoolStream on the emptied mempool: nothing until the next block closes it.
    let empty_stream = grpc.get_mempool_stream(Empty {}).await?.into_inner();
    rpc.generate(1).await?;
    let (empty, status) = drain_stream(empty_stream).await?;
    assert!(status.is_none(), "empty mempool stream failed: {status:?}");
    assert!(empty.is_empty());

    zebrad.kill(false)?;
    let output = zebrad.wait_with_output()?;
    output.assert_failure()?.assert_was_killed()?;

    Ok(())
}

/// The transaction IDs in the mempool, in display order, as reported by `getrawmempool`.
async fn raw_mempool(rpc: &RpcRequestClient) -> Result<Vec<String>> {
    rpc.json_result_from_call("getrawmempool", "[]")
        .await
        .map_err(|err| eyre!(err))
}

/// Reads a server stream to its end, returning its messages and the status that ended
/// it, if it ended with an error.
async fn drain_stream<T>(mut stream: Streaming<T>) -> Result<(Vec<T>, Option<Status>)> {
    let mut messages = Vec::new();

    loop {
        let next = tokio::time::timeout(RESPONSE_TIMEOUT, stream.message())
            .await
            .map_err(|_| eyre!("stream produced nothing for {RESPONSE_TIMEOUT:?}"))?;

        match next {
            Ok(Some(message)) => messages.push(message),
            Ok(None) => return Ok((messages, None)),
            Err(status) => return Ok((messages, Some(status))),
        }
    }
}

/// Converts a block hash sent by the server, in internal byte order, into a [`block::Hash`].
fn block_hash_from_lightwalletd(bytes: &[u8]) -> Result<block::Hash> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| eyre!("block hashes must be 32 bytes, got {}", bytes.len()))?;

    Ok(block::Hash(bytes))
}

/// Converts a transaction ID sent by the server, in internal byte order, into a
/// [`transaction::Hash`].
fn txid_from_lightwalletd(bytes: &[u8]) -> Result<transaction::Hash> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| eyre!("transaction ids must be 32 bytes, got {}", bytes.len()))?;

    Ok(transaction::Hash(bytes))
}

/// A [`BlockId`] that selects a block by height.
fn block_id_for_height(height: u64) -> BlockId {
    BlockId {
        height,
        hash: Vec::new(),
    }
}

/// A [`BlockId`] that selects a block by hash, sent in internal byte order.
fn block_id_for_hash(hash: block::Hash) -> BlockId {
    BlockId {
        height: 0,
        hash: hash.0.to_vec(),
    }
}

/// An inclusive [`BlockRange`] between two heights, in the order given.
fn block_range(start: u64, end: u64) -> BlockRange {
    BlockRange {
        start: Some(block_id_for_height(start)),
        end: Some(block_id_for_height(end)),
    }
}

/// A [`TxFilter`] that selects a transaction by its ID, sent in internal byte order.
fn tx_filter_for_hash(hash: transaction::Hash) -> TxFilter {
    TxFilter {
        block: None,
        index: 0,
        hash: hash.0.to_vec(),
    }
}
