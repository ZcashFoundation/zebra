//! Status reporting: structured tracing logs plus Sentry events on
//! failure/recovery transitions.
//!
//! Every check cycle is logged through `tracing`. When a Sentry DSN is
//! configured, logs flow to Sentry via the tracing integration, and explicit
//! Sentry events are captured only when a check transitions between passing
//! and failing, so a persistent failure does not spam one event per cycle.

use std::collections::{BTreeMap, HashMap};

use sentry::protocol::{Event, Map, Value};

use crate::checks::{CheckOutcome, CheckStatus};

/// Active alert suppression window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlertSuppression {
    /// Why alerts are currently suppressed.
    pub reason: &'static str,

    /// Unix timestamp when suppression expires.
    pub until_epoch_seconds: u64,
}

/// A status transition worth reporting as a discrete event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transition {
    /// The check failed and was previously passing (or this is the first
    /// observation).
    Failed,
    /// The check passed after previously failing.
    Recovered,
}

/// Decides whether the new status is a reportable transition.
///
/// First observations only report failures: a check that starts out passing
/// is the expected steady state and does not need an event.
pub fn transition(previous: Option<CheckStatus>, current: CheckStatus) -> Option<Transition> {
    match (previous, current) {
        (None | Some(CheckStatus::Pass), CheckStatus::Fail) => Some(Transition::Failed),
        (Some(CheckStatus::Fail), CheckStatus::Pass) => Some(Transition::Recovered),
        _ => None,
    }
}

/// Tracks per-check status and emits logs and Sentry transition events.
pub struct Reporter {
    sentry_enabled: bool,
    last_status: HashMap<&'static str, CheckStatus>,
}

impl Reporter {
    /// Creates a reporter. `sentry_enabled` should reflect whether a Sentry
    /// client was initialized.
    pub fn new(sentry_enabled: bool) -> Self {
        Self {
            sentry_enabled,
            last_status: HashMap::new(),
        }
    }

    /// Logs the outcome of one check cycle and captures a Sentry event when
    /// the check transitioned between passing and failing.
    ///
    /// Suppressed failures are still logged locally at info level, but they do
    /// not update alert state. If the failure persists after suppression
    /// expires, the next unsuppressed cycle reports a normal failure transition.
    pub fn report(
        &mut self,
        check_name: &'static str,
        outcome: &CheckOutcome,
        suppression: Option<&AlertSuppression>,
    ) {
        if let Some(suppression) = suppression {
            self.report_suppressed(check_name, outcome, suppression);
            return;
        }

        match outcome.status {
            CheckStatus::Pass => {
                tracing::info!(check = check_name, summary = %outcome.summary, "check passed");
            }
            CheckStatus::Fail => {
                tracing::warn!(
                    check = check_name,
                    summary = %outcome.summary,
                    details = ?outcome.details,
                    "check failed",
                );
            }
        }

        let previous = self.last_status.insert(check_name, outcome.status);
        let Some(transition) = transition(previous, outcome.status) else {
            return;
        };

        match transition {
            Transition::Failed => {
                tracing::error!(
                    check = check_name,
                    summary = %outcome.summary,
                    "check transitioned to failing",
                );
            }
            Transition::Recovered => {
                tracing::info!(
                    check = check_name,
                    summary = %outcome.summary,
                    "check recovered",
                );
            }
        }

        if self.sentry_enabled {
            sentry::capture_event(transition_event(check_name, transition, outcome));
        }
    }

    /// Logs a check outcome while deployment alert suppression is active.
    fn report_suppressed(
        &self,
        check_name: &'static str,
        outcome: &CheckOutcome,
        suppression: &AlertSuppression,
    ) {
        match outcome.status {
            CheckStatus::Pass => {
                tracing::info!(
                    check = check_name,
                    summary = %outcome.summary,
                    suppression_reason = suppression.reason,
                    suppression_until = suppression.until_epoch_seconds,
                    "check passed during alert suppression",
                );
            }
            CheckStatus::Fail => {
                tracing::info!(
                    check = check_name,
                    summary = %outcome.summary,
                    details = ?outcome.details,
                    suppression_reason = suppression.reason,
                    suppression_until = suppression.until_epoch_seconds,
                    "check failed during alert suppression",
                );
            }
        }
    }
}

