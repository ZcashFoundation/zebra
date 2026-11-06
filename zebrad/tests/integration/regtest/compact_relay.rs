//! Real-node compact relay, with passive wire observation on opted-in and ordinary connections.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use zebra_chain::{
    block,
    parameters::{
        testnet::{ConfiguredActivationHeights, RegtestParameters},
        Network,
    },
    serialization::ZcashDeserialize,
    transparent,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
use zebra_test::{args, prelude::*};

use super::nu7::{mempool, send, spend};
use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

#[derive(Default)]
struct WireCounts {
    compact: AtomicUsize,
    full: Mutex<HashSet<block::Hash>>,
    experimental: AtomicUsize,
}

/// Forward unchanged bytes, counting complete A-to-peer frames rather than trusting node logs.
async fn observe(
    target: SocketAddr,
) -> Result<(
    SocketAddr,
    Arc<WireCounts>,
    tokio::task::JoinHandle<std::io::Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let counts = Arc::new(WireCounts::default());
    let observed = counts.clone();
    let task = tokio::spawn(async move {
        let (downstream, _) = listener.accept().await?;
        let upstream = TcpStream::connect(target).await?;
        let (mut from_peer, mut to_peer) = downstream.into_split();
        let (mut from_a, mut to_a) = upstream.into_split();
        tokio::try_join!(
            async { tokio::io::copy(&mut from_peer, &mut to_a).await.map(|_| ()) },
            async {
                loop {
                    let mut header = [0; 24];
                    from_a.read_exact(&mut header).await?;
                    let length =
                        usize::try_from(u32::from_le_bytes(header[16..20].try_into().unwrap()))
                            .unwrap();
                    assert!(length <= zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN);
                    let mut body = vec![0; length];
                    from_a.read_exact(&mut body).await?;
                    let command = &header[4..16];
                    if command == b"cmpctblock\0\0" {
                        observed.compact.fetch_add(1, Ordering::SeqCst);
                    }
                    if command == b"block\0\0\0\0\0\0\0" {
                        let block_header =
                            block::Header::zcash_deserialize(body.as_slice()).unwrap();
                        observed.full.lock().unwrap().insert(block_header.hash());
                    }
                    if [
                        b"sendcmpct\0\0\0",
                        b"cmpctblock\0\0",
                        b"getblocktxn\0",
                        b"blocktxn\0\0\0\0",
                    ]
                    .contains(&command.try_into().unwrap())
                    {
                        observed.experimental.fetch_add(1, Ordering::SeqCst);
                    }
                    to_peer.write_all(&header).await?;
                    to_peer.write_all(&body).await?;
                }
                #[allow(unreachable_code)]
                Ok::<_, std::io::Error>(())
            }
        )?;
        Ok(())
    });
    Ok((address, counts, task))
}

async fn at_height(client: &RpcRequestClient, height: u32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let response: serde_json::Value =
                serde_json::from_str(&client.text_from_call("getblockcount", "[]").await?)?;
            if response["result"].as_u64() == Some(u64::from(height)) {
                return Ok(());
            }
            if !response["error"].is_null() && response["error"]["message"] != "No blocks in state"
            {
                return Err(eyre!("getblockcount failed: {}", response["error"]));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opted_in_nodes_use_compact_blocks_and_ordinary_peers_receive_full_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(300), async {
        let network = Network::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(2),
                ..Default::default()
            },
            should_allow_unshielded_coinbase_spends: Some(true),
            ..Default::default()
        });
        let mut config = os_assigned_rpc_port_config(false, &network)?;
        config.network.experimental_early_block_relay = true;
        config.network.max_connections_per_ip = 4;
        config.network.crawl_new_peer_interval = Duration::from_secs(3600);
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(
            transparent::Address::from_script_hash(
                network.kind(),
                hex_literal::hex!("da1745e9b549bd0bfa1a569971c77eba30cd5a4b"),
            )
            .to_string()
            .parse()?,
        );
        let mut node_a = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let p2p_a = read_listen_addr_from_logs(&mut node_a, "Opened Zcash protocol endpoint at ")?;
        let rpc_a = read_listen_addr_from_logs(&mut node_a, OPENED_RPC_ENDPOINT_MSG)?;
        tokio::time::sleep(LAUNCH_DELAY).await;
        let a = RpcRequestClient::new(rpc_a);
        a.generate(102).await?;
        let coinbase = a
            .get_block(1)
            .await
            .map_err(|e| eyre!(e))?
            .expect("the first mined block exists")
            .transactions[0]
            .clone();

        let (link_b, wire_b, proxy_b) = observe(p2p_a).await?;
        config.network.initial_testnet_peers = [link_b.to_string()].into();
        config.network.peerset_initial_target_size = 1;
        // Keep address discovery from opening another loopback connection around the observers.
        config.network.max_connections_per_ip = 1;
        let mut node_b = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let b = RpcRequestClient::new(read_listen_addr_from_logs(
            &mut node_b,
            OPENED_RPC_ENDPOINT_MSG,
        )?);
        at_height(&b, 102).await?;

        let (link_c, wire_c, proxy_c) = observe(p2p_a).await?;
        config.network.initial_testnet_peers = [link_c.to_string()].into();
        config.network.experimental_early_block_relay = false;
        let mut node_c = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let c = RpcRequestClient::new(read_listen_addr_from_logs(
            &mut node_c,
            OPENED_RPC_ENDPOINT_MSG,
        )?);
        at_height(&c, 102).await?;

        let transaction = spend(&coinbase, false, 10_001)?;
        send(&b, &transaction).await?;
        // Ordinary gossip may beat the RPC, or skip a peer busy syncing. Require the same verified
        // mempool contents in either case rather than making this transport test depend on fanout.
        if let Err(error) = send(&a, &transaction).await {
            assert!(
                [
                    "transaction already exists in mempool",
                    "transaction dropped because it is already queued for download"
                ]
                .iter()
                .any(|message| error.to_string().contains(message)),
                "unexpected transaction admission error: {error:?}",
            );
        }
        mempool(&a, &[transaction.hash()]).await?;
        mempool(&b, &[transaction.hash()]).await?;
        let block = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (block, _) = a.block_from_template(&network).await?;
                if block
                    .transactions
                    .iter()
                    .any(|tx| tx.hash() == transaction.hash())
                {
                    return Ok::<_, color_eyre::Report>(block);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        let block_hash = block.hash();
        a.submit_block(block).await?;
        at_height(&b, 103).await?;
        at_height(&c, 103).await?;
        assert!(
            wire_b.compact.load(Ordering::SeqCst) > 0,
            "the opted-in link must carry cmpctblock"
        );
        assert!(
            !wire_b.full.lock().unwrap().contains(&block_hash),
            "a mempool hit must not download the full block"
        );
        assert!(
            wire_c.full.lock().unwrap().contains(&block_hash),
            "the ordinary link must carry the full block"
        );
        assert_eq!(
            wire_c.experimental.load(Ordering::SeqCst),
            0,
            "no experimental command may reach a non-opted-in peer"
        );
        for node in [&mut node_a, &mut node_b, &mut node_c] {
            node.kill(false)?;
        }
        for node in [node_a, node_b, node_c] {
            node.wait_with_output()?
                .assert_failure()?
                .assert_was_killed()?;
        }
        proxy_b.abort();
        proxy_c.abort();
        Ok(())
    })
    .await?
}
