//! Test tooling for the default-off Zakura Iroh/QUIC stack.

mod clock;
mod cluster;
mod endpoint;
mod gossip;
mod hostile;
mod matrix;
#[cfg(test)]
mod mock_blocksync;
mod node;
mod pinned;
mod recorder;
mod trace_capture;
mod trace_reader;
mod wait;

pub use clock::{Clock, RealClock, TestClock};
pub use cluster::{ClusterTopology, ZakuraTestCluster};
pub use endpoint::LocalEndpointFactory;
pub use gossip::GossipNode;
pub use hostile::HostilePeer;
pub use matrix::{
    default_profiles, default_remote_profiles, expected_outcomes, run_default_matrix,
    ExpectedOutcome, MatrixCell, MatrixExpectation, ProfileId,
};
pub use node::{ZakuraTestNode, ZakuraTestNodeBuilder};
pub use pinned::{
    PeerProfile, PinnedNegotiation, PinnedPeer, PinnedProfileError, PinnedZakuraProfile,
    MAX_PINNED_ALPNS,
};
pub use recorder::{InboundRecorder, RecordedInbound};
pub use trace_capture::TraceCapture;
pub use trace_reader::{TraceQuery, TraceReader, TraceValue};
pub use wait::{await_until, WaitError};
pub use zebra_jsonl_trace::JsonlTracer;
