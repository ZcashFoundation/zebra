//! Bounded metrics for Zebra's peer connection lifecycle.
//!
//! Keeping these metrics at the crate root prevents address-book and peer-set code
//! from reaching through peer implementation details.

use std::io::ErrorKind;

use zebra_chain::{
    parameters::{Network, NetworkKind},
    serialization::SerializationError,
};

use crate::{
    peer::{ConnectedAddr, HandshakeError},
    BoxError, PeerSocketAddr,
};

const CONNECTION_ATTEMPTS_TOTAL: &str = "zcash.net.connection.attempts.total";
const CONNECTION_OUTCOMES_TOTAL: &str = "zcash.net.connection.outcomes.total";
const REMOTE_VERSION_MESSAGES_TOTAL: &str = "zcash.net.peer.version.messages.total";
const REMOTE_VERSION_OUTCOMES_TOTAL: &str = "zcash.net.peer.version.outcomes.total";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionDirection {
    Inbound,
    Outbound,
}

impl ConnectionDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ConnectionErrorLabels {
    stage: &'static str,
    outcome: &'static str,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct RemoteVersionMetricLabels {
    network: &'static str,
    direction: &'static str,
    address_family: &'static str,
    implementation: &'static str,
}

/// Records exactly one terminal outcome after Zebra decodes a remote `Version` message.
///
/// Dropping this guard before an explicit outcome records a cancellation. This makes
/// the metric complete when the outer connection timeout cancels an in-progress handshake.
#[must_use = "dropping the guard records a canceled remote Version outcome"]
pub(crate) struct RemoteVersionOutcomeGuard {
    labels: Option<RemoteVersionMetricLabels>,
    outcome_recorded: bool,
}

impl RemoteVersionOutcomeGuard {
    /// Records a decoded remote `Version` message and starts tracking its terminal outcome.
    pub(crate) fn new(network: &Network, connected_addr: &ConnectedAddr, user_agent: &str) -> Self {
        let labels =
            connection_labels(connected_addr).map(|(direction, addr)| RemoteVersionMetricLabels {
                network: network_kind_label(network),
                direction: direction.label(),
                address_family: address_family_label(addr),
                implementation: implementation_label(user_agent),
            });

        if let Some(labels) = labels {
            metrics::counter!(
                REMOTE_VERSION_MESSAGES_TOTAL,
                "network" => labels.network,
                "direction" => labels.direction,
                "address_family" => labels.address_family,
                "implementation" => labels.implementation,
            )
            .increment(1);
        }

        Self {
            labels,
            outcome_recorded: false,
        }
    }

    /// Records a handshake error and returns it for propagation to the caller.
    pub(crate) fn record_error(&mut self, error: HandshakeError) -> HandshakeError {
        self.record_outcome(handshake_error_outcome(&error));
        error
    }

    /// Records a successful handshake without emitting a cancellation when this guard is dropped.
    pub(crate) fn record_success(mut self) {
        self.record_outcome("success");
    }

    fn record_outcome(&mut self, outcome: &'static str) {
        if std::mem::replace(&mut self.outcome_recorded, true) {
            return;
        }

        let Some(labels) = self.labels else {
            return;
        };

        metrics::counter!(
            REMOTE_VERSION_OUTCOMES_TOTAL,
            "network" => labels.network,
            "direction" => labels.direction,
            "address_family" => labels.address_family,
            "implementation" => labels.implementation,
            "outcome" => outcome,
        )
        .increment(1);
    }
}

impl Drop for RemoteVersionOutcomeGuard {
    fn drop(&mut self) {
        self.record_outcome("canceled");
    }
}

/// Returns a bounded metrics label for `network`.
pub(crate) fn network_kind_label(network: &Network) -> &'static str {
    match network.kind() {
        NetworkKind::Mainnet => "mainnet",
        NetworkKind::Testnet => "testnet",
        NetworkKind::Regtest => "regtest",
    }
}

/// Records that Zebra started a TCP connection attempt.
pub(crate) fn record_connection_attempt_started(
    network: &Network,
    direction: ConnectionDirection,
    addr: PeerSocketAddr,
) {
    metrics::counter!(
        CONNECTION_ATTEMPTS_TOTAL,
        "network" => network_kind_label(network),
        "direction" => direction.label(),
        "address_family" => address_family_label(addr),
    )
    .increment(1);
}

/// Records the terminal result of a TCP connection and Zcash handshake attempt.
pub(crate) fn record_connection_attempt_finished(
    network: &Network,
    direction: ConnectionDirection,
    addr: PeerSocketAddr,
    error: Option<&BoxError>,
) {
    let labels = error.map_or(
        ConnectionErrorLabels {
            stage: "handshake",
            outcome: "success",
        },
        |error| classify_connection_error(direction, error),
    );

    metrics::counter!(
        CONNECTION_OUTCOMES_TOTAL,
        "network" => network_kind_label(network),
        "direction" => direction.label(),
        "address_family" => address_family_label(addr),
        "stage" => labels.stage,
        "outcome" => labels.outcome,
    )
    .increment(1);
}

