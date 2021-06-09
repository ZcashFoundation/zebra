//! Miscellaneous test code for Zebra.
#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_test")]
// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]
// Each lazy_static variable uses additional recursion
#![recursion_limit = "256"]

use color_eyre::section::PanicMessage;
use owo_colors::OwoColorize;
use tracing_error::ErrorLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use std::sync::Once;

#[allow(missing_docs)]
pub mod command;
pub mod prelude;
pub mod transcript;
pub mod vectors;

static INIT: Once = Once::new();

/// Initialize globals for tests such as the tracing subscriber and panic / error
/// reporting hooks
pub fn init() {
    INIT.call_once(|| {
        let fmt_layer = fmt::layer().with_target(false);
        // Use the RUST_LOG env var, or by default:
        //  - warn for most tests, and
        //  - for some modules, hide expected warn logs
        let filter_layer = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::try_new("warn")
                .unwrap()
                .add_directive("zebra_consensus=error".parse().unwrap())
                .add_directive("zebrad=error".parse().unwrap())
        });

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();

        color_eyre::config::HookBuilder::default()
            .add_frame_filter(Box::new(|frames| {
                let mut displayed = std::collections::HashSet::new();
                let filters = &[
                    "tokio::",
                    "<futures_util::",
                    "std::panic",
                    "test::run_test_in_process",
                    "core::ops::function::FnOnce::call_once",
                    "std::thread::local",
                    "<core::future::",
                    "<alloc::boxed::Box",
                    "<std::panic::AssertUnwindSafe",
                    "core::result::Result",
                    "<T as futures_util",
                    "<tracing_futures::Instrumented",
                    "test::assert_test_result",
                    "spandoc::",
                ];

                frames.retain(|frame| {
                    let loc = (frame.lineno, &frame.filename);
                    let inserted = displayed.insert(loc);

                    if !inserted {
                        return false;
                    }

                    !filters.iter().any(|f| {
                        let name = if let Some(name) = frame.name.as_ref() {
                            name.as_str()
                        } else {
                            return true;
                        };

                        name.starts_with(f)
                    })
                });
            }))
            .panic_message(SkipTestReturnedErrPanicMessages)
            .install()
            .unwrap();
    })
}

struct SkipTestReturnedErrPanicMessages;

impl PanicMessage for SkipTestReturnedErrPanicMessages {
    fn display(
        &self,
        pi: &std::panic::PanicInfo<'_>,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let payload = pi
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| pi.payload().downcast_ref::<&str>().cloned())
            .unwrap_or("<non string panic payload>");

        // skip panic output that is clearly from tests that returned an `Err`
        // and assume that the test handler has already printed the value inside
        // of the `Err`.
        if payload.contains("the test returned a termination value with a non-zero status code") {
            return write!(f, "---- end of test output ----");
        }

        writeln!(f, "{}", "\nThe application panicked (crashed).".red())?;

        write!(f, "Message:  ")?;
        writeln!(f, "{}", payload.cyan())?;

        // If known, print panic location.
        write!(f, "Location: ")?;
        if let Some(loc) = pi.location() {
            write!(f, "{}", loc.file().purple())?;
            write!(f, ":")?;
            write!(f, "{}", loc.line().purple())?;
        } else {
            write!(f, "<unknown>")?;
        }

        Ok(())
    }
}
