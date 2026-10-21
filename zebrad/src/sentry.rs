//! Integration with sentry.io for event reporting.

use std::{
    collections::BTreeMap,
    env,
    panic::PanicHookInfo,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Mutex, MutexGuard, OnceLock, PoisonError,
    },
    thread,
    time::{Duration, SystemTime},
};

use sentry::{
    integrations::{backtrace::current_stacktrace, tracing::EventFilter},
    protocol::{
        Context, Envelope, Event, Exception, Frame, Log, LogAttribute, Map, Mechanism,
        SessionAttributes, SessionStatus, SessionUpdate, Stacktrace, Value,
    },
    types::random_uuid,
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

/// The release health session in progress.
///
/// Zebra tracks sessions itself instead of through the SDK. The SDK keeps a session per hub, and
/// a thread's hub keeps the session it copied when the thread first used Sentry, so a session
/// the SDK started later, such as a daily one, would miss errors reported on other threads.
static SESSION: Mutex<Option<SessionUpdate<'static>>> = Mutex::new(None);

/// Errors reported from any thread since the session in progress started.
static SESSION_ERRORS: AtomicU64 = AtomicU64::new(0);

/// How long one release health session lasts before it is ended and replaced.
///
/// Sentry drops session updates for sessions that started more than five days earlier, so a
/// longer session could never report how it ended.
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
            // Suspect commits need in-app stack frames. Only ERROR events and panics are sent as
            // events, so capturing the stack stays off the common paths.
            .attach_stacktrace(true)
            .before_send(|mut event| {
                if event.level >= sentry::Level::Error {
                    SESSION_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
                mark_zebra_frames_in_app(&mut event);
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

    if guard.is_enabled() {
        start_run_sessions();
    }

    *CLIENT_GUARD.lock().unwrap_or_else(PoisonError::into_inner) = Some(guard);

    let rotation = thread::Builder::new()
        .name("sentry session rotation".to_owned())
        .spawn(|| loop {
            thread::sleep(SESSION_ROTATION_INTERVAL);
            rotate_session();
        });
    if let Err(error) = rotation {
        warn!(?error, "unable to start Sentry session rotation");
    }
}

/// Starts the first release health session of this run.
///
/// Every session of the run reports the same random ID as its distinct ID, so Sentry's crash-free
/// users rate is the share of runs without a crash.
fn start_run_sessions() {
    let Some(client) = Hub::current().client() else {
        return;
    };
    let options = client.options();
    let Some(release) = options.release.clone() else {
        return;
    };

    let attributes = SessionAttributes {
        release,
        environment: options.environment.clone(),
        ip_address: None,
        user_agent: None,
    };

    start_session(attributes, random_uuid().to_string());
}

/// Starts a release health session and reports that it started.
fn start_session(attributes: SessionAttributes<'static>, run_id: String) {
    let session = SessionUpdate {
        session_id: random_uuid(),
        distinct_id: Some(run_id),
        sequence: None,
        timestamp: None,
        started: SystemTime::now(),
        init: true,
        duration: None,
        status: SessionStatus::Ok,
        errors: 0,
        attributes,
    };

    send_session(session.clone());
    *lock_session() = Some(session);
}

/// Ends the session in progress with `status`, reports it, and returns it.
fn end_session(status: SessionStatus) -> Option<SessionUpdate<'static>> {
    let mut session = lock_session().take()?;

    session.init = false;
    session.status = status;
    session.errors = SESSION_ERRORS.swap(0, Ordering::Relaxed);
    session.timestamp = Some(SystemTime::now());
    session.duration = session
        .started
        .elapsed()
        .ok()
        .map(|duration| duration.as_secs_f64());

    send_session(session.clone());
    Some(session)
}

/// Ends the session in progress as exited and starts the next session of the same run.
fn rotate_session() {
    if let Some(ended) = end_session(SessionStatus::Exited) {
        let run_id = ended
            .distinct_id
            .expect("every session is started with the run ID");
        start_session(ended.attributes, run_id);
    }
}

fn send_session(session: SessionUpdate<'static>) {
    if let Some(client) = Hub::current().client() {
        let mut envelope = Envelope::new();
        envelope.add_item(session);
        client.send_envelope(envelope);
    }
}

fn lock_session() -> MutexGuard<'static, Option<SessionUpdate<'static>>> {
    SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends the release health session with `status`, then closes the client.
///
/// Returns `false` if queued events and logs could not be sent before the shutdown timeout.
pub(crate) fn end(status: SessionStatus) -> bool {
    end_session(status);

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

/// Marks the stack frames from Zebra's crates as in-app, and every other frame as not in-app.
///
/// The SDK classifies frames by function name, but release builds keep only line tables, so
/// inlined frames lose their module path. When no frame matches, the SDK marks every frame
/// in-app. Every frame keeps its source file, so Zebra classifies frames by path instead.
fn mark_zebra_frames_in_app(event: &mut Event<'static>) {
    let exception_stacktraces = event
        .exception
        .values
        .iter_mut()
        .filter_map(|exception| exception.stacktrace.as_mut());
    let thread_stacktraces = event
        .threads
        .values
        .iter_mut()
        .filter_map(|thread| thread.stacktrace.as_mut());

    for stacktrace in exception_stacktraces
        .chain(thread_stacktraces)
        .chain(event.stacktrace.as_mut())
    {
        for frame in &mut stacktrace.frames {
            frame.in_app = Some(frame.abs_path.as_deref().is_some_and(is_zebra_source));
        }
    }
}

/// Returns whether `path` is a source file in one of Zebra's crates.
///
/// A crate's sources are in `<crate directory>/src/`. The directory is the crate name in a
/// checkout, such as `/zebra/zebrad/src/` in the Docker images, and the crate name and version
/// in the Cargo registry, such as `zebra-chain-14.0.0/src/`.
fn is_zebra_source(path: &str) -> bool {
    let is_zebra_crate = |dir: &str| {
        dir == "zebrad"
            || ["zebrad-", "zebra-", "tower-batch-control", "tower-fallback"]
                .iter()
                .any(|prefix| dir.starts_with(prefix))
    };

    path.split(['/', '\\'])
        .collect::<Vec<_>>()
        .windows(2)
        .any(|dirs| dirs[1] == "src" && is_zebra_crate(dirs[0]))
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
    release_name_from(&env_var)
}

fn release_name_from<F>(lookup: &F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    lookup_value(lookup, "SENTRY_RELEASE").unwrap_or_else(|| build_version().to_string())
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
        protocol::{
            Context, EnvelopeItem, Event, Frame, Log, LogAttribute, LogLevel, SessionStatus,
            Stacktrace,
        },
        test::{with_captured_envelopes_options, with_captured_events_options},
        Client, ClientOptions, Hub, Scope,
    };
    use tracing_subscriber::{layer::SubscriberExt, Registry};
    use zebra_chain::{block::Height, parameters::NetworkUpgrade};

    use crate::application::build_version;

    use super::{
        add_chain_tags, chain_log_attributes, end_session, is_zebra_source, merge_log_attributes,
        panic_event_from, release_name_from, rotate_session, start_run_sessions, tracing_layer,
        without_panic_handling_frames, Metadata,
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

        let release = release_name_from(&|key| env.get(key).cloned());

        assert_eq!(release, "zebrad@4.4.0-rc.1");
    }

    #[test]
    fn release_name_defaults_to_build_version() {
        let env: HashMap<&str, String> = HashMap::new();

        let release = release_name_from(&|key| env.get(key).cloned());

        assert_eq!(release, build_version().to_string());
    }

    #[test]
    fn client_options_leave_sessions_to_zebra_and_roundtrip_environment() {
        let env = HashMap::from([("SENTRY_ENVIRONMENT", "stage".to_string())]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        let options = metadata.client_options_with_release("zebrad@test".to_string());

        assert!(!options.auto_session_tracking);
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
    fn errors_from_any_hub_count_toward_the_session_in_progress() {
        let metadata = Metadata::from_lookup(|_: &str| -> Option<String> { None });

        let envelopes = with_captured_envelopes_options(
            || {
                start_run_sessions();
                // Hubs on other threads are copied once, before any later session starts.
                let earlier_hub = Hub::new_from_top(Hub::current());

                rotate_session();
                earlier_hub.capture_event(Event {
                    level: sentry::Level::Error,
                    ..Default::default()
                });
                end_session(SessionStatus::Crashed);
            },
            metadata.client_options_with_release("zebrad@test".to_string()),
        );

        let sessions: Vec<_> = envelopes
            .iter()
            .flat_map(|envelope| envelope.items())
            .filter_map(|item| match item {
                EnvelopeItem::SessionUpdate(session) => Some(session),
                _ => None,
            })
            .collect();

        let summary: Vec<_> = sessions
            .iter()
            .map(|session| (session.init, session.status, session.errors))
            .collect();
        assert_eq!(
            summary,
            [
                (true, SessionStatus::Ok, 0),
                (false, SessionStatus::Exited, 0),
                (true, SessionStatus::Ok, 0),
                (false, SessionStatus::Crashed, 1),
            ],
        );
        assert_eq!(sessions[0].session_id, sessions[1].session_id);
        assert_eq!(sessions[2].session_id, sessions[3].session_id);
        assert_ne!(sessions[0].session_id, sessions[2].session_id);
        assert!(
            sessions.iter().all(|session| session.distinct_id.is_some()
                && session.distinct_id == sessions[0].distinct_id),
            "every session of a run reports the run ID",
        );
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
        // Optimized builds can inline the panicking closure and shorten function names, so check
        // that no frame that handles the panic remains, rather than naming the newest frame. Older
        // frames, such as the test's own `catch_unwind`, are expected to remain.
        let frames = &exception
            .stacktrace
            .as_ref()
            .expect("panic events carry a stack trace")
            .frames;
        let panic_handling = [
            "current_stacktrace",
            "panic_event_from",
            "rust_begin_unwind",
            "begin_panic_handler",
            "rust_panic_with_hook",
            "panic_with_hook",
            "panic_fmt",
        ];
        assert!(!frames.is_empty(), "the stack keeps the code that panicked");
        for function in frames.iter().filter_map(|frame| frame.function.as_deref()) {
            assert!(
                !panic_handling.iter().any(|name| function.contains(name)),
                "the stack ends at the code that panicked, without the panic handling: {function}",
            );
        }

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

    #[test]
    fn panic_stacks_end_at_the_code_that_panicked() {
        let frame = |function: &str| Frame {
            function: Some(function.to_owned()),
            ..Default::default()
        };
        // Oldest first, as Sentry orders frames.
        let stacktrace = Stacktrace {
            frames: vec![
                frame("std::rt::lang_start"),
                frame("std::panicking::catch_unwind"),
                frame("zebrad::components::start"),
                frame("zebra_network::open_listener"),
                frame("core::panicking::panic_fmt"),
                frame("__rustc::rust_begin_unwind"),
                frame("std::panicking::panic_with_hook"),
                frame("zebrad::application::panic_hook"),
                frame("zebrad::sentry::panic_event_from"),
            ],
            ..Default::default()
        };

        let functions: Vec<_> = without_panic_handling_frames(stacktrace)
            .frames
            .into_iter()
            .filter_map(|frame| frame.function)
            .collect();

        assert_eq!(
            functions,
            [
                "std::rt::lang_start",
                "std::panicking::catch_unwind",
                "zebrad::components::start",
                "zebra_network::open_listener",
            ],
        );
    }

    /// Returns the frames Sentry uses for suspect commits: the exception stack traces, or the
    /// current thread's stack trace when no exception has one.
    fn suspect_commit_stacktrace<'a>(event: &'a Event<'static>) -> Option<&'a Stacktrace> {
        event
            .exception
            .iter()
            .find_map(|exception| exception.stacktrace.as_ref())
            .or_else(|| {
                event
                    .threads
                    .iter()
                    .find(|thread| thread.current)
                    .and_then(|thread| thread.stacktrace.as_ref())
            })
    }

    #[test]
    fn error_events_carry_in_app_frames_for_suspect_commits() {
        let metadata = Metadata::default();

        let events = with_captured_events_options(
            || {
                let layer = tracing_layer().expect("the test client is enabled");
                let subscriber = Registry::default().with(layer);

                tracing::subscriber::with_default(subscriber, || {
                    tracing::error!("block commit failed");

                    let error = std::io::Error::other("disk full");
                    tracing::error!(
                        error = &error as &dyn std::error::Error,
                        "block commit failed"
                    );
                });
            },
            // `sentry::init` adds the default integrations, which capture and classify stacks.
            sentry::apply_defaults(metadata.client_options_with_release("zebrad@test".to_string())),
        );

        assert_eq!(events.len(), 2, "both ERROR events are sent: {events:?}");

        for event in &events {
            let frames = &suspect_commit_stacktrace(event)
                .expect("ERROR events carry a stack trace")
                .frames;
            let in_app_frames: Vec<_> = frames
                .iter()
                .filter(|frame| frame.in_app == Some(true))
                .collect();

            assert!(
                in_app_frames.iter().any(|frame| frame
                    .abs_path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("zebrad/src/sentry.rs"))),
                "the code that logged the error is an in-app frame with its source path: {frames:?}",
            );
            assert!(
                in_app_frames.iter().all(|frame| frame
                    .abs_path
                    .as_deref()
                    .is_some_and(|path| path.contains("zebrad/src/"))),
                "dependency frames are not in-app: {in_app_frames:?}",
            );
        }
    }

    #[test]
    fn zebra_sources_are_recognized_in_checkouts_and_the_registry() {
        for zebra_path in [
            "/zebra/zebrad/src/components/sync.rs",
            "/home/user/zebra/zebra-network/src/peer_set/initialize.rs",
            "/root/.cargo/registry/src/index.crates.io-0000/zebra-chain-14.0.0/src/block.rs",
            "/zebra/tower-batch-control/src/service.rs",
        ] {
            assert!(is_zebra_source(zebra_path), "{zebra_path} is Zebra's");
        }

        for other_path in [
            "/root/.cargo/registry/src/index.crates.io-0000/tokio-1.47.0/src/runtime/task.rs",
            "/rustc/f8297e351a40c1439a467bbbb6879088047f50b3/library/std/src/panicking.rs",
            "/zebra/zebrad/build.rs",
        ] {
            assert!(!is_zebra_source(other_path), "{other_path} is not Zebra's");
        }
    }
}
