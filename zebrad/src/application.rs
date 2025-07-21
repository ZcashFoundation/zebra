//! Zebrad Abscissa Application
//!
//! This is the code that starts `zebrad`, and launches its tasks and services.
//! See [the crate docs](crate) and [the start docs](crate::commands::start) for more details.

use std::{env, fmt::Write as _, io::Write as _, process, sync::Arc};

use abscissa_core::{
    application::{self, AppCell},
    config::CfgCell,
    status_err,
    terminal::{component::Terminal, stderr, stdout, ColorChoice},
    Application, Component, Configurable, FrameworkError, Shutdown, StandardPaths,
};
use semver::{BuildMetadata, Version};

use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::{
    constants::LOCK_FILE_ERROR, state_database_format_version_in_code,
    state_database_format_version_on_disk,
};

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

/// Returns the `zebrad` version for this build, in SemVer 2.0 format.
///
/// Includes `git describe` build metatata if available:
/// - the number of commits since the last version tag, and
/// - the git commit.
///
/// For details, see <https://semver.org/>
pub fn build_version() -> Version {
    // CARGO_PKG_VERSION is always a valid SemVer 2.0 version.
    const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

    // We're using the same library as cargo uses internally, so this is guaranteed.
    let fallback_version = CARGO_PKG_VERSION.parse().unwrap_or_else(|error| {
        panic!(
            "unexpected invalid CARGO_PKG_VERSION: {error:?} in {CARGO_PKG_VERSION:?}, \
             should have been checked by cargo"
        )
    });

    vergen_build_version().unwrap_or(fallback_version)
}

/// Returns the `zebrad` version from this build, if available from `vergen`.
fn vergen_build_version() -> Option<Version> {
    // VERGEN_GIT_DESCRIBE should be in the format:
    // - v1.0.0-rc.9-6-g319b01bb84
    // - v1.0.0-6-g319b01bb84
    // but sometimes it is just a short commit hash. See #6879 for details.
    //
    // Currently it is the output of `git describe --tags --dirty --match='v*.*.*'`,
    // or whatever is specified in zebrad/build.rs.
    const VERGEN_GIT_DESCRIBE: Option<&str> = option_env!("VERGEN_GIT_DESCRIBE");

    // The SemVer 2.0 format is:
    // - 1.0.0-rc.9+6.g319b01bb84
    // - 1.0.0+6.g319b01bb84
    //
    // Or as a pattern:
    // - version: major`.`minor`.`patch
    // - optional pre-release: `-`tag[`.`tag ...]
    // - optional build: `+`tag[`.`tag ...]
    // change the git describe format to the semver 2.0 format
    let vergen_git_describe = VERGEN_GIT_DESCRIBE?;

    // `git describe` uses "dirty" for uncommitted changes,
    // but users won't understand what that means.
    let vergen_git_describe = vergen_git_describe.replace("dirty", "modified");

    // Split using "git describe" separators.
    let mut vergen_git_describe = vergen_git_describe.split('-').peekable();

    // Check the "version core" part.
    let mut version = vergen_git_describe.next()?;

    // strip the leading "v", if present.
    version = version.strip_prefix('v').unwrap_or(version);

    // If the initial version is empty, just a commit hash, or otherwise invalid.
    if Version::parse(version).is_err() {
        return None;
    }

    let mut semver = version.to_string();

    // Check if the next part is a pre-release or build part,
    // but only consume it if it is a pre-release tag.
    let Some(part) = vergen_git_describe.peek() else {
        // No pre-release or build.
        return semver.parse().ok();
    };

    if part.starts_with(char::is_alphabetic) {
        // It's a pre-release tag.
        semver.push('-');
        semver.push_str(part);

        // Consume the pre-release tag to move on to the build tags, if any.
        let _ = vergen_git_describe.next();
    }

    // Check if the next part is a build part.
    let Some(build) = vergen_git_describe.peek() else {
        // No build tags.
        return semver.parse().ok();
    };

    if !build.starts_with(char::is_numeric) {
        // It's not a valid "commit count" build tag from "git describe".
        return None;
    }

    // Append the rest of the build parts with the correct `+` and `.` separators.
    let build_parts: Vec<_> = vergen_git_describe.collect();
    let build_parts = build_parts.join(".");

    semver.push('+');
    semver.push_str(&build_parts);

    semver.parse().ok()
}

