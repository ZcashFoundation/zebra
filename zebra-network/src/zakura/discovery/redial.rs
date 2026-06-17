//! Supervised native Zakura dialing and redial policy.

use std::{future::Future, pin::Pin, time::Duration};

use iroh::NodeAddr;
use tokio::time::Instant;

use crate::zakura::{ZakuraEndpoint, ZakuraLocalLimits, ZakuraPeerId};

/// A connection that served at least this long is treated as healthy, so the
/// next re-dial after it drops starts from the initial (fast) backoff again
/// instead of penalising a long-lived peer for an eventual disconnect.
pub(super) const ZAKURA_REDIAL_HEALTHY_CONNECTION: Duration = Duration::from_secs(60);

/// Controls how [`native_dial_supervised`] retries and re-dials a peer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RedialPolicy {
    initial_backoff: Duration,
    max_backoff: Duration,
    /// Stop after this many consecutive failed attempts; `None` retries forever.
    max_attempts: Option<usize>,
    /// How long an already-registered peer must remain registered before a
    /// connect-once dial treats the connection as healthy and exits.
    registered_settle_timeout: Duration,
    /// Re-dial again after a healthy connection drops. Configured bootstrap
    /// peers and legacy->Zakura upgrade hand-offs set this because they are the
    /// active owners of the Zakura dial.
    redial_after_drop: bool,
}

impl RedialPolicy {
    /// Maintain a connection indefinitely, re-dialing on drop (bootstrap peers).
    pub(crate) fn maintain(initial_backoff: Duration, max_backoff: Duration) -> Self {
        Self {
            initial_backoff,
            max_backoff,
            max_attempts: None,
            registered_settle_timeout: ZAKURA_REDIAL_HEALTHY_CONNECTION,
            redial_after_drop: true,
        }
    }

    /// Connect once, retrying only the initial dial up to `attempts` times.
    #[cfg(test)]
    fn connect_once(initial_backoff: Duration, max_backoff: Duration, attempts: usize) -> Self {
        Self::connect_once_with_registered_settle(
            initial_backoff,
            max_backoff,
            attempts,
            ZAKURA_REDIAL_HEALTHY_CONNECTION,
        )
    }

    #[cfg(test)]
    fn connect_once_with_registered_settle(
        initial_backoff: Duration,
        max_backoff: Duration,
        attempts: usize,
        registered_settle_timeout: Duration,
    ) -> Self {
        Self {
            initial_backoff,
            max_backoff,
            max_attempts: Some(attempts),
            registered_settle_timeout,
            redial_after_drop: false,
        }
    }
}

/// Outcome of one dial attempt, as seen by [`run_dial_supervisor`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DialResult {
    /// Connected and served at least [`ZAKURA_REDIAL_HEALTHY_CONNECTION`].
    Healthy,
    /// Failed to establish, or served only briefly (e.g. a duplicate was closed).
    Failed,
}

/// Maintain a Zakura connection to `node_addr`, re-dialing with bounded backoff.
///
/// `native_bootstrap_dial` returns when the connection fails to establish or,
/// on success, when serving ends (the peer dropped or a duplicate was closed),
/// so a single loop covers both the initial connect, including the startup
/// race where the seed's endpoint is not yet listening, and reconnection after
/// a drop. The retry/backoff policy lives in [`run_dial_supervisor`]; this just
/// supplies the real dial attempt and the supervisor's registration watch.
pub(crate) async fn native_dial_supervised(
    endpoint: ZakuraEndpoint,
    node_addr: NodeAddr,
    limits: ZakuraLocalLimits,
    policy: RedialPolicy,
) {
    let Ok(peer_id) = ZakuraPeerId::new(node_addr.node_id.as_bytes().to_vec()) else {
        tracing::warn!(?node_addr, "invalid Zakura bootstrap node id; not dialing");
        return;
    };

    let shutdown = endpoint.background_shutdown_token();
    let registered = endpoint.supervisor().subscribe();
    let supervised = run_dial_supervisor(peer_id, registered, policy, move || {
        let endpoint = endpoint.clone();
        let node_addr = node_addr.clone();
        let limits = limits.clone();
        Box::pin(async move {
            let started = Instant::now();
            match super::dialer::native_bootstrap_dial(&endpoint, node_addr, &limits).await {
                Ok(()) if started.elapsed() >= ZAKURA_REDIAL_HEALTHY_CONNECTION => {
                    DialResult::Healthy
                }
                Ok(()) => DialResult::Failed,
                Err(error) => {
                    tracing::debug!(?error, "Zakura native dial failed; will retry");
                    DialResult::Failed
                }
            }
        }) as Pin<Box<dyn Future<Output = DialResult> + Send>>
    });

    // Endpoint shutdown cancels this token; stop maintaining the dial promptly
    // rather than looping on against a torn-down router. A `maintain` policy
    // never exits on its own (no max attempts), and the supervisor registration
    // watch stays open while this task holds an endpoint clone, so this is the
    // only signal that reliably ends the loop at teardown.
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {}
        _ = supervised => {}
    }
}

