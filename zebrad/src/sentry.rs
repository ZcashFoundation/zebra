//! Integration with sentry.io for event reporting.

use std::{
    collections::BTreeMap,
    env,
    panic::PanicHookInfo,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock, PoisonError,
    },
    thread,
    time::Duration,
};

use sentry::{
    integrations::{backtrace::current_stacktrace, tracing::EventFilter},
    protocol::{
        Context, Event, Exception, Frame, Log, LogAttribute, Map, Mechanism, SessionStatus,
        Stacktrace, Value,
    },
    ClientInitGuard, ClientOptions, Hub, Scope,
};
use tracing::Level;
use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
};

use crate::{
    application::{build_version, ZebradApp},
    components::sync::progress,
};

/// Keeps the Sentry client open until [`end`] drops it.
///
/// `zebrad` exits with `process::exit`, which skips destructors, so the guard must be dropped
/// explicitly to end the release health session and flush queued events and logs.
static CLIENT_GUARD: Mutex<Option<ClientInitGuard>> = Mutex::new(None);

/// The network this node runs on, used to find the network upgrade at the tip.
static NETWORK: OnceLock<Network> = OnceLock::new();

/// The best chain tip height, or [`UNKNOWN_TIP_HEIGHT`] until the progress task records one.
///
/// Events and logs read this instead of the chain tip watch channel, because Sentry calls
/// `before_send` and `before_send_log` from inside tracing, where instrumented chain tip reads
/// would re-enter the tracing layers.
static TIP_HEIGHT: AtomicU32 = AtomicU32::new(UNKNOWN_TIP_HEIGHT);

/// Above `Height::MAX`, so it is never a real height.
const UNKNOWN_TIP_HEIGHT: u32 = u32::MAX;

/// How long one release health session lasts before it is ended and replaced.
///
/// Sentry only receives a session that has no errors when it ends, and drops session updates
/// for sessions that started more than five days earlier. Daily sessions keep long runs, and
/// runs that are killed, visible in release health.
const SESSION_ROTATION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Environment and CI metadata attached to all Sentry events.
#[derive(Debug, Default)]
struct Metadata {
    environment: Option<String>,
    tags: BTreeMap<&'static str, String>,
    ci_context: BTreeMap<&'static str, String>,
}

impl Metadata {
    fn from_env() -> Self {
        Self::from_lookup(env_var)
    }

    fn from_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut metadata = Self {
            environment: lookup_value(&lookup, "SENTRY_ENVIRONMENT"),
            ..Default::default()
        };

        insert_lookup_values(
            &lookup,
            &mut metadata.ci_context,
            &[
                ("GITHUB_RUN_ID", "run_id"),
                ("GITHUB_RUN_ATTEMPT", "run_attempt"),
                ("GITHUB_WORKFLOW", "workflow"),
                ("GITHUB_JOB", "job"),
            ],
        );

        insert_lookup_values(
            &lookup,
            &mut metadata.tags,
            &[
                ("GITHUB_EVENT_NAME", "deploy.trigger"),
                ("CI_PR_NUMBER", "pr.number"),
                ("CI_TEST_ID", "test.id"),
            ],
        );

        if let Some(git_ref) = slugged_git_ref(&lookup) {
            metadata.tags.insert("git.ref", git_ref);
        }

        let git_sha = lookup_value(&lookup, "GITHUB_SHA")
            .or_else(|| ZebradApp::git_commit().map(ToOwned::to_owned));
        if let Some(git_sha) = git_sha {
            metadata.tags.insert("git.sha", git_sha);
        }