/// Records an inbound TCP connection that Zebra rejected before its Zcash handshake.
pub(crate) fn record_inbound_connection_rejected(
    network: &Network,
    addr: PeerSocketAddr,
    outcome: &'static str,
) {
    metrics::counter!(
        CONNECTION_OUTCOMES_TOTAL,
        "network" => network_kind_label(network),
        "direction" => ConnectionDirection::Inbound.label(),
        "address_family" => address_family_label(addr),
        "stage" => "admission",
        "outcome" => outcome,
    )
    .increment(1);
}

fn classify_connection_error(
    direction: ConnectionDirection,
    error: &BoxError,
) -> ConnectionErrorLabels {
    if error.is::<tower::timeout::error::Elapsed>() {
        return ConnectionErrorLabels {
            stage: match direction {
                ConnectionDirection::Inbound => "handshake",
                ConnectionDirection::Outbound => "tcp_or_handshake",
            },
            outcome: "timeout",
        };
    }

    if error.is::<tokio::time::error::Elapsed>() {
        return ConnectionErrorLabels {
            stage: "handshake",
            outcome: "timeout",
        };
    }

    if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
        return classify_io_error("tcp_connect", io_error);
    }

    let Some(handshake_error) = error.downcast_ref::<HandshakeError>() else {
        return ConnectionErrorLabels {
            stage: "unknown",
            outcome: "other",
        };
    };

    let stage = match handshake_error {
        HandshakeError::RemoteNonceReuse
        | HandshakeError::ObsoleteVersion(_)
        | HandshakeError::MissingRequiredServices { .. } => "version",
        _ => "handshake",
    };

    ConnectionErrorLabels {
        stage,
        outcome: handshake_error_outcome(handshake_error),
    }
}

fn handshake_error_outcome(error: &HandshakeError) -> &'static str {
    match error {
        HandshakeError::UnexpectedMessage(_) => "unexpected_message",
        HandshakeError::RemoteNonceReuse => "self_connection",
        HandshakeError::LocalDuplicateNonce => "local_nonce_collision",
        HandshakeError::ConnectionClosed => "connection_closed",
        HandshakeError::Io(io_error) => io_error_outcome(io_error),
        HandshakeError::Serialization(SerializationError::Io(io_error)) => {
            io_error_outcome(io_error)
        }
        HandshakeError::Serialization(_) => "protocol_parse_error",
        HandshakeError::ObsoleteVersion(_) => "obsolete_version",
        HandshakeError::MissingRequiredServices { .. } => "missing_required_services",
        HandshakeError::Timeout => "timeout",
    }
}

fn classify_io_error(stage: &'static str, error: &std::io::Error) -> ConnectionErrorLabels {
    ConnectionErrorLabels {
        stage,
        outcome: io_error_outcome(error),
    }
}

fn io_error_outcome(error: &std::io::Error) -> &'static str {
    match error.kind() {
        ErrorKind::ConnectionRefused => "connection_refused",
        ErrorKind::ConnectionReset => "connection_reset",
        ErrorKind::ConnectionAborted => "connection_aborted",
        ErrorKind::NotConnected => "not_connected",
        ErrorKind::AddrInUse => "address_in_use",
        ErrorKind::AddrNotAvailable => "address_not_available",
        ErrorKind::BrokenPipe => "broken_pipe",
        ErrorKind::TimedOut => "timeout",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::NetworkUnreachable => "network_unreachable",
        ErrorKind::HostUnreachable => "host_unreachable",
        _ => "io_error",
    }
}

fn connection_labels(
    connected_addr: &ConnectedAddr,
) -> Option<(ConnectionDirection, PeerSocketAddr)> {
    match connected_addr {
        ConnectedAddr::OutboundDirect { addr } => Some((ConnectionDirection::Outbound, *addr)),
        ConnectedAddr::InboundDirect { addr } => Some((ConnectionDirection::Inbound, *addr)),
        ConnectedAddr::OutboundProxy {
            transient_local_addr,
            ..
        } => Some((
            ConnectionDirection::Outbound,
            (*transient_local_addr).into(),
        )),
        ConnectedAddr::InboundProxy { transient_addr } => {
            Some((ConnectionDirection::Inbound, (*transient_addr).into()))
        }
        ConnectedAddr::Isolated => None,
    }
}

fn address_family_label(addr: PeerSocketAddr) -> &'static str {
    if addr.is_ipv4() {
        "ipv4"
    } else {
        "ipv6"
    }
}

