//! Check orchestration: one-shot deploy verification and continuous
//! systemd operation.

use std::{
    thread,
    time::{Duration, Instant},
};

use crate::{
    checks::{Check, CheckStatus},
    config::Config,
    reporting::Reporter,
};

/// Runs every check once, reporting each outcome. Returns true when all
/// checks passed.
fn run_cycle(checks: &[Box<dyn Check>], reporter: &mut Reporter) -> bool {
    let mut all_passed = true;

    for check in checks {
        let outcome = check.run_once();
        reporter.report(check.name(), &outcome);

        if outcome.status == CheckStatus::Fail {
            all_passed = false;
        }
    }

    all_passed
}

/// One-shot deploy verification.
///
/// Retries every `sync_check_interval` seconds until all checks pass or
/// `sync_check_timeout` seconds have elapsed, mirroring the retry loop of
/// `deploy/zcashd-compat/sync-check.sh`. Returns true on success.
pub fn one_shot(config: &Config, checks: &[Box<dyn Check>], reporter: &mut Reporter) -> bool {
    let started = Instant::now();
    let timeout = Duration::from_secs(config.sync_check_timeout);

    loop {
        if run_cycle(checks, reporter) {
            tracing::info!("all watchdog checks passed");
            return true;
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            tracing::error!(
                elapsed_secs = elapsed.as_secs(),
                timeout_secs = timeout.as_secs(),
                "watchdog check timed out",
            );
            return false;
        }

        tracing::info!(
            retry_in_secs = config.sync_check_interval,
            "retrying watchdog checks",
        );
        thread::sleep(Duration::from_secs(config.sync_check_interval));
    }
}

/// Continuous watchdog operation for systemd.
///
/// Runs all checks every `watchdog_interval` seconds forever. Failures are
/// reported through the [`Reporter`] (Sentry events on transitions); the
/// process itself only exits when killed, so systemd `Restart=always`
/// covers watchdog crashes.
pub fn run_forever(config: &Config, checks: &[Box<dyn Check>], reporter: &mut Reporter) -> ! {
    tracing::info!(
        interval_secs = config.watchdog_interval,
        checks = checks.len(),
        "starting continuous watchdog",
    );

    loop {
        run_cycle(checks, reporter);
        thread::sleep(Duration::from_secs(config.watchdog_interval));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::checks::CheckOutcome;

    use super::*;

    /// A scripted check that fails for the first `failures` runs and passes
    /// afterwards.
    struct ScriptedCheck {
        failures: usize,
        runs: AtomicUsize,
    }

    impl Check for ScriptedCheck {
        fn name(&self) -> &'static str {
            "scripted"
        }

        fn run_once(&self) -> CheckOutcome {
            let run = self.runs.fetch_add(1, Ordering::SeqCst);
            if run < self.failures {
                CheckOutcome::fail("scripted failure", BTreeMap::new())
            } else {
                CheckOutcome::pass("scripted pass", BTreeMap::new())
            }
        }
    }

    fn test_config(timeout: u64, interval: u64) -> Config {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            config: Config,
        }

        let mut config = TestCli::try_parse_from(["zebra-watchdog"])
            .expect("defaults parse")
            .config;
        config.sync_check_timeout = timeout;
        config.sync_check_interval = interval;
        config
    }

    #[test]
    fn one_shot_passes_immediately_when_checks_pass() {
        let checks: Vec<Box<dyn Check>> = vec![Box::new(ScriptedCheck {
            failures: 0,
            runs: AtomicUsize::new(0),
        })];
        let mut reporter = Reporter::new(false);

        assert!(one_shot(&test_config(5, 1), &checks, &mut reporter));
    }

    #[test]
    fn one_shot_retries_until_checks_pass() {
        let checks: Vec<Box<dyn Check>> = vec![Box::new(ScriptedCheck {
            failures: 2,
            runs: AtomicUsize::new(0),
        })];
        let mut reporter = Reporter::new(false);

        assert!(one_shot(&test_config(30, 0), &checks, &mut reporter));
    }

    #[test]
    fn one_shot_fails_after_timeout() {
        let checks: Vec<Box<dyn Check>> = vec![Box::new(ScriptedCheck {
            failures: usize::MAX,
            runs: AtomicUsize::new(0),
        })];
        let mut reporter = Reporter::new(false);

        assert!(!one_shot(&test_config(0, 0), &checks, &mut reporter));
    }
}
