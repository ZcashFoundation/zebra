//! The Abscissa component for Zebra's `tracing` implementation.

use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use abscissa_core::{Component, FrameworkError, Shutdown};

use tokio::sync::watch;
use tracing::{field::Visit, Level};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    fmt, layer::SubscriberExt, reload::Handle, util::SubscriberInitExt, EnvFilter, Layer, Registry,
};
use zebra_chain::parameters::Network;

use crate::{
    application::build_version,
    components::tracing::{Config, LogFormat},
};

#[cfg(feature = "flamegraph")]
use super::flame;

// Art generated with these two images.
// Zebra logo: book/theme/favicon.png
// License: MIT or Apache 2.0
//
// Heart image: https://commons.wikimedia.org/wiki/File:Love_Heart_SVG.svg
// Author: Bubinator
// License: Public Domain or Unconditional Use
//
// How to render
//
// Convert heart image to PNG (2000px):
// curl -o heart.svg https://upload.wikimedia.org/wikipedia/commons/4/42/Love_Heart_SVG.svg
// cargo install resvg
// resvg --width 2000 --height 2000 heart.svg heart.png
//
// Then to text (40x20):
// img2txt -W 40 -H 20 -f utf8 -d none heart.png > heart.utf8
// img2txt -W 40 -H 20 -f utf8 -d none favicon.png > logo.utf8
// paste -d "\0" logo.utf8 heart.utf8 > zebra.utf8
static ZEBRA_ART: [u8; include_bytes!("zebra.utf8").len()] = *include_bytes!("zebra.utf8");

/// A type-erased boxed writer that can be sent between threads safely.
pub type BoxWrite = Box<dyn Write + Send + Sync + 'static>;

/// Abscissa component for initializing the `tracing` subsystem
pub struct Tracing {
    /// The installed filter reloading handle, if the `filter-reload` feature is enabled.
    filter_handle: Option<Handle<EnvFilter, Registry>>,

    /// The originally configured filter.
    initial_filter: String,

    /// The installed flame graph collector, if enabled.
    #[cfg(feature = "flamegraph")]
    flamegrapher: Option<flame::Grapher>,

    /// The OpenTelemetry tracer provider, if enabled.
    #[cfg(feature = "opentelemetry")]
    otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,

    /// Drop guard for worker thread of non-blocking logger,
    /// responsible for flushing any remaining logs when the program terminates.
    //
    // Correctness: must be listed last in the struct, so it drops after other drops have logged.
    _guard: Option<WorkerGuard>,
}

