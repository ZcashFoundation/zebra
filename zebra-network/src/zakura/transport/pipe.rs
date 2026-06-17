//! The core per-peer pipe vocabulary shared by every Zakura service.
//!
//! A *pipe* is the single async function that drives one peer's ordered stream
//! from connect to disconnect. This module owns the shared abstraction — the
//! per-stage control flow ([`Flow`]), the per-peer context ([`PipeCx`]), the
//! checked-documentation DAG ([`PipeShape`]), the generic inbound runner
//! ([`Pipe`]/[`PipeSink`]), and the panic-containment launcher
//! ([`spawn_supervised_pipe`]). Services depend on this vocabulary but never
//! re-implement it.
//!
//! Anti-framework rule: [`PipeShape`] is documentation that is *checked*, never
//! an interpreter that is *executed*. Stages are plain `fn`s; the hot path is a
//! `match`, not a walk over `PipeShape.edges`.

use std::future::Future;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{Frame, FramedRecv, SinkReject};
use crate::zakura::transport::guard::{Admit, SessionGuard};
use crate::zakura::ZakuraPeerId;

/// Per-stage control flow.
///
/// Continue with a value, finish this frame cleanly (throttled/deduped — *not*
/// an error), or reject the peer.
pub(crate) enum Flow<T> {
    /// Continue the traversal carrying `T`.
    Continue(T),
    /// Finish this frame cleanly without rejecting the peer.
    Done,
    /// Reject the peer; the connection should close.
    Reject(SinkReject),
}

/// Per-peer context handed to every stage by reference.
///
/// Local state is owned here (`S`); shared state is reached through `env`.
pub(crate) struct PipeCx<'a, S, Env> {
    /// Authenticated identity of the peer this pipe drives.
    pub(crate) peer_id: &'a ZakuraPeerId,
    /// The per-peer state machine — no locking.
    pub(crate) local: &'a mut S,
    /// Arc-cloneable shared environment handle.
    pub(crate) env: &'a Env,
}

