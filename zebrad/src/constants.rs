//! Zebrad constants

/// The name of the current Zebra release.
pub const RELEASE_NAME: &str = "Zebra 1.0.0-rc.5";

/// The date of the current Zebra release.
///
/// This must be a valid `chrono::DateTime` with timezone string.
pub const RELEASE_DATE: &str = "2023-02-23 00:00:00 +00:00";

/// The maximum number of days after `RELEASE_DATE` where a Zebra server can be started.
///
/// Notes:
///
/// - Zebra will refuse to start if the current date is bigger than the `RELEASE_DATE` date plus this number of days.
/// - Zebra release periods are of around 2 weeks.
pub const RELEASE_DURATION_DAYS: u64 = 180;

/// A string which is part of the panic that will be displayed if Zebra release is too old.
pub const ZEBRA_PANIC_MESSAGE_HEADER: &str = "Zebra refuses to run";
