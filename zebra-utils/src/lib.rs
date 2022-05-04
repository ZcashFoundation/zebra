//! Utilities for Zebra development, not for library or application users.
//!
//! Currently this consists only of the zebra-checkpoints binary.
#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_utils")]

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialise tracing using its defaults.
pub fn init_tracing() {
    tracing_subscriber::Registry::default()
        .with(tracing_error::ErrorLayer::default())
        .init();
}