/// Builds a Sentry event for a check transition, tagged with the check name
/// and carrying the outcome details as event extras.
fn transition_event(
    check_name: &'static str,
    transition: Transition,
    outcome: &CheckOutcome,
) -> Event<'static> {
    let (level, message) = match transition {
        Transition::Failed => (
            sentry::Level::Error,
            format!("watchdog check failed: {check_name}: {}", outcome.summary),
        ),
        Transition::Recovered => (
            sentry::Level::Info,
            format!(
                "watchdog check recovered: {check_name}: {}",
                outcome.summary
            ),
        ),
    };

    let mut tags = BTreeMap::new();
    tags.insert("watchdog.check".to_owned(), check_name.to_owned());
    tags.insert(
        "watchdog.transition".to_owned(),
        match transition {
            Transition::Failed => "failed".to_owned(),
            Transition::Recovered => "recovered".to_owned(),
        },
    );

    let extra: Map<String, Value> = outcome
        .details
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();

    Event {
        message: Some(message),
        level,
        tags,
        extra,
        ..Default::default()
    }
}

/// Initializes Sentry when `SENTRY_DSN` is set in the environment.
///
/// Returns the client guard (which must stay alive for events to be sent)
/// when Sentry is enabled, and `None` otherwise. The DSN, environment, and
/// release are read from the standard `SENTRY_DSN`, `SENTRY_ENVIRONMENT`,
/// and `SENTRY_RELEASE` environment variables by the Sentry SDK.
pub fn init_sentry() -> Option<sentry::ClientInitGuard> {
    std::env::var_os("SENTRY_DSN")?;

    let release = std::env::var("SENTRY_RELEASE")
        .unwrap_or_else(|_| format!("zebra-watchdog@{}", env!("CARGO_PKG_VERSION")));

    let guard = sentry::init(sentry::ClientOptions {
        release: Some(release.into()),
        enable_logs: true,
        ..Default::default()
    });

    sentry::configure_scope(|scope| {
        scope.set_tag("service", "zebra-watchdog");
    });

    Some(guard)
}

/// A tracing layer that forwards logs to Sentry.
///
/// Errors and warnings become Sentry logs, info events become breadcrumbs
/// attached to transition events. Discrete Sentry events are only captured
/// explicitly by [`Reporter`] on transitions, never by this layer.
pub fn sentry_tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    use sentry::integrations::tracing::EventFilter;

    sentry::integrations::tracing::layer().event_filter(|metadata| match *metadata.level() {
        tracing::Level::ERROR | tracing::Level::WARN => EventFilter::Log,
        tracing::Level::INFO => EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_reports_failures_only() {
        assert_eq!(
            transition(None, CheckStatus::Fail),
            Some(Transition::Failed)
        );
        assert_eq!(transition(None, CheckStatus::Pass), None);
    }

    #[test]
    fn steady_states_are_not_reported() {
        assert_eq!(transition(Some(CheckStatus::Pass), CheckStatus::Pass), None);
        assert_eq!(transition(Some(CheckStatus::Fail), CheckStatus::Fail), None);
    }

    #[test]
    fn transitions_are_reported() {
        assert_eq!(
            transition(Some(CheckStatus::Pass), CheckStatus::Fail),
            Some(Transition::Failed)
        );
        assert_eq!(
            transition(Some(CheckStatus::Fail), CheckStatus::Pass),
            Some(Transition::Recovered)
        );
    }

    #[test]
    fn transition_event_carries_check_details() {
        let mut details = std::collections::BTreeMap::new();
        details.insert("height_drift".to_owned(), "42".to_owned());

        let outcome = CheckOutcome::fail("drift too large", details);
        let event = transition_event("zcashd_compat_sync", Transition::Failed, &outcome);

        assert_eq!(event.level, sentry::Level::Error);
        assert_eq!(
            event.tags.get("watchdog.check").map(String::as_str),
            Some("zcashd_compat_sync")
        );
        assert_eq!(
            event.extra.get("height_drift"),
            Some(&Value::String("42".to_owned()))
        );
        assert!(event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("drift too large")));
    }

    #[test]
    fn suppressed_failures_do_not_update_alert_state() {
        let mut reporter = Reporter::new(false);
        let outcome = CheckOutcome::fail("deployment restart", BTreeMap::new());
        let suppression = AlertSuppression {
            reason: "deployment",
            until_epoch_seconds: u64::MAX,
        };

        reporter.report("zcashd_compat_sync", &outcome, Some(&suppression));

        assert_eq!(reporter.last_status.get("zcashd_compat_sync"), None);

        reporter.report("zcashd_compat_sync", &outcome, None);

        assert_eq!(
            reporter.last_status.get("zcashd_compat_sync"),
            Some(&CheckStatus::Fail)
        );
    }
}
