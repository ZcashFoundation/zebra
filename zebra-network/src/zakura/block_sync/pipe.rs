//! block_sync/pipe.rs — the per-peer block-sync pipe (stream 6).
//!
//! THE PHASE-3A DAG SLICE IS THIS DIAGRAM. The code below is a mechanical
//! transcription; the [`PIPE_SHAPE`] const is the inspectable, drift-checked
//! copy of it.
//!
//!  recv ─▶ guard ─▶ decode ─▶ branch(msg)
//!                             ├─ Status           ─▶ emit(WireMessage)
//!                             ├─ GetBlocks        ─▶ emit(WireMessage)
//!                             ├─ Block            ─▶ emit(WireMessage)
//!                             ├─ BlocksDone       ─▶ emit(WireMessage)
//!                             └─ RangeUnavailable ─▶ emit(WireMessage)
//!
//! Phase 3a lifts stream-6 inbound processing onto the shared pipe while
//! retaining the compatibility reactor for semantic validate/mutate/emit work.
//! The pipe owns the single frame decode path used by both production and
//! `Service::deliver_frame`; decoded messages still flow to the reactor as
//! `WireMessage` events.

use std::sync::Arc;

use super::{events::*, wire::*, *};
use crate::zakura::{Edge, Flow, Node, NodeKind, PipeCx, PipeShape, SinkReject, ZakuraPeerId};

/// Per-peer block-sync local state.
///
/// Phase 3a has no peer-local block-sync state to move yet; the semantic peer
/// state remains in the compatibility reactor.
pub(super) struct BsLocal;

/// Shared environment handed to every block-sync pipe.
#[derive(Clone)]
pub(super) struct BsEnv {
    /// Bounded reactor event queue used for decoded stream-6 wire events.
    events: mpsc::Sender<BlockSyncEvent>,
}

impl BsEnv {
    /// Wrap the cloneable reactor event sender as the pipe's shared environment.
    pub(super) fn new(events: mpsc::Sender<BlockSyncEvent>) -> Self {
        Self { events }
    }
}

/// The Phase-3a block-sync pipe DAG slice, as checked documentation.
pub(super) const PIPE_SHAPE: PipeShape = PipeShape {
    service: "block-sync",
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
            id: "emit",
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
            to: "emit",
            on: "Status",
        },
        Edge {
            from: "branch",
            to: "emit",
            on: "GetBlocks",
        },
        Edge {
            from: "branch",
            to: "emit",
            on: "Block",
        },
        Edge {
            from: "branch",
            to: "emit",
            on: "BlocksDone",
        },
        Edge {
            from: "branch",
            to: "emit",
            on: "RangeUnavailable",
        },
    ],
};

/// Executable transcription of [`PIPE_SHAPE`] — the production entry function.
///
/// The shared guard has already admitted this frame before `run_inbound` is
/// reached. Decode failures are forwarded as `WireDecodeFailed` events and
/// reject the peer, matching the old sink path. Successful decodes branch by
/// message variant, then every compatibility branch emits the same `WireMessage`
/// event for the retained reactor to handle semantically.
pub(super) fn run_inbound(cx: &mut PipeCx<'_, BsLocal, BsEnv>, frame: Frame) -> Flow<()> {
    let msg = match decode(&cx.env.events, cx.peer_id.clone(), frame) {
        Flow::Continue(msg) => msg,
        Flow::Done => return Flow::Done,
        Flow::Reject(reject) => return Flow::Reject(reject),
    };

    match msg {
        BlockSyncMessage::Status(_)
        | BlockSyncMessage::GetBlocks { .. }
        | BlockSyncMessage::Block(_)
        | BlockSyncMessage::BlocksDone { .. }
        | BlockSyncMessage::RangeUnavailable { .. } => forward(
            &cx.env.events,
            BlockSyncEvent::WireMessage {
                peer: cx.peer_id.clone(),
                msg,
            },
        ),
    }
}

/// The single frame decode stage shared by production and `deliver_frame`.
fn decode(
    events: &mpsc::Sender<BlockSyncEvent>,
    peer_id: ZakuraPeerId,
    frame: Frame,
) -> Flow<BlockSyncMessage> {
    match BlockSyncMessage::decode_frame(frame) {
        Ok(msg) => Flow::Continue(msg),
        Err(error) => {
            // Block bodies are validated against committed headers in B1+.
            let protocol_error =
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
            let _ = events.try_send(BlockSyncEvent::WireDecodeFailed {
                peer: peer_id,
                error: Arc::new(error),
            });
            Flow::Reject(SinkReject::protocol(protocol_error))
        }
    }
}

