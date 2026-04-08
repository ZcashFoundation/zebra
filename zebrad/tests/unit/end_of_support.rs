use std::{fs, time::Duration};

use color_eyre::eyre::Result;

use zebra_chain::{
    block::Height,
    chain_tip::mock::MockChainTip,
    parameters::{Network, Network::*},
};
use zebra_test::{args, prelude::*};
use zebrad::components::sync::end_of_support::{self, EOS_PANIC_AFTER, ESTIMATED_RELEASE_HEIGHT};

use crate::common::{
    config::{default_test_config, testdir},
    launch::ZebradTestDirExt,
};

// Estimated blocks per day with the current 75 seconds block spacing.
const ESTIMATED_BLOCKS_PER_DAY: u32 = 1152;

/// Check that the end of support code is called at least once.
#[test]
fn end_of_support_is_checked_at_start() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;
    let mut child = testdir.spawn_child(args!["start"])?;

    // Give enough time to start up the eos task.
    std::thread::sleep(Duration::from_secs(30));

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Zebra started
    output.stdout_line_contains("Starting zebrad")?;

    // End of support task started.
    output.stdout_line_contains("Starting end of support task")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

/// Check that Zebra does not depend on any crates from git sources.
#[test]
#[ignore]
fn check_no_git_dependencies() {
    let cargo_lock_contents =
        fs::read_to_string("../Cargo.lock").expect("should have Cargo.lock file in root dir");

    if cargo_lock_contents.contains(r#"source = "git+"#) {
        panic!("Cargo.lock includes git sources")
    }
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[should_panic(expected = "Zebra refuses to run if the release date is older than")]
fn end_of_support_panic() {
    // We are in panic
    let panic = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * ESTIMATED_BLOCKS_PER_DAY) + 1;

    end_of_support::check(Height(panic), &Network::Mainnet);
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[tracing_test::traced_test]
fn end_of_support_function() {
    // We are away from warn or panic
    let no_warn = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * ESTIMATED_BLOCKS_PER_DAY)
        - (30 * ESTIMATED_BLOCKS_PER_DAY);

    end_of_support::check(Height(no_warn), &Network::Mainnet);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain("Zebra release is supported"));

    // We are in warn range
    let warn = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * 1152) - (3 * ESTIMATED_BLOCKS_PER_DAY);

    end_of_support::check(Height(warn), &Network::Mainnet);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain(
        "Your Zebra release is too old and it will stop running at block"
    ));

    // Panic is tested in `end_of_support_panic`
}

/// Test that we are never in end of support warning or panic.
#[test]
#[tracing_test::traced_test]
fn end_of_support_date() {
    // Get the list of checkpoints.
    let list = Network::Mainnet.checkpoint_list();

    // Get the last one we have and use it as tip.
    let higher_checkpoint = list.max_height();

    end_of_support::check(higher_checkpoint, &Network::Mainnet);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(!logs_contain(
        "Your Zebra release is too old and it will stop running in"
    ));
}

/// Check that the end of support task is working.
#[tokio::test]
#[tracing_test::traced_test]
async fn end_of_support_task() -> Result<()> {
    let (latest_chain_tip, latest_chain_tip_sender) = MockChainTip::new();
    latest_chain_tip_sender.send_best_tip_height(Height(10));

    let eos_future = end_of_support::start(Network::Mainnet, latest_chain_tip);

    tokio::time::timeout(Duration::from_secs(15), eos_future)
        .await
        .expect_err(
            "end of support task unexpectedly exited: it should keep running until Zebra exits",
        );

    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));

    assert!(logs_contain("Zebra release is supported"));

    Ok(())
}
