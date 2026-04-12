//! Integration tests for the `zebrad genesis` command.
//!
//! These tests verify that the genesis block generation command works correctly
//! and produces valid genesis blocks that can be submitted to a custom testnet.

use color_eyre::eyre::{eyre, Context, Result};

use zebra_chain::{
    block::Block, parameters::testnet::ConfiguredActivationHeights,
    serialization::ZcashDeserializeInto, work::difficulty::U256,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{client::SubmitBlockResponse, server::OPENED_RPC_ENDPOINT_MSG};
use zebra_test::args;

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

/// Test that the `zebrad genesis` command runs successfully and produces output.
pub fn genesis_command_produces_output() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;

    // Run zebrad genesis with a custom message
    let child = testdir.spawn_child(args![
        "genesis",
        "--message",
        "Integration Test Genesis Block",
        "--difficulty",
        "2007ffff"
    ])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // Verify the command produced expected output
    output.stderr_line_contains("Generating genesis block")?;
    output.stderr_line_contains("Mining genesis block")?;
    output.stderr_line_contains("Genesis block mined successfully")?;
    output.stderr_line_contains("Block hash:")?;
    output.stderr_line_contains("Block size:")?;

    // Verify stdout contains hex data (the genesis block)
    let stdout = String::from_utf8_lossy(&output.output.stdout);
    let block_hex = stdout.trim();

    // Genesis block hex should be non-empty and valid hex
    assert!(
        !block_hex.is_empty(),
        "genesis command should output block hex"
    );
    assert!(
        block_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "genesis output should be valid hex"
    );

    // Verify we can deserialize the block
    let block_bytes = hex::decode(block_hex).context("block hex should be valid")?;
    let block: Block = block_bytes
        .zcash_deserialize_into()
        .context("block should deserialize")?;

    // Verify block structure
    assert_eq!(
        block.transactions.len(),
        1,
        "genesis block should have exactly one transaction"
    );
    assert!(
        block.transactions[0].is_coinbase(),
        "genesis transaction should be coinbase"
    );

    Ok(())
}

/// Test that the genesis command with --output flag writes to a file.
pub fn genesis_command_writes_to_file() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let output_file = testdir.as_ref().join("genesis_block.hex");

    // Run zebrad genesis with output file
    let child = testdir.spawn_child(args![
        "genesis",
        "--message",
        "File Output Test",
        "--difficulty",
        "2007ffff",
        "--output",
        output_file.to_str().expect("path should be valid UTF-8")
    ])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // Verify file was written
    output.stderr_line_contains("Genesis block written to:")?;

    // Verify file exists and contains valid data
    let file_contents =
        std::fs::read_to_string(&output_file).context("should be able to read output file")?;

    assert!(!file_contents.is_empty(), "output file should not be empty");
    assert!(
        file_contents.chars().all(|c| c.is_ascii_hexdigit()),
        "output file should contain valid hex"
    );

    // Verify we can deserialize the block from file
    let block_bytes =
        hex::decode(file_contents.trim()).context("file contents should be valid hex")?;
    let _block: Block = block_bytes
        .zcash_deserialize_into()
        .context("block from file should deserialize")?;

    Ok(())
}

/// Test that a generated genesis block can be submitted to a custom testnet.
///
/// This test:
/// 1. Generates a genesis block with `zebrad genesis`
/// 2. Starts a Zebra node configured for a custom testnet with that genesis hash
/// 3. Submits the genesis block via RPC
/// 4. Verifies the block was accepted
pub async fn genesis_block_can_be_submitted_to_custom_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Step 1: Generate a genesis block
    let genesis_testdir = testdir()?;
    let genesis_output_file = genesis_testdir.as_ref().join("genesis.hex");

    let genesis_child = genesis_testdir.spawn_child(args![
        "genesis",
        "--message",
        "Custom Testnet Integration Test",
        "--difficulty",
        "2007ffff",
        "--output",
        genesis_output_file
            .to_str()
            .expect("path should be valid UTF-8")
    ])?;

    let genesis_output = genesis_child.wait_with_output()?;
    let genesis_output = genesis_output.assert_success()?;

    // Extract the block hash from stderr
    let stderr = String::from_utf8_lossy(&genesis_output.output.stderr);
    let hash_line = stderr
        .lines()
        .find(|line| line.contains("Block hash:"))
        .ok_or_else(|| eyre!("could not find block hash in output"))?;

    let genesis_hash = hash_line
        .split("Block hash:")
        .nth(1)
        .ok_or_else(|| eyre!("could not parse block hash"))?
        .trim();

    // Read the genesis block hex
    let genesis_hex =
        std::fs::read_to_string(&genesis_output_file).context("should read genesis file")?;
    let genesis_hex = genesis_hex.trim();

    // Step 2: Start Zebra with custom testnet configuration
    // Create a custom testnet with the generated genesis hash
    let network = zebra_chain::parameters::testnet::Parameters::build()
        .with_network_name("GenesisIntegrationTest")
        .context("network name should be valid")?
        .with_genesis_hash(genesis_hash)
        .context("genesis hash should be valid")?
        .with_target_difficulty_limit(U256::from_big_endian(&[0x7f; 32]))
        .context("target difficulty limit should be valid")?
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            nu6: Some(1),
            ..Default::default()
        })
        .context("activation heights should be valid")?
        .extend_funding_streams()
        .clear_checkpoints()
        .context("clearing checkpoints should succeed")?
        .with_disable_pow(true)
        .to_network()
        .context("testnet parameters should be valid")?;

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.network.initial_testnet_peers = Default::default();
    config.mempool.debug_enable_at_height = Some(0);

    let zebra_testdir = testdir()?;
    let mut zebrad = zebra_testdir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    let rpc_address = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;

    tokio::time::sleep(LAUNCH_DELAY).await;

    // Step 3: Submit the genesis block
    let client = RpcRequestClient::new(rpc_address);

    let submit_response: SubmitBlockResponse = client
        .json_result_from_call("submitblock", format!(r#"["{genesis_hex}"]"#))
        .await
        .map_err(|err| eyre!("submitblock RPC failed: {err}"))?;

    match submit_response {
        SubmitBlockResponse::Accepted => {}
        SubmitBlockResponse::ErrorResponse(err) => {
            return Err(eyre!("genesis block submission failed: {err:?}"));
        }
    }

    // Step 4: Verify the block was accepted by checking block count
    let block_count: u32 = client
        .json_result_from_call("getblockcount", "[]".to_string())
        .await
        .map_err(|err| eyre!("getblockcount RPC failed: {err}"))?;

    assert_eq!(
        block_count, 0,
        "block count should be 0 after submitting genesis block"
    );

    // Clean up
    zebrad.kill(false)?;
    let output = zebrad.wait_with_output()?;
    output.assert_failure()?.assert_was_killed()?;

    Ok(())
}
