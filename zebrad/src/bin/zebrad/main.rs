//! Main entry point for Zebrad

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    if cfg!(feature = "enable-sentry") {
        // The Sentry default config pulls in the DSN from the `SENTRY_DSN`
        // environment variable.
        let _guard = sentry::init(());
    }

    abscissa_core::boot(&APPLICATION);
}