/// One inbound frame's whole traversal — the only fn-ptr indirection per frame.
pub(crate) type PipeEntry<S, Env> = fn(&mut PipeCx<'_, S, Env>, Frame) -> Flow<()>;

/// A node kind in a [`PipeShape`] DAG.
///
/// `Validate`/`Outcome`-style effect nodes and the `Stage`/`branch!`/`Outcome`
/// scaffolding for them are deliberately absent: today every service forwards a
/// decoded `WireMessage` to its compatibility reactor, so the live shapes only
/// use these variants. The effect-returning `Core` migration (see the pipelines
/// plan) reintroduces exactly what it needs when it lands.
#[derive(Copy, Clone, Debug)]
pub(crate) enum NodeKind {
    /// The shared protection stage.
    Guard,
    /// `Frame` to typed message.
    Decode,
    /// The single `match` on the typed message.
    Branch,
    /// Local/shared state mutation producing an effect.
    Mutate,
    /// Effect execution (the async sends/actions/fanout).
    Emit,
}

/// One node in a [`PipeShape`] DAG.
#[derive(Copy, Clone, Debug)]
// `PIPE_SHAPE` constructs these nodes in non-test lib code, but the fields are
// only *read* by the per-service `pipe_shape_matches_runtime` drift test, so the
// lib build still sees them as unread.
#[allow(dead_code)]
pub(crate) struct Node {
    /// Stable node identifier, referenced by [`Edge`]s.
    pub(crate) id: &'static str,
    /// The kind of work this node performs.
    pub(crate) kind: NodeKind,
}

/// One directed edge in a [`PipeShape`] DAG.
#[derive(Copy, Clone, Debug)]
// Read only by the per-service drift test (see [`Node`]).
#[allow(dead_code)]
pub(crate) struct Edge {
    /// Source [`Node::id`].
    pub(crate) from: &'static str,
    /// Destination [`Node::id`].
    pub(crate) to: &'static str,
    /// The condition (branch arm or stage result) this edge is taken on.
    pub(crate) on: &'static str,
}

/// The inspectable DAG for one service's per-peer pipe.
///
/// This is checked documentation: a per-service drift test asserts the runtime
/// `match` matches this shape. It is never walked to decide control flow.
#[derive(Copy, Clone, Debug)]
// Read only by the per-service drift test (see [`Node`]).
#[allow(dead_code)]
pub(crate) struct PipeShape {
    /// Stable service name for diagnostics.
    pub(crate) service: &'static str,
    /// Every node in the DAG.
    pub(crate) nodes: &'static [Node],
    /// Every directed edge in the DAG.
    pub(crate) edges: &'static [Edge],
}

impl PipeShape {
    /// Validate that every edge endpoint names a real node.
    ///
    /// This is the reusable half of the per-service drift test; the
    /// service-specific half (matching the runtime `match` arms) lands with each
    /// service migration. Called only from those `#[cfg(test)]` drift tests.
    #[allow(dead_code)] // called only from per-service drift tests
    pub(crate) fn validate(&self) -> Result<(), String> {
        for edge in self.edges {
            if !self.nodes.iter().any(|node| node.id == edge.from) {
                return Err(format!(
                    "{}: edge from unknown node {:?}",
                    self.service, edge.from
                ));
            }
            if !self.nodes.iter().any(|node| node.id == edge.to) {
                return Err(format!(
                    "{}: edge to unknown node {:?}",
                    self.service, edge.to
                ));
            }
        }
        Ok(())
    }
}

/// A per-peer pipe, built once per connected peer and consumed by the runner.
pub(crate) struct Pipe<S, Env> {
    peer_id: ZakuraPeerId,
    local: S,
    env: Env,
    guard: SessionGuard,
    entry: PipeEntry<S, Env>,
    // The declared shape is retained for per-service drift diagnostics; it is
    // checked documentation, never walked at runtime, so the runner never reads
    // it. The per-service drift test reads the same `&'static PipeShape`.
    #[allow(dead_code)] // retained for diagnostics; the runner never reads it
    shape: &'static PipeShape,
}

impl<S, Env> Pipe<S, Env> {
    /// Build a per-peer pipe from its local state, environment, guard, entry,
    /// and declared shape.
    pub(crate) fn new(
        peer_id: ZakuraPeerId,
        local: S,
        env: Env,
        guard: SessionGuard,
        entry: PipeEntry<S, Env>,
        shape: &'static PipeShape,
    ) -> Self {
        Self {
            peer_id,
            local,
            env,
            guard,
            entry,
            shape,
        }
    }

    /// Run one inbound frame: admit through the guard, then the entry traversal.
    ///
    /// This is the single inbound traversal used by both the production sink and
    /// the test/recorder path, so the two can never disagree.
    pub(crate) fn run_one(&mut self, frame: Frame) -> Flow<()> {
        match self.guard.admit(&frame) {
            Admit::Throttle => Flow::Done,
            Admit::Reject(reason) => Flow::Reject(SinkReject::protocol(reason)),
            Admit::Pass => {
                let mut cx = PipeCx {
                    peer_id: &self.peer_id,
                    local: &mut self.local,
                    env: &self.env,
                };
                (self.entry)(&mut cx, frame)
            }
        }
    }

    /// Borrow the peer-local state for service-specific side channels.
    pub(crate) fn local_mut(&mut self) -> &mut S {
        &mut self.local
    }
}

/// The generic inbound runner for a per-peer pipe.
///
/// `PipeSink::run` is the single inbound loop: `recv → guard.admit → entry →
/// map Flow`. block_sync drives its stream-6 inbound path through it directly;
/// header_sync and discovery fork their own loops (command draining / async
/// handler handoff) that are genuinely different shapes.
#[allow(dead_code)]
pub(crate) struct PipeSink<S, Env> {
    pipe: Pipe<S, Env>,
    recv: FramedRecv,
    cancel: CancellationToken,
}

#[allow(dead_code)]
impl<S, Env> PipeSink<S, Env>
where
    S: Send + 'static,
    Env: Send + Sync + 'static,
{
    /// Build the inbound runner around a pipe and its receive half.
    pub(crate) fn new(pipe: Pipe<S, Env>, recv: FramedRecv, cancel: CancellationToken) -> Self {
        Self { pipe, recv, cancel }
    }

    /// Run the inbound loop until the stream closes or the peer is cancelled.
    ///
    /// `Throttle` continues the loop, `Reject` returns `Err(SinkReject)`, and
    /// stream end or cancellation returns `Ok(())`.
    pub(crate) async fn run(mut self) -> Result<(), SinkReject> {
        loop {
            let frame = tokio::select! {
                () = self.cancel.cancelled() => return Ok(()),
                frame = self.recv.recv() => frame,
            };
            let Some(frame) = frame else {
                return Ok(());
            };
            match self.pipe.run_one(frame) {
                Flow::Continue(()) | Flow::Done => continue,
                Flow::Reject(reject) => return Err(reject),
            }
        }
    }
}

/// Cleanup that runs when a supervised pipe task ends, on every exit path.
///
/// Living in `Drop`, the teardown runs whether the pipe future returns normally,
/// returns after a reject, or unwinds on a panic — all from within the single
/// spawned pipe task, with no second `tokio::spawn`. This depends on the build
/// unwinding rather than aborting: under `panic = "unwind"` the `Drop` runs as
/// the panic unwinds the task and tokio catches the panic at the task boundary,
/// so the blast radius is exactly one peer (security_requirements.md SR-1). The
/// `#[cfg(panic = "abort")] compile_error!` above [`spawn_supervised_pipe`]
/// refuses to build the node with abort, where this `Drop` could not run and a
/// single peer panic would kill the whole node.
struct PipeTeardown<F: FnOnce(), P: FnOnce()> {
    peer_id: ZakuraPeerId,
    cancel: CancellationToken,
    on_teardown: Option<F>,
    on_panic: Option<P>,
}

impl<F: FnOnce(), P: FnOnce()> Drop for PipeTeardown<F, P> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            metrics::counter!("zakura.pipe.panic").increment(1);
            tracing::error!(
                peer_id = ?self.peer_id,
                "Zakura peer pipe panicked; disconnecting peer only"
            );
            if let Some(on_panic) = self.on_panic.take() {
                on_panic();
            }
        }
        self.cancel.cancel();
        if let Some(on_teardown) = self.on_teardown.take() {
            on_teardown();
        }
    }
}

