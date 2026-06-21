use super::{config::*, events::*, wire::*, *};
use crate::zakura::{
    handle_pipe_exit, spawn_supervised_pipe, FramedRecv, FramedSend, OrderedSendError, Peer,
    PeerStreamSession, Service, SinkReject, Stream, StreamMode, ZakuraPeerId, FRAME_HEADER_BYTES,
};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum frame bytes for one stream-6 body frame plus protocol framing.
///
/// A block body is still decoded and validated against Zebra's
/// `MAX_BLOCK_BYTES`; this frame cap has extra slack so stream-6 can classify
/// oversized or incompatible block-sync payloads in the codec instead of
/// dropping them at the raw transport gate.
pub const MAX_BS_FRAME_BYTES: u32 = {
    // This cast is safe: MAX_BS_MESSAGE_BYTES is asserted below 4 MiB.
    (MAX_BS_MESSAGE_BYTES + FRAME_HEADER_BYTES) as u32
};

const BLOCK_SYNC_SERVICE_STREAMS: [Stream; 1] = [Stream {
    kind: ZAKURA_STREAM_BLOCK_SYNC,
    version: ZAKURA_BLOCK_SYNC_STREAM_VERSION,
    frame_cap: MAX_BS_FRAME_BYTES,
    capability: ZAKURA_CAP_BLOCK_SYNC,
    mode: StreamMode::Ordered,
}];

/// Service-declared streams for native block sync.
pub(crate) fn block_sync_streams() -> &'static [Stream] {
    &BLOCK_SYNC_SERVICE_STREAMS
}

/// Cloneable typed stream-6 sender.
#[derive(Clone, Debug)]
pub struct BlockSyncPeerSession {
    peer_id: ZakuraPeerId,
    direction: ServicePeerDirection,
    send: FramedSend,
    cancel_token: CancellationToken,
}

impl BlockSyncPeerSession {
    pub(crate) fn new(session: &PeerStreamSession, direction: ServicePeerDirection) -> Self {
        Self {
            peer_id: session.peer_id().clone(),
            direction,
            send: session.sender(),
            cancel_token: session.cancel_token(),
        }
    }

    /// Authenticated peer identity for this block-sync session.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    /// Direction of the underlying Zakura connection.
    pub fn direction(&self) -> ServicePeerDirection {
        self.direction
    }

    /// Peer disconnect/local shutdown cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Send a typed status advertisement.
    pub fn try_send_status(&self, status: BlockSyncStatus) -> Result<(), OrderedSendError> {
        self.try_send_message(BlockSyncMessage::Status(status))
    }

    /// Send a typed block range request.
    pub fn try_send_get_blocks(
        &self,
        start_height: block::Height,
        count: u32,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(BlockSyncMessage::GetBlocks {
            start_height,
            count,
        })
    }

    /// Send one typed block body frame.
    pub fn try_send_block(&self, block: Arc<block::Block>) -> Result<(), OrderedSendError> {
        self.try_send_message(BlockSyncMessage::Block(block))
    }

    /// Send one typed block body frame, waiting for transport queue capacity.
    pub async fn send_block(&self, block: Arc<block::Block>) -> Result<(), OrderedSendError> {
        self.send_message(BlockSyncMessage::Block(block)).await
    }

    /// Send a typed response terminator.
    pub fn try_send_blocks_done(
        &self,
        start_height: block::Height,
        returned: u32,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(BlockSyncMessage::BlocksDone {
            start_height,
            returned,
        })
    }

    /// Send a typed response terminator, waiting for transport queue capacity.
    pub async fn send_blocks_done(
        &self,
        start_height: block::Height,
        returned: u32,
    ) -> Result<(), OrderedSendError> {
        self.send_message(BlockSyncMessage::BlocksDone {
            start_height,
            returned,
        })
        .await
    }

    /// Send a typed unavailable-range response.
    pub fn try_send_range_unavailable(
        &self,
        start_height: block::Height,
        count: u32,
    ) -> Result<(), OrderedSendError> {
        self.try_send_message(BlockSyncMessage::RangeUnavailable {
            start_height,
            count,
        })
    }

    /// Send a typed unavailable-range response, waiting for transport queue capacity.
    pub async fn send_range_unavailable(
        &self,
        start_height: block::Height,
        count: u32,
    ) -> Result<(), OrderedSendError> {
        self.send_message(BlockSyncMessage::RangeUnavailable {
            start_height,
            count,
        })
        .await
    }

