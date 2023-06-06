//! Zebrad Abscissa Application

use std::{env, fmt::Write as _, io::Write as _, process, sync::Arc};

use abscissa_core::{
    application::{self, AppCell},
    config::CfgCell,
    status_err,
    terminal::{component::Terminal, stderr, stdout, ColorChoice},
    Application, Component, Configurable, FrameworkError, Shutdown, StandardPaths, Version,
};

use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::{DATABASE_FORMAT_VERSION, LOCK_FILE_ERROR};

use crate::{
    commands::EntryPoint,
    components::{sync::end_of_support::EOS_PANIC_MESSAGE_HEADER, tracing::Tracing},
    config::ZebradConfig,
};

/// See <https://docs.rs/abscissa_core/latest/src/abscissa_core/application/exit.rs.html#7-10>
/// Print a fatal error message and exit
fn fatal_error(app_name: String, err: &dyn std::error::Error) -> ! {
    status_err!("{} fatal error: {}", app_name, err);
    process::exit(1)
}

/// Application state
pub static APPLICATION: AppCell<ZebradApp> = AppCell::new();

/// Returns the zebrad version for this build, in SemVer 2.0 format.
///
/// Includes the git commit and the number of commits since the last version
/// tag, if available.
///
/// For details, see <https://semver.org/>
pub fn app_version() -> Version {
    const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
    let vergen_git_describe: Option<&str> = option_env!("VERGEN_GIT_DESCRIBE");

    match vergen_git_describe {
        // change the git describe format to the semver 2.0 format
        Some(mut vergen_git_describe) if !vergen_git_describe.is_empty() => {
            // strip the leading "v", if present
            if &vergen_git_describe[0..1] == "v" {
                vergen_git_describe = &vergen_git_describe[1..];
            }

            // split into tag, commit count, hash
            let rparts: Vec<_> = vergen_git_describe.rsplitn(3, '-').collect();

            match rparts.as_slice() {
                // assume it's a cargo package version or a git tag with no hash
                [_] | [_, _] => vergen_git_describe.parse().unwrap_or_else(|_| {
                    panic!(
                        "VERGEN_GIT_DESCRIBE without a hash {vergen_git_describe:?} must be valid semver 2.0"
                    )
                }),

                // it's the "git describe" format, which doesn't quite match SemVer 2.0
                [hash, commit_count, tag] => {
                    let semver_fix = format!("{tag}+{commit_count}.{hash}");
                    semver_fix.parse().unwrap_or_else(|_|
                                                      panic!("Modified VERGEN_GIT_DESCRIBE {vergen_git_describe:?} -> {rparts:?} -> {semver_fix:?} must be valid. Note: CARGO_PKG_VERSION was {CARGO_PKG_VERSION:?}."))
                }

                _ => unreachable!("split is limited to 3 parts"),
            }
        }
        _ => CARGO_PKG_VERSION.parse().unwrap_or_else(|_| {
            panic!("CARGO_PKG_VERSION {CARGO_PKG_VERSION:?} must be valid semver 2.0")
        }),
    }
}

/// The Zebra current release version.
pub fn release_version() -> String {
    app_version()
        .to_string()
        .split('+')
        .next()
        .expect("always at least 1 slice")
        .to_string()
}

/// The User-Agent string provided by the node.
///
/// This must be a valid [BIP 14] user agent.
///
/// [BIP 14]: https://github.com/bitcoin/bips/blob/master/bip-0014.mediawiki
pub fn user_agent() -> String {
    let release_version = release_version();
    format!("/Zebra:{release_version}/")
}

/// Zebrad Application
#[derive(Debug, Default)]
pub struct ZebradApp {
    /// Application configuration.
    config: CfgCell<ZebradConfig>,

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
        const GIT_COMMIT_VERGEN: Option<&str> = option_env!("VERGEN_GIT_SHA");

        GIT_COMMIT_GCLOUD.or(GIT_COMMIT_VERGEN)
    }
}

impl Application for ZebradApp {
    /// Entrypoint command for this application.
    type Cmd = EntryPoint;

    /// Application configuration.
    type Cfg = ZebradConfig;

    /// Paths to resources within the application.
    type Paths = StandardPaths;

    /// Accessor for application configuration.
    fn config(&self) -> Arc<ZebradConfig> {
        self.config.read()
    }

    /// Borrow the application state immutably.
    fn state(&self) -> &application::State<Self> {
        &self.state
    }