// Peer-pipe panic containment (security_requirements.md SR-1) depends on
// unwinding: `PipeTeardown`'s `Drop` runs during the unwind to disconnect and
// clean up the panicked peer, and tokio catches the task panic so the blast
// radius is one peer. Under `panic = "abort"` none of that can happen — a single
// peer panic aborts the whole node — so refuse to build that way. The workspace
// [profile.*] tables set `panic = "unwind"`; this guard catches a silent
// regression back to abort.
#[cfg(panic = "abort")]
compile_error!(
    "Zakura peer-pipe panic containment requires `panic = \"unwind\"` \
     (security_requirements.md SR-1); this build sets `panic = \"abort\"`. \
     Set panic = \"unwind\" in the workspace [profile.dev] and [profile.release]."
);

/// Launch a per-peer pipe in its own supervised task.
///
/// This is the single way pipes are launched. The pipe runs inside one spawned
/// task guarded by a [`PipeTeardown`], which cancels the caller-supplied `cancel`
/// token and runs `on_teardown` (which must be idempotent) on every exit path —
/// normal return, reject, or panic. On panic only, it also runs `on_panic`.
/// Callers pass the token whose cancellation is safe on *every* exit (e.g. a
/// per-service token, not a shared connection token that other services ride
/// on); connection-level teardown that must only happen on a fatal reject
/// belongs inside the `pipe` future (see [`handle_pipe_exit`]), and
/// connection-level teardown for panic belongs in `on_panic`. A panicking peer is
/// contained to its own task without a nested `tokio::spawn`.
///
/// Returns the task's [`JoinHandle`]: services let it drop to detach the task (it
/// self-reaps; the `PipeTeardown` still runs on every exit), while the
/// panic-containment test awaits it to observe the contained panic.
pub(crate) fn spawn_supervised_pipe(
    peer_id: ZakuraPeerId,
    cancel: CancellationToken,
    on_teardown: impl FnOnce() + Send + 'static,
    on_panic: impl FnOnce() + Send + 'static,
    pipe: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let _teardown = PipeTeardown {
            peer_id,
            cancel,
            on_teardown: Some(on_teardown),
            on_panic: Some(on_panic),
        };
        pipe.await;
    })
}