fn implementation_label(user_agent: &str) -> &'static str {
    if user_agent.starts_with("/Zakura:") {
        "zakura"
    } else if user_agent.starts_with("/Zebra:") {
        "zebra"
    } else if user_agent.starts_with("/MagicBean:") {
        "legacy_zcashd"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future, io,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use super::*;
    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };
    use tower::{service_fn, ServiceExt};

    #[derive(Default)]
    struct CounterRegistrationRecorder {
        keys: Mutex<Vec<(String, Vec<(String, String)>)>>,
    }

    impl CounterRegistrationRecorder {
        fn label_values(&self, metric_name: &str, label_name: &str) -> Vec<String> {
            self.keys
                .lock()
                .expect("the test recorder mutex must not be poisoned")
                .iter()
                .filter(|(name, _)| name == metric_name)
                .filter_map(|(_, labels)| {
                    labels
                        .iter()
                        .find(|(key, _)| key == label_name)
                        .map(|(_, value)| value.clone())
                })
                .collect()
        }
    }

    impl Recorder for CounterRegistrationRecorder {
        fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {
        }

        fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

        fn describe_histogram(
            &self,
            _key: KeyName,
            _unit: Option<Unit>,
            _description: SharedString,
        ) {
        }

        fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
            let labels = key
                .labels()
                .map(|label| (label.key().to_owned(), label.value().to_owned()))
                .collect();

            self.keys
                .lock()
                .expect("the test recorder mutex must not be poisoned")
                .push((key.name().to_owned(), labels));

            Counter::noop()
        }

        fn register_gauge(&self, _key: &Key, _metadata: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }

        fn register_histogram(&self, _key: &Key, _metadata: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

    #[test]
    fn user_agent_implementation_labels_are_bounded() {
        assert_eq!(implementation_label("/Zakura:1.2.3/"), "zakura");
        assert_eq!(implementation_label("/Zebra:5.1.0/"), "zebra");
        assert_eq!(implementation_label("/MagicBean:6.2.0/"), "legacy_zcashd");
        assert_eq!(implementation_label("/another-node:1.0/"), "other");
        assert_eq!(implementation_label(""), "other");
    }

    #[test]
    fn serialization_io_errors_keep_their_transport_meaning() {
        let error: BoxError = HandshakeError::Serialization(SerializationError::Io(Arc::new(
            io::Error::new(ErrorKind::ConnectionReset, "reset by test peer"),
        )))
        .into();

        assert_eq!(
            classify_connection_error(ConnectionDirection::Inbound, &error),
            ConnectionErrorLabels {
                stage: "handshake",
                outcome: "connection_reset",
            }
        );
    }

    #[test]
    fn tcp_errors_are_distinct_from_handshake_errors() {
        let error: BoxError =
            io::Error::new(ErrorKind::ConnectionRefused, "test listener is closed").into();

        assert_eq!(
            classify_connection_error(ConnectionDirection::Outbound, &error),
            ConnectionErrorLabels {
                stage: "tcp_connect",
                outcome: "connection_refused",
            }
        );
    }

    #[tokio::test]
    async fn outbound_outer_timeout_keeps_its_combined_stage() {
        let service = tower::timeout::Timeout::new(
            service_fn(|()| future::pending::<Result<(), BoxError>>()),
            Duration::ZERO,
        );
        let error = service
            .oneshot(())
            .await
            .expect_err("the pending test service must time out");

        assert_eq!(
            classify_connection_error(ConnectionDirection::Outbound, &error),
            ConnectionErrorLabels {
                stage: "tcp_or_handshake",
                outcome: "timeout",
            }
        );
    }

    #[tokio::test]
    async fn inbound_outer_timeout_is_a_handshake_timeout() {
        let service = tower::timeout::Timeout::new(
            service_fn(|()| future::pending::<Result<(), BoxError>>()),
            Duration::ZERO,
        );
        let error = service
            .oneshot(())
            .await
            .expect_err("the pending test service must time out");

        assert_eq!(
            classify_connection_error(ConnectionDirection::Inbound, &error),
            ConnectionErrorLabels {
                stage: "handshake",
                outcome: "timeout",
            }
        );
    }

    #[tokio::test]
    async fn tokio_timeout_is_a_handshake_timeout() {
        let error: BoxError = tokio::time::timeout(Duration::ZERO, future::pending::<()>())
            .await
            .expect_err("the pending test future must time out")
            .into();

        assert_eq!(
            classify_connection_error(ConnectionDirection::Inbound, &error),
            ConnectionErrorLabels {
                stage: "handshake",
                outcome: "timeout",
            }
        );
    }

    #[test]
    fn remote_version_outcomes_are_cancellation_safe() {
        let recorder = CounterRegistrationRecorder::default();
        let connected_addr = ConnectedAddr::InboundDirect {
            addr: "127.0.0.1:8233"
                .parse()
                .expect("the test peer address must be valid"),
        };

        metrics::with_local_recorder(&recorder, || {
            drop(RemoteVersionOutcomeGuard::new(
                &Network::Mainnet,
                &connected_addr,
                "/Zakura:1.2.3/",
            ));

            RemoteVersionOutcomeGuard::new(&Network::Mainnet, &connected_addr, "/Zebra:3.0.0/")
                .record_success();
        });

        assert_eq!(
            recorder.label_values(REMOTE_VERSION_MESSAGES_TOTAL, "implementation"),
            ["zakura", "zebra"]
        );
        assert_eq!(
            recorder.label_values(REMOTE_VERSION_OUTCOMES_TOTAL, "outcome"),
            ["canceled", "success"]
        );
    }
}
