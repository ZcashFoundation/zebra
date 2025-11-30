//! Ephemeral Tests: Verifies Zebra's ephemeral state behavior.

#![allow(clippy::unwrap_in_result)]

use crate::assert_with_context;

use std::{collections::HashSet, fs};

use crate::common::{
    check::{EphemeralCheck, EphemeralConfig},
    config::{default_test_config, persistent_test_config, testdir},
    launch::{ZebradTestDirExt, EXTENDED_LAUNCH_DELAY},
};

use zebra_chain::parameters::Network::Mainnet;
use zebra_test::{args, prelude::*};

#[test]
fn ephemeral_existing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::ExistingDirectory)
}

#[test]
fn ephemeral_missing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::MissingDirectory)
}

#[test]
fn misconfigured_ephemeral_existing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::ExistingDirectory,
    )
}

#[test]
fn misconfigured_ephemeral_missing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::MissingDirectory,
    )
}

/// Check that the state directory created on disk matches the state config.
///
/// TODO: do a similar test for `network.cache_dir`
#[tracing::instrument]
fn ephemeral(cache_dir_config: EphemeralConfig, cache_dir_check: EphemeralCheck) -> Result<()> {
    use std::io::ErrorKind;

    let _init_guard = zebra_test::init();

    let mut config = default_test_config(&Mainnet)?;
    let run_dir = testdir()?;

    let ignored_cache_dir = run_dir.path().join("state");
    if cache_dir_config == EphemeralConfig::MisconfiguredCacheDir {
        // Write a configuration that sets both the cache_dir and ephemeral options
        config.state.cache_dir.clone_from(&ignored_cache_dir);
    }
    if cache_dir_check == EphemeralCheck::ExistingDirectory {
        // We set the cache_dir config to a newly created empty temp directory,
        // then make sure that it is empty after the test
        fs::create_dir(&ignored_cache_dir)?;
    }

    let mut child = run_dir
        .path()
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let (expected_run_dir_file_names, optional_run_dir_file_names) = match cache_dir_check {
        // we created the state directory, so it should still exist
        EphemeralCheck::ExistingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect("ignored_cache_dir should still exist")
                    .count()
                    == 0,
                &output,
                "ignored_cache_dir not empty for ephemeral {:?} {:?}: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );
            (["state", "zebrad.toml"].iter(), ["network"].iter())
        }

        // we didn't create the state directory, so it should not exist
        EphemeralCheck::MissingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect_err("ignored_cache_dir should not exist")
                    .kind()
                    == ErrorKind::NotFound,
                &output,
                "unexpected creation of ignored_cache_dir for ephemeral {:?} {:?}: the cache dir exists and contains these files: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );

            (["zebrad.toml"].iter(), ["network"].iter())
        }
    };

    let expected_run_dir_file_names: HashSet<std::ffi::OsString> =
        expected_run_dir_file_names.map(Into::into).collect();

    let optional_run_dir_file_names: HashSet<std::ffi::OsString> =
        optional_run_dir_file_names.map(Into::into).collect();

    let run_dir_file_names = run_dir
        .path()
        .read_dir()
        .expect("run_dir should still exist")
        .map(|dir_entry| dir_entry.expect("run_dir is readable").file_name())
        // ignore directory list order, because it can vary based on the OS and filesystem
        .collect::<HashSet<_>>();

    let has_expected_file_paths = expected_run_dir_file_names
        .iter()
        .all(|expected_file_name| run_dir_file_names.contains(expected_file_name));

    let has_only_allowed_file_paths = run_dir_file_names.iter().all(|file_name| {
        optional_run_dir_file_names.contains(file_name)
            || expected_run_dir_file_names.contains(file_name)
    });

    assert_with_context!(
        has_expected_file_paths && has_only_allowed_file_paths,
        &output,
        "run_dir not empty for ephemeral {:?} {:?}: expected {:?}, actual: {:?}",
        cache_dir_config,
        cache_dir_check,
        expected_run_dir_file_names,
        run_dir_file_names
    );

    Ok(())
}

/// Check that the block state and peer list caches are written to disk.
#[test]
fn persistent_mode() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "state directory empty despite persistent state config"
    );

    let cache_dir = testdir.path().join("network");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "network directory empty despite persistent network config"
    );

    Ok(())
}
