use std::{
    cmp::Ordering,
    path::PathBuf,
    time::{Duration, Instant},
};

use color_eyre::{
    eyre::{eyre, Result, WrapErr},
    Help,
};
use semver::Version;
use zebra_test::command::ContextFrom;

use zebra_chain::{
    block::Height,
    parameters::Network::{self, *},
};
use zebra_state::{constants::LOCK_FILE_ERROR, state_database_format_version_in_code};
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{default_test_config, persistent_test_config, testdir},
    launch::{spawn_zebrad_without_rpc, ZebradTestDirExt, LAUNCH_DELAY},
    sync::TINY_CHECKPOINT_TIMEOUT,
    test_type::TestType::*,
};

pub const MAX_ASYNC_BLOCKING_TIME: Duration = zebra_test::mock_service::DEFAULT_MAX_REQUEST_DELAY;

#[tokio::test]
async fn db_init_outside_future_executor() -> Result<()> {
    let _init_guard = zebra_test::init();
    let config = default_test_config(&Mainnet);

    let start = Instant::now();

    // This test doesn't need UTXOs to be verified efficiently, because it uses an empty state.
    let db_init_handle = {
        let config = config.clone();
        tokio::spawn(async move {
            zebra_state::init(
                config.state.clone(),
                &config.network.network,
                Height::MAX,
                0,
            )
            .await
        })
    };

    // it's faster to panic if it takes longer than expected, since the executor
    // will wait indefinitely for blocking operation to finish once started
    let block_duration = start.elapsed();
    assert!(
        block_duration <= MAX_ASYNC_BLOCKING_TIME,
        "futures executor was blocked longer than expected ({block_duration:?})",
    );

    db_init_handle.await.map_err(|e| eyre!(e))?;

    Ok(())
}

/// Start 2 zebrad nodes using the same state directory, but different Zcash
/// listener ports. The first node should get exclusive access to the database.
/// The second node will panic with the Zcash state conflict hint added in #1535.
#[test]
fn zebra_state_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // A persistent config has a fixed temp state directory, but asks the OS to
    // automatically choose an unused port
    let mut config = persistent_test_config(&Mainnet)?;
    config.network.cache_dir = false.into();
    let dir_conflict = testdir()?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let contains = if cfg!(unix) {
        let mut dir_conflict_full = PathBuf::new();
        dir_conflict_full.push(dir_conflict.path());
        dir_conflict_full.push("state");
        dir_conflict_full.push(format!(
            "v{}",
            zebra_state::state_database_format_version_in_code().major,
        ));
        dir_conflict_full.push(config.network.network.to_string().to_lowercase());
        format!(
            "Opened Zebra state cache at {}",
            dir_conflict_full.display()
        )
    } else {
        String::from("Opened Zebra state cache at ")
    };

    check_config_conflict(
        dir_conflict.path(),
        regex::escape(&contains).as_str(),
        dir_conflict.path(),
        LOCK_FILE_ERROR.as_str(),
    )?;

    Ok(())
}

/// Launch a node in `first_dir`, wait until it acquires the resource, then launch a node
/// in `second_dir`. Require the second node to exit with `second_stderr_regex` while
/// the first node still holds the resource.
#[tracing::instrument]
pub fn check_config_conflict<T, U>(
    first_dir: T,
    first_stdout_regex: &str,
    second_dir: U,
    second_stderr_regex: &str,
) -> Result<()>
where
    T: ZebradTestDirExt + std::fmt::Debug,
    U: ZebradTestDirExt + std::fmt::Debug,
{
    // Start the first node
    let mut node1 = first_dir
        .spawn_child(args!["start"])?
        .with_timeout(LAUNCH_DELAY);

    // Wait until node1 has used the conflicting resource.
    node1.expect_stdout_line_matches(first_stdout_regex)?;

    // Spawn the second node
    let node2 = second_dir.spawn_child(args!["start"]);
    let (node2, mut node1) = node1.kill_on_error(node2)?;

    // Keep the conflicting resource held until node2 exits by itself.
    let output2 = node2
        .with_timeout(LAUNCH_DELAY)
        .wait_with_output()
        .context_from(&mut node1)?;
    node1.kill(false).context_from(&output2)?;
    let output1 = node1
        .with_timeout(LAUNCH_DELAY)
        .wait_with_output()
        .context_from(&output2)?;

    // Make sure the first node was killed, rather than exiting with an error.
    output1
        .assert_was_killed()
        .warning("Possible port conflict. Are there other zebrad tests running?")
        .context_from(&output2)?;

    // Make sure node2 has the expected resource conflict.
    output2
        .stderr_line_matches(second_stderr_regex)
        .context_from(&output1)?;
    output2
        .assert_failure()?
        .assert_was_not_killed()
        .warning("Possible port conflict. Are there other zebrad tests running?")
        .context_from(&output1)?;

    Ok(())
}

