use abscissa_core::{Component, FrameworkError, FrameworkErrorKind, Shutdown};
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    fmt::Formatter, layer::SubscriberExt, reload::Handle, util::SubscriberInitExt, EnvFilter,
    FmtSubscriber,
};

use crate::config::TracingSection;

use super::flame;

/// Abscissa component for initializing the `tracing` subsystem
pub struct Tracing {
    filter_handle: Handle<EnvFilter, Formatter>,
    flamegrapher: Option<flame::Grapher>,
}

impl Tracing {
    /// Try to create a new [`Tracing`] component with the given `filter`.
    pub fn new(config: TracingSection) -> Result<Self, FrameworkError> {
        let filter = config.filter.unwrap_or_else(|| "".to_string());
        let flame_root = &config.flamegraph;

        // Only use color if tracing output is being sent to a terminal
        let use_color = config.use_color && atty::is(atty::Stream::Stdout);

        // Construct a tracing subscriber with the supplied filter and enable reloading.
        let builder = FmtSubscriber::builder()
            .with_ansi(use_color)
            .with_env_filter(filter)
            .with_filter_reloading();
        let filter_handle = builder.reload_handle();

        let (flamelayer, flamegrapher) = if let Some(path) = flame_root {
            let (flamelayer, flamegrapher) = flame::layer(path);
            (Some(flamelayer), Some(flamegrapher))
        } else {
            (None, None)
        };

        let journaldlayer = if config.use_journald {
            let layer = tracing_journald::layer()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?;
            Some(layer)
        } else {
            None
        };

        let subscriber = builder.finish().with(ErrorLayer::default());
        match (flamelayer, journaldlayer) {
            (None, None) => subscriber.init(),
            (Some(layer1), None) => subscriber.with(layer1).init(),
            (None, Some(layer2)) => subscriber.with(layer2).init(),
            (Some(layer1), Some(layer2)) => subscriber.with(layer1).with(layer2).init(),
        };

        Ok(Self {
            filter_handle,
            flamegrapher,
        })
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
    pub fn reload_filter(&self, filter: impl Into<EnvFilter>) {
        self.filter_handle
            .reload(filter)
            .expect("the subscriber is not dropped before the component is");
    }
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
        abscissa_core::Version::parse("1.0.0-alpha.6").unwrap()
    }

    fn before_shutdown(&self, _kind: Shutdown) -> Result<(), FrameworkError> {
        if let Some(ref grapher) = self.flamegrapher {
            tracing::info!("writing flamegraph");
            grapher
                .write_flamegraph()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?
        }
        Ok(())
    }
}
