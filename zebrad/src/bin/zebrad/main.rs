//! Main entry point for Zebrad

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    if cfg!(feature = "enable-sentry") {
        let tracing_integration = sentry_tracing::TracingIntegration::default();

        // The Sentry default config pulls in the DSN from the `SENTRY_DSN`
        // environment variable.
        std::mem::forget(sentry::init(
            sentry::ClientOptions {
                debug: true,
                ..Default::default()
            }
            .add_integration(tracing_integration),
        ));
    }

    abscissa_core::boot(&APPLICATION);
}