    /// Returns the framework components used by this application.
    fn framework_components(
        &mut self,
        _command: &Self::Cmd,
    ) -> Result<Vec<Box<dyn Component<Self>>>, FrameworkError> {
        // TODO: Open a PR in abscissa to add a TerminalBuilder for opting out
        //       of the `color_eyre::install` part of `Terminal::new` without
        //       ColorChoice::Never?

        // The Tracing component uses stdout directly and will apply colors
        // `if Self::outputs_are_ttys() && config.tracing.use_colors`
        //
        // Note: It's important to use `ColorChoice::Never` here to avoid panicking in
        //       `register_components()` below if `color_eyre::install()` is called
        //       after `color_spantrace` has been initialized.
        let terminal = Terminal::new(ColorChoice::Never);

        Ok(vec![Box::new(terminal)])
    }

    /// Register all components used by this application.
    ///
    /// If you would like to add additional components to your application
    /// beyond the default ones provided by the framework, this is the place
    /// to do so.
    #[allow(clippy::print_stderr)]
    #[allow(clippy::unwrap_in_result)]
    fn register_components(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
        use crate::components::{
            metrics::MetricsEndpoint, tokio::TokioComponent, tracing::TracingEndpoint,
        };

        let mut components = self.framework_components(command)?;

        // Load config *after* framework components so that we can
        // report an error to the terminal if it occurs.
        let config = match command.config_path() {
            Some(path) => match self.load_config(&path) {
                Ok(config) => config,
                Err(e) => {
                    status_err!("Zebra could not parse the provided config file. This might mean you are using a deprecated format of the file. You can generate a valid config by running \"zebrad generate\", and diff it against yours to examine any format inconsistencies.");
                    return Err(e);
                }
            },
            None => ZebradConfig::default(),
        };

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
            ("features", env!("VERGEN_CARGO_FEATURES").to_string()),
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
            ("rust compiler", env!("VERGEN_RUSTC_SEMVER")),
            ("rust release date", env!("VERGEN_RUSTC_COMMIT_DATE")),
            ("optimization level", env!("VERGEN_CARGO_OPT_LEVEL")),
            ("debug checks", env!("VERGEN_CARGO_DEBUG")),
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
            write!(&mut metadata_section, "\n{k}: {}", &v)
                .expect("unexpected failure writing to string");
        }