/// Forward a successfully decoded inbound event to the compatibility reactor.
fn forward(events: &mpsc::Sender<BlockSyncEvent>, event: BlockSyncEvent) -> Flow<()> {
    match events.try_send(event) {
        Ok(()) => Flow::Continue(()),
        Err(error) => Flow::Reject(SinkReject::local(format!(
            "block-sync queue closed: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::service::block_sync_pipe;
    use super::*;

    const MESSAGE_BRANCHES: [&str; 5] = [
        "Status",
        "GetBlocks",
        "Block",
        "BlocksDone",
        "RangeUnavailable",
    ];

    fn peer() -> ZakuraPeerId {
        ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds")
    }

    /// A disallowed/unknown stream-6 frame type must reach the decode stage and
    /// surface as `WireDecodeFailed` (so the reactor records `MalformedMessage`),
    /// not get silently dropped by a pre-decode guard reject. This is the BS1
    /// regression guard: it builds the *real* production pipe, so reverting the
    /// guard back to an allowed-type filter would make this fail.
    #[test]
    fn run_one_unknown_type_reaches_decode_and_emits_wire_decode_failed() {
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let mut pipe = block_sync_pipe(peer(), events_tx);

        let flow = pipe.run_one(Frame {
            // 99 is outside the stream-6 message-type set; the codec rejects it.
            message_type: 99,
            flags: 0,
            payload: Vec::new(),
        });

        assert!(
            matches!(flow, Flow::Reject(SinkReject::Protocol(_))),
            "unknown type rejects the peer"
        );
        assert!(
            matches!(
                events_rx.try_recv(),
                Ok(BlockSyncEvent::WireDecodeFailed { .. })
            ),
            "unknown type still reports malformed-message misbehavior"
        );
    }

    /// A malformed payload of an otherwise-allowed type also decodes-then-rejects
    /// with `WireDecodeFailed`, unchanged by the BS1 fix.
    #[test]
    fn run_one_malformed_payload_emits_wire_decode_failed() {
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let mut pipe = block_sync_pipe(peer(), events_tx);

        let flow = pipe.run_one(Frame {
            message_type: u16::from(MSG_BS_STATUS),
            flags: 0,
            payload: Vec::new(),
        });

        assert!(matches!(flow, Flow::Reject(SinkReject::Protocol(_))));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(BlockSyncEvent::WireDecodeFailed { .. })
        ));
    }

    /// A well-formed stream-6 message decodes and forwards a `WireMessage` to the
    /// reactor, continuing the peer.
    #[test]
    fn run_one_valid_message_forwards_wire_message() {
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let peer = peer();
        let mut pipe = block_sync_pipe(peer.clone(), events_tx);

        let message = BlockSyncMessage::GetBlocks {
            start_height: block::Height(1),
            count: 4,
        };
        let frame = message.clone().encode_frame().expect("message encodes");

        let flow = pipe.run_one(frame);

        assert!(matches!(flow, Flow::Continue(())));
        match events_rx.try_recv() {
            Ok(BlockSyncEvent::WireMessage { peer: got, msg }) => {
                assert_eq!(got, peer);
                assert_eq!(msg, message);
            }
            other => panic!("expected WireMessage, got {other:?}"),
        }
    }

    #[test]
    fn pipe_shape_matches_runtime() {
        // (a) The declared shape is internally consistent.
        PIPE_SHAPE
            .validate()
            .expect("block-sync PIPE_SHAPE edges name only real nodes");

        // (b) Phase 3a's real runtime branch is the decoded stream-6 message
        // variant. Semantic handling remains in the compatibility reactor, so
        // every branch currently terminates at the single forward/emit stage.
        let branches: Vec<&str> = PIPE_SHAPE
            .edges
            .iter()
            .filter(|edge| edge.from == "branch")
            .map(|edge| edge.on)
            .collect();

        assert_eq!(
            branches.len(),
            MESSAGE_BRANCHES.len(),
            "branch has exactly one edge per decoded stream-6 message variant"
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
        assert!(
            PIPE_SHAPE
                .nodes
                .iter()
                .any(|node| node.id == "emit" && matches!(node.kind, NodeKind::Emit)),
            "the pipe terminates at a single compatibility emit node"
        );
    }
}