    fn try_send_message(&self, msg: BlockSyncMessage) -> Result<(), OrderedSendError> {
        let frame = msg
            .encode_frame()
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        match self.send.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_frame)) => Err(OrderedSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_frame)) => Err(OrderedSendError::Closed),
        }
    }

    async fn send_message(&self, msg: BlockSyncMessage) -> Result<(), OrderedSendError> {
        let frame = msg
            .encode_frame()
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        self.send
            .send(frame)
            .await
            .map_err(|_error| OrderedSendError::Closed)
    }
}

/// Native stream-6 block-sync service scaffold.
#[derive(Debug)]
pub(crate) struct BlockSyncService {
    inner: Arc<BlockSyncServiceInner>,
    _held_events: Option<Arc<StdMutex<mpsc::Receiver<BlockSyncEvent>>>>,
    _reactor_task: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct BlockSyncServiceInner {
    config: ZakuraBlockSyncConfig,
    lifecycle: mpsc::UnboundedSender<BlockSyncEvent>,
    /// Shared download primitives every per-peer pipe-routine is wired with at
    /// `add_peer` (S4). `None` for the inert/handle-less constructors that never
    /// spawn routines (they only observe `events`/`lifecycle`).
    routine_wiring: Option<super::state::RoutineWiring>,
    peers: StdMutex<HashMap<ZakuraPeerId, BlockSyncPeerRecord>>,
    next_session_id: AtomicU64,
}

#[derive(Debug)]
struct BlockSyncPeerRecord {
    session_id: u64,
    direction: ServicePeerDirection,
    cancel_token: CancellationToken,
}

impl BlockSyncService {
    pub(crate) fn new(config: ZakuraBlockSyncConfig) -> Self {
        Self::new_with_startup(BlockSyncStartup::inert(config))
    }

    pub(crate) fn new_with_handle(config: ZakuraBlockSyncConfig, handle: BlockSyncHandle) -> Self {
        Self {
            inner: Arc::new(BlockSyncServiceInner {
                config,
                lifecycle: handle.lifecycle.clone(),
                routine_wiring: handle.routine_wiring.clone(),
                peers: StdMutex::new(HashMap::new()),
                next_session_id: AtomicU64::new(1),
            }),
            _held_events: None,
            _reactor_task: None,
        }
    }

