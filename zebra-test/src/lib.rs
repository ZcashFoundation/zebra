//! Miscellaneous test code for Zebra.
#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_test")]
// Each lazy_static variable uses additional recursion
#![recursion_limit = "512"]

use std::sync::Once;

use color_eyre::section::PanicMessage;
use once_cell::sync::Lazy;
use owo_colors::OwoColorize;
use tracing_error::ErrorLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[allow(missing_docs)]
pub mod command;

pub mod mock_service;
pub mod net;
pub mod network_addr;
pub mod prelude;
pub mod service_extensions;
pub mod transcript;
pub mod vectors;
pub mod zip0143;
pub mod zip0243;
pub mod zip0244;

/// A single-threaded Tokio runtime that can be shared between tests.
/// This runtime should be used for tests that need a single thread for consistent timings.
///
/// This shared runtime should be used in tests that use shared background tasks. An example is
/// with shared global `Lazy<BatchVerifier>` types, because they spawn a background task when they
/// are first initialized. This background task is stopped when the runtime is shut down, so having
/// a runtime per test means that only the first test actually manages to successfully use the
/// background task. Using the shared runtime allows the background task to keep running for the
/// other tests that also use it.
///
/// A shared runtime should not be used in tests that need to pause and resume the Tokio timer.
/// This is because multiple tests might be sharing the runtime at the same time, so there could be
/// conflicts with pausing and resuming the timer at incorrect points. Even if only one test runs
/// at a time, there's a risk of a test finishing while the timer is paused (due to a test failure,
/// for example) and that means that the next test will already start with an incorrect timer
/// state.
pub static SINGLE_THREADED_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime")
});

/// A multi-threaded Tokio runtime that can be shared between tests.
/// This runtime should be used for tests that spawn blocking threads.
///
/// See [`SINGLE_THREADED_RUNTIME`] for details.
pub static MULTI_THREADED_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime")
});

static INIT: Once = Once::new();

/// Initialize global and thread-specific settings for tests,
/// such as tracing configs, panic hooks, and `cargo insta` settings.
///
/// This function should be called at the start of every test.
///
/// It returns a drop guard that must be stored in a variable, so that it
/// gets dropped when the test finishes.
#[must_use]
pub fn init() -> impl Drop {
    // Set test mode environment variable
    std::env::set_var("TEST_MODE", "1");

    // Per-test

    // Settings for threads that snapshots data using `insta`

    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let drop_guard = settings.bind_to_scope();

    // Globals

    INIT.call_once(|| {
        let fmt_layer = fmt::layer().with_target(false);
        // Use the RUST_LOG env var, or by default:
        //  - warn for most tests, and
        //  - for some modules, hide expected warn logs
        let filter_layer = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| {
                // These filters apply when RUST_LOG isn't set
                EnvFilter::try_new("warn")
                    .unwrap()
                    .add_directive("zebra_consensus=error".parse().unwrap())
                    .add_directive("zebra_network=error".parse().unwrap())
                    .add_directive("zebra_state=error".parse().unwrap())
                    .add_directive("zebrad=error".parse().unwrap())
                    .add_directive("tor_circmgr=error".parse().unwrap())
            })
            // These filters apply on top of RUST_LOG.
            // Avoid adding filters to this list, because users can't override them.
            //
            // (There are currently no always-on directives.)
            ;

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
    });

    drop_guard
}

/// Initialize globals for tests that need a separate Tokio runtime instance.
///
/// This is generally used in proptests, which don't support the `#[tokio::test]` attribute.
///
/// If a runtime needs to be shared between tests, use the [`SINGLE_THREADED_RUNTIME`] or
/// [`MULTI_THREADED_RUNTIME`] instances instead.
///
/// See also the [`init`] function, which is called by this function.
pub fn init_async() -> (tokio::runtime::Runtime, impl Drop) {
    let drop_guard = init();

    (
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime"),
        drop_guard,
    )
}

struct SkipTestReturnedErrPanicMessages;

impl PanicMessage for SkipTestReturnedErrPanicMessages {
    fn display(
        &self,
        pi: &std::panic::PanicHookInfo<'_>,
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
