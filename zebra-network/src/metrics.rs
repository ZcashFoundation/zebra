//! Descriptions of the metrics this crate records.
//!
//! Applications that install a [`metrics`] recorder call [`describe`] right after installing it, so
//! that exporters can document each metric, for example in Prometheus `# HELP` lines.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric this crate records.
///
/// - `network`: `mainnet`, `testnet`, or `regtest`.
/// - `direction`: `inbound` or `outbound`.
/// - `address_family`: `ipv4` or `ipv6`.
/// - `implementation`: `zebra`, `zakura`, `legacy_zcashd`, or `other`, from the peer's user agent.
/// - `stage`: `admission`, `tcp_connect`, `tcp_or_handshake`, `version`, `handshake`, or
///   `unknown`.
/// - `outcome`: `success`, `canceled`, `banned`, `capacity_or_rate_limited`, or a handshake or
///   I/O error kind such as `timeout`, `connection_refused`, or `obsolete_version`.
/// - `command`: a Zcash protocol command, a request or response name, or, for
///   `zebra.net.connection.state`, a connection state such as `AwaitingRequest`.
/// - `error`: a serialization error kind: `io`, `parse`, `utf8_error`, `try_from_slice_error`,
///   `try_from_int_error`, `from_hex_error`, `amount`, `bad_transaction_balance`, `script`,
///   `opcode`, or `num`.
/// - `result`: `success` or `failure`.
/// - `reason`: for handshake failures, `unexpected_message`, `nonce_reuse`, `duplicate_nonce`,
///   `connection_closed`, `io_error`, `serialization`, `obsolete_version`,
///   `missing_required_services`, or `timeout`; for peer set fallbacks, `no_serving_peer`,
///   `queue_full`, or `expired`.
/// - `kind`: `instant` or `waited`.
/// - `remote_version` and `negotiated_version`: a protocol version Zebra knows, or `other`.
/// - `min_version`: the minimum protocol version Zebra accepts for the current network upgrade.
/// - `missing_service`: the required service a rejected peer doesn't advertise: `NODE_NETWORK`.
/// - `seed`: on the `zcash.net.peers.initial` series that count the peers from configured initial
///   peers, the configured entry when it is one of Zebra's default DNS seeders, or `custom` for any
///   other entry. The series without it counts the peers from the peer cache file.
pub const LABELS: &[(&str, &[&str])] = &[
    (
        "zcash.net.connection.attempts.total",
        &["network", "direction", "address_family"],
    ),
    (
        "zcash.net.connection.outcomes.total",
        &["network", "direction", "address_family", "stage", "outcome"],
    ),
    (
        "zcash.net.peer.version.messages.total",
        &["network", "direction", "address_family", "implementation"],
    ),
    (
        "zcash.net.peer.version.outcomes.total",
        &[
            "network",
            "direction",
            "address_family",
            "implementation",
            "outcome",
        ],
    ),
    ("zebra.net.connection.state", &["command"]),
    ("zebra.net.in.requests", &["command"]),
    ("zebra.net.in.responses", &["command"]),
    ("zebra.net.out.requests", &["command"]),
    ("zebra.net.out.requests.canceled", &["command"]),
    ("zebra.net.out.responses", &["command"]),
    ("zebra.net.in.errors", &["error"]),
    ("zcash.net.in.messages", &["command"]),
    ("zcash.net.out.messages", &["command"]),
    ("zcash.net.peer.handshake.duration_seconds", &["result"]),
    ("zcash.net.peer.handshake.failures.total", &["reason"]),
    (
        "zcash.net.peers.obsolete",
        &["remote_version", "min_version"],
    ),
    ("zcash.net.peers.missing_services", &["missing_service"]),
    (
        "zcash.net.peers.connected",
        &["remote_version", "negotiated_version", "min_version"],
    ),
    ("zcash.net.peers.initial", &["seed"]),
    ("zcash.net.peers", &["network"]),
    ("zcash.net.peer_set.find_fallback", &["reason", "network"]),
    ("zcash.net.peer_set.find_queued", &["network"]),
    ("zcash.net.peer_set.inventory_queued", &["network"]),
    ("zcash.net.peer_set.inventory_refusal", &["kind", "network"]),
    ("pool.num_ready", &["network"]),
    ("pool.num_unready", &["network"]),
    ("crawler.in_flight_handshakes", &["network"]),
    ("candidate_set.responded", &["network"]),
    ("candidate_set.gossiped", &["network"]),
    ("candidate_set.failed", &["network"]),
    ("candidate_set.pending", &["network"]),
    ("candidate_set.recently_live", &["network"]),
    ("candidate_set.disconnected", &["network"]),
];

