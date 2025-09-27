//! Latency tracking for ping/pong messages sent to peers.

use crate::protocol::external::types::Nonce;
use std::time::{Duration, Instant};

/// Tracks ping latency metrics for a peer connection.
///
/// This struct is used by `Connection` to record round-trip times (RTT)
/// for `ping` and `pong` messages exchanged with a remote peer.
///
/// It is updated within the connection task and not designed for use
/// across threads or shared between tasks.
#[derive(Clone, Debug, Default)]
pub struct PeerLatency {
    /// The timestamp when the last `ping` message was sent.
    ///
    /// Used to calculate round-trip time when a `pong` with the same nonce is received.
    pub ping_sent_at: Option<Instant>,

    /// The most recent round-trip time (RTT) for a completed ping.
    ///
    /// Measures the time between sending a `ping` and receiving the matching `pong`.
    pub last_ping_rtt: Option<Duration>,

    /// Indicates whether a `pong` response is currently expected.
    ///
    /// This is set to `true` after sending a `ping` and cleared once a matching
    /// `pong` is received or the ping times out.
    pub is_waiting_for_pong: bool,

    /// The nonce used in the most recent `ping` message.
    ///
    /// Used to match incoming `pong` messages with the corresponding `ping`.
    pub waiting_nonce: Option<Nonce>,
}
