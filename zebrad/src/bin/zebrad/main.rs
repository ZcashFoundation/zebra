//! Main entry point for Zebrad

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

// use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    // if cfg!(feature = "enable-sentry") {

    // The Sentry default config pulls in the DSN from the `SENTRY_DSN`
    // environment variable.
    let _guard = sentry::init(
        sentry::ClientOptions {
            debug: true,
            ..Default::default()
        }, // .add_integration(sentry_tracing::TracingIntegration::default()),
    );
    // }

    let next = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        println!("Caught panic");
        sentry::integrations::panic::panic_handler(info);
        next(info);
    }));

    sentry::capture_message("Hello World!", sentry::Level::Info);

    panic!("Everything is on fire!");

    //abscissa_core::boot(&APPLICATION);
}
