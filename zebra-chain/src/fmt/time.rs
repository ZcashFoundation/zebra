//! Human-readable formats for times and durations.

use std::time::Duration;

/// The minimum amount of time displayed with only seconds (no milliseconds).
pub const MIN_SECONDS_ONLY_TIME: Duration = Duration::from_secs(5);

/// Returns a human-friendly formatted string for the whole number of seconds in `duration`.
pub fn duration_short(duration: impl Into<Duration>) -> String {
    let duration = duration.into();

    if duration >= MIN_SECONDS_ONLY_TIME {
        humantime_seconds(duration)
    } else {
        humantime_milliseconds(duration)
    }
}

// TODO: rename these functions to duration_*

/// Returns a human-friendly formatted string for the whole number of seconds in `duration`.
pub fn humantime_seconds(duration: impl Into<Duration>) -> String {
    let duration = duration.into();

    // Truncate fractional seconds.
    let duration = Duration::from_secs(duration.as_secs());

    let duration = humantime::format_duration(duration);

    format!("{duration}")
}

/// Returns a human-friendly formatted string for the whole number of milliseconds in `duration`.
pub fn humantime_milliseconds(duration: impl Into<Duration>) -> String {
    let duration = duration.into();

    // Truncate fractional seconds.
    let duration_secs = Duration::from_secs(duration.as_secs());
    let duration_millis = Duration::from_millis(duration.subsec_millis().into());

    let duration = humantime::format_duration(duration_secs + duration_millis);

    format!("{duration}")
}
