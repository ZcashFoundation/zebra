//! Zebrad Abscissa Application

use crate::{commands::ZebradCmd, components::tracing::Tracing, config::ZebradConfig};
use abscissa_core::{
    application::{self, AppCell},
    config,
    config::Configurable,
    terminal::component::Terminal,
    terminal::ColorChoice,
    Application, Component, EntryPoint, FrameworkError, Shutdown, StandardPaths, Version,
};
use application::fatal_error;
use std::process;

use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::{DATABASE_FORMAT_VERSION, LOCK_FILE_ERROR};

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

/// Returns the zebrad version for this build, in SemVer 2.0 format.
///
/// Includes the git commit and the number of commits since the last version
/// tag, if available.
///
/// For details, see https://semver.org/
pub fn app_version() -> Version {
    const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
    let vergen_git_semver: Option<&str> = option_env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT");

    match vergen_git_semver {
        // change the git semver format to the semver 2.0 format
        Some(mut vergen_git_semver) if !vergen_git_semver.is_empty() => {
            // strip the leading "v", if present
            if &vergen_git_semver[0..1] == "v" {
                vergen_git_semver = &vergen_git_semver[1..];
            }

            // split into tag, commit count, hash
            let rparts: Vec<_> = vergen_git_semver.rsplitn(3, '-').collect();

            match rparts.as_slice() {
                // assume it's a cargo package version or a git tag with no hash
                [_] | [_, _] => vergen_git_semver.parse().unwrap_or_else(|_| {
                    panic!(
                        "VERGEN_GIT_SEMVER without a hash {:?} must be valid semver 2.0",
                        vergen_git_semver
                    )
                }),

                // it's the "git semver" format, which doesn't quite match SemVer 2.0
                [hash, commit_count, tag] => {
                    let semver_fix = format!("{}+{}.{}", tag, commit_count, hash);
                    semver_fix.parse().unwrap_or_else(|_|
                                                      panic!("Modified VERGEN_GIT_SEMVER {:?} -> {:?} -> {:?} must be valid. Note: CARGO_PKG_VERSION was {:?}.",
                                                             vergen_git_semver,
                                                             rparts,
                                                             semver_fix,
                                                             CARGO_PKG_VERSION))
                }

                _ => unreachable!("split is limited to 3 parts"),
            }
        }
        _ => CARGO_PKG_VERSION.parse().unwrap_or_else(|_| {
            panic!(
                "CARGO_PKG_VERSION {:?} must be valid semver 2.0",
                CARGO_PKG_VERSION
            )
        }),
    }
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
    /// Are standard output and standard error both connected to ttys?
    fn outputs_are_ttys() -> bool {
        atty::is(atty::Stream::Stdout) && atty::is(atty::Stream::Stderr)
    }

    /// Returns the git commit for this build, if available.
    ///
    ///
    /// # Accuracy
    ///
    /// If the user makes changes, but does not commit them, the git commit will
    /// not match the compiled source code.
    pub fn git_commit() -> Option<&'static str> {
        const GIT_COMMIT_GCLOUD: Option<&str> = option_env!("SHORT_SHA");
        const GIT_COMMIT_VERGEN: Option<&str> = option_env!("VERGEN_GIT_SHA_SHORT");

        GIT_COMMIT_GCLOUD.or(GIT_COMMIT_VERGEN)
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
            if !Self::outputs_are_ttys() {
                term_colors = ColorChoice::Never;
            }
        }
        let terminal = Terminal::new(term_colors);

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

        let theme = if Self::outputs_are_ttys() && config.tracing.use_color {
            color_eyre::config::Theme::dark()
        } else {
            color_eyre::config::Theme::new()
        };

        // collect the common metadata for the issue URL and panic report,
        // skipping any env vars that aren't present

        let app_metadata = vec![
            // cargo or git tag + short commit
            ("version", app_version().to_string()),
            // config
            ("Zcash network", config.network.network.to_string()),
            // constants
            ("state version", DATABASE_FORMAT_VERSION.to_string()),
        ];

        // git env vars can be skipped if there is no `.git` during the
        // build, so they must all be optional
        let git_metadata: &[(_, Option<_>)] = &[
            ("branch", option_env!("VERGEN_GIT_BRANCH")),
            ("git commit", Self::git_commit()),
            (
                "commit timestamp",
                option_env!("VERGEN_GIT_COMMIT_TIMESTAMP"),
            ),
        ];
        // skip missing metadata
        let git_metadata: Vec<(_, String)> = git_metadata
            .iter()
            .filter_map(|(k, v)| Some((k, (*v)?)))
            .map(|(k, v)| (*k, v.to_string()))
            .collect();

        let build_metadata: Vec<_> = [
            ("target triple", env!("VERGEN_CARGO_TARGET_TRIPLE")),
            ("build profile", env!("VERGEN_CARGO_PROFILE")),
        ]
        .iter()
        .map(|(k, v)| (*k, v.to_string()))
        .collect();

        let panic_metadata: Vec<_> = app_metadata
            .iter()
            .chain(git_metadata.iter())
            .chain(build_metadata.iter())
            .collect();

        let mut builder = color_eyre::config::HookBuilder::default();
        let mut metadata_section = "Metadata:".to_string();
        for (k, v) in panic_metadata {
            builder = builder.add_issue_metadata(k, v.clone());
            metadata_section.push_str(&format!("\n{}: {}", k, &v));
        }

        builder = builder
            .theme(theme)
            .panic_section(metadata_section)
            .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues/new"))
            .issue_filter(|kind| match kind {
                color_eyre::ErrorKind::NonRecoverable(error) => {
                    let error_str = match error.downcast_ref::<String>() {
                        Some(as_string) => as_string,
                        None => return true,
                    };
                    // listener port conflicts
                    if PORT_IN_USE_ERROR.is_match(error_str) {
                        return false;
                    }
                    // RocksDB lock file conflicts
                    if LOCK_FILE_ERROR.is_match(error_str) {
                        return false;
                    }
                    true
                }
                color_eyre::ErrorKind::Recoverable(error) => {
                    // type checks should be faster than string conversions
                    if error.is::<tower::timeout::error::Elapsed>()
                        || error.is::<tokio::time::error::Elapsed>()
                    {
                        return false;
                    }

                    let error_str = error.to_string();
                    !error_str.contains("timed out") && !error_str.contains("duplicate hash")
                }
            });

        // This MUST happen after `Terminal::new` to ensure our preferred panic
        // handler is the last one installed
        let (panic_hook, eyre_hook) = builder.into_hooks();
        eyre_hook.install().unwrap();

        // The Sentry default config pulls in the DSN from the `SENTRY_DSN`
        // environment variable.
        #[cfg(feature = "enable-sentry")]
        let guard = sentry::init(
            sentry::ClientOptions {
                debug: true,
                release: Some(app_version().to_string().into()),
                ..Default::default()
            }
            .add_integration(sentry_tracing::TracingIntegration::default()),
        );

        std::panic::set_hook(Box::new(move |panic_info| {
            let panic_report = panic_hook.panic_report(panic_info);
            eprintln!("{}", panic_report);

            #[cfg(feature = "enable-sentry")]
            {
                let event = crate::sentry::panic_event_from(panic_report);
                sentry::capture_event(event);

                if !guard.close(None) {
                    warn!("unable to flush sentry events during panic");
                }
            }
        }));

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

        // Ignore the tracing filter for short-lived commands
        let mut tracing_config = cfg_ref.tracing.clone();
        if is_server {
            // Override the default tracing filter based on the command-line verbosity.
            tracing_config.filter = tracing_config
                .filter
                .or_else(|| Some(default_filter.to_owned()));
        } else {
            // Don't apply the configured filter for short-lived commands.
            tracing_config.filter = Some(default_filter.to_owned());
            tracing_config.flamegraph = None;
        }
        components.push(Box::new(Tracing::new(tracing_config)?));

        // Activate the global span, so it's visible when we load the other
        // components. Space is at a premium here, so we use an empty message,
        // short commit hash, and the unique part of the network name.
        let net = &self.config.clone().unwrap().network.network.to_string()[..4];
        let global_span = if let Some(git_commit) = ZebradApp::git_commit() {
            error_span!("", zebrad = git_commit, net)
        } else {
            error_span!("", net)
        };

        let global_guard = global_span.enter();
        // leak the global span, to make sure it stays active
        std::mem::forget(global_guard);

        // Launch network and async endpoints only for long-running commands.
        if is_server {
            components.push(Box::new(TokioComponent::new()?));
            components.push(Box::new(TracingEndpoint::new(cfg_ref)?));
            components.push(Box::new(MetricsEndpoint::new(cfg_ref)?));
        }

        self.state.components.register(components)
    }

    /// Load this application's configuration and initialize its components.
    fn init(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
        // Create and register components with the application.
        // We do this first to calculate a proper dependency ordering before
        // application configuration is processed
        self.register_components(command)?;

        // Fire callback to signal state in the application lifecycle
        let config = self.config.take().unwrap();
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

    fn version(&self) -> Version {
        app_version()
    }
}