/// Cleanup that runs when a supervised *non-pipe* peer task ends, on every exit
/// path.
///
/// This is the task-level sibling of [`PipeTeardown`] for the peer-influenced
/// service tasks that are not the generic inbound [`Pipe`] runner — e.g. the
/// discovery source/admission helpers and (once adopted) the legacy gossip
/// replay/receive loops. Unlike [`PipeTeardown`] it owns no [`CancellationToken`]
/// of its own: those tasks decide which token(s) a panic must cancel from inside
/// their `on_panic` hook, because some of them ride directly on the shared
/// connection token, which must *not* be cancelled on a normal exit (a clean
/// stream-end of one service must not tear the whole connection down). On panic
/// it runs `on_panic` during the unwind; on every exit it runs `on_teardown`.
struct PeerTaskTeardown<F: FnOnce(), P: FnOnce()> {
    peer_id: ZakuraPeerId,
    on_teardown: Option<F>,
    on_panic: Option<P>,
}

impl<F: FnOnce(), P: FnOnce()> Drop for PeerTaskTeardown<F, P> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            metrics::counter!("zakura.pipe.panic").increment(1);
            tracing::error!(
                peer_id = ?self.peer_id,
                "Zakura peer task panicked; disconnecting peer only"
            );
            if let Some(on_panic) = self.on_panic.take() {
                on_panic();
            }
        }
        if let Some(on_teardown) = self.on_teardown.take() {
            on_teardown();
        }
    }
}

/// Launch a peer-influenced, non-pipe service task in its own supervised task.
///
/// This is the task-level counterpart to [`spawn_supervised_pipe`] for the
/// peer-driven helper tasks that are *not* the generic inbound pipe runner
/// (discovery source/admission, legacy gossip replay/receive). The task runs
/// inside one spawned task guarded by a [`PeerTaskTeardown`], which runs
/// `on_teardown` (which must be idempotent) on every exit path — normal return
/// or panic — and `on_panic` on panic only. Callers put whatever connection /
/// service cancellation a panic requires inside `on_panic`, so a buggy or hostile
/// peer that panics one of these tasks still disconnects *that one peer* and runs
/// its cleanup instead of leaving stale service state behind a half-live
/// connection (security_requirements.md SR-1). Like [`spawn_supervised_pipe`]
/// this depends on the build unwinding rather than aborting; the
/// `#[cfg(panic = "abort")] compile_error!` above guards that for both wrappers.
///
/// Returns the task's [`JoinHandle`]: services let it drop to detach the task (it
/// self-reaps; the `PeerTaskTeardown` still runs on every exit), while the
/// panic-containment test awaits it to observe the contained panic.
pub(crate) fn spawn_supervised_peer_task(
    peer_id: ZakuraPeerId,
    on_teardown: impl FnOnce() + Send + 'static,
    on_panic: impl FnOnce() + Send + 'static,
    task: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let _teardown = PeerTaskTeardown {
            peer_id,
            on_teardown: Some(on_teardown),
            on_panic: Some(on_panic),
        };
        task.await;
    })
}