impl Tracing {
    /// Try to create a new [`Tracing`] component with the given `config`.
    ///
    /// If `uses_intro` is true, show a welcome message, the `network`,
    /// and the Zebra logo on startup. (If the terminal supports it.)
    //
    // This method should only print to stderr, because stdout is for tracing logs.
    #[allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_in_result)]
    pub fn new(
        network: &Network,
        config: Config,
        uses_intro: bool,
    ) -> Result<Self, FrameworkError> {
        // Only use color if tracing output is being sent to a terminal or if it was explicitly
        // forced to.
        let use_color = config.use_color_stdout();
        let use_color_stderr = config.use_color_stderr();

        let filter = config.filter.clone().unwrap_or_default();
        let flame_root = &config.flamegraph;

        // Only show the intro for user-focused node server commands like `start`
        // Also skip the intro for regtest, since it pollutes the QA test logs,
        // and for JSON logs, since log collectors usually read standard error as well.
        if uses_intro && !network.is_regtest() && config.log_format == LogFormat::Text {
            // If it's a terminal and color escaping is enabled: clear screen and
            // print Zebra logo (here `use_color` is being interpreted as
            // "use escape codes")
            if use_color_stderr {
                // Clear screen
                eprint!("\x1B[2J");
                eprintln!(
                    "{}",
                    std::str::from_utf8(&ZEBRA_ART)
                        .expect("should always work on a UTF-8 encoded constant")
                );
            }

            eprintln!(
                "Thank you for running a {} zebrad {} node!",
                network.lowercase_name(),
                build_version()
            );
            eprintln!(
                "You're helping to strengthen the network and contributing to a social good :)"
            );
        }

        let writer = if let Some(log_file) = config.log_file.as_ref() {
            let text_logs = config.log_format == LogFormat::Text;
            Box::new(open_log_file(log_file, text_logs, uses_intro)?) as BoxWrite
        } else {
            let stdout = std::io::stdout();
            Box::new(stdout) as BoxWrite
        };

        // Builds a lossy NonBlocking logger with a default line limit of 128_000 or an explicit buffer_limit.
        // The write method queues lines down a bounded channel with this capacity to a worker thread that writes to stdout.
        // Increments error_counter and drops lines when the buffer is full.
        let (non_blocking, worker_guard) = NonBlockingBuilder::default()
            .buffered_lines_limit(config.buffer_limit.max(100))
            .finish(writer);

        let (event_format, field_format) = super::format::EventFormat::new(config.log_format);
        let logger = fmt::layer()
            .fmt_fields(field_format)
            .event_format(event_format)
            .with_ansi(use_color)
            .with_writer(non_blocking);
        let warn_error_layer = LastWarnErrorLayer {
            last_warn_error_sender: crate::application::LAST_WARN_ERROR_LOG_SENDER.clone(),
        };

        // The subscriber is built on `Registry`, so individual layers can add their own filters.
        //
        // The configured filter applies to every layer, except in tokio-console builds, where
        // the console layer needs every tokio span and event.
        #[cfg(not(all(feature = "tokio-console", tokio_unstable)))]
        let (subscriber, filter_handle) = {
            let env_filter = EnvFilter::from(&filter);

            // A reloadable filter takes a lock on every filter check, so it is only installed
            // when that feature is selected.
            #[cfg(feature = "filter-reload")]
            let (env_filter, filter_handle) = {
                let (env_filter, handle) = tracing_subscriber::reload::Layer::new(env_filter);
                (env_filter, Some(handle))
            };

            #[cfg(not(feature = "filter-reload"))]
            let filter_handle = None;

            let subscriber = tracing_subscriber::registry()
                .with(env_filter)
                .with(logger)
                .with(warn_error_layer)
                .with(ErrorLayer::new(field_format));

            (subscriber, filter_handle)
        };

        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        let (subscriber, filter_handle) = {
            // TODO: find out why crawl_and_dial and try_to_sync evade this filter,
            //       and why they also don't get the global net/commit span.
            // Note: this might have been fixed by tracing 0.3.15, or by recent Zebra refactors.
            let subscriber = tracing_subscriber::registry()
                .with(logger.with_filter(EnvFilter::from(&filter)))
                .with(warn_error_layer.with_filter(EnvFilter::from(&filter)))
                .with(ErrorLayer::new(field_format).with_filter(EnvFilter::from(&filter)));

            (subscriber, None)
        };

        // Add optional layers based on dynamic and compile-time configs

        // Add a flamegraph
        #[cfg(feature = "flamegraph")]
        let (flamelayer, flamegrapher) = if let Some(path) = flame_root {
            let (flamelayer, flamegrapher) = flame::layer(path);

            (Some(flamelayer), Some(flamegrapher))
        } else {
            (None, None)
        };
        #[cfg(feature = "flamegraph")]
        let subscriber = subscriber.with(flamelayer);

        #[cfg(feature = "journald")]
        let journaldlayer = if config.use_journald {
            use abscissa_core::FrameworkErrorKind;

            let layer = tracing_journald::layer()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?;

            #[cfg(all(feature = "tokio-console", tokio_unstable))]
            let layer = layer.with_filter(EnvFilter::from(&filter));

            Some(layer)
        } else {
            None
        };
        #[cfg(feature = "journald")]
        let subscriber = subscriber.with(journaldlayer);

        #[cfg(feature = "sentry")]
        let sentry_layer = crate::sentry::tracing_layer();
        #[cfg(feature = "sentry")]
        let sentry_installed = sentry_layer.is_some();
        #[cfg(feature = "sentry")]
        let subscriber = subscriber.with(sentry_layer);

        // OpenTelemetry layer - zero overhead when config.opentelemetry_endpoint is None
        #[cfg(feature = "opentelemetry")]
        let (otel_layer, otel_provider, otel_resolved_config) = {
            // Check standard OTEL_* env vars as fallback (lower precedence than config/ZEBRA_*)
            let endpoint = config
                .opentelemetry_endpoint
                .clone()
                .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok());
            let service_name = config
                .opentelemetry_service_name
                .clone()
                .or_else(|| std::env::var("OTEL_SERVICE_NAME").ok());
            let sample_percent = config.opentelemetry_sample_percent;

            // Capture resolved values for logging once the subscriber is installed
            let sampler_problem = if sample_percent.is_none() {
                super::otel::sampler_env_problem(
                    std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref(),
                    std::env::var("OTEL_TRACES_SAMPLER_ARG").ok().as_deref(),
                )
            } else {
                None
            };
            let resolved_config = (
                endpoint.clone(),
                service_name.clone().unwrap_or_else(|| "zebra".to_string()),
                super::otel::sampler_description(sample_percent),
                sampler_problem,
            );

            match super::otel::layer(endpoint.as_deref(), service_name.as_deref(), sample_percent) {
                Ok((layer, provider)) => (layer, provider, resolved_config),
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        "failed to initialize OpenTelemetry, traces will not be exported"
                    );
                    (None, None, resolved_config)
                }
            }
        };
        #[cfg(feature = "opentelemetry")]
        let subscriber = subscriber.with(otel_layer);

        // spawn the console server in the background, and apply the console layer
        // TODO: set Builder::poll_duration_histogram_max() if needed
        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        let subscriber = subscriber.with(console_subscriber::spawn());

        // Initialise the global tracing subscriber
        subscriber.init();

        // Log the tracing stack we just created
        tracing::info!(
            ?filter,
            TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
            LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
            "started tracing component",
        );

        if flame_root.is_some() {
            if cfg!(feature = "flamegraph") {
                info!(flamegraph = ?flame_root, "installed flamegraph tracing layer");
            } else {
                warn!(
                    flamegraph = ?flame_root,
                    "unable to activate configured flamegraph: \
                     enable the 'flamegraph' feature when compiling zebrad",
                );
            }
        }

        if config.use_journald {
            if cfg!(feature = "journald") {
                info!("installed journald tracing layer");
            } else {
                warn!(
                    "unable to activate configured journald tracing: \
                     enable the 'journald' feature when compiling zebrad",
                );
            }
        }

        #[cfg(feature = "sentry")]
        if sentry_installed {
            info!("installed sentry tracing layer");
        }

        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        info!(
            TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
            LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
            "installed tokio-console tracing layer",
        );

        // Log OpenTelemetry status
        #[cfg(feature = "opentelemetry")]
        if otel_provider.is_some() {
            let (ref endpoint, ref service_name, ref sampler, sampler_problem) =
                otel_resolved_config;
            info!(
                ?endpoint,
                %service_name,
                %sampler,
                "installed OpenTelemetry tracing layer",
            );
            if let Some(problem) = sampler_problem {
                warn!(%sampler, "{problem}");
            }
        }

        // Warning if OpenTelemetry is configured but feature not compiled
        #[cfg(not(feature = "opentelemetry"))]
        if config.opentelemetry_endpoint.is_some() {
            warn!(
                endpoint = ?config.opentelemetry_endpoint,
                "unable to activate OpenTelemetry tracing: \
                 enable the 'opentelemetry' feature when compiling zebrad",
            );
        }

        // Write any progress reports sent by other tasks to the terminal
        //
        // TODO: move this to its own module?
        #[cfg(feature = "progress-bar")]
        if let Some(progress_bar_config) = config.progress_bar.as_ref() {
            use howudoin::consumers::TermLine;
            use std::time::Duration;

            // Stops flickering during the initial sync.
            const PROGRESS_BAR_DEBOUNCE: Duration = Duration::from_secs(2);

            let terminal_consumer = TermLine::with_debounce(PROGRESS_BAR_DEBOUNCE);
            howudoin::init(terminal_consumer);

            info!(?progress_bar_config, "activated progress bars");
        } else {
            info!(
                "set 'tracing.progress_bar =\"summary\"' in zebrad.toml to activate progress bars"
            );
        }

        Ok(Self {
            filter_handle,
            initial_filter: filter,
            #[cfg(feature = "flamegraph")]
            flamegrapher,
            #[cfg(feature = "opentelemetry")]
            otel_provider,
            _guard: Some(worker_guard),
        })
    }

    /// Drops guard for worker thread of non-blocking logger,
    /// to flush any remaining logs when the program terminates.
    pub fn shutdown(&mut self) {
        self.filter_handle.take();

        #[cfg(feature = "flamegraph")]
        self.flamegrapher.take();

        #[cfg(feature = "opentelemetry")]
        if let Some(provider) = self.otel_provider.take() {
            if let Err(e) = provider.shutdown() {
                tracing::warn!(?e, "OpenTelemetry shutdown error");
            }
        }

        self._guard.take();
    }

    /// Return the currently-active tracing filter.
    pub fn filter(&self) -> String {
        if let Some(filter_handle) = self.filter_handle.as_ref() {
            filter_handle
                .with_current(|filter| filter.to_string())
                .expect("the subscriber is not dropped before the component is")
        } else {
            self.initial_filter.clone()
        }
    }

    /// Reload the currently-active filter with the supplied value.
    ///
    /// This can be used to provide a dynamic tracing filter endpoint.
    pub fn reload_filter(&self, filter: impl Into<EnvFilter>) {
        let filter = filter.into();

        if let Some(filter_handle) = self.filter_handle.as_ref() {
            tracing::info!(
                ?filter,
                TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
                LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
                "reloading tracing filter",
            );

            filter_handle
                .reload(filter)
                .expect("the subscriber is not dropped before the component is");
        } else {
            tracing::warn!(
                ?filter,
                TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
                LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
                "attempted to reload tracing filter, but filter reloading is disabled",
            );
        }
    }
}

