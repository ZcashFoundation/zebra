use std::{fs, path::Path, time::Duration};

use color_eyre::eyre::Result;

use zebra_chain::{
    block::Height,
    chain_tip::mock::MockChainTip,
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        Network,
        Network::*,
        NetworkUpgrade,
    },
};
use zebra_test::{args, prelude::*};
use zebrad::components::sync::end_of_support::{
    self, EOS_PANIC_AFTER, EOS_WARN_AFTER, ESTIMATED_RELEASE_HEIGHT,
};

use crate::common::{
    config::{default_test_config, testdir},
    launch::ZebradTestDirExt,
};

/// Check that the end of support code is called at least once.
#[test]
fn end_of_support_is_checked_at_start() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet))?;
    let mut child = testdir
        .spawn_child(args!["start"])?
        .with_timeout(Duration::from_secs(30));

    child.expect_stdout_line_matches("Starting zebrad")?;
    child.expect_stdout_line_matches("Starting end of support task")?;

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

/// Check that Zebra does not depend on any crates from git sources.
#[test]
#[ignore]
fn check_no_git_dependencies() {
    let workspace_cargo_lock_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("zebrad manifest directory should have a workspace root parent")
        .join("Cargo.lock");
    let cargo_lock_contents = fs::read_to_string(workspace_cargo_lock_path)
        .expect("workspace root should have Cargo.lock file");

    if cargo_lock_contents.contains(r#"source = "git+"#) {
        panic!("Cargo.lock includes git sources")
    }
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[should_panic(expected = "Zebra refuses to run if the release date is older than")]
fn end_of_support_panic() {
    // We are in panic
    let panic = end_of_support::estimated_height_after_release(&Network::Mainnet, EOS_PANIC_AFTER);
    let panic = (panic + 1).expect("the support window ends far below Height::MAX");

    end_of_support::check(panic, &Network::Mainnet);
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[tracing_test::traced_test]
fn end_of_support_function() {
    // One day before the warn range opens
    let no_warn =
        end_of_support::estimated_height_after_release(&Network::Mainnet, EOS_WARN_AFTER - 1);

    end_of_support::check(no_warn, &Network::Mainnet);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain("Zebra release is supported"));

    // One day into the warn range
    let warn =
        end_of_support::estimated_height_after_release(&Network::Mainnet, EOS_WARN_AFTER + 1);

    end_of_support::check(warn, &Network::Mainnet);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain(
        "Your Zebra release is too old and it will stop running at block"
    ));

    // Panic is tested in `end_of_support_panic`
}

/// Test that the end of support height is reported on Mainnet and not on test networks.
#[test]
fn end_of_support_height_per_network() {
    // The reported height is the last supported height: `check()` runs at it without panicking
    // and halts one block after it (covered by `end_of_support_panic`).
    let last_supported_height =
        end_of_support::estimated_height_after_release(&Network::Mainnet, EOS_PANIC_AFTER);
    assert_eq!(
        end_of_support::end_of_support_height(&Network::Mainnet),
        Some(last_supported_height)
    );
    end_of_support::check(last_supported_height, &Network::Mainnet);

    // End of support is not enforced on test networks, so no height is reported.
    assert_eq!(
        end_of_support::end_of_support_height(&Network::new_default_testnet()),
        None
    );
}

/// The panic height is the last block within the support window's days of target block time.
#[test]
fn end_of_support_height_counts_whole_days() {
    let release = Height(ESTIMATED_RELEASE_HEIGHT);
    let panic_height = end_of_support::estimated_height_after_release(&Mainnet, EOS_PANIC_AFTER);
    let window = chrono::Duration::days(EOS_PANIC_AFTER.into());

    // Sum each block's target spacing, so this holds whatever the NU7 height is.
    assert!(NetworkUpgrade::duration_between_heights(&Mainnet, release, panic_height) <= window);
    let next_height = (panic_height + 1).expect("far below Height::MAX");
    assert!(NetworkUpgrade::duration_between_heights(&Mainnet, release, next_height) > window);

    // Without an NU7 height every block takes 75 seconds, as in the old constant arithmetic.
    if NetworkUpgrade::Nu7.activation_height(&Mainnet).is_none() {
        assert_eq!(
            panic_height,
            Height(ESTIMATED_RELEASE_HEIGHT + EOS_PANIC_AFTER * 1152)
        );
    }
}

/// With NU7 inside the support window, the panic height follows 25-second blocks after NU7.
#[test]
fn end_of_support_height_follows_nu7_target_spacing() {
    // NU7 activates 30 days (of 75-second blocks) after the release height.
    let nu7 = ESTIMATED_RELEASE_HEIGHT + 30 * 1152;
    let network = Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(ESTIMATED_RELEASE_HEIGHT / 2),
            nu7: Some(nu7),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");

    let panic_height = end_of_support::estimated_height_after_release(&network, EOS_PANIC_AFTER);
    assert_ne!(
        panic_height,
        Height(ESTIMATED_RELEASE_HEIGHT + EOS_PANIC_AFTER * 1152)
    );
    // The blocks up to the one before NU7 take 75 seconds, the rest take 25 seconds.
    let height_after = |days: u32| Height(nu7 - 1 + ((days - 30) * 24 * 60 * 60 + 75) / 25);
    assert_eq!(panic_height, height_after(EOS_PANIC_AFTER));
    assert_eq!(
        end_of_support::estimated_height_after_release(&network, EOS_WARN_AFTER),
        height_after(EOS_WARN_AFTER)
    );
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