#[test]
fn delete_old_databases() -> Result<()> {
    use std::fs::{canonicalize, create_dir};

    let _init_guard = zebra_test::init();

    let mut config = default_test_config(&Mainnet);
    config.network.cache_dir = false.into();
    let run_dir = testdir()?;
    let cache_dir = run_dir.path().join("state");

    // create cache dir
    create_dir(cache_dir.clone())?;

    // create a v1 dir outside cache dir that should not be deleted
    let outside_dir = run_dir.path().join("v1");
    create_dir(&outside_dir)?;
    assert!(outside_dir.as_path().exists());

    // create a `v1` dir inside cache dir that should be deleted
    let inside_dir = cache_dir.join("v1");
    create_dir(&inside_dir)?;
    let canonicalized_inside_dir = canonicalize(inside_dir.clone()).ok().unwrap();
    assert!(inside_dir.as_path().exists());

    // modify config with our cache dir and not ephemeral configuration
    // (delete old databases function will not run when ephemeral = true)
    config.state.cache_dir = cache_dir;
    config.state.ephemeral = false;

    // run zebra with our config
    let mut child = run_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(LAUNCH_DELAY);

    // delete checker running
    child.expect_stdout_line_matches("checking for old database versions".to_string())?;

    // inside dir was deleted
    child.expect_stdout_line_matches(format!(
        "deleted outdated state database directory.*deleted_db.*=.*{canonicalized_inside_dir:?}"
    ))?;
    assert!(!inside_dir.as_path().exists());

    // deleting old databases task ended
    child.expect_stdout_line_matches("finished old database version cleanup task".to_string())?;

    // outside dir was not deleted
    assert!(outside_dir.as_path().exists());

    // finish
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}

/// Check that new states are created with the current state format version,
/// and that restarting `zebrad` doesn't change the format version.
#[tokio::test]
async fn new_state_format() -> Result<()> {
    for network in Network::iter() {
        state_format_test("new_state_format_test", &network, 2, None).await?;
    }

    Ok(())
}

/// Check that outdated states are updated to the current state format version,
/// and that restarting `zebrad` doesn't change the updated format version.
///
/// TODO: test partial updates, once we have some updates that take a while.
///       (or just add a delay during tests)
#[tokio::test]
async fn update_state_format() -> Result<()> {
    let mut fake_version = state_database_format_version_in_code();
    fake_version.minor = 0;
    fake_version.patch = 0;

    for network in Network::iter() {
        state_format_test("update_state_format_test", &network, 3, Some(&fake_version)).await?;
    }

    Ok(())
}

/// Check that newer state formats are downgraded to the current state format version,
/// and that restarting `zebrad` doesn't change the format version.
///
/// Future version compatibility is a best-effort attempt, this test can be disabled if it fails.
#[tokio::test]
async fn downgrade_state_format() -> Result<()> {
    let mut fake_version = state_database_format_version_in_code();
    fake_version.minor = u16::MAX.into();
    fake_version.patch = 0;

    for network in Network::iter() {
        state_format_test(
            "downgrade_state_format_test",
            &network,
            3,
            Some(&fake_version),
        )
        .await?;
    }

    Ok(())
}