/// Opens `log_file` for appending, and creates its directory if it doesn't exist.
///
/// Plain text notices are printed only with text logs, because log collectors usually read
/// standard output and standard error as well. Opening the file reports any error.
///
/// # Security
///
/// If the user is running Zebra with elevated permissions ("root"), they should create the log
/// file directory before running Zebra, and make sure the Zebra user account has exclusive
/// access to that directory, and other users can't modify its parent directories.
///
/// This avoids a TOCTOU security issue in the Rust filesystem API.
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn open_log_file(log_file: &Path, text_logs: bool, uses_intro: bool) -> std::io::Result<File> {
    // If the log is configured in the current directory, it won't have a parent directory.
    let missing_dir = log_file.parent().filter(|dir| !dir.exists());

    if let Some(log_file_dir) = missing_dir {
        if text_logs {
            eprintln!("Directory for log file {log_file:?} does not exist, trying to create it...");
        }

        let created = fs::create_dir_all(log_file_dir);
        if let (Err(create_dir_error), true) = (created, text_logs) {
            eprintln!("Failed to create directory for log file: {create_dir_error}");
            eprintln!("Trying log file anyway...");
        }
    }

    if uses_intro && text_logs {
        // We want this to appear on stdout instead of the usual log messages.
        println!("Sending logs to {log_file:?}...");
    }

    File::options().append(true).create(true).open(log_file)
}

