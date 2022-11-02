//! Example of how to run the submit_block test:
//! ZEBRA_CACHED_STATE_DIR=/path/to/zebra/chain cargo test submit_block --features getblocktemplate-rpcs --release  -- --ignored --nocapture

use super::*;

pub(crate) mod submit_block {

    use std::path::PathBuf;

    use color_eyre::eyre::{eyre, Context, Result};

    use futures::TryFutureExt;
    use indexmap::IndexSet;
    use reqwest::Client;
    use tower::{Service, ServiceExt};
    use zebra_chain::{block::Height, parameters::Network, serialization::ZcashSerialize};
    use zebra_state::HashOrHeight;
    use zebra_test::args;

    use crate::common::{
        cached_state::{copy_state_directory, start_state_service_with_cache_dir},
        config::{persistent_test_config, testdir},
        launch::ZebradTestDirExt,
        lightwalletd::random_known_rpc_port_config,
    };

    use super::cached_state::{load_tip_height_from_state_directory, ZEBRA_CACHED_STATE_DIR};

    async fn get_future_block_hex_data(
        network: Network,
        zebrad_state_path: &PathBuf,
    ) -> Result<Option<String>> {
        tracing::info!(
            ?zebrad_state_path,
            "getting cached sync height from ZEBRA_CACHED_STATE_DIR path"
        );

        let cached_sync_height =
            load_tip_height_from_state_directory(network, zebrad_state_path.as_ref()).await?;

        let future_block_height = Height(cached_sync_height.0 + 1);

        tracing::info!(
            ?cached_sync_height,
            ?future_block_height,
            "got cached sync height, copying state dir to tempdir"
        );

        let copied_state_path = copy_state_directory(network, &zebrad_state_path).await?;

        let mut config = persistent_test_config()?;
        config.state.debug_stop_at_height = Some(future_block_height.0);

        let mut child = copied_state_path
            .with_config(&mut config)?
            .spawn_child(args!["start"])?
            .bypass_test_capture(true);

        while child.is_running() {
            tokio::task::yield_now().await;
        }

        let _ = child.kill(true);
        let copied_state_path = child.dir.take().unwrap();

        let (_read_write_state_service, mut state, _latest_chain_tip, _chain_tip_change) =
            start_state_service_with_cache_dir(network, copied_state_path.as_ref()).await?;
        let request = zebra_state::ReadRequest::Block(HashOrHeight::Height(future_block_height));

        let response = state
            .ready()
            .and_then(|ready_service| ready_service.call(request))
            .map_err(|error| eyre!(error))
            .await?;

        let block_hex_data = match response {
            zebra_state::ReadResponse::Block(Some(block)) => {
                hex::encode(block.zcash_serialize_to_vec()?)
            }
            zebra_state::ReadResponse::Block(None) => {
                tracing::info!(
                "Reached the end of the finalized chain, state is missing block at {future_block_height:?}",
            );
                return Ok(None);
            }
            _ => unreachable!("Incorrect response from state service: {response:?}"),
        };

        Ok(Some(block_hex_data))
    }

    #[allow(clippy::print_stderr)]
    pub(crate) async fn run() -> Result<(), color_eyre::Report> {
        let _init_guard = zebra_test::init();

        let mut config = random_known_rpc_port_config(true)?;
        let network = config.network.network;
        let rpc_address = config.rpc.listen_addr.unwrap();

        config.state.cache_dir = match std::env::var_os(ZEBRA_CACHED_STATE_DIR) {
            Some(path) => path.into(),
            None => {
                eprintln!(
                    "skipped submitblock test, \
                     set the {ZEBRA_CACHED_STATE_DIR:?} environment variable to run the test",
                );

                return Ok(());
            }
        };

        // TODO: As part of or as a pre-cursor to issue #5015,
        // - Use only original cached state,
        // - sync until the tip
        // - get first 3 blocks in non-finalized state via getblock rpc calls
        // - restart zebra without peers
        // - submit block(s) above the finalized tip height
        let block_hex_data = get_future_block_hex_data(network, &config.state.cache_dir)
            .await?
            .expect("spawned zebrad in get_future_block_hex_data should live until it gets the next block");

        // Runs the rest of this test without an internet connection
        config.network.initial_mainnet_peers = IndexSet::new();
        config.network.initial_testnet_peers = IndexSet::new();
        config.mempool.debug_enable_at_height = Some(0);

        // We're using the cached state
        config.state.ephemeral = false;

        let mut child = testdir()?
            .with_exact_config(&config)?
            .spawn_child(args!["start"])?
            .bypass_test_capture(true);

        child.expect_stdout_line_matches(&format!("Opened RPC endpoint at {rpc_address}"))?;

        // Create an http client
        let client = Client::new();

        let res = client
            .post(format!("http://{}", &rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "submitblock", "params": ["{block_hex_data}"], "id":123 }}"#
            ))
            .header("Content-Type", "application/json")
            .send()
            .await?;

        assert!(res.status().is_success());
        let res_text = res.text().await?;

        // Test rpc endpoint response
        assert!(res_text.contains(r#""result":"null""#));

        child.kill(false)?;

        let output = child.wait_with_output()?;
        let output = output.assert_failure()?;

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

        Ok(())
    }
}
