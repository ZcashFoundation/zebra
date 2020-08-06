//! Zebrad Abscissa Application

use crate::{
    commands::ZebradCmd,
    components::tracing::{FlameGrapher, Tracing},
    config::ZebradConfig,
};
use abscissa_core::{
    application::{self, AppCell},
    config,
    config::Configurable,
    terminal::component::Terminal,
    Application, Component, EntryPoint, FrameworkError, Shutdown, StandardPaths,
};
use application::fatal_error;
use std::{fmt, process};

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
    /// the application exits
    flame_guard: Option<FlameGrapher>,

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

    /// Returns the framework components used by this application.
    fn framework_components(
        &mut self,
        command: &Self::Cmd,
    ) -> Result<Vec<Box<dyn Component<Self>>>, FrameworkError> {
        let terminal = Terminal::new(self.term_colors(command));
        // This MUST happen after `Terminal::new` to ensure our preferred panic
        // handler is the last one installed
        color_eyre::install().unwrap();

        Ok(vec![Box::new(terminal)])
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

        let cfg_ref = self
            .config
            .as_ref()
            .expect("config is loaded before register_components");

        let default_filter = if command.verbose { "info" } else { "warn" };
        let is_server = command
            .command
            .as_ref()
            .map(ZebradCmd::is_server)
            .unwrap_or(false);

        // Launch network endpoints for long-running commands
        if is_server {
            let filter = match cfg_ref.tracing.filter {
                Some(ref filter) => filter.as_str(),
                None => default_filter,
            };
            components.push(Box::new(Tracing::new(filter)?));
            components.push(Box::new(TokioComponent::new()?));
            components.push(Box::new(TracingEndpoint::new(cfg_ref)?));
            components.push(Box::new(MetricsEndpoint::new(cfg_ref)?));
        } else {
            components.push(Box::new(Tracing::new(default_filter)?));
        }

        self.state.components.register(components)
    }

    /// Load this application's configuration and initialize its components.
    fn init(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
        // Load configuration
        let config = command
            .config_path()
            .map(|path| self.load_config(&path))
            .transpose()?
            .unwrap_or_default();

        let config = command.process_config(config)?;
        self.config = Some(config);

        // Create and register components with the application.
        // We do this first to calculate a proper dependency ordering before
        // application configuration is processed
        self.register_components(command)?;

        let config = self.config.take().unwrap();

        // Fire callback regardless of whether any config was loaded to
        // in order to signal state in the application lifecycle
        self.after_config(config)?;

        Ok(())
    }

    /// Post-configuration lifecycle callback.
    ///
    /// Called regardless of whether config is loaded to indicate this is the
    /// time in app lifecycle when configuration would be loaded if
    /// possible.
    fn after_config(&mut self, config: Self::Cfg) -> Result<(), FrameworkError> {
        // Configure components
        self.state.components.after_config(&config)?;
        self.config = Some(config);

        Ok(())
    }

    fn shutdown(&mut self, shutdown: Shutdown) -> ! {
        if let Err(e) = self.state().components.shutdown(self, shutdown) {
            fatal_error(self, &e)
        }

        // Swap out a fake app so we can trigger the destructor on the original
        let _ = std::mem::take(self);

        match shutdown {
            Shutdown::Graceful => process::exit(0),
            Shutdown::Forced => process::exit(1),
            Shutdown::Crash => process::exit(2),
        }
    }
}