    pub(crate) fn new_with_header_tip(
        config: ZakuraBlockSyncConfig,
        header_tip: watch::Receiver<(block::Height, block::Hash)>,
    ) -> Self {
        let best_header_tip = *header_tip.borrow();
        let startup = BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height::MIN,
                verified_block_tip: block::Height::MIN,
                verified_block_hash: block::Hash([0; 32]),
            },
            best_header_tip,
            header_tip,
            config,
        );
        Self::new_with_startup(startup)
    }

    fn new_with_startup(startup: BlockSyncStartup) -> Self {
        let config = startup.config.clone();
        let (handle, _actions, reactor_task) = spawn_block_sync_reactor(startup);
        Self {
            inner: Arc::new(BlockSyncServiceInner {
                config,
                lifecycle: handle.lifecycle.clone(),
                routine_wiring: handle.routine_wiring.clone(),
                peers: StdMutex::new(HashMap::new()),
                next_session_id: AtomicU64::new(1),
            }),
            _held_events: None,
            _reactor_task: Some(reactor_task),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        config: ZakuraBlockSyncConfig,
    ) -> (Self, mpsc::Receiver<BlockSyncEvent>) {
        let (events, event_rx) = mpsc::channel(config.peer_limits.inbound_queue_depth.max(1));
        let (lifecycle, mut lifecycle_rx) = mpsc::unbounded_channel();
        let events_for_lifecycle = events.clone();
        tokio::spawn(async move {
            while let Some(event) = lifecycle_rx.recv().await {
                let _ = events_for_lifecycle.send(event).await;
            }
        });
        (
            Self {
                inner: Arc::new(BlockSyncServiceInner {
                    config,
                    lifecycle,
                    routine_wiring: None,
                    peers: StdMutex::new(HashMap::new()),
                    next_session_id: AtomicU64::new(1),
                }),
                _held_events: None,
                _reactor_task: None,
            },
            event_rx,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_handle_for_test(
        config: ZakuraBlockSyncConfig,
        handle: BlockSyncHandle,
    ) -> Self {
        Self::new_with_handle(config, handle)
    }

    #[cfg(test)]
    pub(crate) fn peer_count(&self) -> usize {
        self.inner
            .peers
            .lock()
            .expect("block-sync peer map mutex is never poisoned")
            .len()
    }

    fn peer_slots_free(&self, direction: ServicePeerDirection) -> bool {
        let peers = self
            .inner
            .peers
            .lock()
            .expect("block-sync peer map mutex is never poisoned");
        let count = peers
            .values()
            .filter(|record| record.direction == direction)
            .count();
        let cap = match direction {
            ServicePeerDirection::Inbound => self.inner.config.peer_limits.max_inbound_peers,
            ServicePeerDirection::Outbound => self.inner.config.peer_limits.max_outbound_peers,
        };
        count < cap
    }

    /// Whether `add_peer` may install a session for this peer. A peer that is
    /// already registered may always *replace* its session (the
    /// connection-symmetry collision where both sides opened a block-sync stream
    /// resolves to the winner's stream by replacing the loser's already-counted
    /// session); only genuinely new peers are held to the per-direction cap.
    fn can_admit_peer(&self, peer_id: &ZakuraPeerId, direction: ServicePeerDirection) -> bool {
        let peers = self
            .inner
            .peers
            .lock()
            .expect("block-sync peer map mutex is never poisoned");
        if peers.contains_key(peer_id) {
            return true;
        }
        let count = peers
            .values()
            .filter(|record| record.direction == direction)
            .count();
        let cap = match direction {
            ServicePeerDirection::Inbound => self.inner.config.peer_limits.max_inbound_peers,
            ServicePeerDirection::Outbound => self.inner.config.peer_limits.max_outbound_peers,
        };
        count < cap
    }
}

impl Service for BlockSyncService {
    fn name(&self) -> &'static str {
        "block-sync"
    }

    fn streams(&self) -> &[Stream] {
        block_sync_streams()
    }

    fn wants_peer(
        &self,
        _peer: &ZakuraPeerId,
        _negotiated: u64,
        direction: ServicePeerDirection,
    ) -> bool {
        self.peer_slots_free(direction)
    }

    fn add_peer(&self, mut peer: Peer) {
        if !self.can_admit_peer(&peer.id, peer.direction) {
            return;
        }

        let Some((recv, send)) = peer.take_stream(ZAKURA_STREAM_BLOCK_SYNC) else {
            return;
        };

        let peer_id = peer.id.clone();
        let session = PeerStreamSession::new(
            peer_id.clone(),
            ZAKURA_STREAM_BLOCK_SYNC,
            recv,
            send,
            peer.service_cancel_token(),
        );
        let service_cancel_token = session.cancel_token();
        let connection_cancel_token = peer.cancel_token();
        let block_sync_session = BlockSyncPeerSession::new(&session, peer.direction);
        let session_id = self.inner.next_session_id.fetch_add(1, Ordering::Relaxed);
        let (_session_peer, _stream_kind, recv, send, _session_cancel) = session.into_parts();

        // Production outbound block-sync frames go directly through
        // `BlockSyncPeerSession` (the per-peer routine's `try_send_get_blocks` /
        // the reactor's `try_send_status`/serving sends), so the raw transport
        // sender taken from the stream here is redundant. The outbound stream stays
        // alive through the `BlockSyncPeerSession` clone the reactor holds, so
        // nothing is lost by dropping it.
        drop(send);

        let run_cancel = service_cancel_token.clone();
        let on_teardown = {
            let lifecycle = self.inner.lifecycle.clone();
            let peer_id = peer_id.clone();
            let inner = self.inner.clone();
            move || {
                let should_notify = if let Ok(mut peers) = inner.peers.lock() {
                    if peers
                        .get(&peer_id)
                        .is_some_and(|record| record.session_id == session_id)
                    {
                        peers.remove(&peer_id);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if should_notify {
                    let _ = lifecycle.send(BlockSyncEvent::PeerDisconnected(peer_id));
                }
            }
        };
        let on_panic = {
            let connection_cancel_token = connection_cancel_token.clone();
            move || connection_cancel_token.cancel()
        };
        // S4: the per-peer pipe-routine is spawned HERE (the pipe spawn point), so
        // a protocol reject still cancels the whole connection via
        // `handle_pipe_exit`. The routine owns `recv` (the transport read), decodes
        // each frame, and runs the download/serving dispatch in its own task —
        // there is no reactor inbound demux. When the service has no reactor wiring
        // (inert/handle-less test constructors) there is no routine to run; drain
        // the stream so frames are not silently mishandled and the lifecycle still
        // flows.
        let pipe = {
            let connection_cancel_token = connection_cancel_token.clone();
            let routine_wiring = self.inner.routine_wiring.clone();
            let block_sync_session = block_sync_session.clone();
            let peer_id = peer_id.clone();
            let direction = peer.direction;
            async move {
                let result = match routine_wiring {
                    Some(wiring) => {
                        let generation = wiring.registry.admit(&peer_id, direction, &wiring.config);
                        let routine = super::peer_routine::PeerRoutine::new(
                            peer_id,
                            block_sync_session,
                            recv,
                            wiring.config,
                            generation,
                            wiring.budget,
                            wiring.work,
                            wiring.registry,
                            wiring.received_throughput,
                            wiring.sequencer_input,
                            wiring.actions,
                            wiring.routine_to_reactor,
                            wiring.view,
                            run_cancel,
                            wiring.trace,
                        );
                        routine.run().await
                    }
                    None => drain_inbound(recv, run_cancel).await,
                };
                handle_pipe_exit("block-sync", &connection_cancel_token, result);
            }
        };
        // Let the returned handle drop to detach the supervised task (like
        // `tokio::spawn`); the `PipeTeardown` still runs on every exit path.
        spawn_supervised_pipe(
            peer_id.clone(),
            service_cancel_token.clone(),
            on_teardown,
            on_panic,
            pipe,
        );

        {
            let mut peers = self
                .inner
                .peers
                .lock()
                .expect("block-sync peer map mutex is never poisoned");
            if let Some(old_record) = peers.insert(
                peer_id.clone(),
                BlockSyncPeerRecord {
                    session_id,
                    direction: peer.direction,
                    cancel_token: service_cancel_token,
                },
            ) {
                old_record.cancel_token.cancel();
            }
        }

        let _ = self
            .inner
            .lifecycle
            .send(BlockSyncEvent::PeerConnected(block_sync_session));
    }

    fn remove_peer(&self, peer: &ZakuraPeerId) {
        let removed = self
            .inner
            .peers
            .lock()
            .expect("block-sync peer map mutex is never poisoned")
            .remove(peer);
        if let Some(record) = removed {
            record.cancel_token.cancel();
        }
        let _ = self
            .inner
            .lifecycle
            .send(BlockSyncEvent::PeerDisconnected(peer.clone()));
    }

    fn deliver_frame(
        &self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _frame: Frame,
    ) -> Result<(), SinkReject> {
        // S4 inverted the inbound data flow: block sync is an `Ordered` stream
        // whose `FramedRecv` is taken by `add_peer` and owned by the per-peer
        // pipe-routine ([`PeerRoutine`](super::peer_routine)), which decodes and
        // dispatches every frame in its own task. The `Service::deliver_frame`
        // entry point (driven only by the testkit recorder / `registry.deliver`,
        // never the production ordered-stream reader) therefore has no routine to
        // route into and no reactor inbound path to emit to. It is not the
        // block-sync inbound path; accept-and-ignore rather than constructing a
        // detached one-shot decode that could never reach the owning routine. No
        // production frame reaches here (the routine consumes the stream), so this
        // drops nothing that the routine would otherwise handle.
        Ok(())
    }
}

/// Drain a peer's inbound block-sync stream when the service has no reactor
/// wiring to spawn a pipe-routine (the inert / handle-less test constructors).
/// Frames are read and discarded until cancellation or stream close, so the
/// transport reader makes progress and the lifecycle still fires; no routine
/// exists to act on them.
async fn drain_inbound(mut recv: FramedRecv, cancel: CancellationToken) -> Result<(), SinkReject> {
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            frame = recv.recv() => {
                if frame.is_none() {
                    return Ok(());
                }
            }
        }
    }
}
