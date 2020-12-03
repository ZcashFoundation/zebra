//! Zebrad Abscissa Application

use crate::{commands::ZebradCmd, components::tracing::Tracing, config::ZebradConfig};
use abscissa_core::{
    application::{self, AppCell},
    config,
    config::Configurable,
    terminal::component::Terminal,
    terminal::ColorChoice,
    Application, Component, EntryPoint, FrameworkError, Shutdown, StandardPaths,
};
use application::fatal_error;
use std::process;

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
#[derive(Debug)]
pub struct ZebradApp {
    /// Application configuration.
    config: Option<ZebradConfig>,

    /// Application state.
    state: application::State<Self>,
}

impl ZebradApp {
    pub fn git_commit() -> &'static str {
        const GIT_COMMIT_VERGEN: &str = env!("VERGEN_SHA_SHORT");
        const GIT_COMMIT_GCLOUD: Option<&str> = option_env!("SHORT_SHA");

        GIT_COMMIT_GCLOUD.unwrap_or(GIT_COMMIT_VERGEN)
    }
}

/// Initialize a new application instance.
///
/// By default no configuration is loaded, and the framework state is
/// initialized to a default, empty state (no components, threads, etc).
impl Default for ZebradApp {
    fn default() -> Self {
        Self {
            config: None,
            state: application::State::default(),
        }
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
        // Automatically use color if we're outputting to a terminal
        //
        // The `abcissa` docs claim that abscissa implements `Auto`, but it
        // does not - except in `color_backtrace` backtraces.
        let mut term_colors = self.term_colors(command);
        if term_colors == ColorChoice::Auto {
            // We want to disable colors on a per-stream basis, but that feature
            // can only be implemented inside the terminal component streams.
            // Instead, if either output stream is not a terminal, disable
            // colors.
            //
            // We'd also like to check `config.tracing.use_color` here, but the
            // config has not been loaded yet.
            if !atty::is(atty::Stream::Stdout) || !atty::is(atty::Stream::Stderr) {
                term_colors = ColorChoice::Never;
            }
        }
        let terminal = Terminal::new(term_colors);

        // This MUST happen after `Terminal::new` to ensure our preferred panic
        // handler is the last one installed
        //
        // color_eyre always uses color, but that's an issue we want to solve upstream
        // (color_backtrace automatically disables color if stderr is a file)
        color_eyre::config::HookBuilder::default()
            .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues/new"))
            .add_issue_metadata("version", env!("CARGO_PKG_VERSION"))
            .add_issue_metadata("git commit", Self::git_commit())
            .issue_filter(|kind| match kind {
                color_eyre::ErrorKind::NonRecoverable(_) => true,
                color_eyre::ErrorKind::Recoverable(error) => {
                    !error.is::<tower::timeout::error::Elapsed>()
                }
            })
            .install()
            .unwrap();

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

        // Load config *after* framework components so that we can
        // report an error to the terminal if it occurs.
        let config = command
            .config_path()
            .map(|path| self.load_config(&path))
            .transpose()?
            .unwrap_or_default();

        let config = command.process_config(config)?;
        self.config = Some(config);

        let cfg_ref = self
            .config
            .as_ref()
            .expect("config is loaded before register_components");

        let default_filter = if command.verbose { "debug" } else { "info" };
        let is_server = command
            .command
            .as_ref()
            .map(ZebradCmd::is_server)
            .unwrap_or(false);

        // Launch network endpoints only for long-running commands.
        if is_server {
            // Override the default tracing filter based on the command-line verbosity.
            let mut tracing_config = cfg_ref.tracing.clone();
            tracing_config.filter = tracing_config
                .filter
                .or_else(|| Some(default_filter.to_owned()));

            components.push(Box::new(Tracing::new(tracing_config)?));
            components.push(Box::new(TokioComponent::new()?));
            components.push(Box::new(TracingEndpoint::new(cfg_ref)?));
            components.push(Box::new(MetricsEndpoint::new(cfg_ref)?));
        } else {
            // Don't apply the configured filter for short-lived commands.
            let mut tracing_config = cfg_ref.tracing.clone();
            tracing_config.filter = Some(default_filter.to_owned());
            tracing_config.flamegraph = None;
            components.push(Box::new(Tracing::new(tracing_config)?));
        }

        self.state.components.register(components)
    }

    /// Load this application's configuration and initialize its components.
    fn init(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
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