/// Test state format changes, see calling tests for details.
async fn state_format_test(
    base_test_name: &str,
    network: &Network,
    reopen_count: usize,
    fake_version: Option<&Version>,
) -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_name = &format!("{base_test_name}/new");

    // # Create a new state and check it has the current version

    let zebrad = spawn_zebrad_without_rpc(network.clone(), test_name, false, false, None, false)?;

    // Skip the test unless it has the required state and environmental variables.
    let Some(mut zebrad) = zebrad else {
        return Ok(());
    };

    tracing::info!(?network, "running {test_name} using zebrad");

    zebrad.expect_stdout_line_matches("creating new database with the current format")?;
    zebrad.expect_stdout_line_matches("loaded Zebra state cache")?;

    // Give Zebra enough time to actually write the database to disk.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let logs = zebrad.kill_and_return_output(false)?;

    assert!(
        !logs.contains("marked database format as upgraded"),
        "unexpected format upgrade in logs:\n\
         {logs}"
    );
    assert!(
        !logs.contains("marked database format as downgraded"),
        "unexpected format downgrade in logs:\n\
         {logs}"
    );

    let output = zebrad.wait_with_output()?;
    let mut output = output.assert_failure()?;

    let mut dir = output
        .take_dir()
        .expect("dir should not already have been taken");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    // # Apply the fake version if needed
    let mut expect_older_version = false;
    let mut expect_newer_version = false;

    if let Some(fake_version) = fake_version {
        let test_name = &format!("{base_test_name}/apply_fake_version/{fake_version}");
        tracing::info!(?network, "running {test_name} using zebra-state");

        let config = UseAnyState
            .zebrad_config(test_name, false, Some(dir.path()), network)
            .expect("already checked config")?;

        zebra_state::write_state_database_format_version_to_disk(
            &config.state,
            fake_version,
            network,
        )
        .expect("can't write fake database version to disk");

        // Give zebra_state enough time to actually write the database version to disk.
        tokio::time::sleep(Duration::from_secs(1)).await;

        let running_version = state_database_format_version_in_code();

        match fake_version.cmp(&running_version) {
            Ordering::Less => expect_older_version = true,
            Ordering::Equal => {}
            Ordering::Greater => expect_newer_version = true,
        }
    }

    // # Reopen that state and check the version hasn't changed

    for reopened in 0..reopen_count {
        let test_name = &format!("{base_test_name}/reopen/{reopened}");

        if reopened > 0 {
            expect_older_version = false;
            expect_newer_version = false;
        }

        let mut zebrad =
            spawn_zebrad_without_rpc(network.clone(), test_name, false, false, dir, false)?
                .expect("unexpectedly missing required state or env vars");

        tracing::info!(?network, "running {test_name} using zebrad");

        if expect_older_version {
            zebrad.expect_stdout_line_matches("trying to open older database format")?;
            zebrad.expect_stdout_line_matches("marked database format as upgraded")?;
            zebrad.expect_stdout_line_matches("database is fully upgraded")?;
        } else if expect_newer_version {
            zebrad.expect_stdout_line_matches("trying to open newer database format")?;
            zebrad.expect_stdout_line_matches("marked database format as downgraded")?;
        } else {
            zebrad.expect_stdout_line_matches("trying to open current database format")?;
            zebrad.expect_stdout_line_matches("loaded Zebra state cache")?;
        }

        // Give Zebra enough time to actually write the database to disk.
        tokio::time::sleep(Duration::from_secs(1)).await;

        let logs = zebrad.kill_and_return_output(false)?;

        if !expect_older_version {
            assert!(
                !logs.contains("marked database format as upgraded"),
                "unexpected format upgrade in logs:\n\
                 {logs}"
            );
        }

        if !expect_newer_version {
            assert!(
                !logs.contains("marked database format as downgraded"),
                "unexpected format downgrade in logs:\n\
                 {logs}"
            );
        }

        let output = zebrad.wait_with_output()?;
        let mut output = output.assert_failure()?;

        dir = output
            .take_dir()
            .expect("dir should not already have been taken");

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;
    }
    Ok(())
}

