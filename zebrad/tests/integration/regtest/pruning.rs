//! Exercise opt-in body pruning through the real node, RPCs, and consensus verifier.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use serde_json::{json, Value};
use zebra_chain::{
    amount::Amount,
    block::Height,
    parameters::{testnet::ConfiguredActivationHeights, Network},
    serialization::ZcashSerialize,
    transaction::{Transaction, TransparentSigningKey},
    transparent::OutPoint,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{BlockTemplateResponse, SendRawTransactionResponse},
    proposal_block_from_template,
};
use zebra_state::{constants::MAX_BLOCK_REORG_HEIGHT, StateInitError};
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{rpc_port_config, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

/// Waits for the worker's deletion to become visible, rather than assuming it follows the tip RPC.
async fn pruned_info(client: &RpcRequestClient, floor: u32) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let info: Value = client
                .json_result_from_call("getblockchaininfo", "[]")
                .await
                .map_err(|err| eyre!(err))?;
            if info["pruned"] == true && info["pruneheight"] == floor {
                return Ok(info);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("pruning did not reach height {floor}"))?
}

/// Pruned bodies stay unavailable across restart, but their mature UTXOs remain spendable.
#[tokio::test(flavor = "multi_thread")]
async fn pruning_preserves_spends_across_restart() -> Result<()> {
    const RETENTION: u32 = MAX_BLOCK_REORG_HEIGHT + 1;
    const MARGIN: u32 = 10;
    const TIP: u32 = MAX_BLOCK_REORG_HEIGHT + RETENTION + MARGIN;
    const FLOOR: u32 = MARGIN + 1;
    const TEST_TIMEOUT: Duration = Duration::from_secs(300);

    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let key = TransparentSigningKey::random();
        let address = key.address(network.kind());
        // Logs are filtered to warnings, so reserve the RPC port up front instead of reading
        // the OS-assigned port from the startup log.
        let listen_port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let mut config = rpc_port_config(listen_port, false, &network)?;
        config.state.ephemeral = false;
        config.state.prune_blocks_older_than = Some(RETENTION);
        // Persist each non-finalized update before publishing the tip, including across restart.
        config.state.debug_skip_non_finalized_state_backup_task = true;
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(address.into());
        // Avoid filling the child's log pipe while generating thousands of blocks.
        config.tracing.filter = Some("warn".to_owned());
        let rpc_address = config.rpc.listen_addr.expect("the fixture enables RPC");
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?
            .with_timeout(TEST_TIMEOUT)?;
        let test_dir = child
            .dir
            .take()
            .expect("retain the state directory for restart");
        let client = RpcRequestClient::new_with_timeout(rpc_address, TEST_TIMEOUT);
        wait_for_mempool_activation(&client).await?;

        client.generate(1).await?;
        let early = client
            .get_block(1)
            .await
            .map_err(|err| eyre!(err))?
            .expect("the first generated block exists");
        let coinbase = &early.transactions[0];
        let (index, output) = coinbase
            .outputs()
            .into_iter()
            .enumerate()
            .find(|(_, output)| output.lock_script == address.script())
            .expect("the coinbase pays the configured key");
        let outpoint = OutPoint {
            hash: coinbase.hash(),
            index: u32::try_from(index)?,
        };
        let output_params = json!([outpoint.hash.to_string(), outpoint.index, false]).to_string();
        let before: Value = client
            .json_result_from_call("gettxout", &output_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(before["version"], 5);
        assert_eq!(before["coinbase"], true);
        assert!(before["value"].as_f64().is_some_and(|value| value > 0.0));

        // Retention starts at the durable finalized tip, not at the best non-finalized tip.
        let hashes = client.generate(TIP - 1).await?;
        let tip_hash = *hashes.last().expect("more than one block was generated");
        let info = pruned_info(&client, FLOOR).await?;
        assert_eq!(info["blocks"], TIP);
        assert_eq!(info["bestblockhash"], tip_hash.to_string());
        for verbosity in [0, 2] {
            let response: Value = serde_json::from_str(
                &client
                    .text_from_call("getblock", format!(r#"["1",{verbosity}]"#))
                    .await?,
            )?;
            assert_eq!(response["error"]["code"], -32603);
            assert_eq!(
                response["error"]["message"],
                "Block not available (pruned data)"
            );
        }
        let indexed: Value = client
            .json_result_from_call("getblock", r#"["1",1]"#)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(indexed["tx"], json!([coinbase.hash().to_string()]));
        let unspent: Value = client
            .json_result_from_call("gettxout", &output_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(unspent.get("version"), Some(&Value::Null));
        // The consensus amount limit fits in f64's 53-bit significand, so this cast is exact.
        let expected_zec = u64::from(output.value()) as f64 / 100_000_000.0;
        assert_eq!(unspent["value"], json!(expected_zec));
        assert_eq!(unspent["value"], before["value"]);
        assert_eq!(unspent["coinbase"], true);

        let retained_height = TIP - MAX_BLOCK_REORG_HEIGHT;
        let retained = client
            .get_block(i32::try_from(retained_height)?)
            .await
            .map_err(|err| eyre!(err))?
            .expect("the finalized tip body is retained");
        assert_eq!(retained.coinbase_height(), Some(Height(retained_height)));
        // Every NU5 coinbase is V5 here, so the retained finalized tip supplies the legacy-chain
        // check's branch-ID witness. Restart must not need the pruned prefix to reach it.
        assert_eq!(retained.transactions[0].version(), 5);
        assert!(retained.transactions[0].network_upgrade().is_some());

        child.kill(false)?;
        child
            .wait_with_output()?
            .assert_failure()?
            .assert_was_killed()?;
        let mut child = test_dir
            .spawn_child(args!["start"])?
            .with_timeout(TEST_TIMEOUT)?;
        let test_dir = child
            .dir
            .take()
            .expect("retain the state directory for the archive attempt");
        let client = RpcRequestClient::new_with_timeout(rpc_address, TEST_TIMEOUT);
        wait_for_mempool_activation(&client).await?;
        let restarted = pruned_info(&client, FLOOR).await?;
        assert_eq!(restarted["blocks"], TIP);
        assert_eq!(restarted["bestblockhash"], tip_hash.to_string());

        let spend = Transaction::signed_transparent(
            &network,
            Height(TIP + 1),
            &key,
            &[(outpoint, output)],
            &[(address, Amount::try_from(100_000_000)?)],
        )
        .map_err(|err| eyre!(err))?;
        let raw = hex::encode(spend.zcash_serialize_to_vec()?);
        let sent: SendRawTransactionResponse = client
            .json_result_from_call("sendrawtransaction", json!([raw]).to_string())
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(sent.hash(), spend.hash());

        // Long polling waits for template updates, not a guessed delay after mempool admission.
        let template: BlockTemplateResponse =
            tokio::time::timeout(Duration::from_secs(30), async {
                let mut params = "[]".to_owned();
                loop {
                    let template: BlockTemplateResponse = client
                        .json_result_from_call("getblocktemplate", params)
                        .await
                        .map_err(|err| eyre!(err))?;
                    if template
                        .transactions()
                        .iter()
                        .any(|tx| tx.hash() == spend.hash())
                    {
                        return Ok::<_, color_eyre::Report>(template);
                    }
                    params =
                        json!([{"longpollid": template.long_poll_id().to_string()}]).to_string();
                }
            })
            .await
            .map_err(|_| eyre!("the template did not include the pre-floor spend"))??;
        assert_eq!(template.height(), TIP + 1);
        assert_eq!(template.previous_block_hash(), tip_hash);
        // Regtest disables PoW; submitting the populated template exercises full block validation.
        let block = proposal_block_from_template(&template, None, &network)?;
        let block_hash = block.hash();
        client.submit_block(block).await?;
        let mined = client
            .get_block(i32::try_from(TIP + 1)?)
            .await
            .map_err(|err| eyre!(err))?
            .expect("the accepted block is readable");
        assert_eq!(mined.hash(), block_hash);
        assert!(mined
            .transactions
            .iter()
            .any(|tx| tx.hash() == spend.hash()));
        let info = pruned_info(&client, FLOOR + 1).await?;
        assert_eq!(info["blocks"], TIP + 1);
        assert_eq!(info["bestblockhash"], block_hash.to_string());
        let spent: Value = client
            .json_result_from_call("gettxout", &output_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(spent, Value::Null);

        child.kill(false)?;
        child
            .wait_with_output()?
            .assert_failure()?
            .assert_was_killed()?;
        config.state.prune_blocks_older_than = None;
        let expected = StateInitError::PrunedDatabase {
            path: config.state.db_path(
                "state",
                zebra_state::state_database_format_version_in_code().major,
                &network,
            ),
            lowest_retained_height: Height(FLOOR + 1),
        };
        let output = test_dir
            .replace_config(&mut config)?
            .spawn_child(args!["start"])?
            .with_timeout(LAUNCH_DELAY)?
            .wait_with_output()?;
        output.stderr_line_matches(regex::escape(&format!("{expected:?}")))?;
        output.assert_failure()?.assert_was_not_killed()?;
        Ok(())
    })
    .await?
}

/// A pruned node never advertises `NODE_NETWORK`, and never offers peers blocks it can't serve.
///
/// Pruned zcashd-derived nodes have advertised `NODE_NETWORK` and then failed to serve
/// historical blocks, stalling peers that synced from them. This checks the wire behavior
/// before and after deletion with a real handshake.
#[tokio::test(flavor = "multi_thread")]
async fn pruned_node_never_advertises_unservable_blocks() -> Result<()> {
    use std::{collections::HashSet, str::FromStr};

    use tower::{Service, ServiceExt};
    use zebra_chain::block;
    use zebra_network::{
        connect_isolated_tcp_direct, types::PeerServices, Client, InventoryResponse,
        Request as PeerRequest, Response as PeerResponse,
    };

    const RETENTION: u32 = MAX_BLOCK_REORG_HEIGHT + 1;
    const MARGIN: u32 = 10;
    const TIP: u32 = MAX_BLOCK_REORG_HEIGHT + RETENTION + MARGIN;
    const FLOOR: u32 = MARGIN + 1;
    const TEST_TIMEOUT: Duration = Duration::from_secs(300);
    const PEER_TIMEOUT: Duration = Duration::from_secs(30);

    async fn connect(network: &Network, p2p_address: std::net::SocketAddr) -> Result<Client> {
        let client = tokio::time::timeout(
            PEER_TIMEOUT,
            connect_isolated_tcp_direct(network, p2p_address, String::new()),
        )
        .await
        .map_err(|_| eyre!("handshake with the pruned node timed out"))?
        .map_err(|err| eyre!(err))?;
        assert!(
            !client
                .connection_info
                .remote
                .services
                .contains(PeerServices::NODE_NETWORK),
            "a node configured to prune must not advertise NODE_NETWORK",
        );
        Ok(client)
    }

    async fn call(client: &mut Client, request: PeerRequest) -> Result<PeerResponse> {
        tokio::time::timeout(PEER_TIMEOUT, async {
            client.ready().await?.call(request).await
        })
        .await
        .map_err(|_| eyre!("peer request timed out"))?
        .map_err(|err| eyre!("{err:?}"))
    }

    async fn local_services(client: &RpcRequestClient) -> Result<String> {
        let info: Value = client
            .json_result_from_call("getnetworkinfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;
        Ok(info["localservices"]
            .as_str()
            .expect("getnetworkinfo reports localservices")
            .to_owned())
    }

    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        // Logs are filtered to warnings, so reserve both ports up front instead of reading the
        // OS-assigned ports from the startup log.
        let reserve_port = || -> Result<u16> {
            Ok(std::net::TcpListener::bind("127.0.0.1:0")?
                .local_addr()?
                .port())
        };
        let mut config = rpc_port_config(reserve_port()?, false, &network)?;
        let p2p_address: std::net::SocketAddr = ([127, 0, 0, 1], reserve_port()?).into();
        config.network.listen_addr = p2p_address;
        // The test handshakes before and after deletion, both from localhost.
        config.network.max_connections_per_ip = 2;
        config.state.ephemeral = false;
        config.state.prune_blocks_older_than = Some(RETENTION);
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(
            TransparentSigningKey::random()
                .address(network.kind())
                .into(),
        );
        // Avoid filling the child's log pipe while generating thousands of blocks.
        config.tracing.filter = Some("warn".to_owned());
        let rpc_address = config.rpc.listen_addr.expect("the fixture enables RPC");
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?
            .with_timeout(TEST_TIMEOUT)?;
        let rpc = RpcRequestClient::new_with_timeout(rpc_address, TEST_TIMEOUT);
        wait_for_mempool_activation(&rpc).await?;

        // Configuring pruning withdraws the service before anything has been deleted.
        let info: Value = rpc
            .json_result_from_call("getblockchaininfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(info["pruned"], false);
        drop(connect(&network, p2p_address).await?);
        assert_eq!(local_services(&rpc).await?, "0000000000000000");

        let genesis: String = rpc
            .json_result_from_call("getblockhash", "[0]")
            .await
            .map_err(|err| eyre!(err))?;
        let genesis = block::Hash::from_str(&genesis)?;
        let hashes = rpc.generate(TIP).await?;
        let height_of = |hash: &block::Hash| {
            hashes
                .iter()
                .position(|generated| generated == hash)
                .map(|index| index + 1)
        };
        pruned_info(&rpc, FLOOR).await?;

        let mut peer = connect(&network, p2p_address).await?;
        assert_eq!(local_services(&rpc).await?, "0000000000000000");

        // A pruned block is answered with `notfound`, not an empty or partial block.
        let pruned = hashes[0];
        let error = call(
            &mut peer,
            PeerRequest::BlocksByHash(HashSet::from([pruned])),
        )
        .await
        .expect_err("the pruned node must not serve a deleted body");
        assert!(
            error.to_string().contains("NotFoundResponse"),
            "unexpected error for a pruned block: {error}",
        );

        // The connection survives, and retained bodies are still served.
        let retained = hashes[usize::try_from(TIP - MAX_BLOCK_REORG_HEIGHT)? - 1];
        let PeerResponse::Blocks(blocks) = call(
            &mut peer,
            PeerRequest::BlocksByHash(HashSet::from([retained])),
        )
        .await?
        else {
            panic!("block requests return blocks");
        };
        assert!(
            matches!(
                blocks.as_slice(),
                [InventoryResponse::Available((block, _))] if block.hash() == retained
            ),
            "the retained finalized tip must be served",
        );

        // Block inventory never offers hashes below the retained floor. When the gap starts
        // right after the locator, the node sends no inventory, so the request times out.
        match call(
            &mut peer,
            PeerRequest::FindBlocks {
                known_blocks: vec![genesis],
                stop: None,
            },
        )
        .await
        {
            Ok(PeerResponse::BlockHashes(offered)) => {
                for hash in offered {
                    let height = height_of(&hash).expect("offered hashes are on the best chain");
                    assert!(
                        height >= usize::try_from(FLOOR)?,
                        "offered unservable block at height {height}",
                    );
                }
            }
            Ok(response) => panic!("unexpected getblocks response: {response:?}"),
            Err(_) => {}
        }

        child.kill(false)?;
        child
            .wait_with_output()?
            .assert_failure()?
            .assert_was_killed()?;
        Ok(())
    })
    .await?
}