impl std::fmt::Debug for Tracing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracing").finish()
    }
}

impl<A: abscissa_core::Application> Component<A> for Tracing {
    fn id(&self) -> abscissa_core::component::Id {
        abscissa_core::component::Id::new("zebrad::components::tracing::component::Tracing")
    }

    fn version(&self) -> abscissa_core::Version {
        build_version()
    }

    fn before_shutdown(&self, _kind: Shutdown) -> Result<(), FrameworkError> {
        #[cfg(feature = "flamegraph")]
        if let Some(ref grapher) = self.flamegrapher {
            use abscissa_core::FrameworkErrorKind;

            info!("writing flamegraph");

            grapher
                .write_flamegraph()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?
        }

        #[cfg(feature = "progress-bar")]
        howudoin::disable();

        Ok(())
    }
}

impl Drop for Tracing {
    fn drop(&mut self) {
        #[cfg(feature = "progress-bar")]
        howudoin::disable();
    }
}

// Visitor to extract only the "message" field from a log event.
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

// Layer to store the last WARN or ERROR log event.
#[derive(Debug, Clone)]
struct LastWarnErrorLayer {
    last_warn_error_sender: watch::Sender<Option<(String, Level, chrono::DateTime<chrono::Utc>)>>,
}

impl<S> Layer<S> for LastWarnErrorLayer
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        let timestamp = chrono::Utc::now();

        if level == Level::WARN || level == Level::ERROR {
            let mut visitor = MessageVisitor { message: None };
            event.record(&mut visitor);

            if let Some(message) = visitor.message {
                let _ = self
                    .last_warn_error_sender
                    .send(Some((message, level, timestamp)));
            }
        }
    }
}