/// The Zebra current release version, without any build metadata.
pub fn release_version() -> Version {
    let mut release_version = build_version();

    release_version.build = BuildMetadata::EMPTY;

    release_version
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

        // The Tracing component uses stdout directly and will apply colors automatically.
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
        // report an error to the terminal if it occurs (unless used with a command that doesn't need the config).
        let config = match command.config_path() {
            Some(path) => match self.load_config(&path) {
                Ok(config) => config,
                // Ignore errors loading the config for some commands.
                Err(_e) if command.cmd().should_ignore_load_config_error() => Default::default(),
                Err(e) => {
                    status_err!("Zebra could not parse the provided config file. This might mean you are using a deprecated format of the file. You can generate a valid config by running \"zebrad generate\", and diff it against yours to examine any format inconsistencies.");
                    return Err(e);
                }
            },
            None => ZebradConfig::default(),
        };

        let config = command.process_config(config)?;

        let theme = if config.tracing.use_color_stdout_and_stderr() {
            color_eyre::config::Theme::dark()
        } else {
            color_eyre::config::Theme::new()
        };

        // collect the common metadata for the issue URL and panic report,
        // skipping any env vars that aren't present

        // reads state disk version file, doesn't open RocksDB database
        let disk_db_version =
            match state_database_format_version_on_disk(&config.state, &config.network.network) {
                Ok(Some(version)) => version.to_string(),
                // This "version" is specially formatted to match a relaxed version regex in CI
                Ok(None) => "creating.new.database".to_string(),
                Err(error) => {
                    let mut error = format!("error: {error:?}");
                    error.truncate(100);
                    error
                }
            };

        let app_metadata = [
            // build-time constant: cargo or git tag + short commit
            ("version", build_version().to_string()),
            // config
            ("Zcash network", config.network.network.to_string()),
            // code constant
            (
                "running state version",
                state_database_format_version_in_code().to_string(),
            ),
            // state disk file, doesn't open database
            ("initial disk state version", disk_db_version),
            // build-time constant
            ("features", env!("VERGEN_CARGO_FEATURES").to_string()),
        ];

        // git env vars can be skipped if there is no `.git` during the
        // build, so they must all be optional
        let git_metadata: &[(_, Option<_>)] = &[
            ("branch", option_env!("VERGEN_GIT_BRANCH")),
            ("git commit", Self::git_commit()),
            ("git tag", option_env!("GIT_TAG")),
            ("git commit full", option_env!("GIT_COMMIT_FULL")),
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
            release: Some(build_version().to_string().into()),
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
                .clone()
                .or_else(|| Some(default_filter.to_owned()));
        } else {
            // Don't apply the configured filter for short-lived commands.
            tracing_config.filter = Some(default_filter.to_owned());
            tracing_config.flamegraph = None;
        }
        components.push(Box::new(Tracing::new(
            &config.network.network,
            tracing_config,
            command.cmd().uses_intro(),
        )?));

        // Log git metadata and platform info when zebrad starts up
        if is_server {
            tracing::info!("Diagnostic {}", metadata_section);
            info!(config_path = ?command.config_path(), config = ?cfg_ref, "loaded zebrad config");
        }

        // Activate the global span, so it's visible when we load the other
        // components. Space is at a premium here, so we use an empty message,
        // short commit hash, and the unique part of the network name.
        let net = config.network.network.to_string();
        let net = match net.as_str() {
            default_net_name @ ("Testnet" | "Mainnet") => &default_net_name[..4],
            other_net_name => other_net_name,
        };
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
