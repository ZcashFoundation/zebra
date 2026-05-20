//! Integration with sentry.io for event reporting.

use std::{collections::BTreeMap, env, sync::Arc};

use sentry::{
    integrations::tracing::EventFilter,
    protocol::{Context, Event, Exception, Log, LogAttribute, Map, Mechanism, Value},
    ClientInitGuard, ClientOptions, Scope,
};
use tracing::Level;

use crate::application::{build_version, ZebradApp};

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
        ClientOptions {
            release: Some(release.into()),
            environment: self.environment.clone().map(Into::into),
            enable_logs: true,
            before_send_log: Some(Arc::new(move |mut log: Log| {
                merge_log_attributes(&mut log, &log_attributes);
                Some(log)
            })),
            ..Default::default()
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

pub(crate) fn init() -> ClientInitGuard {
    let metadata = Metadata::from_env();
    let guard = sentry::init(metadata.client_options());
    sentry::configure_scope(|scope| metadata.apply_to_scope(scope));
    guard
}

pub(crate) fn tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    sentry::integrations::tracing::layer().event_filter(|metadata| match *metadata.level() {
        Level::ERROR => EventFilter::Event | EventFilter::Log,
        Level::WARN => EventFilter::Log | EventFilter::Breadcrumb,
        Level::INFO => EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    })
}

pub(crate) fn panic_event_from<T>(msg: T) -> Event<'static>
where
    T: ToString,
{
    let exception = Exception {
        ty: "panic".into(),
        mechanism: Some(Mechanism {
            ty: "panic".into(),
            handled: Some(false),
            ..Default::default()
        }),
        value: Some(msg.to_string()),
        ..Default::default()
    };

    Event {
        exception: vec![exception].into(),
        level: sentry::Level::Fatal,
        ..Default::default()
    }
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
        time::SystemTime,
    };

    use sentry::protocol::{Log, LogAttribute, LogLevel};

    use crate::application::build_version;

    use super::{merge_log_attributes, release_name_from, Metadata};

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
    fn client_options_enables_logs_and_roundtrips_environment() {
        let env = HashMap::from([("SENTRY_ENVIRONMENT", "stage".to_string())]);
        let metadata = Metadata::from_lookup(|key| env.get(key).cloned());

        let options = metadata.client_options_with_release("zebrad@test".to_string());

        assert!(options.enable_logs);
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
}
