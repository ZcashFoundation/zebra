//! discovery/pipe.rs — the per-peer discovery pipe (stream 4).
//!
//! THE PHASE-3B DAG SLICE IS THIS DIAGRAM. The code below is a mechanical
//! transcription; the [`PIPE_SHAPE`] const is the inspectable, drift-checked
//! copy of it.
//!
//!  recv ─▶ guard ─▶ decode ─▶ branch(msg)
//!                             ├─ Hello       ─▶ handoff_async
//!                             ├─ GetPeers    ─▶ handoff_async
//!                             ├─ Peers       ─▶ handoff_async
//!                             ├─ GetServices ─▶ handoff_async
//!                             └─ Services    ─▶ handoff_async
//!
//! Discovery's semantic handlers perform real async runtime work and may send
//! responses on this peer's stream. Phase 3b therefore keeps those handlers in
//! `service.rs`: this pipe owns the shared guard/decode/branch traversal, then
//! stores the decoded message in peer-local state for the async runner to take
//! and handle immediately after `Pipe::run_one`.

use super::protocol::{DiscoveryMessage, MAX_DISCOVERY_MESSAGE_BYTES};
use crate::zakura::{
    Edge, Flow, Frame, Node, NodeKind, Pipe, PipeCx, PipeShape, SessionGuard, SinkReject,
    ZakuraPeerId,
};

/// Frame message type carrying a discovery payload (matches the native wire).
pub(super) const DISCOVERY_FRAME_MESSAGE_TYPE: u16 = 1;

const DISCOVERY_ALLOWED_FRAME_TYPES: &[u8] = &[1];

/// Per-peer discovery local state.
pub(super) struct DsLocal {
    decoded: Option<DiscoveryMessage>,
}

impl DsLocal {
    fn new() -> Self {
        Self { decoded: None }
    }

    fn set_decoded(&mut self, msg: DiscoveryMessage) {
        self.decoded = Some(msg);
    }

    /// Take the decoded message handed off by the synchronous pipe traversal.
    pub(super) fn take_decoded(&mut self) -> Option<DiscoveryMessage> {
        self.decoded.take()
    }
}

/// Shared discovery pipe environment.
#[derive(Clone)]
pub(super) struct DsEnv;

/// The Phase-3b discovery pipe DAG slice, as checked documentation.
pub(super) const PIPE_SHAPE: PipeShape = PipeShape {
    service: "discovery",
    nodes: &[
        Node {
            id: "guard",
            kind: NodeKind::Guard,
        },
        Node {
            id: "decode",
            kind: NodeKind::Decode,
        },
        Node {
            id: "branch",
            kind: NodeKind::Branch,
        },
        Node {
            id: "handoff_async",
            kind: NodeKind::Emit,
        },
    ],
    edges: &[
        Edge {
            from: "guard",
            to: "decode",
            on: "Pass",
        },
        Edge {
            from: "decode",
            to: "branch",
            on: "Ok",
        },
        Edge {
            from: "branch",
            to: "handoff_async",
            on: "Hello",
        },
        Edge {
            from: "branch",
            to: "handoff_async",
            on: "GetPeers",
        },
        Edge {
            from: "branch",
            to: "handoff_async",
            on: "Peers",
        },
        Edge {
            from: "branch",
            to: "handoff_async",
            on: "GetServices",
        },
        Edge {
            from: "branch",
            to: "handoff_async",
            on: "Services",
        },
    ],
};

/// Build one peer-owned discovery pipe.
pub(super) fn discovery_pipe(peer_id: ZakuraPeerId) -> Pipe<DsLocal, DsEnv> {
    Pipe::new(
        peer_id,
        DsLocal::new(),
        DsEnv,
        // The transport already applies the connection-global count bucket.
        // Discovery's service guard owns the stream-4 frame type boundary and
        // the payload cap; protocol-message variants remain payload-level
        // decode decisions because discovery uses one frame envelope type.
        SessionGuard::new(
            DISCOVERY_ALLOWED_FRAME_TYPES,
            // `MAX_DISCOVERY_MESSAGE_BYTES` is 16 KiB, so it fits in `u32`.
            MAX_DISCOVERY_MESSAGE_BYTES as u32,
            None,
        ),
        run_inbound,
        &PIPE_SHAPE,
    )
}

