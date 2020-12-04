//! Main entry point for Zebrad

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    let _guard = sentry::init((
        "https://94059ee72a44420286310990b7c614b5@o485484.ingest.sentry.io/5540918",
        sentry::ClientOptions {
            debug: true,
            ..Default::default()
        },
    ));

    sentry::capture_message("Hello World!", sentry::Level::Info);

    abscissa_core::boot(&APPLICATION);
}