/// Retry/backoff loop shared by configured bootstrap peers and the upgrade dial.
///
/// Before each dial it skips a peer that is already registered (it may have
/// dialed us first) so the two directions do not churn duplicate connections.
/// Exits when `policy.max_attempts` consecutive attempts fail, when a
/// `connect_once` peer connects or finishes serving, or when the supervisor's
/// registration watch closes (node shutdown). `dial` is injected so the loop is
/// unit-testable without real network I/O.
async fn run_dial_supervisor<F>(
    peer_id: ZakuraPeerId,
    mut registered: tokio::sync::watch::Receiver<Vec<ZakuraPeerId>>,
    policy: RedialPolicy,
    mut dial: F,
) where
    F: FnMut() -> Pin<Box<dyn Future<Output = DialResult> + Send>>,
{
    let mut backoff = policy.initial_backoff;
    let mut failures = 0usize;

    loop {
        // Already connected (possibly an inbound dial from the same peer).
        if registered
            .borrow_and_update()
            .iter()
            .any(|id| id == &peer_id)
        {
            if !policy.redial_after_drop {
                // Wait for it to deregister, then re-dial promptly.
                tokio::select! {
                    changed = registered.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        backoff = policy.initial_backoff;
                        failures = 0;
                        continue;
                    }
                    _ = tokio::time::sleep(policy.registered_settle_timeout) => {
                        return;
                    }
                }
            }
            if registered.changed().await.is_err() {
                return;
            }
            backoff = policy.initial_backoff;
            failures = 0;
            continue;
        }

        match dial().await {
            DialResult::Healthy => {
                if !policy.redial_after_drop {
                    return;
                }
                backoff = policy.initial_backoff;
                failures = 0;
                continue;
            }
            DialResult::Failed => {}
        }

        failures += 1;
        if policy.max_attempts.is_some_and(|max| failures >= max) {
            return;
        }

        // Back off, but wake early only if the peer appears in the supervisor
        // from another connection. Ignore unrelated peer-set changes,
        // including the deregistration from the failed attempt we just
        // observed.
        let sleep = tokio::time::sleep(backoff);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                changed = registered.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if registered.borrow().iter().any(|id| id == &peer_id) {
                        break;
                    }
                }
                _ = &mut sleep => break,
            }
        }
        backoff = backoff.saturating_mul(2).min(policy.max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redial_test_peer_id() -> ZakuraPeerId {
        ZakuraPeerId::new(vec![9u8; 32]).expect("32-byte node id is valid")
    }

    fn count_dial(
        calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        result: DialResult,
    ) -> impl FnMut() -> Pin<Box<dyn Future<Output = DialResult> + Send>> {
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { result }) as Pin<Box<dyn Future<Output = DialResult> + Send>>
        }
    }

    fn dial_count(calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A `connect_once` dial that keeps failing gives up after `max_attempts`.
    #[tokio::test]
    async fn dial_supervisor_connect_once_gives_up_after_max_attempts() {
        let (_tx, registered) = tokio::sync::watch::channel(Vec::<ZakuraPeerId>::new());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let policy =
            RedialPolicy::connect_once(Duration::from_millis(1), Duration::from_millis(1), 3);

        tokio::time::timeout(
            Duration::from_secs(5),
            run_dial_supervisor(
                redial_test_peer_id(),
                registered,
                policy,
                count_dial(&calls, DialResult::Failed),
            ),
        )
        .await
        .expect("connect_once must stop after exhausting its attempts");

        assert_eq!(dial_count(&calls), 3);
    }

    /// A `connect_once` dial that connects healthily stops without re-dialing.
    #[tokio::test]
    async fn dial_supervisor_connect_once_stops_after_healthy_connection() {
        let (_tx, registered) = tokio::sync::watch::channel(Vec::<ZakuraPeerId>::new());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let policy =
            RedialPolicy::connect_once(Duration::from_millis(1), Duration::from_millis(1), 3);

        tokio::time::timeout(
            Duration::from_secs(5),
            run_dial_supervisor(
                redial_test_peer_id(),
                registered,
                policy,
                count_dial(&calls, DialResult::Healthy),
            ),
        )
        .await
        .expect("connect_once returns once it has connected");

        assert_eq!(dial_count(&calls), 1);
    }

    /// A peer already registered (e.g. it dialed us first) is not re-dialed if
    /// it remains registered long enough to count as healthy.
    #[tokio::test]
    async fn dial_supervisor_connect_once_skips_stably_registered_peer() {
        let peer_id = redial_test_peer_id();
        let (_tx, registered) = tokio::sync::watch::channel(vec![peer_id.clone()]);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let policy = RedialPolicy::connect_once_with_registered_settle(
            Duration::from_millis(1),
            Duration::from_millis(1),
            3,
            Duration::from_millis(5),
        );

        tokio::time::timeout(
            Duration::from_secs(5),
            run_dial_supervisor(
                peer_id,
                registered,
                policy,
                count_dial(&calls, DialResult::Failed),
            ),
        )
        .await
        .expect("an already-connected connect-once peer returns immediately");

        assert_eq!(dial_count(&calls), 0);
    }

    /// A connect-once upgrade hand-off must not give up just because the peer
    /// was momentarily registered by an inbound dial. If that registration drops
    /// before it is healthy, the outbound hand-off should retry.
    #[tokio::test]
    async fn dial_supervisor_connect_once_redials_after_transient_registration() {
        let peer_id = redial_test_peer_id();
        let (registered_tx, registered) = tokio::sync::watch::channel(vec![peer_id.clone()]);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let policy = RedialPolicy::connect_once_with_registered_settle(
            Duration::from_millis(1),
            Duration::from_millis(1),
            3,
            Duration::from_secs(1),
        );

        let supervisor = tokio::spawn(run_dial_supervisor(
            peer_id,
            registered,
            policy,
            count_dial(&calls, DialResult::Healthy),
        ));

        registered_tx
            .send(Vec::new())
            .expect("dial supervisor still watches registrations");

        tokio::time::timeout(Duration::from_secs(5), supervisor)
            .await
            .expect("connect_once exits after retrying a transient registration")
            .expect("dial supervisor task must not panic");

        assert_eq!(dial_count(&calls), 1);
    }

    /// A `maintain` peer keeps re-dialing after each connection drops.
    #[tokio::test]
    async fn dial_supervisor_maintain_redials_after_drop() {
        let (_tx, registered) = tokio::sync::watch::channel(Vec::<ZakuraPeerId>::new());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let policy = RedialPolicy::maintain(Duration::from_millis(1), Duration::from_millis(1));

        let dial_calls = calls.clone();
        let dial = move || {
            dial_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(2)).await;
                DialResult::Healthy
            }) as Pin<Box<dyn Future<Output = DialResult> + Send>>
        };

        let supervisor = tokio::spawn(run_dial_supervisor(
            redial_test_peer_id(),
            registered,
            policy,
            dial,
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        while dial_count(&calls) < 3 {
            assert!(
                Instant::now() < deadline,
                "maintain never re-dialed after the connection dropped",
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        supervisor.abort();
    }
}
