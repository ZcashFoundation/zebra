use tracing_log::LogTracer;
use tracing_subscriber::{fmt::Formatter, reload::Handle, EnvFilter, FmtSubscriber};

use abscissa_core::{Component, FrameworkError, FrameworkErrorKind};

/// Abscissa component for initializing the `tracing` subsystem
#[derive(Component, Debug)]
pub struct Tracing {
    filter_handle: Handle<EnvFilter, Formatter>,
}

impl Tracing {
    /// Try to create a new [`Tracing`] component with the given `filter`.
    pub fn new(filter: String) -> Result<Self, FrameworkError> {
        // Configure log/tracing interoperability by setting a `LogTracer` as
        // the global logger for the log crate, which converts all log events
        // into tracing events.
        LogTracer::init().map_err(|e| FrameworkErrorKind::ComponentError.context(e))?;

        // Construct a tracing subscriber with the supplied filter and enable reloading.
        let builder = FmtSubscriber::builder()
            .with_ansi(true)
            .with_env_filter(filter)
            .with_filter_reloading();
        let filter_handle = builder.reload_handle();
        let subscriber = builder.finish();

        // Now set it as the global tracing subscriber and save the handle.
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?;

        Ok(Self { filter_handle })
    }

    /// Return the currently-active tracing filter.
    pub fn filter(&self) -> String {
        self.filter_handle
            .with_current(|filter| filter.to_string())
            .expect("the subscriber is not dropped before the component is")
    }

    /// Reload the currently-active filter with the supplied value.
    ///
    /// This can be used to provide a dynamic tracing filter endpoint.
    pub fn reload_filter(&mut self, filter: impl Into<EnvFilter>) {
        self.filter_handle
            .reload(filter)
            .expect("the subscriber is not dropped before the component is");
    }
}
