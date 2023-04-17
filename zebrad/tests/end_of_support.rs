//! Testing the end of support feature.

use zebra_chain::block::Height;

use zebrad::components::sync;

use zebra_network::constants::{EOS_PANIC_AFTER, ESTIMATED_RELEASE_HEIGHT};

/// Test that the `end_of_support` function is working as expected.
#[test]
#[should_panic(expected = "Zebra refuses to run if the release date is older than")]
fn end_of_support_panic() {
    // We are in panic
    let panic = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * 1152) + 1;

    sync::progress::end_of_support(Height(panic));
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[tracing_test::traced_test]
fn end_of_support_function() {
    // We are away from warn or panic
    let no_warn = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * 1152) - (30 * 1152);

    sync::progress::end_of_support(Height(no_warn));
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain("Zebra release is under support"));

    // We are in warn range
    let warn = ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * 1152) - (3 * 1152);

    sync::progress::end_of_support(Height(warn));
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(logs_contain(
        "Your Zebra release is too old and it will stop running in"
    ));

    // Panic is tested in `end_of_support_panic`
}

/// Test that we are never in end of support warning or panic.
#[test]
#[tracing_test::traced_test]
fn end_of_support_date() {
    //
    let list = zebra_consensus::CheckpointList::new(zebra_chain::parameters::Network::Mainnet);

    //
    let higher_checkpoint = list.max_height();

    sync::progress::end_of_support(higher_checkpoint);
    assert!(logs_contain(
        "Checking if Zebra release is inside support range ..."
    ));
    assert!(!logs_contain(
        "Your Zebra release is too old and it will stop running in"
    ));
}