/// Map a finished pipe run to its connection-teardown effect — the single place
/// the "is this exit fatal to the whole connection?" decision lives.
///
/// A protocol reject is fatal: it cancels the shared `connection_cancel` token so
/// the whole connection tears down. A local reject (e.g. a closed service queue)
/// tears down only this stream — the per-service token is already cancelled by
/// the [`PipeTeardown`] — so it is logged and the connection is left for other
/// services. `Ok` is a normal/parked exit and does nothing here. Panic-path
/// connection teardown is separate (`on_panic`), because a panic never returns a
/// `Result` to inspect.
pub(crate) fn handle_pipe_exit(
    service: &'static str,
    connection_cancel: &CancellationToken,
    result: Result<(), SinkReject>,
) {
    match result {
        Ok(()) => {}
        Err(SinkReject::Protocol(error)) => {
            tracing::debug!(
                ?error,
                service,
                "Zakura stream rejected protocol-invalid frame"
            );
            connection_cancel.cancel();
        }
        Err(SinkReject::Local(error)) => {
            tracing::debug!(?error, service, "Zakura stream stopped on local error");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::zakura::transport::framed_channel;
    use crate::zakura::transport::guard::ByteBudget;

    const SHAPE: PipeShape = PipeShape {
        service: "test",
        nodes: &[
            Node {
                id: "guard",
                kind: NodeKind::Guard,
            },
            Node {
                id: "decode",
                kind: NodeKind::Decode,
            },
        ],
        edges: &[Edge {
            from: "guard",
            to: "decode",
            on: "Pass",
        }],
    };

    const DANGLING_SHAPE: PipeShape = PipeShape {
        service: "test-dangling",
        nodes: &[Node {
            id: "guard",
            kind: NodeKind::Guard,
        }],
        edges: &[Edge {
            from: "guard",
            to: "missing",
            on: "Pass",
        }],
    };

    fn peer_id() -> ZakuraPeerId {
        ZakuraPeerId::new(vec![1, 2, 3]).expect("3-byte id is within the node-id bound")
    }

    fn noop_entry(_cx: &mut PipeCx<'_, (), ()>, _frame: Frame) -> Flow<()> {
        Flow::Continue(())
    }

    fn pass_guard() -> SessionGuard {
        // Allow message type 1, generous size cap, no byte budget.
        SessionGuard::new(&[1], 1_024, None)
    }

    fn frame(message_type: u16) -> Frame {
        Frame {
            message_type,
            flags: 0,
            payload: Vec::new(),
        }
    }

    #[test]
    fn pipe_shape_validate_accepts_consistent_graph() {
        assert!(SHAPE.validate().is_ok());
    }

    #[test]
    fn pipe_shape_validate_rejects_dangling_edge() {
        let err = DANGLING_SHAPE
            .validate()
            .expect_err("dangling edge target must fail validation");
        assert!(err.contains("missing"), "error names the bad node: {err}");
    }

    #[test]
    fn run_one_passes_admitted_frame_to_entry() {
        let mut pipe = Pipe::new(peer_id(), (), (), pass_guard(), noop_entry, &SHAPE);
        assert!(matches!(pipe.run_one(frame(1)), Flow::Continue(())));
    }

    #[test]
    fn run_one_throttles_when_budget_exhausted() {
        // A budget that admits one 4-byte payload then throttles the next.
        let guard = SessionGuard::new(&[1], 1_024, Some(ByteBudget::new(4)));
        let mut pipe = Pipe::new(peer_id(), (), (), guard, noop_entry, &SHAPE);
        let mut framed = frame(1);
        framed.payload = vec![0u8; 4];
        assert!(matches!(pipe.run_one(framed.clone()), Flow::Continue(())));
        assert!(matches!(pipe.run_one(framed), Flow::Done));
    }

    #[test]
    fn run_one_rejects_disallowed_type() {
        let mut pipe = Pipe::new(peer_id(), (), (), pass_guard(), noop_entry, &SHAPE);
        assert!(matches!(pipe.run_one(frame(2)), Flow::Reject(_)));
    }

    #[tokio::test]
    async fn pipe_sink_run_returns_ok_on_stream_end() {
        let (send, recv) = framed_channel(4);
        let cancel = CancellationToken::new();
        let pipe = Pipe::new(peer_id(), (), (), pass_guard(), noop_entry, &SHAPE);
        let sink = PipeSink::new(pipe, recv, cancel);
        // One admitted frame, then close the stream by dropping the sender.
        send.send(frame(1)).await.expect("channel has capacity");
        drop(send);
        assert!(sink.run().await.is_ok());
    }

    #[tokio::test]
    async fn pipe_sink_run_returns_err_on_reject() {
        let (send, recv) = framed_channel(4);
        let cancel = CancellationToken::new();
        let pipe = Pipe::new(peer_id(), (), (), pass_guard(), noop_entry, &SHAPE);
        let sink = PipeSink::new(pipe, recv, cancel);
        // A disallowed type rejects the peer.
        send.send(frame(2)).await.expect("channel has capacity");
        assert!(sink.run().await.is_err());
    }

    #[tokio::test]
    async fn supervised_pipe_runs_teardown_on_panic() {
        let cancel = CancellationToken::new();
        let torn_down = Arc::new(AtomicBool::new(false));
        let flag = torn_down.clone();
        let panic_disconnected = Arc::new(AtomicBool::new(false));
        let panic_flag = panic_disconnected.clone();

        // Production drops this handle to detach the task; here we await it to
        // observe the contained panic.
        let handle = spawn_supervised_pipe(
            peer_id(),
            cancel.clone(),
            move || flag.store(true, Ordering::SeqCst),
            move || panic_flag.store(true, Ordering::SeqCst),
            async {
                panic!("peer pipe panics");
            },
        );

        // The pipe runs in a single task with no nested unwind-isolation spawn,
        // so the task itself surfaces the panic to its `JoinHandle`. The
        // `Drop`-based teardown still runs during the unwind, so cleanup is
        // guaranteed and the panic is contained to this one task.
        let join_error = handle
            .await
            .expect_err("a panicking pipe surfaces a join error");
        assert!(
            join_error.is_panic(),
            "the pipe panic is reported as a panic, not a cancellation"
        );

        assert!(
            torn_down.load(Ordering::SeqCst),
            "teardown runs even when the pipe panics"
        );
        assert!(
            panic_disconnected.load(Ordering::SeqCst),
            "panic-only disconnect hook runs when the pipe panics"
        );
        assert!(cancel.is_cancelled(), "the peer connection is cancelled");
    }

    #[tokio::test]
    async fn supervised_peer_task_runs_teardown_and_disconnect_on_panic() {
        // The surprising input: a peer-influenced helper task (e.g. discovery
        // source/admission, legacy gossip recv loop) panics *after* its per-peer
        // service state is registered, before its normal-path cleanup runs.
        let torn_down = Arc::new(AtomicBool::new(false));
        let teardown_flag = torn_down.clone();
        let disconnected = Arc::new(AtomicBool::new(false));
        let disconnect_flag = disconnected.clone();

        // Production drops this handle to detach the task; here we await it to
        // observe the contained panic.
        let handle = spawn_supervised_peer_task(
            peer_id(),
            move || teardown_flag.store(true, Ordering::SeqCst),
            move || disconnect_flag.store(true, Ordering::SeqCst),
            async {
                panic!("peer task panics after state registration");
            },
        );

        let join_error = handle
            .await
            .expect_err("a panicking peer task surfaces a join error");
        assert!(
            join_error.is_panic(),
            "the task panic is reported as a panic, not a cancellation"
        );
        // The safe expectation (SR-1): the panic still ran cleanup and the
        // peer-disconnect hook, so no stale service state survives behind a
        // half-live connection.
        assert!(
            torn_down.load(Ordering::SeqCst),
            "teardown runs even when the peer task panics"
        );
        assert!(
            disconnected.load(Ordering::SeqCst),
            "panic-only disconnect hook runs when the peer task panics"
        );
    }

    #[tokio::test]
    async fn supervised_peer_task_skips_disconnect_on_normal_exit() {
        let torn_down = Arc::new(AtomicBool::new(false));
        let teardown_flag = torn_down.clone();
        let disconnected = Arc::new(AtomicBool::new(false));
        let disconnect_flag = disconnected.clone();

        let handle = spawn_supervised_peer_task(
            peer_id(),
            move || teardown_flag.store(true, Ordering::SeqCst),
            move || disconnect_flag.store(true, Ordering::SeqCst),
            async {},
        );

        handle
            .await
            .expect("a normal peer task exit does not panic");
        assert!(
            torn_down.load(Ordering::SeqCst),
            "teardown runs on a normal exit"
        );
        // A clean exit (e.g. one service's stream ends) must NOT trip the
        // panic-only disconnect — that would tear down peers that rode on the
        // same connection.
        assert!(
            !disconnected.load(Ordering::SeqCst),
            "the panic-only disconnect hook must not fire on a normal exit"
        );
    }
}
