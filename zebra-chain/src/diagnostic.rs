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

    /// `true` if this timer has finished.
    has_finished: bool,
}

impl CodeTimer {
    /// Start timing the execution of a function, method, or other code region.
    ///
    /// Returns a guard that finishes timing the code when dropped,
    /// or when [`CodeTimer::finish()`] is called.
    #[track_caller]
    pub fn start() -> Self {
        let start = Instant::now();
        trace!(
            target: "run time",
            ?start,
            "started code timer",
        );

        Self {
            start,
            min_info_time: DEFAULT_MIN_INFO_TIME,
            min_warn_time: DEFAULT_MIN_WARN_TIME,
            has_finished: false,
        }
    }

    /// Finish timing the execution of a function, method, or other code region.
    pub fn finish<S>(
        mut self,
        module_path: &'static str,
        line: u32,
        description: impl Into<Option<S>>,
    ) where
        S: ToString,
    {
        self.finish_inner(Some(module_path), Some(line), description);
    }

    /// Ignore this timer: it will not check the elapsed time or log any warnings.
    pub fn ignore(mut self) {
        self.has_finished = true;
    }

    /// Finish timing the execution of a function, method, or other code region.
    ///
    /// This private method can be called from [`CodeTimer::finish()`] or `drop()`.
    fn finish_inner<S>(
        &mut self,
        module_path: impl Into<Option<&'static str>>,
        line: impl Into<Option<u32>>,
        description: impl Into<Option<S>>,
    ) where
        S: ToString,
    {
        if self.has_finished {
            return;
        }

        self.has_finished = true;

        let execution = self.start.elapsed();
        let time = duration_short(execution);
        let time = time.as_str();

        let module = module_path.into().unwrap_or_default();

        let line = line.into().map(|line| line.to_string()).unwrap_or_default();
        let line = line.as_str();

        let description = description
            .into()
            .map(|desc| desc.to_string())
            .unwrap_or_default();

        if execution >= self.min_warn_time {
            warn!(
                target: "run time",
                %time,
                %module,
                %line,
                "very long {description}",
            );
        } else if execution >= self.min_info_time {
            info!(
                target: "run time",
                %time,
                %module,
                %line,
                "long {description}",
            );
        } else {
            trace!(
                target: "run time",
                %time,
                %module,
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
