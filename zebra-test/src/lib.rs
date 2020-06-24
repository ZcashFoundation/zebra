//! Miscellaneous test code for Zebra.
use std::sync::Once;
use tracing_error::ErrorLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

static INIT: Once = Once::new();

/// Initialize globals for tests such as the tracing subscriber and panic / error
/// reporting hooks
pub fn init() {
    INIT.call_once(|| {
        let fmt_layer = fmt::layer().with_target(false);
        let filter_layer = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();

        color_eyre::install().unwrap();
    })
}

pub mod transcript;
pub mod vectors;
