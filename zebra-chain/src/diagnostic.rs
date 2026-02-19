//! Diagnostic types and functions for Zebra:
//! - code performance
//! - task handling
//! - errors and panics

pub mod task;

// Tracing the execution time of functions.
//
// TODO:
// - move this to a `timing` submodule
// - also trace polling time for futures, using a `Future` wrapper

use std::time::{Duration, Instant};

use crate::fmt::duration_short;

/// The default minimum info-level message time.
///
/// This is high enough to ignore most slow code.
pub const DEFAULT_MIN_INFO_TIME: Duration = Duration::from_secs(5 * 60);

/// The default minimum warning message time.
///
/// This is a little lower than the block verify timeout.
pub const DEFAULT_MIN_WARN_TIME: Duration = Duration::from_secs(9 * 60);

/// A guard that logs code execution time when dropped.
#[derive(Debug)]
pub struct CodeTimer {
    /// The time that the code started executing.
    start: Instant,

    /// The minimum duration for info-level messages.
    min_info_time: Duration,

    /// The minimum duration for warning messages.
    min_warn_time: Duration,

    /// The description configured when the timer is started.
    description: &'static str,

    /// `true` if this timer has finished.
    has_finished: bool,
}

impl CodeTimer {
    /// Start timing the execution of a function, method, or other code region.
    ///
    /// See [`CodeTimer::start_desc`] for more details.
    #[track_caller]
    pub fn start() -> Self {
        Self::start_desc("")
    }

    /// Start timing the execution of a function, method, or other code region.
    ///
    /// Returns a guard that finishes timing the code when dropped,
    /// or when [`CodeTimer::finish()`] is called.
    #[track_caller]
    pub fn start_desc(description: &'static str) -> Self {
        let start = Instant::now();
        trace!(
            target: "run time",
            ?start,
            "started code timer",
        );

        Self {
            start,
            description,
            min_info_time: DEFAULT_MIN_INFO_TIME,
            min_warn_time: DEFAULT_MIN_WARN_TIME,
            has_finished: false,
        }
    }

    /// Finish timing the execution of a function, method, or other code region.
    #[track_caller]
    pub fn finish_desc(mut self, description: &'static str) {
        let location = std::panic::Location::caller();
        self.finish_inner(Some(location.file()), Some(location.line()), description);
    }

    /// Ignore this timer: it will not check the elapsed time or log any warnings.
    pub fn ignore(mut self) {
        self.has_finished = true;
    }

    /// Finish timing the execution of a function, method, or other code region.
    pub fn finish_inner(
        &mut self,
        file: impl Into<Option<&'static str>>,
        line: impl Into<Option<u32>>,
        description: &'static str,
    ) {
        if self.has_finished {
            return;
        }

        let description = if description.is_empty() {
            self.description
        } else {
            description
        };

        self.has_finished = true;

        let execution = self.start.elapsed();
        let time = duration_short(execution);
        let time = time.as_str();

        let file = file.into().unwrap_or_default();

        let line = line.into().map(|line| line.to_string()).unwrap_or_default();
        let line = line.as_str();

        if execution >= self.min_warn_time {
            warn!(
                target: "run time",
                %time,
                %file,
                %line,
                "very long {description}",
            );
        } else if execution >= self.min_info_time {
            info!(
                target: "run time",
                %time,
                %file,
                %line,
                "long {description}",
            );
        } else {
            trace!(
                target: "run time",
                %time,
                %file,
                %line,
                "finished {description} code timer",
            );
        }
    }
}

impl Drop for CodeTimer {
    fn drop(&mut self) {
        self.finish_inner(None, None, "(dropped, cancelled, or aborted)")
    }
}