/// Describes every metric this crate records to the installed [`metrics`] recorder.
pub fn describe() {
    // Connection lifecycle
    describe_counter!(
        "zcash.net.connection.attempts.total",
        Unit::Count,
        "TCP connection attempts started."
    );
    describe_counter!(
        "zcash.net.connection.outcomes.total",
        Unit::Count,
        "Finished connection attempts, by the stage they reached and how they ended."
    );
    describe_counter!(
        "zcash.net.peer.version.messages.total",
        Unit::Count,
        "Version messages decoded from peers during handshakes."
    );
    describe_counter!(
        "zcash.net.peer.version.outcomes.total",
        Unit::Count,
        "Handshakes that decoded a peer's version message, by how they ended."
    );
    describe_gauge!(
        "zebra.net.connection.state",
        Unit::Count,
        "Live peer connections in each connection state."
    );

    // Handshakes
    describe_histogram!(
        "zcash.net.peer.handshake.duration_seconds",
        Unit::Seconds,
        "Time taken to complete or fail a peer handshake."
    );
    describe_counter!(
        "zcash.net.peer.handshake.failures.total",
        Unit::Count,
        "Failed peer handshakes, by reason."
    );
    describe_counter!(
        "zcash.net.peers.connected",
        Unit::Count,
        "Successful peer handshakes, by protocol version."
    );
    describe_gauge!(
        "zcash.net.peers.version.connected",
        Unit::Count,
        "Protocol version of the peer in the most recent successful handshake."
    );
    describe_counter!(
        "zcash.net.peers.obsolete",
        Unit::Count,
        "Handshakes rejected because the peer's protocol version is obsolete."
    );
    describe_gauge!(
        "zcash.net.peers.version.obsolete",
        Unit::Count,
        "Protocol version of the peer in the most recent handshake rejected as obsolete."
    );
    describe_counter!(
        "zcash.net.peers.missing_services",
        Unit::Count,
        "Handshakes rejected because the peer doesn't advertise the required services."
    );

    // Messages
    describe_counter!(
        "zcash.net.in.messages",
        Unit::Count,
        "Protocol messages received from peers, by command."
    );
    describe_counter!(
        "zcash.net.out.messages",
        Unit::Count,
        "Protocol messages sent to peers, by command."
    );
    describe_counter!(
        "zcash.net.in.bytes.total",
        Unit::Bytes,
        "Bytes of protocol messages received from peers."
    );
    describe_counter!(
        "zcash.net.out.bytes.total",
        Unit::Bytes,
        "Bytes of protocol messages sent to peers."
    );
    describe_counter!(
        "zebra.net.in.errors",
        Unit::Count,
        "Messages from peers that failed to deserialize, by error kind."
    );
    describe_counter!(
        "zebra.net.in.requests",
        Unit::Count,
        "Requests received from peers, by request."
    );
    describe_counter!(
        "zebra.net.out.responses",
        Unit::Count,
        "Responses sent to peers, by response."
    );
    describe_counter!(
        "zebra.net.out.requests",
        Unit::Count,
        "Requests sent to peers, by request."
    );
    describe_counter!(
        "zebra.net.in.responses",
        Unit::Count,
        "Responses received from peers, by response."
    );
    describe_counter!(
        "zebra.net.out.requests.canceled",
        Unit::Count,
        "Requests to peers canceled before they were sent, by request."
    );
    describe_counter!(
        "peer.canceled",
        Unit::Count,
        "Requests to peers canceled before they were sent."
    );

    // Inbound overload
    describe_counter!(
        "pool.closed.loadshed",
        Unit::Count,
        "Peer connections closed because Zebra was overloaded with inbound requests."
    );
    describe_counter!(
        "pool.closed.inbound.timeout",
        Unit::Count,
        "Peer connections closed because Zebra timed out handling an inbound request."
    );
    describe_counter!(
        "pool.ignored.loadshed",
        Unit::Count,
        "Inbound requests ignored because Zebra was overloaded."
    );
    describe_counter!(
        "pool.ignored.inbound.timeout",
        Unit::Count,
        "Inbound requests ignored because Zebra timed out handling them."
    );

    // Peer set
    describe_gauge!(
        "zcash.net.peers",
        Unit::Count,
        "Peers connected to the peer set, ready or busy."
    );
    describe_gauge!(
        "pool.num_ready",
        Unit::Count,
        "Peers in the peer set that are ready for a request."
    );
    describe_gauge!(
        "pool.num_unready",
        Unit::Count,
        "Peers in the peer set that are busy or not yet ready."
    );
    describe_counter!(
        "zcash.net.peer_set.find_queued",
        Unit::Count,
        "FindBlocks and FindHeaders requests queued until a peer that serves them is ready."
    );
    describe_counter!(
        "zcash.net.peer_set.find_fallback",
        Unit::Count,
        "FindBlocks and FindHeaders requests sent to a peer that might not serve them, by reason."
    );
    describe_counter!(
        "zcash.net.peer_set.inventory_queued",
        Unit::Count,
        "Inventory requests waiting for a busy peer that might have the inventory."
    );
    describe_counter!(
        "zcash.net.peer_set.inventory_refusal",
        Unit::Count,
        "Inventory requests failed because no peer that might have the inventory could take them."
    );
    describe_counter!(
        "pool.inventory.dropped",
        Unit::Count,
        "Times the inventory registry fell behind and dropped inventory changes."
    );
    describe_counter!(
        "pool.inventory.dropped.messages",
        Unit::Count,
        "Inventory changes dropped because the inventory registry fell behind."
    );

    // Peer discovery
    describe_gauge!(
        "crawler.in_flight_handshakes",
        Unit::Count,
        "Outbound handshakes the peer crawler has in progress."
    );
    describe_counter!(
        "zcash.net.peers.initial",
        Unit::Count,
        "Initial peer addresses from each DNS seeder and the peer cache."
    );
    describe_counter!(
        "zcash.net.peers.cache",
        Unit::Count,
        "Peer addresses written to the peer cache file."
    );
    describe_gauge!(
        "candidate_set.responded",
        Unit::Count,
        "Address book peers that responded to Zebra recently."
    );
    describe_gauge!(
        "candidate_set.gossiped",
        Unit::Count,
        "Address book peers gossiped by other peers that Zebra hasn't tried yet."
    );
    describe_gauge!(
        "candidate_set.failed",
        Unit::Count,
        "Address book peers whose latest connection attempt failed."
    );
    describe_gauge!(
        "candidate_set.pending",
        Unit::Count,
        "Address book peers with a connection attempt in progress."
    );
    describe_gauge!(
        "candidate_set.recently_live",
        Unit::Count,
        "Address book peers that were live recently."
    );
    describe_gauge!(
        "candidate_set.disconnected",
        Unit::Count,
        "Address book peers that stopped responding recently."
    );
}
