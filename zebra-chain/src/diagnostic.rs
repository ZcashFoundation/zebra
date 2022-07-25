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
        trace!(?start, "starting code timer");

        Self {
            start,
            min_info_time: DEFAULT_MIN_INFO_TIME,
            min_warn_time: DEFAULT_MIN_WARN_TIME,
            has_finished: false,
        }
    }

    /// Finish timing the execution of a function, method, or other code region.
    #[track_caller]
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

    /// Finish timing the execution of a function, method, or other code region.
    ///
    /// This private method can be called from [`CodeTimer::finish()`] or `drop()`.
    #[track_caller]
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
        let execution_time = humantime_milliseconds(execution);

        let module_path = module_path.into();
        let line = line.into();
        let description = description
            .into()
            .map(|desc| desc.to_string() + " ")
            .unwrap_or_default();

        if execution >= self.min_warn_time {
            warn!(
                ?execution_time,
                ?module_path,
                ?line,
                "{description}code took a long time to execute",
            );
        } else if execution >= self.min_info_time {
            info!(
                ?execution_time,
                ?module_path,
                ?line,
                "{description}code took longer than expected to execute",
            );
        } else {
            trace!(
                ?execution_time,
                ?module_path,
                ?line,
                "{description}code timer finished",
            );
        }
    }
}

impl Drop for CodeTimer {
    #[track_caller]
    fn drop(&mut self) {
        self.finish_inner(None, None, "(dropped, cancelled, or aborted)")
    }
}
