//! Tracing the execution time of functions.
//!
//! TODO: also trace polling time for futures, using a `Future` wrapper

use std::time::{Duration, Instant};

use crate::fmt::humantime_milliseconds;

/// The default minimum info-level message time.
pub const DEFAULT_MIN_INFO_TIME: Duration = Duration::from_secs(5);

/// The default minimum warning message time.
pub const DEFAULT_MIN_WARN_TIME: Duration = Duration::from_secs(20);

/// A guard that logs code execution time when dropped.
#[derive(Debug)]
pub struct CodeTimer {
    /// The time that the code started executing.
    start: Instant,

    /// The minimum duration for info-level messages.
    min_info_time: Duration,

    /// The minimum duration for warning messages.
    min_warn_time: Duration,
}

impl CodeTimer {
    /// Start timing the execution of a function, method, or other code region.
    ///
    /// Returns a guard that finishes timing the code when dropped,
    /// or when [`CodeTimer::finish()`] is called.
    #[track_caller]
    pub fn start() -> Self {
        trace!("starting code timer",);

        Self {
            start: Instant::now(),
            min_info_time: DEFAULT_MIN_INFO_TIME,
            min_warn_time: DEFAULT_MIN_WARN_TIME,
        }
    }

    /// Finish timing the execution of a function, method, or other code region.
    #[track_caller]
    pub fn finish(self) {
        std::mem::drop(self);
    }
}

impl Drop for CodeTimer {
    #[track_caller]
    fn drop(&mut self) {
        let execution = self.start.elapsed();
        let execution_time = humantime_milliseconds(execution);

        if execution >= self.min_warn_time {
            warn!(
                ?execution_time,
                start_time = ?self.start,
                "code took a long time to execute",
            );
        } else if execution >= self.min_info_time {
            info!(
                ?execution_time,
                start_time = ?self.start,
                "code took longer than expected to execute",
            );
        } else {
            trace!(
                ?execution_time,
                start_time = ?self.start,
                "finishing code timer",
            );
        }
    }
}
