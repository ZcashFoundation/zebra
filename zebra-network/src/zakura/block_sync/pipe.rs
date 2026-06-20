//! block_sync/pipe.rs — the per-peer block-sync pipe shape (stream 6).
//!
//! S4 inverted the inbound data flow: the per-peer pipe-routine
//! ([`PeerRoutine`](super::peer_routine)) owns its `FramedRecv`, decodes each
//! frame, and runs the download/serving dispatch in the **same task** — there is
//! no reactor inbound demux and no `WireMessage` event. This module keeps the
//! drift-checked [`PIPE_SHAPE`] (now describing the routine's decode → dispatch
//! shape) and the shared ingress [`block_sync_guard`] the routine applies before
//! decode.
//!
//!  recv ─▶ guard ─▶ decode ─▶ branch(msg)
//!                             ├─ Status           ─▶ local servable/caps + advertise
//!                             ├─ GetBlocks        ─▶ serve (reactor)
//!                             ├─ Block            ─▶ local match + Sequencer accept
//!                             ├─ BlocksDone       ─▶ local finish/retry
//!                             └─ RangeUnavailable ─▶ local retry
//!
//! A disallowed/unknown stream-6 type or a malformed payload surfaces as a
//! `MalformedMessage` misbehavior + a protocol reject (the routine's decode-error
//! path), rather than a pre-decode guard reject dropping the signal — see BS1.

use crate::zakura::{Edge, Node, NodeKind, PipeShape, SessionGuard};

use super::wire::MAX_BS_MESSAGE_BYTES;

pub(super) fn block_sync_guard() -> SessionGuard {
    // The transport already applies the per-connection count bucket and frame
    // cap; this guard adds the same payload cap the codec enforces. Type
    // validity is left to the decode stage on purpose: a disallowed or unknown
    // stream-6 type must surface as a `MalformedMessage` misbehavior + protocol
    // reject from the routine's decode-error path (see BS1), rather than a
    // pre-decode guard reject dropping that signal. The block-sync byte budget
    // likewise stays in the routine's reserve/reorder accounting so existing
    // request/retry accounting is not double-counted.
    SessionGuard::oversize_only(MAX_BS_MESSAGE_BYTES as u32)
}

/// The block-sync pipe-routine DAG slice, as checked documentation. After S4 the
/// branch terminals are the routine's local download/serving dispatch, not a
/// single `WireMessage` emit. Drift-checked by `pipe_shape_matches_runtime`;
/// it is documentation, so it is only referenced from that test.
#[cfg_attr(not(test), allow(dead_code))]
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
            id: "dispatch",
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
            to: "dispatch",
            on: "Status",
        },
        Edge {
            from: "branch",
            to: "dispatch",
            on: "GetBlocks",
        },
        Edge {
            from: "branch",
            to: "dispatch",
            on: "Block",
        },
        Edge {
            from: "branch",
            to: "dispatch",
            on: "BlocksDone",
        },
        Edge {
            from: "branch",
            to: "dispatch",
            on: "RangeUnavailable",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE_BRANCHES: [&str; 5] = [
        "Status",
        "GetBlocks",
        "Block",
        "BlocksDone",
        "RangeUnavailable",
    ];

    #[test]
    fn pipe_shape_matches_runtime() {
        // (a) The declared shape is internally consistent.
        PIPE_SHAPE
            .validate()
            .expect("block-sync PIPE_SHAPE edges name only real nodes");

        // (b) The runtime branch is the decoded stream-6 message variant. After
        // S4 every branch terminates at the routine's local download/serving
        // dispatch (the single `dispatch` node) rather than a `WireMessage` emit.
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
                .any(|node| node.id == "dispatch" && matches!(node.kind, NodeKind::Emit)),
            "the pipe terminates at the routine's local dispatch node"
        );
    }
}