#[tokio::test]
async fn restores_non_finalized_state_and_commits_new_blocks() -> Result<()> {
    use std::sync::Arc;

    use futures::{stream::FuturesUnordered, StreamExt};

    use zebra_chain::parameters::{
        testnet::{ConfiguredCheckpoints, RegtestParameters},
        Network,
    };
    use zebra_chain::serialization::ZcashSerialize;
    use zebra_node_services::rpc_client::RpcRequestClient;
    use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;

    use crate::common::{
        config::{os_assigned_rpc_port_config, read_listen_addr_from_logs},
        regtest::MiningRpcMethods,
    };

    let network = Network::new_regtest(Default::default());

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.state.ephemeral = false;
    let test_dir = testdir()?.with_config(&mut config)?;

    // Start Zebra and generate some blocks.

    tracing::info!("starting Zebra and generating some blocks");
    let mut child = test_dir
        .spawn_child(args!["start"])?
        .with_timeout(Duration::from_secs(30));
    // Avoid dropping the test directory and cleanup of the state cache needed by the next zebrad instance.
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Startup has its own 30-second bound; generation, backup observation and cleanup need longer.
    let mut child = child.with_timeout(TINY_CHECKPOINT_TIMEOUT + LAUNCH_DELAY);
    let rpc_client = RpcRequestClient::new(rpc_address);
    let genesis = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(block) = rpc_client.get_block(0).await.map_err(|err| eyre!(err))? {
                return Ok::<_, color_eyre::Report>(block);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for committed genesis body")??;
    assert_eq!(genesis.hash(), network.genesis_hash());
    let generated_block_hashes = rpc_client.generate(50).await?;
    let mut checkpointed_blocks = Vec::new();
    for height in 1..=50 {
        checkpointed_blocks.push(
            rpc_client
                .get_block(height)
                .await
                .map_err(|err| eyre!(err))?
                .expect("generated block must exist at its height"),
        );
    }

    // Wait for complete backup contents, not an assumed number of backup-task intervals.
    let backup_dir = config
        .state
        .non_finalized_state_backup_dir(&network)
        .expect("persistent state has non-finalized backups enabled");
    tokio::time::timeout(Duration::from_secs(30), async {
        for block in &checkpointed_blocks {
            // Backup filenames use the hash's display byte order, not its serialized bytes.
            let path = backup_dir.join(block.hash().to_string());
            let expected = block.zcash_serialize_to_vec()?;
            loop {
                match tokio::fs::read(&path).await {
                    // Backup files prefix the serialized block with its deferred pool balance.
                    Ok(bytes)
                        if bytes.get(size_of::<zebra_chain::amount::Amount>()..)
                            == Some(expected.as_slice()) =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Ok::<_, color_eyre::Report>(())
    })
    .await
    .wrap_err("timed out waiting for complete non-finalized block backups")??;

    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad to fully terminate")?;
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
    let new_block_hashes = rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");
    assert_eq!(
        rpc_client.blockchain_info().await?.best_block_hash(),
        *new_block_hashes.last().expect("ten blocks were generated"),
        "new blocks must extend the restored chain"
    );

    tracing::info!(
        "restarting Zebra to check that non-finalized state is _not_ restored when \
         the finalized tip is below the max checkpoint height"
    );
    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad to fully terminate")?;

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
        .spawn_child(args!["start"])?
        .with_timeout(TINY_CHECKPOINT_TIMEOUT + LAUNCH_DELAY);
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
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

    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(result) = submit_block_futs.next().await {
            result?;
        }
        Ok::<_, color_eyre::Report>(())
    })
    .await
    .wrap_err("timed out replaying checkpointed blocks")??;
    assert_eq!(
        rpc_client.blockchain_info().await?.best_block_hash(),
        last_hash,
        "replayed checkpointed blocks must reach the original chain tip"
    );

    // Commit some blocks to check that Zebra's state will still commit blocks, and generate enough blocks
    // for Zebra's finalized tip to pass the max checkpoint height.

    rpc_client
        .generate(200)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad process to exit after kill")?;

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

    tracing::info!("checking that Zebra commits blocks with empty non-finalized state");
    rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)
}
