//! Zebrad Abscissa Application

use crate::{commands::ZebradCmd, config::ZebradConfig};
use abscissa_core::{
    application::{self, AppCell},
    config,
    terminal::component::Terminal,
    trace::Tracing,
    Application, Component, EntryPoint, FrameworkError, StandardPaths,
};
use std::fmt;

/// Application state
pub static APPLICATION: AppCell<ZebradApp> = AppCell::new();

/// Obtain a read-only (multi-reader) lock on the application state.
///
/// Panics if the application state has not been initialized.
pub fn app_reader() -> application::lock::Reader<ZebradApp> {
    APPLICATION.read()
}

/// Obtain an exclusive mutable lock on the application state.
pub fn app_writer() -> application::lock::Writer<ZebradApp> {
    APPLICATION.write()
}

/// Obtain a read-only (multi-reader) lock on the application configuration.
///
/// Panics if the application configuration has not been loaded.
pub fn app_config() -> config::Reader<ZebradApp> {
    config::Reader::new(&APPLICATION)
}

/// Zebrad Application
pub struct ZebradApp {
    /// Application configuration.
    config: Option<ZebradConfig>,

    /// drop handle for tracing-flame layer to ensure it flushes its buffer when
    /// the applicatoin exits
    flame_guard: Option<Box<dyn Drop + Send + Sync + 'static>>,

    /// Application state.
    state: application::State<Self>,
}

/// Initialize a new application instance.
///
/// By default no configuration is loaded, and the framework state is
/// initialized to a default, empty state (no components, threads, etc).
impl Default for ZebradApp {
    fn default() -> Self {
        Self {
            config: None,
            flame_guard: None,
            state: application::State::default(),
        }
    }
}

impl fmt::Debug for ZebradApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZebraApp")
            .field("config", &self.config)
            .field(
                "flame_guard",
                &self
                    .flame_guard
                    .as_ref()
                    .map(|_| "Some(impl Drop)")
                    .unwrap_or("None"),
            )
            .field("state", &self.state)
            .finish()
    }
}

impl Application for ZebradApp {
    /// Entrypoint command for this application.
    type Cmd = EntryPoint<ZebradCmd>;

    /// Application configuration.
    type Cfg = ZebradConfig;

    /// Paths to resources within the application.
    type Paths = StandardPaths;

    /// Accessor for application configuration.
    fn config(&self) -> &ZebradConfig {
        self.config.as_ref().expect("config not loaded")
    }

    /// Borrow the application state immutably.
    fn state(&self) -> &application::State<Self> {
        &self.state
    }

    /// Borrow the application state mutably.
    fn state_mut(&mut self) -> &mut application::State<Self> {
        &mut self.state
    }

    fn framework_components(
        &mut self,
        command: &Self::Cmd,
    ) -> Result<Vec<Box<dyn Component<Self>>>, FrameworkError> {
        let terminal = Terminal::new(self.term_colors(command));
        let (tracing, guard) = self.tracing_component(command);
        self.flame_guard = Some(Box::new(guard));

        Ok(vec![Box::new(terminal), Box::new(tracing)])
    }

    /// Register all components used by this application.
    ///
    /// If you would like to add additional components to your application
    /// beyond the default ones provided by the framework, this is the place
    /// to do so.
    fn register_components(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
        use crate::components::{
            metrics::MetricsEndpoint, tokio::TokioComponent, tracing::TracingEndpoint,
        };

        let mut components = self.framework_components(command)?;
        components.push(Box::new(TokioComponent::new()?));
        components.push(Box::new(TracingEndpoint::new()?));
        components.push(Box::new(MetricsEndpoint::new()?));

        self.state.components.register(components)
    }

    /// Post-configuration lifecycle callback.
    ///
    /// Called regardless of whether config is loaded to indicate this is the
    /// time in app lifecycle when configuration would be loaded if
    /// possible.
    fn after_config(
        &mut self,
        config: Self::Cfg,
        command: &Self::Cmd,
    ) -> Result<(), FrameworkError> {
        // Configure components
        self.state.components.after_config(&config)?;
        self.config = Some(config);

        let level = self.level(command);
        self.state
            .components
            .get_downcast_mut::<Tracing>()
            .expect("Tracing component should be available")
            .reload_filter(level);

        Ok(())
    }
}

impl ZebradApp {
    fn level(&self, command: &EntryPoint<ZebradCmd>) -> String {
        if let Ok(level) = std::env::var("ZEBRAD_LOG") {
            level
        } else if command.verbose {
            "debug".to_string()
        } else if let Some(ZebradConfig {
            tracing:
                crate::config::TracingSection {
                    filter: Some(filter),
                },
            ..
        }) = &self.config
        {
            filter.clone()
        } else {
            "info".to_string()
        }
    }

    fn tracing_component(&self, command: &EntryPoint<ZebradCmd>) -> (Tracing, impl Drop) {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

        // Construct a tracing subscriber with the supplied filter and enable reloading.
        let builder = tracing_subscriber::FmtSubscriber::builder()
            .with_env_filter(self.level(command))
            .with_filter_reloading();
        let filter_handle = builder.reload_handle();
        let (flame_layer, guard) =
            tracing_flame::FlameLayer::with_file("./tracing.folded").unwrap();
        let flame_layer = flame_layer.filter_empty().collapse_threads();

        builder
            .finish()
            .with(tracing_error::ErrorLayer::default())
            .with(flame_layer)
            .init();

        (filter_handle.into(), guard)
    }
}