        builder = builder
            .theme(theme)
            .panic_section(metadata_section.clone())
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
                    // Don't ask users to report old version panics.
                    if error_str.to_string().contains(EOS_PANIC_MESSAGE_HEADER) {
                        return false;
                    }
                    true
                }
                color_eyre::ErrorKind::Recoverable(error) => {
                    // Type checks should be faster than string conversions.
                    //
                    // Don't ask users to create bug reports for timeouts and peer errors.
                    if error.is::<tower::timeout::error::Elapsed>()
                        || error.is::<tokio::time::error::Elapsed>()
                        || error.is::<zebra_network::PeerError>()
                        || error.is::<zebra_network::SharedPeerError>()
                        || error.is::<zebra_network::HandshakeError>()
                    {
                        return false;
                    }

                    // Don't ask users to create bug reports for known timeouts, duplicate blocks,
                    // full disks, or updated binaries.
                    let error_str = error.to_string();
                    !error_str.contains("timed out")
                        && !error_str.contains("duplicate hash")
                        && !error_str.contains("No space left on device")
                        // abscissa panics like this when the running zebrad binary has been updated
                        && !error_str.contains("error canonicalizing application path")
                }
            });

        // This MUST happen after `Terminal::new` to ensure our preferred panic
        // handler is the last one installed
        let (panic_hook, eyre_hook) = builder.into_hooks();
        eyre_hook.install().expect("eyre_hook.install() error");

        // The Sentry default config pulls in the DSN from the `SENTRY_DSN`
        // environment variable.
        #[cfg(feature = "sentry")]
        let guard = sentry::init(sentry::ClientOptions {
            debug: true,
            release: Some(app_version().to_string().into()),
            ..Default::default()
        });

        std::panic::set_hook(Box::new(move |panic_info| {
            let panic_report = panic_hook.panic_report(panic_info);
            eprintln!("{panic_report}");

            #[cfg(feature = "sentry")]
            {
                let event = crate::sentry::panic_event_from(panic_report);
                sentry::capture_event(event);

                if !guard.close(None) {
                    warn!("unable to flush sentry events during panic");
                }
            }
        }));

        // Apply the configured number of threads to the thread pool.
        //
        // TODO:
        // - set rayon panic handler to a function that takes `Box<dyn Any + Send + 'static>`,
        //   which forwards to sentry. If possible, use eyre's panic report for formatting.
        // - do we also need to call this code in `zebra_consensus::init()`,
        //   when that crate is being used by itself?
        rayon::ThreadPoolBuilder::new()
            .num_threads(config.sync.parallel_cpu_threads)
            .thread_name(|thread_index| format!("rayon {thread_index}"))
            .build_global()
            .expect("unable to initialize rayon thread pool");

        let cfg_ref = &config;
        let default_filter = command.cmd().default_tracing_filter(command.verbose);
        let is_server = command.cmd().is_server();

        // Ignore the configured tracing filter for short-lived utility commands
        let mut tracing_config = cfg_ref.tracing.clone();
        let metrics_config = cfg_ref.metrics.clone();
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

        // Log git metadata and platform info when zebrad starts up
        if is_server {
            tracing::info!("Diagnostic {}", metadata_section);
            info!(config_path = ?command.config_path(), config = ?cfg_ref, "loaded zebrad config");
        }

        // Activate the global span, so it's visible when we load the other
        // components. Space is at a premium here, so we use an empty message,
        // short commit hash, and the unique part of the network name.
        let net = &config.network.network.to_string()[..4];
        let global_span = if let Some(git_commit) = ZebradApp::git_commit() {
            error_span!("", zebrad = git_commit, net)
        } else {
            error_span!("", net)
        };

        let global_guard = global_span.enter();
        // leak the global span, to make sure it stays active
        std::mem::forget(global_guard);

        tracing::info!(
            num_threads = rayon::current_num_threads(),
            "initialized rayon thread pool for CPU-bound tasks",
        );

        // Launch network and async endpoints only for long-running commands.
        if is_server {
            components.push(Box::new(TokioComponent::new()?));
            components.push(Box::new(TracingEndpoint::new(cfg_ref)?));
            components.push(Box::new(MetricsEndpoint::new(&metrics_config)?));
        }

        self.state.components_mut().register(components)?;

        // Fire callback to signal state in the application lifecycle
        self.after_config(config)
    }

    /// Load this application's configuration and initialize its components.
    #[allow(clippy::unwrap_in_result)]
    fn init(&mut self, command: &Self::Cmd) -> Result<(), FrameworkError> {
        // Create and register components with the application.
        // We do this first to calculate a proper dependency ordering before
        // application configuration is processed
        self.register_components(command)
    }

    /// Post-configuration lifecycle callback.
    ///
    /// Called regardless of whether config is loaded to indicate this is the
    /// time in app lifecycle when configuration would be loaded if
    /// possible.
    fn after_config(&mut self, config: Self::Cfg) -> Result<(), FrameworkError> {
        // Configure components
        self.state.components_mut().after_config(&config)?;
        self.config.set_once(config);

        Ok(())
    }

    fn shutdown(&self, shutdown: Shutdown) -> ! {
        // Some OSes require a flush to send all output to the terminal.
        // zebrad's logging uses Abscissa, so we flush its streams.
        //
        // TODO:
        // - if this doesn't work, send an empty line as well
        // - move this code to the tracing component's `before_shutdown()`
        let _ = stdout().lock().flush();
        let _ = stderr().lock().flush();

        let shutdown_result = self.state().components().shutdown(self, shutdown);

        self.state()
            .components_mut()
            .get_downcast_mut::<Tracing>()
            .map(Tracing::shutdown);

        if let Err(e) = shutdown_result {
            let app_name = self.name().to_string();
            fatal_error(app_name, &e);
        }

        match shutdown {
            Shutdown::Graceful => process::exit(0),
            Shutdown::Forced => process::exit(1),
            Shutdown::Crash => process::exit(2),
        }
    }
}

/// Boot the given application, parsing subcommand and options from
/// command-line arguments, and terminating when complete.
// <https://docs.rs/abscissa_core/0.7.0/src/abscissa_core/application.rs.html#174-178>
pub fn boot(app_cell: &'static AppCell<ZebradApp>) -> ! {
    let args =
        EntryPoint::process_cli_args(env::args_os().collect()).unwrap_or_else(|err| err.exit());

    ZebradApp::run(app_cell, args);
    process::exit(0);
}
