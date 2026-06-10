//! Standalone watchdog sidecar for Zebra services.
//!
//! Queries the local Zebra and zcashd-compat services and reports their
//! status. Runs either as a one-shot deploy verification (`check`) or as a
//! continuous systemd service (`run`) that reports failure and recovery
//! transitions to Sentry.

mod checks;
mod config;
mod reporting;
mod runner;

use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use config::Config;
use reporting::Reporter;

/// Watchdog sidecar for Zebra services: checks service health and reports
/// statuses to Sentry.
#[derive(Parser, Debug)]
#[command(name = "zebra-watchdog", version)]
struct Cli {
    /// The run mode.
    #[command(subcommand)]
    command: Command,

    /// Shared check configuration.
    #[command(flatten)]
    config: Config,
}

/// Watchdog run modes.
#[derive(Subcommand, Debug)]
enum Command {
    /// Run all checks once, retrying until they pass or the sync check
    /// timeout elapses. Exits 0 on success and 1 on failure.
    ///
    /// This mirrors the behavior of `deploy/zcashd-compat/sync-check.sh`
    /// and is used by the deploy workflow for post-deploy verification.
    Check,

    /// Run all checks continuously on the watchdog interval, reporting
    /// failure and recovery transitions to Sentry. Intended to run under
    /// systemd with `Restart=always`.
    Run,
}

fn main() {
    let cli = Cli::parse();

    let sentry_guard = reporting::init_sentry();
    let sentry_enabled = sentry_guard.is_some();

    init_tracing(sentry_enabled);

    if sentry_enabled {
        tracing::info!("Sentry reporting enabled");
    } else {
        tracing::info!("SENTRY_DSN not set, logging locally only");
    }

    let checks = checks::registry(&cli.config);
    let mut reporter = Reporter::new(sentry_enabled);

    match cli.command {
        Command::Check => {
            let passed = runner::one_shot(&cli.config, &checks, &mut reporter);

            // Flush pending Sentry events before exiting.
            drop(sentry_guard);

            std::process::exit(if passed { 0 } else { 1 });
        }
        Command::Run => runner::run_forever(&cli.config, &checks, &mut reporter),
    }
}

/// Initializes the tracing subscriber, with a Sentry log forwarding layer
/// when Sentry is enabled.
fn init_tracing(sentry_enabled: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_enabled.then(reporting::sentry_tracing_layer))
        .init();
}