        if lookup_value(&lookup, "GITHUB_ACTIONS").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            metadata
                .tags
                .insert("ci.provider", "github-actions".to_owned());
        }

        metadata
    }

    fn client_options(&self) -> ClientOptions {
        self.client_options_with_release(release_name())
    }

    fn client_options_with_release(&self, release: String) -> ClientOptions {
        let log_attributes = self.log_attributes();
        let options = ClientOptions::new()
            .release(release)
            .auto_session_tracking(true)
            .before_send(|mut event| {
                add_chain_tags(&mut event, chain_position());
                Some(event)
            })
            .before_send_log(move |mut log| {
                merge_log_attributes(&mut log, &log_attributes);
                merge_log_attributes(&mut log, &chain_log_attributes(chain_position()));
                Some(log)
            });

        match &self.environment {
            Some(environment) => options.environment(environment.clone()),
            None => options,
        }
    }

    /// Build the static CI/git attributes to attach to every outgoing [`Log`].
    ///
    /// `Scope::set_tag` only propagates to [`Event`]s, so Logs need their own
    /// enrichment path. CI context is namespaced under `ci.*` to mirror the
    /// grouping used for Event contexts.
    fn log_attributes(&self) -> Vec<(String, LogAttribute)> {
        let mut attrs = Vec::with_capacity(self.tags.len() + self.ci_context.len());
        for (key, value) in &self.tags {
            attrs.push(((*key).to_owned(), LogAttribute::from(value.clone())));
        }
        for (key, value) in &self.ci_context {
            attrs.push((format!("ci.{key}"), LogAttribute::from(value.clone())));
        }
        attrs
    }

    fn apply_to_scope(&self, scope: &mut Scope) {
        for (key, value) in &self.tags {
            scope.set_tag(key, value.as_str());
        }

        if !self.ci_context.is_empty() {
            scope.set_context("ci", Context::Other(context_map(&self.ci_context)));
        }
    }

    #[cfg(test)]
    fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }
}

/// Starts the Sentry client and its release health sessions, which are replaced daily.
pub(crate) fn init(network: &Network) {
    let _ = NETWORK.set(network.clone());

    let mut metadata = Metadata::from_env();
    metadata.tags.insert("chain.network", network.to_string());

    let guard = sentry::init(metadata.client_options());
    sentry::configure_scope(|scope| metadata.apply_to_scope(scope));

    *CLIENT_GUARD.lock().unwrap_or_else(PoisonError::into_inner) = Some(guard);

    let rotation = thread::Builder::new()
        .name("sentry session rotation".to_owned())
        .spawn(|| loop {
            thread::sleep(SESSION_ROTATION_INTERVAL);
            rotate_session(&Hub::main());
        });
    if let Err(error) = rotation {
        warn!(?error, "unable to start Sentry session rotation");
    }
}

/// Ends the current release health session as exited and starts a new one.
///
/// Session changes must use the main hub. Other threads copy the main hub's session when they
/// first use Sentry, so a session started on any other hub would only cover that thread.
fn rotate_session(hub: &Hub) {
    hub.end_session();
    hub.start_session();
}

/// Ends the release health session with `status`, then closes the client.
///
/// Returns `false` if queued events and logs could not be sent before the shutdown timeout.
pub(crate) fn end(status: SessionStatus) -> bool {
    Hub::main().end_session_with_status(status);

    let guard = CLIENT_GUARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();

    guard.is_none_or(|guard| guard.close(None))
}

/// Records the best chain tip height for the events and logs sent after this call.
pub(crate) fn record_tip_height(height: Height) {
    TIP_HEIGHT.store(height.0, Ordering::Relaxed);
}

/// Returns the best chain tip height and its network upgrade, once both are known.
fn chain_position() -> Option<(Height, NetworkUpgrade)> {
    let network = NETWORK.get()?;
    let height = TIP_HEIGHT.load(Ordering::Relaxed);
    let height = (height != UNKNOWN_TIP_HEIGHT).then_some(Height(height))?;

    Some((height, NetworkUpgrade::current(network, height)))
}

/// Adds the chain position to `event`, keeping any tags the event already has.
fn add_chain_tags(event: &mut Event<'static>, chain_position: Option<(Height, NetworkUpgrade)>) {
    let Some((height, network_upgrade)) = chain_position else {
        return;
    };

    event
        .tags
        .entry("chain.height".to_owned())
        .or_insert_with(|| height.0.to_string());
    event
        .tags
        .entry("chain.network_upgrade".to_owned())
        .or_insert_with(|| network_upgrade.to_string());
}

