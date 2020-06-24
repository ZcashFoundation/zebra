//! Consensus handling for Zebra.
//!
//! `verify::BlockVerifier` verifies blocks and their transactions, then adds them to
//! `zebra_state::ZebraState`.
//!
//! `mempool::MempoolTransactionVerifier` verifies transactions, and adds them to
//! `mempool::ZebraMempoolState`.
//!
//! Consensus handling is provided using `tower::Service`s, to support backpressure
//! and batch verification.

#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_consensus")]
#![deny(missing_docs)]

pub mod checkpoint;
pub mod mempool;
pub mod verify;

/// Test utility functions
///
/// Submodules have their own specific tests.
#[cfg(test)]
mod tests {
    use std::sync::Once;
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    static LOGGER_INIT: Once = Once::new();

    // TODO(jlusby): Refactor into the zebra-test crate (#515)
    pub(crate) fn install_tracing() {
        LOGGER_INIT.call_once(|| {
            let fmt_layer = fmt::layer().with_target(false);
            let filter_layer = EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("info"))
                .unwrap();

            tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt_layer)
                .with(ErrorLayer::default())
                .init();
        })
    }
}