/// Executable transcription of [`PIPE_SHAPE`] — the synchronous pipe entry.
pub(super) fn run_inbound(cx: &mut PipeCx<'_, DsLocal, DsEnv>, frame: Frame) -> Flow<()> {
    let msg = match decode(frame) {
        Flow::Continue(msg) => msg,
        Flow::Done => return Flow::Done,
        Flow::Reject(reject) => return Flow::Reject(reject),
    };

    match &msg {
        DiscoveryMessage::Hello { .. }
        | DiscoveryMessage::GetPeers { .. }
        | DiscoveryMessage::Peers { .. }
        | DiscoveryMessage::GetServices(_)
        | DiscoveryMessage::Services(_) => {
            cx.local.set_decoded(msg);
            Flow::Continue(())
        }
    }
}

fn decode(frame: Frame) -> Flow<DiscoveryMessage> {
    match decode_discovery_frame(&frame) {
        Ok(msg) => Flow::Continue(msg),
        Err(error) => Flow::Reject(SinkReject::protocol(error)),
    }
}

/// Decodes a discovery message from a transport frame, rejecting a frame whose
/// envelope is not a discovery payload.
pub(super) fn decode_discovery_frame(frame: &Frame) -> Result<DiscoveryMessage, crate::BoxError> {
    if frame.message_type != DISCOVERY_FRAME_MESSAGE_TYPE || frame.flags != 0 {
        return Err(format!(
            "unexpected discovery frame envelope (message_type={}, flags={})",
            frame.message_type, frame.flags
        )
        .into());
    }
    DiscoveryMessage::decode(&frame.payload).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE_BRANCHES: [&str; 5] = ["Hello", "GetPeers", "Peers", "GetServices", "Services"];

    fn peer() -> ZakuraPeerId {
        ZakuraPeerId::new(vec![4; 32]).expect("test peer id is within bounds")
    }

    /// A valid discovery frame decodes and is handed off via peer-local state for
    /// the async runner to take.
    #[test]
    fn run_one_hands_off_decoded_message() {
        let mut pipe = discovery_pipe(peer());
        let payload = DiscoveryMessage::GetPeers {
            limit: 8,
            wanted_services: Vec::new(),
            exclude_node_ids: Vec::new(),
        }
        .encode()
        .expect("message encodes");

        let flow = pipe.run_one(Frame {
            message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
            flags: 0,
            payload,
        });

        assert!(matches!(flow, Flow::Continue(())));
        assert!(
            matches!(
                pipe.local_mut().take_decoded(),
                Some(DiscoveryMessage::GetPeers { .. })
            ),
            "the decoded message is handed off to the async runner"
        );
    }

    /// A frame whose envelope type is not the discovery type is rejected by the
    /// guard before decode, and nothing is handed off.
    #[test]
    fn run_one_rejects_non_discovery_frame_type() {
        let mut pipe = discovery_pipe(peer());

        let flow = pipe.run_one(Frame {
            message_type: DISCOVERY_FRAME_MESSAGE_TYPE + 1,
            flags: 0,
            payload: Vec::new(),
        });

        assert!(matches!(flow, Flow::Reject(SinkReject::Protocol(_))));
        assert!(pipe.local_mut().take_decoded().is_none());
    }

    /// A discovery-typed frame with non-zero flags passes the guard but is
    /// rejected at decode, and nothing is handed off.
    #[test]
    fn run_one_rejects_unexpected_frame_flags() {
        let mut pipe = discovery_pipe(peer());

        let flow = pipe.run_one(Frame {
            message_type: DISCOVERY_FRAME_MESSAGE_TYPE,
            flags: 1,
            payload: Vec::new(),
        });

        assert!(matches!(flow, Flow::Reject(SinkReject::Protocol(_))));
        assert!(pipe.local_mut().take_decoded().is_none());
    }

    #[test]
    fn pipe_shape_matches_runtime() {
        PIPE_SHAPE
            .validate()
            .expect("discovery PIPE_SHAPE edges name only real nodes");

        // The runtime branch is the decoded discovery message variant; the
        // exhaustive non-wildcard `match &msg` in `run_inbound` is what fails the
        // build if a variant is added without a branch, so the shape only needs to
        // carry one edge per current variant.
        let branches: Vec<&str> = PIPE_SHAPE
            .edges
            .iter()
            .filter(|edge| edge.from == "branch")
            .map(|edge| edge.on)
            .collect();

        assert_eq!(
            branches.len(),
            MESSAGE_BRANCHES.len(),
            "branch has exactly one edge per decoded discovery message variant"
        );
        for branch in MESSAGE_BRANCHES {
            assert!(
                branches.contains(&branch),
                "branch edge missing for runtime message variant {branch}"
            );
        }

        assert!(
            PIPE_SHAPE
                .edges
                .iter()
                .any(|edge| edge.from == "guard" && edge.to == "decode"),
            "admitted frames decode before branch"
        );
    }
}
