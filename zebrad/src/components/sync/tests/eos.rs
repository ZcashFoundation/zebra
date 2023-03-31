//! Testing the end of support feature.
use std::str::FromStr;

use crate::components::sync;

/// Test that the `end_of_support` function is working as expected.
#[test]
#[should_panic(expected = "Zebra refuses to run if the release date is older than")]
fn end_of_support_panic() {
    let release_date: chrono::DateTime<chrono::Utc> =
        chrono::DateTime::from_str(zebra_network::constants::RELEASE_DATE).unwrap();

    // We are in panic
    let panic = release_date
        .checked_add_days(chrono::Days::new(
            zebra_network::constants::EOS_PANIC_AFTER + 1,
        ))
        .unwrap();

    sync::progress::end_of_support(panic);
}

/// Test that the `end_of_support` function is working as expected.
#[test]
#[tracing_test::traced_test]
 fn end_of_support_function() {
    let release_date: chrono::DateTime<chrono::Utc> =
        chrono::DateTime::from_str(zebra_network::constants::RELEASE_DATE).unwrap();

    // We are away from warn or panic
    let no_warn = release_date
        .checked_add_days(chrono::Days::new(
            zebra_network::constants::EOS_PANIC_AFTER - 10,
        ))
        .unwrap();

    sync::progress::end_of_support(no_warn);
    assert!(logs_contain("Checking if Zebra release is too old"));

    // We are in warn range
    let warn = release_date
        .checked_add_days(chrono::Days::new(
            zebra_network::constants::EOS_PANIC_AFTER - 3,
        ))
        .unwrap();

    sync::progress::end_of_support(warn);
    assert!(logs_contain("Checking if Zebra release is too old"));
    assert!(logs_contain(
        "Your Zebra release is too old and it will stop running in"
    ));

    // Panic is tested in `end_of_support_panic`
}

/// Test that we are never in end of support warning or panic.
#[test]
#[tracing_test::traced_test]
fn end_of_support_date() {
    // We check this with local clock.
    let now = chrono::Utc::now();

    sync::progress::end_of_support(now);
    assert!(logs_contain("Checking if Zebra release is too old"));
    assert!(!logs_contain(
        "Your Zebra release is too old and it will stop running in"
    ));
}