/// Returns the chain position as log attributes, with the height as a number so that log
/// queries can aggregate it.
fn chain_log_attributes(
    chain_position: Option<(Height, NetworkUpgrade)>,
) -> Vec<(String, LogAttribute)> {
    let Some((height, network_upgrade)) = chain_position else {
        return Vec::new();
    };

    vec![
        ("chain.height".to_owned(), LogAttribute::from(height.0)),
        (
            "chain.network_upgrade".to_owned(),
            LogAttribute::from(network_upgrade.to_string()),
        ),
    ]
}

/// Returns the Sentry tracing layer, or `None` if [`init`] has not bound an enabled client.
///
/// Without a client the layer sends nothing, but it still creates a Sentry span for every
/// INFO-level span. Must be called after [`init`].
pub(crate) fn tracing_layer<S>() -> Option<impl tracing_subscriber::Layer<S>>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let client_enabled = Hub::current()
        .client()
        .is_some_and(|client| client.is_enabled());

    client_enabled.then(|| {
        sentry::integrations::tracing::layer().event_filter(|metadata| match *metadata.level() {
            Level::ERROR => EventFilter::Event | EventFilter::Log,
            Level::WARN => EventFilter::Log | EventFilter::Breadcrumb,
            // Sync progress is logged once a minute, so sending it gives Sentry each node's
            // height and network upgrade over time without the volume of all INFO logs.
            Level::INFO if metadata.target() == progress::LOG_TARGET => {
                EventFilter::Log | EventFilter::Breadcrumb
            }
            Level::INFO => EventFilter::Breadcrumb,
            _ => EventFilter::Ignore,
        })
    })
}

/// Builds the Sentry event for a panic.
///
/// Like Sentry's panic integration, the panic message becomes the issue title and the current
/// stack trace locates and groups it. Zebra's panic report is attached for its diagnostic
/// metadata.
pub(crate) fn panic_event_from(
    panic_info: &PanicHookInfo<'_>,
    panic_report: impl ToString,
) -> Event<'static> {
    let exception = Exception {
        ty: "panic".into(),
        mechanism: Some(Mechanism {
            ty: "panic".into(),
            handled: Some(false),
            ..Default::default()
        }),
        value: Some(
            panic_info
                .payload_as_str()
                .unwrap_or("Box<dyn Any>")
                .to_owned(),
        ),
        stacktrace: current_stacktrace().map(without_panic_handling_frames),
        ..Default::default()
    };

    let mut panic_context = Map::new();
    panic_context.insert("report".to_owned(), panic_report.to_string().into());
    if let Some(location) = panic_info.location() {
        panic_context.insert("location".to_owned(), location.to_string().into());
    }

    let mut event = Event {
        exception: vec![exception].into(),
        level: sentry::Level::Fatal,
        ..Default::default()
    };
    event
        .contexts
        .insert("panic".to_owned(), Context::Other(panic_context));

    event
}

/// Removes the frames that handle the panic: the standard library's panic machinery and the
/// panic hook that builds the event.
///
/// Sentry names an issue after its newest in-app frame, and the hook's frames are in-app, so
/// without this every panic would point at the hook instead of the code that panicked.
fn without_panic_handling_frames(mut stacktrace: Stacktrace) -> Stacktrace {
    let starts_panic = |frame: &Frame| {
        frame.function.as_deref().is_some_and(|function| {
            function.starts_with("std::panicking::") || function.ends_with("rust_begin_unwind")
        })
    };
    let is_runtime = |frame: &Frame| {
        frame.function.as_deref().is_some_and(|function| {
            function.starts_with("std::")
                || function.starts_with("core::")
                || function.ends_with("rust_begin_unwind")
        })
    };

    // Frames are ordered from oldest to newest.
    if let Some(mut first_panic_frame) = stacktrace.frames.iter().rposition(starts_panic) {
        while first_panic_frame > 0 && is_runtime(&stacktrace.frames[first_panic_frame - 1]) {
            first_panic_frame -= 1;
        }
        stacktrace.frames.truncate(first_panic_frame);
    }

    stacktrace
}

