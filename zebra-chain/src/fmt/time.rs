//! Human-readable formats for times and durations.

use std::time::Duration;

/// Returns a human-friendly formatted string for `duration.as_secs()`.
pub fn humantime_seconds(duration: impl Into<Duration>) -> String {
    let duration = duration.into();

    // Truncate fractional seconds.
    let duration = Duration::from_secs(duration.as_secs());

    let duration = humantime::format_duration(duration);

    format!("{}", duration)
}
