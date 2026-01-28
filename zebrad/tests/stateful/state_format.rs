//! State format tests

#![allow(clippy::unwrap_in_result)]

use std::{cmp::Ordering, time::Duration};

use color_eyre::eyre::WrapErr;
use semver::Version;

use zebra_chain::parameters::Network::{self};
use zebra_state::state_database_format_version_in_code;
use zebra_test::prelude::*;

use crate::common::{launch::spawn_zebrad_without_rpc, test_type::TestType::UseAnyState};

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
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

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
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }
    Ok(())
}