/// Merge `attrs` into `log.attributes`, letting existing keys win.
///
/// Tracing event fields are already present on the [`Log`] when the
/// `before_send_log` hook fires, so we must not overwrite them; this keeps
/// our CI/git metadata as a fallback that enriches (but never masks) the
/// log's own attributes.
fn merge_log_attributes(log: &mut Log, attrs: &[(String, LogAttribute)]) {
    for (key, value) in attrs {
        log.attributes
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
}

fn env_var(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn lookup_value<F>(lookup: &F, key: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let value = lookup(key)?;
    let value = value.trim();

    (!value.is_empty()).then(|| value.to_owned())
}

fn insert_lookup_values<F>(
    lookup: &F,
    target: &mut BTreeMap<&'static str, String>,
    mappings: &[(&str, &'static str)],
) where
    F: Fn(&str) -> Option<String>,
{
    for (env_key, target_key) in mappings {
        if let Some(value) = lookup_value(lookup, env_key) {
            target.insert(*target_key, value);
        }
    }
}

fn slugged_git_ref<F>(lookup: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    [
        "GITHUB_REF_POINT_SLUG_URL",
        "GITHUB_HEAD_REF_SLUG_URL",
        "GITHUB_REF_NAME_SLUG_URL",
    ]
    .into_iter()
    .find_map(|key| lookup_value(lookup, key))
}

fn context_map(values: &BTreeMap<&'static str, String>) -> Map<String, Value> {
    values
        .iter()
        .map(|(key, value)| (key.to_string(), Value::String(value.clone())))
        .collect()
}

fn release_name() -> String {
    release_name_from(&env_var, ZebradApp::git_commit())
}

fn release_name_from<F>(lookup: &F, git_commit: Option<&str>) -> String
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(release) = lookup_value(lookup, "SENTRY_RELEASE") {
        return release;
    }

    let version = build_version();

    if version.build.is_empty() {
        if let Some(git_sha) = git_commit {
            return format!("{version}+git.{git_sha}");
        }
    }

    version.to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        panic,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use sentry::{
        protocol::{Context, EnvelopeItem, Event, Log, LogAttribute, LogLevel, SessionStatus},
        test::with_captured_envelopes_options,
        Client, ClientOptions, Hub, Scope, SessionMode,
    };
    use tracing_subscriber::Registry;
    use zebra_chain::{block::Height, parameters::NetworkUpgrade};

    use crate::application::build_version;

    use super::{
        add_chain_tags, chain_log_attributes, merge_log_attributes, panic_event_from,
        release_name_from, rotate_session, tracing_layer, Metadata,
    };

    fn tracing_layer_installed_with(client: Option<Client>) -> bool {
        let hub = Hub::new(client.map(Arc::new), Arc::new(Scope::default()));
        Hub::run(Arc::new(hub), || tracing_layer::<Registry>().is_some())
    }

    #[test]
    fn tracing_layer_requires_a_client() {
        assert!(!tracing_layer_installed_with(None));
    }

    #[test]
    fn tracing_layer_requires_an_enabled_client() {
        let disabled = Client::from(ClientOptions::default());

        assert!(!tracing_layer_installed_with(Some(disabled)));
    }

    #[test]
    fn tracing_layer_is_installed_with_an_enabled_client() {
        let enabled = Client::from(sentry::apply_defaults(
            ClientOptions::new().dsn("https://public@example.com/1"),
        ));

        assert!(tracing_layer_installed_with(Some(enabled)));
    }

    #[test]
    fn metadata_ignores_empty_values() {
        let env = HashMap::from([
            ("SENTRY_ENVIRONMENT", "".to_string()),
            ("CI_TEST_ID", "   ".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        assert_eq!(metadata.environment(), None);
        assert_eq!(metadata.tags.get("test.id"), None);
        assert!(metadata.ci_context.is_empty());
    }

    #[test]
    fn metadata_reads_expected_tags_and_ci_context() {
        let env = HashMap::from([
            ("SENTRY_ENVIRONMENT", "stage".to_string()),
            ("GITHUB_ACTIONS", "true".to_string()),
            ("GITHUB_EVENT_NAME", "push".to_string()),
            ("GITHUB_REF_POINT_SLUG_URL", "main".to_string()),
            ("GITHUB_SHA", "deadbeef".to_string()),
            ("GITHUB_RUN_ID", "42".to_string()),
            ("GITHUB_RUN_ATTEMPT", "2".to_string()),
            ("GITHUB_WORKFLOW", "CI".to_string()),
            ("GITHUB_JOB", "deploy".to_string()),
            ("CI_TEST_ID", "sync-full-mainnet".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        assert_eq!(metadata.environment(), Some("stage"));
        assert_eq!(
            metadata.tags.get("deploy.trigger").map(String::as_str),
            Some("push"),
        );
        assert_eq!(
            metadata.tags.get("git.ref").map(String::as_str),
            Some("main"),
        );
        assert_eq!(
            metadata.tags.get("git.sha").map(String::as_str),
            Some("deadbeef"),
        );
        assert_eq!(
            metadata.tags.get("ci.provider").map(String::as_str),
            Some("github-actions"),
        );
        assert_eq!(
            metadata.tags.get("test.id").map(String::as_str),
            Some("sync-full-mainnet"),
        );
        assert_eq!(
            metadata.ci_context.get("run_id").map(String::as_str),
            Some("42"),
        );
        assert_eq!(
            metadata.ci_context.get("run_attempt").map(String::as_str),
            Some("2"),
        );
        assert_eq!(
            metadata.ci_context.get("workflow").map(String::as_str),
            Some("CI"),
        );
        assert_eq!(
            metadata.ci_context.get("job").map(String::as_str),
            Some("deploy"),
        );
    }

    #[test]
    fn metadata_reads_pull_request_number_from_ci_input() {
        let env = HashMap::from([
            ("GITHUB_ACTIONS", "true".to_string()),
            ("GITHUB_REF_POINT_SLUG_URL", "fix-sentry-tags".to_string()),
            ("CI_PR_NUMBER", "84".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        assert_eq!(
            metadata.tags.get("git.ref").map(String::as_str),
            Some("fix-sentry-tags"),
        );
        assert_eq!(
            metadata.tags.get("pr.number").map(String::as_str),
            Some("84"),
        );
    }

    #[test]
    fn metadata_prefers_slugged_git_ref_when_available() {
        let env = HashMap::from([
            (
                "GITHUB_REF_POINT_SLUG_URL",
                "feature-use-sentry".to_string(),
            ),
            (
                "GITHUB_HEAD_REF_SLUG_URL",
                "feature-use-plus-sentry".to_string(),
            ),
            ("GITHUB_REF_NAME_SLUG_URL", "84-merge".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        assert_eq!(
            metadata.tags.get("git.ref").map(String::as_str),
            Some("feature-use-sentry"),
        );
    }

    #[test]
    fn metadata_ignores_missing_slugged_git_ref() {
        let env = HashMap::from([
            ("GITHUB_ACTIONS", "true".to_string()),
            ("GITHUB_EVENT_NAME", "push".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        assert_eq!(metadata.tags.get("git.ref"), None);
    }

    #[test]
    fn release_name_prefers_sentry_release_override() {
        let env = HashMap::from([("SENTRY_RELEASE", "zebrad@4.4.0-rc.1".to_string())]);

        let release = release_name_from(&|key| env.get(key).cloned(), Some("deadbeef"));

        assert_eq!(release, "zebrad@4.4.0-rc.1");
    }

    #[test]
    fn release_name_appends_git_sha_when_build_metadata_is_empty() {
        let env: HashMap<&str, String> = HashMap::new();

        let release = release_name_from(&|key| env.get(key).cloned(), Some("deadbeef"));

        let version = build_version();
        if version.build.is_empty() {
            assert_eq!(release, format!("{version}+git.deadbeef"));
        } else {
            // Tagged build already carries +build metadata; do not double-append.
            assert_eq!(release, version.to_string());
        }
    }

    #[test]
    fn release_name_falls_back_to_version_without_git_sha() {
        let env: HashMap<&str, String> = HashMap::new();

        let release = release_name_from(&|key| env.get(key).cloned(), None);

        assert_eq!(release, build_version().to_string());
    }

    #[test]
    fn client_options_track_application_sessions_and_roundtrip_environment() {
        let env = HashMap::from([("SENTRY_ENVIRONMENT", "stage".to_string())]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        let options = metadata.client_options_with_release("zebrad@test".to_string());

        assert!(options.auto_session_tracking);
        assert_eq!(options.session_mode, SessionMode::Application);
        assert_eq!(options.environment.as_deref(), Some("stage"));
        assert_eq!(options.release.as_deref(), Some("zebrad@test"));
    }

    #[test]
    fn client_options_omits_environment_when_unset() {
        let metadata = Metadata::from_lookup(|_: &str| -> Option<String> { None });

        let options = metadata.client_options_with_release("zebrad@test".to_string());

        assert!(options.environment.is_none());
    }

    #[test]
    fn log_attributes_include_tags_and_ci_context() {
        let env = HashMap::from([
            ("GITHUB_ACTIONS", "true".to_string()),
            ("GITHUB_EVENT_NAME", "push".to_string()),
            ("GITHUB_SHA", "deadbeef".to_string()),
            ("GITHUB_REF_POINT_SLUG_URL", "main".to_string()),
            ("GITHUB_RUN_ID", "42".to_string()),
            ("GITHUB_RUN_ATTEMPT", "2".to_string()),
            ("GITHUB_WORKFLOW", "CI".to_string()),
            ("GITHUB_JOB", "deploy".to_string()),
            ("CI_TEST_ID", "sync-full-mainnet".to_string()),
            ("CI_PR_NUMBER", "84".to_string()),
        ]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        let attrs: BTreeMap<String, LogAttribute> = metadata.log_attributes().into_iter().collect();

        let expect = |key: &str, value: &str| {
            assert_eq!(
                attrs.get(key).and_then(|attr| attr.0.as_str()),
                Some(value),
                "missing or wrong log attribute {key}",
            );
        };

        expect("test.id", "sync-full-mainnet");
        expect("git.sha", "deadbeef");
        expect("git.ref", "main");
        expect("ci.provider", "github-actions");
        expect("pr.number", "84");
        expect("deploy.trigger", "push");
        expect("ci.run_id", "42");
        expect("ci.run_attempt", "2");
        expect("ci.workflow", "CI");
        expect("ci.job", "deploy");
    }

    #[test]
    fn merge_log_attributes_preserves_existing() {
        let mut log = Log {
            level: LogLevel::Info,
            body: "test".to_string(),
            trace_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            severity_number: None,
            attributes: BTreeMap::new(),
        };

        log.attributes.insert(
            "git.sha".to_string(),
            LogAttribute::from("event-sha".to_string()),
        );

        let attrs = vec![
            (
                "git.sha".to_string(),
                LogAttribute::from("metadata-sha".to_string()),
            ),
            (
                "ci.run_id".to_string(),
                LogAttribute::from("42".to_string()),
            ),
        ];

        merge_log_attributes(&mut log, &attrs);

        assert_eq!(
            log.attributes
                .get("git.sha")
                .and_then(|attr| attr.0.as_str()),
            Some("event-sha"),
            "pre-existing attribute must not be overwritten",
        );
        assert_eq!(
            log.attributes
                .get("ci.run_id")
                .and_then(|attr| attr.0.as_str()),
            Some("42"),
            "new attribute must be inserted",
        );
    }

    #[test]
    fn chain_tags_keep_existing_event_tags() {
        let mut event = Event::default();
        event
            .tags
            .insert("chain.height".to_owned(), "event-height".to_owned());

        add_chain_tags(&mut event, Some((Height(3_146_400), NetworkUpgrade::Nu7)));

        assert_eq!(
            event.tags.get("chain.height").map(String::as_str),
            Some("event-height"),
        );
        assert_eq!(
            event.tags.get("chain.network_upgrade").map(String::as_str),
            Some("Nu7"),
        );
    }

    #[test]
    fn chain_tags_are_omitted_before_the_tip_is_known() {
        let mut event = Event::default();

        add_chain_tags(&mut event, None);

        assert!(event.tags.is_empty());
        assert!(chain_log_attributes(None).is_empty());
    }

    #[test]
    fn chain_log_attributes_report_height_as_a_number() {
        let attrs: BTreeMap<String, LogAttribute> =
            chain_log_attributes(Some((Height(3_146_400), NetworkUpgrade::Nu7)))
                .into_iter()
                .collect();

        assert_eq!(
            attrs.get("chain.height").and_then(|attr| attr.0.as_u64()),
            Some(3_146_400),
        );
        assert_eq!(
            attrs
                .get("chain.network_upgrade")
                .and_then(|attr| attr.0.as_str()),
            Some("Nu7"),
        );
    }

    #[test]
    fn rotation_reports_the_ended_session_and_starts_another() {
        let envelopes = with_captured_envelopes_options(
            || {
                let hub = Hub::current();
                hub.start_session();
                rotate_session(&hub);
                hub.end_session_with_status(SessionStatus::Crashed);
            },
            ClientOptions::new().release("zebrad@test"),
        );

        let sessions: Vec<_> = envelopes
            .iter()
            .flat_map(|envelope| envelope.items())
            .filter_map(|item| match item {
                EnvelopeItem::SessionUpdate(session) => Some(session),
                _ => None,
            })
            .collect();

        assert_eq!(
            sessions.len(),
            2,
            "both sessions are reported: {sessions:?}"
        );
        assert_eq!(sessions[0].status, SessionStatus::Exited);
        assert_eq!(sessions[1].status, SessionStatus::Crashed);
        assert_ne!(sessions[0].session_id, sessions[1].session_id);
        assert!(sessions.iter().all(|session| session.init));
    }

    #[test]
    fn panic_events_use_the_panic_message_and_stack() {
        static EVENT: Mutex<Option<Event<'static>>> = Mutex::new(None);

        let previous_hook = panic::take_hook();
        panic::set_hook(Box::new(|panic_info| {
            *EVENT.lock().expect("only this hook locks it") =
                Some(panic_event_from(panic_info, "full panic report"));
        }));
        let result = panic::catch_unwind(|| panic!("listener failed"));
        panic::set_hook(previous_hook);
        assert!(result.is_err());

        let event = EVENT
            .lock()
            .expect("the hook has returned")
            .take()
            .expect("the hook captured the panic");
        let exception = &event.exception.values[0];

        assert_eq!(exception.value.as_deref(), Some("listener failed"));
        assert_eq!(
            exception.mechanism.as_ref().and_then(|m| m.handled),
            Some(false)
        );
        let newest_frame = exception
            .stacktrace
            .as_ref()
            .and_then(|stacktrace| stacktrace.frames.last())
            .and_then(|frame| frame.function.as_deref())
            .expect("panic events carry a stack trace");
        assert!(
            newest_frame.contains("panic_events_use_the_panic_message_and_stack"),
            "the newest frame is the code that panicked, not the hook: {newest_frame}",
        );

        let Some(Context::Other(panic_context)) = event.contexts.get("panic") else {
            panic!("panic events carry the panic report");
        };
        assert_eq!(
            panic_context
                .get("report")
                .and_then(|report| report.as_str()),
            Some("full panic report"),
        );
        assert!(panic_context
            .get("location")
            .and_then(|location| location.as_str())
            .is_some_and(|location| location.contains("sentry.rs")));
    }
}
