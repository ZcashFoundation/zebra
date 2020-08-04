//! Miscellaneous test code for Zebra.
use std::sync::Once;
use tracing_error::ErrorLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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
            .install()
            .unwrap();
    })
}
